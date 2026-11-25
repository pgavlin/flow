//! Budgeted HTML5 tree sink. Stop DOM allocation before an operation would
//! exceed a budget, then stop feeding the tokenizer at the batch boundary.
//!
//! TreeSink cannot return allocation errors. Rejected creations therefore get
//! inert, named handles while mutations become no-ops. This lets the current
//! tokenizer batch finish without panicking or inserting sentinel nodes into the
//! DOM. No rejected tree is ever finalized as a document.

use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    collections::HashMap,
};

use dom_smoothie::dom_query::{Attr, Document, NodeId, TreeNode};
use html5ever::{
    Attribute, QualName,
    tendril::{StrTendril, TendrilSink},
    tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeSink},
};

use super::{
    HTML_NS, MAX_ATTRIBUTES, MAX_DEPTH, MAX_DOM_BYTES, MAX_NODES, MAX_PARSE_VISITS, MAX_SOURCE,
    PARSE_BATCH_BYTES, WebImportError, check_dom_with_work,
};

#[derive(Clone, Copy)]
struct Limits {
    nodes: usize,
    depth: usize,
    attributes: usize,
    bytes: usize,
    operations: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            nodes: MAX_NODES,
            depth: MAX_DEPTH,
            attributes: MAX_ATTRIBUTES,
            bytes: MAX_DOM_BYTES,
            operations: MAX_PARSE_VISITS,
        }
    }
}

#[derive(Clone, Debug)]
struct Handle {
    node: Option<NodeId>,
    name: QualName,
    rejected_id: usize,
    integration_point: bool,
}

struct BudgetSink {
    document: Document,
    limits: Limits,
    failed: Cell<bool>,
    bytes: Cell<usize>,
    operations: Cell<usize>,
    rejected_ids: Cell<usize>,
    template_owners: RefCell<HashMap<NodeId, NodeId>>,
    template_fragments: RefCell<HashMap<NodeId, NodeId>>,
}

impl BudgetSink {
    fn new(limits: Limits) -> Self {
        Self {
            document: Document::default(),
            limits,
            failed: Cell::new(limits.nodes == 0 || limits.bytes < std::mem::size_of::<TreeNode>()),
            bytes: Cell::new(std::mem::size_of::<TreeNode>()),
            operations: Cell::new(0),
            rejected_ids: Cell::new(0),
            template_owners: RefCell::new(HashMap::new()),
            template_fragments: RefCell::new(HashMap::new()),
        }
    }

    fn reserve(&self, nodes: usize, bytes: usize) -> bool {
        if self.failed.get() {
            return false;
        }
        // Charge structural storage as well as payload: empty attributes and
        // nodes must not be effectively free. Capacity/allocator overhead and
        // tokenizer scratch remain separate from this DOM accounting budget.
        let bytes = bytes.saturating_add(nodes.saturating_mul(std::mem::size_of::<TreeNode>()));
        let fits = self.document.tree.len().saturating_add(nodes) <= self.limits.nodes
            && self.bytes.get().saturating_add(bytes) <= self.limits.bytes
            && self.operations.get() < self.limits.operations;
        if !fits {
            self.failed.set(true);
            return false;
        }
        self.bytes.set(self.bytes.get() + bytes);
        self.operations.set(self.operations.get() + 1);
        true
    }

    fn handle(
        &self,
        node: Option<NodeId>,
        name: Option<QualName>,
        integration_point: bool,
    ) -> Handle {
        let rejected_id = if node.is_none() {
            let id = self.rejected_ids.get();
            self.rejected_ids.set(id + 1);
            id
        } else {
            0
        };
        Handle {
            node,
            name: name.unwrap_or_else(|| QualName::new(None, HTML_NS.into(), "html".into())),
            rejected_id,
            integration_point,
        }
    }

    fn attributes_fit(&self, count: usize) -> bool {
        if count > self.limits.attributes {
            self.failed.set(true);
        }
        !self.failed.get()
    }

    fn attribute_bytes(attrs: &[Attribute]) -> usize {
        attrs.iter().fold(0usize, |bytes, attr| {
            bytes
                .saturating_add(std::mem::size_of::<Attr>())
                .saturating_add(attr.name.local.len())
                .saturating_add(attr.name.prefix.as_ref().map_or(0, |prefix| prefix.len()))
                .saturating_add(attr.value.len())
        })
    }

    fn logical_parent(&self, node: NodeId) -> Option<NodeId> {
        self.document
            .tree
            .get(&node)
            .and_then(|node| node.parent())
            .map(|node| node.id)
            .or_else(|| self.template_owners.borrow().get(&node).copied())
    }

    fn attachment_fits(&self, parent: NodeId, child: Option<NodeId>) -> bool {
        let mut ancestor = Some(parent);
        let mut parent_depth = 0usize;
        while let Some(node) = ancestor {
            if !self.reserve(0, 0) {
                return false;
            }
            if Some(node) == child || parent_depth > self.limits.depth {
                self.failed.set(true);
                return false;
            }
            ancestor = self.logical_parent(node);
            if ancestor.is_some() {
                parent_depth += 1;
            }
        }
        if parent_depth >= self.limits.depth {
            self.failed.set(true);
            return false;
        }
        // Reparenting can move an existing subtree, not just a new element.
        // Include otherwise-detached template fragments in the logical tree.
        let mut pending = child
            .into_iter()
            .map(|child| (child, parent_depth + 1))
            .collect::<Vec<_>>();
        while let Some((node, depth)) = pending.pop() {
            if !self.reserve(0, 0) {
                return false;
            }
            if depth > self.limits.depth {
                self.failed.set(true);
                return false;
            }
            if let Some(node) = self.document.tree.get(&node) {
                pending.extend(node.children_it(false).map(|child| (child.id, depth + 1)));
            }
            if let Some(fragment) = self.template_fragments.borrow().get(&node).copied() {
                pending.push((fragment, depth + 1));
            }
        }
        true
    }

    fn append_at(&self, target: &Handle, child: NodeOrText<Handle>, before: bool) {
        if self.failed.get() {
            return;
        }
        let Some(target) = target.node else {
            return;
        };
        let parent = if before {
            self.logical_parent(target)
        } else {
            Some(target)
        };
        let child = match child {
            NodeOrText::AppendNode(child) => {
                if !self.reserve(0, 0) {
                    return;
                }
                let Some(child) = child.node else {
                    return;
                };
                if parent.is_some_and(|parent| !self.attachment_fits(parent, Some(child))) {
                    return;
                }
                NodeOrText::AppendNode(child)
            }
            NodeOrText::AppendText(text) => {
                let neighbor = if before {
                    self.document.tree.prev_sibling_of(&target)
                } else {
                    self.document.tree.last_child_of(&target)
                };
                let nodes = usize::from(!neighbor.is_some_and(|neighbor| neighbor.is_text()));
                if !self.reserve(nodes, text.len())
                    || parent.is_some_and(|parent| !self.attachment_fits(parent, None))
                {
                    return;
                }
                NodeOrText::AppendText(text)
            }
        };
        if before {
            self.document.append_before_sibling(&target, child);
        } else {
            self.document.append(&target, child);
        }
    }
}

impl TreeSink for BudgetSink {
    type Handle = Handle;
    type Output = Result<Document, WebImportError>;
    type ElemName<'a> = &'a QualName;

    fn finish(self) -> Self::Output {
        if self.failed.get() {
            Err(WebImportError::TooComplex)
        } else {
            Ok(self.document)
        }
    }

    fn parse_error(&self, _message: Cow<'static, str>) {
        // Recovery messages are not semantic diagnostics. Count their work,
        // without retaining an attacker-controlled error list.
        self.reserve(0, 0);
    }

    fn get_document(&self) -> Handle {
        self.handle(Some(self.document.get_document()), None, false)
    }

    fn elem_name<'a>(&'a self, target: &'a Handle) -> Self::ElemName<'a> {
        &target.name
    }

    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, flags: ElementFlags) -> Handle {
        let integration_point = flags.mathml_annotation_xml_integration_point;
        let template = flags.template;
        let bytes = name
            .local
            .len()
            .saturating_add(Self::attribute_bytes(&attrs));
        let node = if self.attributes_fit(attrs.len())
            && self.reserve(1 + usize::from(flags.template), bytes)
        {
            Some(self.document.create_element(name.clone(), attrs, flags))
        } else {
            None
        };
        if let Some(node) = node.filter(|_| template) {
            let fragment = self.document.get_template_contents(&node);
            self.template_owners.borrow_mut().insert(fragment, node);
            self.template_fragments.borrow_mut().insert(node, fragment);
        }
        self.handle(node, Some(name), integration_point)
    }

    fn create_comment(&self, text: StrTendril) -> Handle {
        let node = self
            .reserve(1, text.len())
            .then(|| self.document.create_comment(text));
        self.handle(node, None, false)
    }

    fn create_pi(&self, target: StrTendril, data: StrTendril) -> Handle {
        let node = self
            .reserve(1, target.len().saturating_add(data.len()))
            .then(|| self.document.create_pi(target, data));
        self.handle(node, None, false)
    }

    fn append(&self, parent: &Handle, child: NodeOrText<Handle>) {
        self.append_at(parent, child, false);
    }

    fn append_before_sibling(&self, sibling: &Handle, child: NodeOrText<Handle>) {
        self.append_at(sibling, child, true);
    }

    fn append_based_on_parent_node(
        &self,
        element: &Handle,
        prev_element: &Handle,
        child: NodeOrText<Handle>,
    ) {
        if self.failed.get() {
            return;
        }
        let has_parent = element
            .node
            .and_then(|id| self.document.tree.get(&id))
            .is_some_and(|node| node.parent().is_some());
        self.append_at(
            if has_parent { element } else { prev_element },
            child,
            has_parent,
        );
    }

    fn append_doctype_to_document(
        &self,
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    ) {
        if self.reserve(
            1,
            name.len()
                .saturating_add(public_id.len())
                .saturating_add(system_id.len()),
        ) {
            self.document
                .append_doctype_to_document(name, public_id, system_id);
        }
    }

    fn get_template_contents(&self, target: &Handle) -> Handle {
        // Real template fragments were reserved together with their element.
        let node = target
            .node
            .map(|node| self.document.get_template_contents(&node));
        self.handle(node, None, false)
    }

    fn same_node(&self, left: &Handle, right: &Handle) -> bool {
        match (left.node, right.node) {
            (Some(left), Some(right)) => left == right,
            (None, None) => left.rejected_id == right.rejected_id,
            _ => false,
        }
    }

    fn set_quirks_mode(&self, mode: QuirksMode) {
        if self.reserve(0, 0) {
            self.document.set_quirks_mode(mode);
        }
    }

    fn add_attrs_if_missing(&self, target: &Handle, attrs: Vec<Attribute>) {
        if self.failed.get() {
            return;
        }
        let Some(node) = target.node else {
            return;
        };
        let count = self
            .document
            .tree
            .get(&node)
            .map_or(0, |node| node.attrs().len());
        if self.attributes_fit(count.saturating_add(attrs.len()))
            && self.reserve(0, Self::attribute_bytes(&attrs))
        {
            self.document.add_attrs_if_missing(&node, attrs);
        }
    }

    fn remove_from_parent(&self, target: &Handle) {
        if self.reserve(0, 0)
            && let Some(node) = target.node
        {
            self.document.remove_from_parent(&node);
        }
    }

    fn reparent_children(&self, node: &Handle, new_parent: &Handle) {
        if self.reserve(0, 0)
            && let (Some(node), Some(parent)) = (node.node, new_parent.node)
        {
            let children = self
                .document
                .tree
                .get(&node)
                .map(|node| node.children())
                .unwrap_or_default();
            if children
                .iter()
                .all(|child| self.attachment_fits(parent, Some(child.id)))
            {
                self.document.reparent_children(&node, &parent);
            }
        }
    }

    fn is_mathml_annotation_xml_integration_point(&self, handle: &Handle) -> bool {
        handle.integration_point
    }
}

pub(super) fn parse_bounded(source: &str) -> Result<Document, WebImportError> {
    if source.len() > MAX_SOURCE {
        return Err(WebImportError::TooComplex);
    }
    let mut parser = html5ever::parse_document(
        BudgetSink::new(Limits::default()),
        html5ever::ParseOpts {
            tree_builder: html5ever::tree_builder::TreeBuilderOpts {
                scripting_enabled: false,
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let mut start = 0;
    let mut visits = 0;
    while start < source.len() {
        let mut end = (start + PARSE_BATCH_BYTES).min(source.len());
        while !source.is_char_boundary(end) {
            end -= 1;
        }
        parser.process(source[start..end].into());
        let sink = &parser.tokenizer.sink.sink;
        if sink.failed.get() {
            return Err(WebImportError::TooComplex);
        }
        check_dom_with_work(sink.document.root(), &mut visits)?;
        start = end;
    }
    let document = parser.finish()?;
    check_dom_with_work(document.root(), &mut visits)?;
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser(limits: Limits) -> html5ever::Parser<BudgetSink> {
        html5ever::parse_document(
            BudgetSink::new(limits),
            html5ever::ParseOpts {
                tree_builder: html5ever::tree_builder::TreeBuilderOpts {
                    scripting_enabled: false,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
    }

    #[test]
    fn rejects_before_allocating_excess_nodes_in_one_batch() {
        for source in [
            "<i></i>".repeat(100),
            "<template><template><b>text</b></template></template>".repeat(20),
            "<table>text<tr><td>x</td></tr></table>".repeat(20),
        ] {
            let mut parser = parser(Limits {
                nodes: 12,
                ..Default::default()
            });
            parser.process(source.into());
            let sink = &parser.tokenizer.sink.sink;
            assert!(sink.failed.get());
            assert!(sink.document.tree.len() <= 12);
            let allocated = sink.document.tree.len();
            parser.process("<template><b><i><p>more<!--comment--></p></template>".into());
            assert_eq!(parser.tokenizer.sink.sink.document.tree.len(), allocated);
            assert!(matches!(parser.finish(), Err(WebImportError::TooComplex)));
        }
    }

    #[test]
    fn bounds_text_attribute_and_operation_budgets_before_dom_mutation() {
        for source in [
            "<p>".to_owned() + &"x".repeat(16_384),
            "<p title='".to_owned() + &"x".repeat(16_384) + "'>x",
            "<!--".to_owned() + &"x".repeat(16_384) + "-->",
        ] {
            let mut parser = parser(Limits {
                bytes: 4096,
                ..Default::default()
            });
            parser.process(source.into());
            assert!(parser.tokenizer.sink.sink.failed.get());
            assert!(parser.tokenizer.sink.sink.bytes.get() <= 4096);
            assert!(matches!(parser.finish(), Err(WebImportError::TooComplex)));
        }
        let mut attributes = parser(Limits {
            attributes: 2,
            ..Default::default()
        });
        attributes.process("<p a='1' b='2' c='3'>text".into());
        assert!(attributes.tokenizer.sink.sink.failed.get());
        assert!(
            attributes
                .tokenizer
                .sink
                .sink
                .document
                .select("p")
                .nodes()
                .is_empty()
        );
        let mut operations = parser(Limits {
            operations: 10,
            ..Default::default()
        });
        operations.process("</not-open>".repeat(100).into());
        assert!(operations.tokenizer.sink.sink.failed.get());
        assert_eq!(operations.tokenizer.sink.sink.operations.get(), 10);
    }

    #[test]
    fn bounds_detached_template_depth_before_attachment() {
        let mut parser = parser(Limits {
            depth: 8,
            ..Default::default()
        });
        parser.process(
            format!(
                "{}text{}",
                "<template>".repeat(30),
                "</template>".repeat(30)
            )
            .into(),
        );
        let sink = &parser.tokenizer.sink.sink;
        assert!(sink.failed.get());
        let mut roots = vec![sink.document.root()];
        roots.extend(
            sink.template_owners
                .borrow()
                .keys()
                .filter_map(|id| sink.document.tree.get(id)),
        );
        while let Some(node) = roots.pop() {
            let mut id = Some(node.id);
            let mut depth = 0;
            while let Some(parent) = id.and_then(|id| sink.logical_parent(id)) {
                depth += 1;
                assert!(depth <= 8);
                id = Some(parent);
            }
            roots.extend(node.children_it(false));
        }
        assert!(matches!(parser.finish(), Err(WebImportError::TooComplex)));
    }

    #[test]
    fn reparented_subtrees_cannot_bypass_depth_limits() {
        let sink = BudgetSink::new(Limits {
            depth: 4,
            ..Default::default()
        });
        let element = || {
            sink.create_element(
                QualName::new(None, HTML_NS.into(), "div".into()),
                Vec::new(),
                ElementFlags::default(),
            )
        };
        let root = sink.get_document();
        let a = element();
        let b = element();
        let c = element();
        sink.append(&root, NodeOrText::AppendNode(a.clone()));
        sink.append(&a, NodeOrText::AppendNode(b.clone()));
        sink.append(&b, NodeOrText::AppendNode(c.clone()));
        let x = element();
        let y = element();
        let z = element();
        sink.append(&x, NodeOrText::AppendNode(y.clone()));
        sink.append(&y, NodeOrText::AppendNode(z));
        assert!(!sink.failed.get());
        sink.append(&c, NodeOrText::AppendNode(x.clone()));
        assert!(sink.failed.get());
        assert!(
            sink.document
                .tree
                .get(&x.node.unwrap())
                .unwrap()
                .parent()
                .is_none()
        );
    }

    #[test]
    fn rejection_handles_survive_malformed_recovery_with_tiny_budgets() {
        let tokens = [
            "<b>",
            "</b>",
            "<i>",
            "</p>",
            "<table>",
            "<tr><td>",
            "</table>",
            "<template>",
            "</template>",
            "<svg><foreignObject>",
            "</svg>",
            "<math><annotation-xml encoding='text/html'>",
            "</math>",
            "<select><option>",
            "</select>",
            "<frameset>",
            "<!--comment-->",
            "<!doctype html>",
            "text",
            "<b a b c d e>",
        ];
        for nodes in [1, 2, 4, 8, 16] {
            for seed in 0..64u64 {
                let mut state = seed;
                let mut source = String::new();
                for _ in 0..64 {
                    state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                    let index =
                        usize::try_from(state % u64::try_from(tokens.len()).unwrap()).unwrap();
                    source.push_str(tokens[index]);
                }
                let mut parser = parser(Limits {
                    nodes,
                    depth: 8,
                    attributes: 4,
                    bytes: 2048,
                    operations: 128,
                });
                parser.process(source.into());
                let sink = &parser.tokenizer.sink.sink;
                assert!(
                    sink.document.tree.len() <= nodes,
                    "nodes={nodes}, seed={seed}"
                );
                assert!(sink.bytes.get() <= 2048);
                let failed = sink.failed.get();
                let result = parser.finish();
                if failed {
                    assert!(matches!(result, Err(WebImportError::TooComplex)));
                }
            }
        }
    }

    #[test]
    fn preserves_html5_recovery_templates_and_foreign_integration_points() {
        for source in [
            "<!doctype html><p>First<b>bold<i>both</b>italic</i><table>fostered<tr><td>Cell</table>",
            "<template><p>Hidden<template><i>nested</i></template></template><p>visible",
            "<math><annotation-xml encoding='text/html'><p>HTML <b>integration</b></p></annotation-xml></math><svg><foreignObject><p>HTML again</p></foreignObject></svg>",
            "<html lang='en'><body><html dir='rtl'><p>a<!--comment-->b",
        ] {
            let actual = parse_bounded(source).unwrap();
            assert_eq!(
                actual.root().html(),
                Document::from(source).root().html(),
                "{source}"
            );
        }
    }
}
