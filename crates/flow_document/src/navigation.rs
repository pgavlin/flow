use crate::{
    layout::{ComposedDocument, ContinuousLayout, GeneratedAction},
    model::{
        TextRange,
        flow::{
            self, FlowDocument, LinkRelation, NodeId, NodeKind, TextPosition as FlowTextPosition,
        },
    },
    pagination::{PageFragment, PaginatedLayout},
};
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum NavigationKind {
    Heading,
    Link,
    FootnoteReference,
    FootnoteReturn,
    CodeBlock,
    Table,
    Image,
    Task,
    Anchor,
}

#[derive(Clone, Debug)]
pub struct NavigationTarget {
    pub kind: NavigationKind,
    pub range: TextRange,
    pub label: String,
    pub heading_level: Option<u8>,
    pub(crate) flow: Option<FlowNavigationIdentity>,
    pub(crate) document_y: Option<f32>,
}

impl PartialEq for NavigationTarget {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.range == other.range
            && self.label == other.label
            && self.heading_level == other.heading_level
            && self.flow == other.flow
    }
}

impl Eq for NavigationTarget {}

#[derive(Clone, Debug)]
pub(crate) struct FlowNavigationIdentity {
    pub(crate) node: NodeId,
    pub(crate) position: FlowTextPosition,
    pub(crate) anchors: Vec<flow::AnchorId>,
    pub(crate) destination: Option<flow::Destination>,
    pub(crate) relation: Option<LinkRelation>,
    pub(crate) generated_action: Option<GeneratedAction>,
    pub(crate) owned_nodes: Vec<NodeId>,
}

impl PartialEq for FlowNavigationIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.node == other.node
            && self.position == other.position
            && self.anchors == other.anchors
            && self.destination == other.destination
            && self.relation == other.relation
            && self.generated_action == other.generated_action
    }
}

impl Eq for FlowNavigationIdentity {}

impl Hash for FlowNavigationIdentity {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.node.hash(state);
        self.position.hash(state);
        self.anchors.hash(state);
        self.destination.hash(state);
        self.relation.hash(state);
        self.generated_action.hash(state);
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct NavigationIdentity {
    pub kind: NavigationKind,
    pub start: usize,
    pub end: usize,
    pub label: String,
    pub(crate) flow: Option<FlowNavigationIdentity>,
}

impl NavigationTarget {
    pub fn identity(&self) -> NavigationIdentity {
        NavigationIdentity {
            kind: self.kind,
            start: self.range.start,
            end: self.range.end,
            label: self.label.clone(),
            flow: self.flow.clone(),
        }
    }

    #[cfg(feature = "render")]
    pub(crate) fn flow_node(&self) -> Option<NodeId> {
        self.flow.as_ref().map(|flow| flow.node)
    }

    pub fn flow_position(&self) -> Option<FlowTextPosition> {
        self.flow.as_ref().map(|flow| flow.position)
    }

    /// Returns this target's flow destination, if it activates a link.
    pub fn flow_destination(&self) -> Option<&flow::Destination> {
        self.flow.as_ref()?.destination.as_ref()
    }

    /// Returns the semantic relation carried independently from the destination.
    pub fn flow_relation(&self) -> Option<&LinkRelation> {
        self.flow.as_ref()?.relation.as_ref()
    }

    pub(crate) fn generated_action(&self) -> Option<&GeneratedAction> {
        self.flow.as_ref()?.generated_action.as_ref()
    }

    pub(crate) fn flow_owns_node(&self, node: Option<NodeId>) -> bool {
        node.is_some_and(|node| {
            self.flow
                .as_ref()
                .is_some_and(|flow| flow.owned_nodes.contains(&node))
        })
    }

    /// Returns the authoritative flow anchors associated with this target.
    pub fn flow_anchors(&self) -> &[flow::AnchorId] {
        self.flow
            .as_ref()
            .map_or(&[], |identity| identity.anchors.as_slice())
    }
}

#[derive(Clone, Debug, Default)]
pub struct NavigationIndex {
    targets: Vec<NavigationTarget>,
    headings: Vec<usize>,
}

impl NavigationIndex {
    pub fn new(document: &FlowDocument, layout: &ContinuousLayout) -> Self {
        let mut targets = Vec::new();
        let mut stack = vec![document.root()];
        while let Some(id) = stack.pop() {
            let node = document
                .get_node(id)
                .expect("finalized flow navigation traversal retains valid identities");
            let target = match &node.kind {
                NodeKind::Heading(_) => flow_heading_target(document, id).map(|mut target| {
                    let position = target.flow.as_ref().unwrap().position;
                    target.document_y = flow_geometry(layout, id, position);
                    target
                }),
                NodeKind::Link(link) => {
                    let mut target = flow_target(
                        document,
                        layout,
                        id,
                        match link.relation {
                            LinkRelation::Ordinary => NavigationKind::Link,
                            LinkRelation::NoteReference { .. } => NavigationKind::FootnoteReference,
                            LinkRelation::NoteReturn => NavigationKind::FootnoteReturn,
                        },
                        flow_label(document, node.text_range),
                        Some(link.destination.clone()),
                        Some(link.relation.clone()),
                    );
                    target
                        .flow
                        .as_mut()
                        .expect("flow link target retains its identity")
                        .owned_nodes = flow_owned_nodes(document, id);
                    Some(target)
                }
                NodeKind::Preformatted(_) => Some(flow_target(
                    document,
                    layout,
                    id,
                    NavigationKind::CodeBlock,
                    first_line(&flow_label(document, node.text_range)),
                    None,
                    None,
                )),
                NodeKind::Table(_) => Some(flow_target(
                    document,
                    layout,
                    id,
                    NavigationKind::Table,
                    flow_table_label(document, node),
                    None,
                    None,
                )),
                NodeKind::Image(image) => Some(flow_target(
                    document,
                    layout,
                    id,
                    NavigationKind::Image,
                    flow_image_label(document, node, image),
                    None,
                    None,
                )),
                NodeKind::ListItem(_) if flow_list_item_has_checkbox(document, node) => {
                    Some(flow_target(
                        document,
                        layout,
                        id,
                        NavigationKind::Task,
                        first_line(&flow_label(document, node.text_range)),
                        None,
                        None,
                    ))
                }
                _ => None,
            };
            if let Some(target) = target {
                targets.push(target);
            }
            stack.extend(node.children.iter().rev().copied());
        }

        let mut target_by_position = HashMap::new();
        for (index, target) in targets.iter().enumerate() {
            if let Some(flow) = &target.flow {
                target_by_position
                    .entry((flow.node, flow.position))
                    .or_insert(index);
            }
        }
        for (anchor_id, anchor) in document.anchors() {
            let key = (anchor.target.node, anchor.target.position);
            if let Some(index) = target_by_position.get(&key).copied()
                && targets[index]
                    .flow
                    .as_ref()
                    .is_some_and(|flow| flow.position == anchor.target.position)
            {
                let anchors = &mut targets[index]
                    .flow
                    .as_mut()
                    .expect("flow target indices retain flow identities")
                    .anchors;
                if !anchors.contains(anchor_id) {
                    anchors.push(anchor_id.clone());
                }
                continue;
            }
            let index = targets.len();
            targets.push(flow_anchor_target(document, layout, anchor_id, anchor));
            target_by_position.insert(key, index);
        }
        for (line, action) in layout
            .blocks
            .iter()
            .flat_map(|block| &block.lines)
            .flat_map(|line| {
                line.runs.iter().filter_map(move |run| {
                    run.generated_action.as_ref().map(|action| (line, action))
                })
            })
        {
            let GeneratedAction::NoteReturn { definition, .. } = action;
            let Some(node) = document.get_node(*definition) else {
                continue;
            };
            let position = FlowTextPosition::new(node.text_range.end, flow::Affinity::Upstream);
            targets.push(NavigationTarget {
                kind: NavigationKind::FootnoteReturn,
                range: TextRange {
                    start: node.text_range.end.as_usize(),
                    end: node.text_range.end.as_usize(),
                },
                label: "Return to footnote reference".to_owned(),
                heading_level: None,
                flow: Some(FlowNavigationIdentity {
                    node: *definition,
                    position,
                    anchors: document
                        .anchors_for(*definition)
                        .map(|(anchor, _)| anchor.clone())
                        .collect(),
                    destination: None,
                    relation: None,
                    generated_action: Some(action.clone()),
                    owned_nodes: vec![*definition],
                }),
                document_y: Some(line.y),
            });
        }

        targets.sort_by(|left, right| {
            left.range
                .start
                .cmp(&right.range.start)
                .then_with(|| {
                    left.flow
                        .as_ref()
                        .map(|flow| flow.node.layout_index())
                        .cmp(&right.flow.as_ref().map(|flow| flow.node.layout_index()))
                })
                .then_with(|| navigation_rank(left.kind).cmp(&navigation_rank(right.kind)))
                .then_with(|| left.label.cmp(&right.label))
        });
        targets.dedup_by(|right, left| right.identity() == left.identity());
        let headings = targets
            .iter()
            .enumerate()
            .filter_map(|(index, target)| (target.kind == NavigationKind::Heading).then_some(index))
            .collect();
        Self { targets, headings }
    }

    pub fn targets(&self) -> &[NavigationTarget] {
        &self.targets
    }

    pub fn target(&self, identity: &NavigationIdentity) -> Option<&NavigationTarget> {
        if let Some(exact) = self
            .targets
            .iter()
            .find(|target| target.identity() == *identity)
        {
            return Some(exact);
        }
        if let Some(identity_flow) = &identity.flow {
            if !identity_flow.anchors.is_empty() {
                let mut candidates = self.targets.iter().filter(|target| {
                    target.kind == identity.kind
                        && target.flow.as_ref().is_some_and(|target_flow| {
                            target_flow
                                .anchors
                                .iter()
                                .any(|anchor| identity_flow.anchors.contains(anchor))
                        })
                });
                let candidate = candidates.next()?;
                return candidates.next().is_none().then_some(candidate);
            }
            let mut candidates = self.targets.iter().filter(|target| {
                target.kind == identity.kind
                    && target.label == identity.label
                    && target.flow.as_ref().is_some_and(|target_flow| {
                        target_flow.destination == identity_flow.destination
                            && target_flow.relation == identity_flow.relation
                    })
            });
            let candidate = candidates.next()?;
            return candidates.next().is_none().then_some(candidate);
        }
        None
    }

    pub(crate) fn flow_target_at_text(
        &self,
        range: TextRange,
        destination: &flow::Destination,
    ) -> Option<&NavigationTarget> {
        let mut candidates = self.targets.iter().filter(|target| {
            target.flow_destination() == Some(destination)
                && target.range.start <= range.start
                && range.end <= target.range.end
        });
        let candidate = candidates.next()?;
        candidates.next().is_none().then_some(candidate)
    }

    pub fn flow_target_at_point(
        &self,
        layout: &ContinuousLayout,
        x: f32,
        y: f32,
    ) -> Option<&NavigationTarget> {
        if let Some(action) = layout.generated_action_at(x, y) {
            return self
                .targets
                .iter()
                .find(|target| target.generated_action() == Some(action));
        }
        let (destination, range) = layout.flow_link_range_at(x, y)?;
        if let Some(atomic) = layout.atomic_at(x, y) {
            let mut candidates = self.targets.iter().filter(|target| {
                target.flow_destination() == Some(destination) && target.flow_owns_node(atomic.node)
            });
            let candidate = candidates.next()?;
            return candidates.next().is_none().then_some(candidate);
        }
        self.flow_target_at_text(range, destination)
    }

    pub fn flow_target_at_paginated_point(
        &self,
        document: &ComposedDocument,
        layout: &PaginatedLayout,
        page_index: usize,
        x: f32,
        y: f32,
    ) -> Option<&NavigationTarget> {
        let page = layout.pages.get(page_index)?;
        for fragment in &layout.fragments[page.fragments.as_range()] {
            match fragment {
                PageFragment::Atomic(fragment)
                    if fragment.x <= x
                        && x < fragment.x + fragment.width
                        && fragment.y <= y
                        && y < fragment.y + fragment.height =>
                {
                    let object = document
                        .blocks
                        .get(fragment.block)?
                        .layout
                        .atomic_objects
                        .get(fragment.atomic)?;
                    let destination = object.flow_link.as_ref()?;
                    let mut candidates = self.targets.iter().filter(|target| {
                        target.flow_destination() == Some(destination)
                            && target.flow_owns_node(object.node)
                    });
                    let candidate = candidates.next()?;
                    return candidates.next().is_none().then_some(candidate);
                }
                PageFragment::Lines(fragment)
                    if fragment.y <= y && y < fragment.y + fragment.height =>
                {
                    let block = document.blocks.get(fragment.block)?;
                    let first = block.layout.lines.get(fragment.line_start)?;
                    let y_offset = fragment.y - first.y;
                    for line in &block.layout.lines[fragment.line_start..fragment.line_end] {
                        if y < y_offset + line.y || y >= y_offset + line.y + line.height {
                            continue;
                        }
                        for run in &line.runs {
                            if x < run.x || x >= run.x + run.width {
                                continue;
                            }
                            if let Some(action) = run.generated_action.as_ref() {
                                return self
                                    .targets
                                    .iter()
                                    .find(|target| target.generated_action() == Some(action));
                            }
                            if let Some(destination) = run.flow_link.as_ref() {
                                return self.flow_target_at_text(run.range, destination);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }

    pub fn flow_target_for_anchor(&self, anchor: &flow::AnchorId) -> Option<&NavigationTarget> {
        self.targets
            .iter()
            .find(|target| target.flow_anchors().contains(anchor))
    }

    pub fn adjacent(
        &self,
        current: Option<&NavigationIdentity>,
        direction: isize,
        headings_only: bool,
    ) -> Option<&NavigationTarget> {
        let candidates: Vec<usize> = if headings_only {
            self.headings.clone()
        } else {
            (0..self.targets.len()).collect()
        };
        if candidates.is_empty() {
            return None;
        }
        let current_target = current.and_then(|identity| self.target(identity));
        let current_position = current_target.and_then(|current| {
            candidates
                .iter()
                .position(|index| std::ptr::eq(&self.targets[*index], current))
        });
        let position = match (current_position, direction.is_negative()) {
            (Some(position), false) => (position + 1).min(candidates.len() - 1),
            (Some(position), true) => position.saturating_sub(1),
            (None, false) => 0,
            (None, true) => candidates.len() - 1,
        };
        self.targets.get(candidates[position])
    }
}

pub(crate) fn flow_heading_target(
    document: &FlowDocument,
    node_id: NodeId,
) -> Option<NavigationTarget> {
    let node = document.get_node(node_id)?;
    let NodeKind::Heading(heading) = &node.kind else {
        return None;
    };
    let default_position = FlowTextPosition::new(node.text_range.start, flow::Affinity::Downstream);
    let (position, anchors) = flow_position_and_anchors(document, node_id, default_position);
    Some(NavigationTarget {
        kind: NavigationKind::Heading,
        range: text_range(node.text_range),
        label: flow_label(document, node.text_range),
        heading_level: Some(heading.level.get()),
        flow: Some(FlowNavigationIdentity {
            node: node_id,
            position,
            anchors,
            destination: None,
            relation: None,
            generated_action: None,
            owned_nodes: vec![node_id],
        }),
        document_y: None,
    })
}

fn flow_target(
    document: &FlowDocument,
    layout: &ContinuousLayout,
    node_id: NodeId,
    kind: NavigationKind,
    label: String,
    destination: Option<flow::Destination>,
    relation: Option<LinkRelation>,
) -> NavigationTarget {
    let node = document
        .get_node(node_id)
        .expect("finalized flow navigation targets retain valid identities");
    let default_position = FlowTextPosition::new(node.text_range.start, flow::Affinity::Downstream);
    let (position, anchors) = flow_position_and_anchors(document, node_id, default_position);
    NavigationTarget {
        kind,
        range: text_range(node.text_range),
        label,
        heading_level: None,
        flow: Some(FlowNavigationIdentity {
            node: node_id,
            position,
            anchors,
            destination,
            relation,
            generated_action: None,
            owned_nodes: vec![node_id],
        }),
        document_y: flow_geometry(layout, node_id, position),
    }
}

fn flow_owned_nodes(document: &FlowDocument, root: NodeId) -> Vec<NodeId> {
    let mut nodes = Vec::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        nodes.push(id);
        if let Some(node) = document.get_node(id) {
            stack.extend(node.children.iter().rev().copied());
        }
    }
    nodes
}

fn flow_anchor_target(
    _document: &FlowDocument,
    layout: &ContinuousLayout,
    anchor_id: &flow::AnchorId,
    anchor: &flow::Anchor,
) -> NavigationTarget {
    let offset = anchor.target.position.offset.as_usize();
    NavigationTarget {
        kind: NavigationKind::Anchor,
        range: TextRange {
            start: offset,
            end: offset,
        },
        label: anchor_id.as_str().to_owned(),
        heading_level: None,
        flow: Some(FlowNavigationIdentity {
            node: anchor.target.node,
            position: anchor.target.position,
            anchors: vec![anchor_id.clone()],
            destination: None,
            relation: None,
            generated_action: None,
            owned_nodes: vec![anchor.target.node],
        }),
        document_y: flow_geometry(layout, anchor.target.node, anchor.target.position),
    }
}

fn flow_geometry(
    layout: &ContinuousLayout,
    node: NodeId,
    position: FlowTextPosition,
) -> Option<f32> {
    if let Some(atomic) = layout
        .blocks
        .iter()
        .flat_map(|block| &block.atomic_objects)
        .find(|atomic| atomic.node == Some(node))
    {
        return Some(atomic.y);
    }
    let block = layout.blocks.iter().find(|block| block.node == Some(node));
    if let Some(block) = block
        && (block.range.start == block.range.end || position.offset.as_usize() == block.range.start)
    {
        return Some(block.y);
    }
    let offset = match position.affinity {
        flow::Affinity::Downstream => position.offset.as_usize(),
        flow::Affinity::Upstream => position.offset.as_usize().saturating_sub(1),
    };
    let line = layout
        .blocks
        .iter()
        .flat_map(|block| &block.lines)
        .find(|line| line.range.contains(offset) || line.range.start == offset)
        .or_else(|| {
            (position.affinity == flow::Affinity::Downstream)
                .then(|| {
                    layout
                        .blocks
                        .iter()
                        .flat_map(|block| &block.lines)
                        .filter(|line| line.range.start > offset)
                        .min_by_key(|line| line.range.start)
                })
                .flatten()
        });
    line.map(|line| line.y)
        .or_else(|| layout.y_for_text(offset))
        .or_else(|| block.map(|block| block.y))
}

fn flow_position_and_anchors(
    document: &FlowDocument,
    node: NodeId,
    default_position: FlowTextPosition,
) -> (FlowTextPosition, Vec<flow::AnchorId>) {
    let anchors: Vec<_> = document.anchors_for(node).collect();
    let position = anchors
        .iter()
        .find(|(_, anchor)| anchor.target.position == default_position)
        .or_else(|| anchors.first())
        .map_or(default_position, |(_, anchor)| anchor.target.position);
    let aliases = anchors
        .into_iter()
        .filter(|(_, anchor)| anchor.target.position == position)
        .map(|(id, _)| id.clone())
        .collect();
    (position, aliases)
}

fn flow_label(document: &FlowDocument, range: flow::TextRange) -> String {
    document.get_text(range).unwrap_or_default().to_owned()
}

fn flow_list_item_has_checkbox(document: &FlowDocument, item: &flow::Node) -> bool {
    let Some(paragraph) = item
        .children
        .first()
        .and_then(|child| document.get_node(*child))
        .filter(|node| matches!(node.kind, NodeKind::Paragraph))
    else {
        return false;
    };
    for child in &paragraph.children {
        let Some(node) = document.get_node(*child) else {
            return false;
        };
        match node.kind {
            NodeKind::LocationMarker(_) | NodeKind::BreakOpportunity(_) => continue,
            NodeKind::Checkbox(_) => return true,
            _ => return false,
        }
    }
    false
}

fn flow_table_label(document: &FlowDocument, table: &flow::Node) -> String {
    let mut stack: Vec<_> = table.children.iter().rev().copied().collect();
    while let Some(id) = stack.pop() {
        let Some(node) = document.get_node(id) else {
            continue;
        };
        if matches!(node.kind, NodeKind::Row) {
            return flow_label(document, node.text_range).replace('\t', " | ");
        }
        if !matches!(node.kind, NodeKind::Table(_)) {
            stack.extend(node.children.iter().rev().copied());
        }
    }
    String::new()
}

fn flow_image_label(document: &FlowDocument, node: &flow::Node, image: &flow::Image) -> String {
    let canonical = flow_label(document, node.text_range);
    if !canonical.is_empty() {
        return canonical;
    }
    image
        .title
        .clone()
        .or_else(|| {
            document
                .get_resource(image.resource)
                .map(|resource| resource.identity.clone())
        })
        .unwrap_or_else(|| "image".to_owned())
}

fn text_range(range: flow::TextRange) -> TextRange {
    TextRange {
        start: range.start.as_usize(),
        end: range.end.as_usize(),
    }
}

fn navigation_rank(kind: NavigationKind) -> u8 {
    match kind {
        NavigationKind::Heading
        | NavigationKind::CodeBlock
        | NavigationKind::Image
        | NavigationKind::Table
        | NavigationKind::Task => 0,
        NavigationKind::Link
        | NavigationKind::FootnoteReference
        | NavigationKind::FootnoteReturn => 1,
        NavigationKind::Anchor => 2,
    }
}
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().to_owned()
}

#[cfg(any(feature = "render", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FlowDestinationActivation<'a> {
    Local(&'a flow::AnchorId),
    Relative {
        path: &'a str,
        anchor: Option<&'a flow::AnchorId>,
    },
    PublicationItem {
        identity: &'a str,
        anchor: Option<&'a flow::AnchorId>,
    },
    Host {
        identity: &'a str,
        anchor: Option<&'a flow::AnchorId>,
    },
    External(&'a str),
}

#[cfg(any(feature = "render", test))]
pub(crate) fn flow_destination_activation(
    destination: &flow::Destination,
) -> FlowDestinationActivation<'_> {
    match destination {
        flow::Destination::Local(anchor) => FlowDestinationActivation::Local(anchor),
        flow::Destination::Document { document, anchor } => match document {
            flow::DocumentReference::Relative(path) => FlowDestinationActivation::Relative {
                path,
                anchor: anchor.as_ref(),
            },
            flow::DocumentReference::PublicationItem(identity) => {
                FlowDestinationActivation::PublicationItem {
                    identity,
                    anchor: anchor.as_ref(),
                }
            }
            flow::DocumentReference::Host(identity) => FlowDestinationActivation::Host {
                identity,
                anchor: anchor.as_ref(),
            },
        },
        flow::Destination::External(uri) => FlowDestinationActivation::External(uri),
    }
}

#[cfg(any(feature = "render", test))]
pub(crate) fn local_flow_anchor<'a>(
    destination: &'a flow::Destination,
    document_name: &str,
) -> Option<&'a flow::AnchorId> {
    match flow_destination_activation(destination) {
        FlowDestinationActivation::Local(anchor) => Some(anchor),
        FlowDestinationActivation::Relative { path, anchor }
            if path == document_name
                || path
                    .strip_prefix("./")
                    .is_some_and(|path| path == document_name) =>
        {
            anchor
        }
        FlowDestinationActivation::Relative { .. }
        | FlowDestinationActivation::PublicationItem { .. }
        | FlowDestinationActivation::Host { .. }
        | FlowDestinationActivation::External(_) => None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        formats::markdown as flow_markdown,
        layout::{LayoutOptions, TextMeasurer, TextMetrics, TextStyle, layout_flow_document},
        model::flow::StyleContextKey,
    };

    struct FixedMeasurer;

    impl TextMeasurer for FixedMeasurer {
        fn measure(&self, text: &str, style: &TextStyle) -> TextMetrics {
            TextMetrics {
                width: text.chars().count() as f32 * style.size * 0.5,
                ascent: style.size * 0.8,
                descent: style.size * 0.2,
            }
        }
    }

    fn index_document(document: FlowDocument, width: f32) -> (FlowDocument, NavigationIndex) {
        let styles = document.resolve_styles(StyleContextKey::default());
        let layout = layout_flow_document(
            &document,
            &styles,
            &FixedMeasurer,
            &LayoutOptions {
                width,
                ..LayoutOptions::default()
            },
        );
        let index = NavigationIndex::new(&document, &layout);
        (document, index)
    }

    fn flow_index(source: &str, width: f32) -> (FlowDocument, NavigationIndex) {
        let document = flow_markdown::parse(source, Default::default())
            .unwrap()
            .document;
        index_document(document, width)
    }
    #[test]
    fn flow_index_uses_semantics_destinations_anchors_and_layout_geometry() {
        let source = "# Head *styled*\n\n[local](#head) [doc](guide.md#part) [web](https://example.com)\n\n- [ ] task\n\n```rs\nfn main() {}\n```\n\n| A | B |\n|---|---|\n| one | two |\n\n![chart](chart.png)\n\nnote[^n]\n\n[^n]: body [back](#fnref-n)";
        let (document, index) = flow_index(source, 180.0);
        let kinds: Vec<_> = index.targets().iter().map(|target| target.kind).collect();
        for kind in [
            NavigationKind::Heading,
            NavigationKind::Link,
            NavigationKind::Task,
            NavigationKind::CodeBlock,
            NavigationKind::Table,
            NavigationKind::Image,
            NavigationKind::FootnoteReference,
            NavigationKind::FootnoteReturn,
            NavigationKind::Anchor,
        ] {
            assert!(kinds.contains(&kind), "missing {kind:?}: {kinds:?}");
        }
        let heading = index
            .targets()
            .iter()
            .find(|target| target.kind == NavigationKind::Heading)
            .unwrap();
        assert_eq!(heading.label, "Head styled");
        assert!(
            heading
                .flow
                .as_ref()
                .unwrap()
                .anchors
                .iter()
                .any(|id| id.as_str() == "head-styled")
        );
        assert!(
            index
                .targets()
                .iter()
                .all(|target| target.document_y.is_some())
        );

        let links: Vec<_> = index
            .targets()
            .iter()
            .filter(|target| target.kind == NavigationKind::Link)
            .map(|target| target.flow.as_ref().unwrap().destination.as_ref().unwrap())
            .collect();
        assert!(links.contains(&&flow::Destination::Local(flow::AnchorId::new("head"))));
        assert!(links.iter().any(|destination| matches!(
            destination,
            flow::Destination::Document { document: flow::DocumentReference::Relative(path), anchor: Some(anchor) }
                if path == "guide.md" && anchor.as_str() == "part"
        )));
        assert!(links.contains(&&flow::Destination::External(
            "https://example.com".to_owned()
        )));
        assert!(
            index
                .targets()
                .iter()
                .any(|target| { target.kind == NavigationKind::Table && target.label == "A | B" })
        );

        let local = index
            .targets()
            .iter()
            .find(|target| target.kind == NavigationKind::Link && target.label == "local")
            .unwrap();
        let destination = local.flow_destination().unwrap();
        assert!(std::ptr::eq(
            index.flow_target_at_text(local.range, destination).unwrap(),
            local
        ));
        assert!(matches!(
            flow_destination_activation(destination),
            FlowDestinationActivation::Local(anchor) if anchor.as_str() == "head"
        ));
        assert_eq!(
            local_flow_anchor(destination, "document.md")
                .unwrap()
                .as_str(),
            "head"
        );
        assert!(local.flow_position().is_some());
        assert_eq!(local.flow_relation(), Some(&LinkRelation::Ordinary));
        let document_link = index
            .targets()
            .iter()
            .find(|target| target.kind == NavigationKind::Link && target.label == "doc")
            .unwrap();
        assert!(matches!(
            flow_destination_activation(document_link.flow_destination().unwrap()),
            FlowDestinationActivation::Relative { path: "guide.md", anchor: Some(anchor) }
                if anchor.as_str() == "part"
        ));
        assert_eq!(
            local_flow_anchor(document_link.flow_destination().unwrap(), "guide.md")
                .unwrap()
                .as_str(),
            "part"
        );
        let nested = flow::Destination::Document {
            document: flow::DocumentReference::Relative("subdir/guide.md".to_owned()),
            anchor: Some(flow::AnchorId::new("part")),
        };
        assert!(local_flow_anchor(&nested, "guide.md").is_none());
        let web = index
            .targets()
            .iter()
            .find(|target| target.kind == NavigationKind::Link && target.label == "web")
            .unwrap();
        assert_eq!(
            flow_destination_activation(web.flow_destination().unwrap()),
            FlowDestinationActivation::External("https://example.com")
        );
        let publication = flow::Destination::Document {
            document: flow::DocumentReference::PublicationItem("chapter-2".to_owned()),
            anchor: Some(flow::AnchorId::new("part")),
        };
        assert!(matches!(
            flow_destination_activation(&publication),
            FlowDestinationActivation::PublicationItem { identity: "chapter-2", anchor: Some(anchor) }
                if anchor.as_str() == "part"
        ));
        let host = flow::Destination::Document {
            document: flow::DocumentReference::Host("opaque:item".to_owned()),
            anchor: None,
        };
        assert_eq!(
            flow_destination_activation(&host),
            FlowDestinationActivation::Host {
                identity: "opaque:item",
                anchor: None,
            }
        );
        let stable_identity = heading.identity();
        let identities = std::collections::HashSet::from([stable_identity.clone()]);
        let mut presentation_changed = stable_identity.clone();
        presentation_changed
            .flow
            .as_mut()
            .unwrap()
            .owned_nodes
            .push(document.root());
        assert_eq!(presentation_changed, stable_identity);
        assert!(identities.contains(&presentation_changed));
        let heading_anchor = heading.flow_anchors().first().unwrap();
        assert!(std::ptr::eq(
            index.flow_target_for_anchor(heading_anchor).unwrap(),
            heading
        ));
    }

    #[test]
    fn flow_point_lookup_distinguishes_same_destination_links() {
        let document = flow_markdown::parse("[one *two*](same) [other](same)", Default::default())
            .unwrap()
            .document;
        let styles = document.resolve_styles(StyleContextKey::default());
        let layout = layout_flow_document(
            &document,
            &styles,
            &FixedMeasurer,
            &LayoutOptions {
                width: 300.0,
                ..LayoutOptions::default()
            },
        );
        let index = NavigationIndex::new(&document, &layout);
        let first = index
            .targets()
            .iter()
            .find(|target| target.kind == NavigationKind::Link && target.label == "one two")
            .unwrap();
        let line = &layout.blocks[0].lines[0];
        let [left, right] = line
            .runs
            .windows(2)
            .find(|runs| {
                runs[0].flow_link == runs[1].flow_link
                    && first.range.start <= runs[0].range.start
                    && runs[1].range.end <= first.range.end
                    && runs[0].x + runs[0].width < runs[1].x
            })
            .unwrap()
        else {
            unreachable!()
        };
        let x = (left.x + left.width + right.x) / 2.0;
        let target = index
            .flow_target_at_point(&layout, x, line.y + line.height / 2.0)
            .unwrap();

        assert!(std::ptr::eq(target, first));
    }

    #[test]
    fn flow_geometry_distinguishes_atomic_trailing_edge_affinity() {
        let document = flow_markdown::parse("![x](x.png) following words", Default::default())
            .unwrap()
            .document;
        let styles = document.resolve_styles(StyleContextKey::default());
        let layout = layout_flow_document(
            &document,
            &styles,
            &FixedMeasurer,
            &LayoutOptions {
                width: 45.0,
                ..LayoutOptions::default()
            },
        );
        let paragraph = document
            .preorder(document.root())
            .find(|id| matches!(document.get_node(*id).unwrap().kind, NodeKind::Paragraph))
            .unwrap();
        let image = document
            .preorder(document.root())
            .find(|id| matches!(document.get_node(*id).unwrap().kind, NodeKind::Image(_)))
            .unwrap();
        let edge = document.get_node(image).unwrap().text_range.end;
        let upstream_offset = edge.as_usize() - 1;
        let downstream_offset = edge.as_usize();
        let upstream_y = layout
            .blocks
            .iter()
            .flat_map(|block| &block.lines)
            .find(|line| line.range.contains(upstream_offset))
            .unwrap()
            .y;
        let downstream_y = layout
            .blocks
            .iter()
            .flat_map(|block| &block.lines)
            .filter(|line| line.range.start >= downstream_offset)
            .min_by_key(|line| line.range.start)
            .unwrap()
            .y;
        assert!(downstream_y > upstream_y);
        assert_eq!(
            flow_geometry(
                &layout,
                paragraph,
                FlowTextPosition::new(edge, flow::Affinity::Upstream),
            ),
            Some(upstream_y)
        );
        assert_eq!(
            flow_geometry(
                &layout,
                paragraph,
                FlowTextPosition::new(edge, flow::Affinity::Downstream),
            ),
            Some(downstream_y)
        );
    }

    #[test]
    fn flow_table_labels_skip_column_declarations() {
        let one = std::num::NonZeroU32::new(1).unwrap();
        let mut builder = flow::FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let table = builder
            .add_node(NodeKind::Table(flow::Table { column_count: one }))
            .unwrap();
        let columns = builder
            .add_node(NodeKind::ColumnGroup(flow::ColumnGroup { span: one }))
            .unwrap();
        let column = builder
            .add_node(NodeKind::Column(flow::Column { span: one }))
            .unwrap();
        builder.append(columns, column).unwrap();
        builder.append(table, columns).unwrap();
        let rows = builder
            .add_node(NodeKind::RowGroup(flow::RowGroup::Body))
            .unwrap();
        let row = builder.add_node(NodeKind::Row).unwrap();
        let cell = builder
            .add_node(NodeKind::Cell(flow::Cell {
                kind: flow::CellKind::Data,
                column_start: 0,
                column_span: one,
                row_span: one,
                scope: None,
                explicit_headers: None,
            }))
            .unwrap();
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let text = builder.add_text("value").unwrap();
        builder.append(paragraph, text).unwrap();
        builder.append(cell, paragraph).unwrap();
        builder.append(row, cell).unwrap();
        builder.append(rows, row).unwrap();
        builder.append(table, rows).unwrap();
        builder.append(root, table).unwrap();
        let (_, index) = index_document(builder.finish().unwrap(), 200.0);

        assert_eq!(
            index
                .targets()
                .iter()
                .find(|target| target.kind == NavigationKind::Table)
                .unwrap()
                .label,
            "value"
        );
    }

    #[test]
    fn flow_anchor_aliases_coalesce_and_trailing_affinity_is_preserved() {
        let mut builder = flow::FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let content = builder.add_node(NodeKind::Paragraph).unwrap();
        let text = builder.add_text("content").unwrap();
        builder.append(content, text).unwrap();
        builder.append(root, content).unwrap();
        let trailing = builder.add_node(NodeKind::Paragraph).unwrap();
        builder.append(root, trailing).unwrap();
        builder
            .add_anchor(flow::AnchorId::new("first"), trailing)
            .unwrap();
        builder
            .add_anchor(flow::AnchorId::new("second"), trailing)
            .unwrap();
        let (_, index) = index_document(builder.finish().unwrap(), 200.0);

        let anchors: Vec<_> = index
            .targets()
            .iter()
            .filter(|target| target.kind == NavigationKind::Anchor)
            .collect();
        assert_eq!(anchors.len(), 1);
        let identity = anchors[0].flow.as_ref().unwrap();
        assert_eq!(identity.anchors.len(), 2);
        assert_eq!(identity.position.affinity, flow::Affinity::Upstream);
        assert!(anchors[0].document_y.is_some());
    }

    #[test]
    fn flow_recovery_declines_ambiguous_unanchored_replacements() {
        fn headings(prefix: bool, count: usize) -> FlowDocument {
            let mut builder = flow::FlowBuilder::new();
            let root = builder.add_node(NodeKind::Document).unwrap();
            if prefix {
                let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
                let text = builder.add_text("prefix").unwrap();
                builder.append(paragraph, text).unwrap();
                builder.append(root, paragraph).unwrap();
            }
            for _ in 0..count {
                let heading = builder
                    .add_node(NodeKind::Heading(flow::Heading {
                        level: flow::HeadingLevel::H1,
                    }))
                    .unwrap();
                let text = builder.add_text("Same").unwrap();
                builder.append(heading, text).unwrap();
                builder.append(root, heading).unwrap();
            }
            builder.finish().unwrap()
        }
        let (_, original) = index_document(headings(true, 1), 200.0);
        let identity = original.targets()[0].identity();
        let (_, replacement) = index_document(headings(false, 2), 200.0);

        assert!(replacement.target(&identity).is_none());
    }

    #[test]
    fn flow_identity_survives_reflow_and_recovers_by_exact_anchor_after_edits() {
        let (_, narrow) = flow_index("# Target\n\n[long linked label](#target)", 80.0);
        let heading = narrow
            .targets()
            .iter()
            .find(|target| target.kind == NavigationKind::Heading)
            .unwrap();
        let identity = heading.identity();

        let (_, wide) = flow_index("# Target\n\n[long linked label](#target)", 400.0);
        assert_eq!(wide.target(&identity).unwrap().label, "Target");

        let (_, edited) = flow_index("intro\n\n# Target\n\n[long linked label](#target)", 400.0);
        let recovered = edited.target(&identity).unwrap();
        assert_eq!(recovered.label, "Target");
        assert_ne!(
            recovered.flow.as_ref().unwrap().node,
            identity.flow.unwrap().node
        );
    }
}
