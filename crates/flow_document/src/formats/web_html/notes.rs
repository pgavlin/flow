//! Bounded snapshots of explicitly referenced local notes. Restoring a note
//! copies an already-parsed tree; it never invokes an unbudgeted HTML parser.

use std::collections::{HashMap, HashSet, VecDeque};

use dom_smoothie::dom_query::{Attr, Document, NodeData, NodeId, Selection};
use url::Url;

use super::visibility::is_explicitly_hidden;
use super::{
    HTML_NS, MAX_DEPTH, MAX_NODES, MAX_SOURCE, WebImportError, check_dom, local_fragment, resolve,
};

const NOTE_REFERENCES: &str = "a[role~='doc-noteref'], a[rel~='footnote']";

struct RetainedNode {
    data: NodeData,
    parent: Option<usize>,
}

pub(super) struct RetainedNote {
    nodes: Vec<RetainedNode>,
    depth: usize,
}

fn note_targets(
    selection: &Selection<'_>,
    source_url: Option<&Url>,
    base: Option<&Url>,
) -> Vec<String> {
    selection
        .select(NOTE_REFERENCES)
        .nodes()
        .iter()
        .filter_map(|link| {
            let href = link.attr("href")?;
            let reference = resolve(&href, base)
                .map(String::from)
                .unwrap_or_else(|| href.to_string());
            local_fragment(&reference, source_url)
        })
        .collect()
}

fn retained_bytes(data: &NodeData) -> Option<usize> {
    let bytes = std::mem::size_of::<RetainedNode>();
    match data {
        NodeData::Text { contents } => Some(bytes.saturating_add(contents.len())),
        NodeData::Element(element) => {
            if &*element.name.ns != HTML_NS
                || matches!(
                    &*element.name.local,
                    "script"
                        | "style"
                        | "head"
                        | "link"
                        | "meta"
                        | "template"
                        | "form"
                        | "input"
                        | "button"
                        | "select"
                        | "textarea"
                        | "object"
                        | "embed"
                )
            {
                return None;
            }
            Some(element.attrs.iter().fold(
                bytes.saturating_add(element.name.local.len()),
                |bytes, attr| {
                    bytes
                        .saturating_add(std::mem::size_of::<Attr>())
                        .saturating_add(attr.name.local.len())
                        .saturating_add(attr.value.len())
                },
            ))
        }
        _ => None,
    }
}

pub(super) fn capture_referenced_notes(
    document: &Document,
    source_url: Option<&Url>,
    base: Option<&Url>,
) -> Result<HashMap<String, RetainedNote>, WebImportError> {
    let targets: HashSet<_> = note_targets(&Selection::from(document.root()), source_url, base)
        .into_iter()
        .collect();
    if targets.is_empty() {
        return Ok(HashMap::new());
    }
    // Cache inherited visibility once. Repeatedly parsing a large ancestor's
    // inline style for every note or child would otherwise multiply work.
    let mut hidden = HashSet::new();
    let mut pending = vec![(document.root(), false)];
    while let Some((node, ancestor_hidden)) = pending.pop() {
        let invisible = ancestor_hidden || is_explicitly_hidden(node);
        if invisible {
            hidden.insert(node.id);
        }
        pending.extend(node.children_it(false).map(|child| (child, invisible)));
    }
    let roots: Vec<_> = document
        .select("[id]")
        .nodes()
        .iter()
        .copied()
        .filter(|node| {
            !hidden.contains(&node.id) && node.attr("id").is_some_and(|id| targets.contains(&*id))
        })
        .collect();
    // Assign inferred note roles before taking any snapshot, including notes
    // nested in another retained note.
    for node in &roots {
        let role = node.attr("role").unwrap_or_default();
        if !role
            .split_ascii_whitespace()
            .any(|role| matches!(role, "doc-footnote" | "doc-endnote"))
        {
            let role = format!("{role} doc-footnote");
            node.set_attr("role", role.trim());
        }
    }
    let mut notes = HashMap::new();
    let mut bytes = 0usize;
    for root in roots {
        let id = root.attr("id").unwrap().to_string();
        if notes.contains_key(&id) {
            continue;
        }
        let mut note = RetainedNote {
            nodes: Vec::new(),
            depth: 0,
        };
        let mut pending = vec![(root, None, 0)];
        while let Some((node, parent, depth)) = pending.pop() {
            if hidden.contains(&node.id) {
                continue;
            }
            let Some(size) = node.query(|inner| retained_bytes(&inner.data)).flatten() else {
                continue;
            };
            bytes = bytes.checked_add(size).ok_or(WebImportError::TooComplex)?;
            if bytes > MAX_SOURCE {
                return Err(WebImportError::TooComplex);
            }
            let data = node.query(|inner| inner.data.clone()).unwrap();
            let index = note.nodes.len();
            note.nodes.push(RetainedNode { data, parent });
            note.depth = note.depth.max(depth);
            pending.extend(
                node.children_it(true)
                    .map(|child| (child, Some(index), depth + 1)),
            );
        }
        if !note.nodes.is_empty() {
            notes.insert(id, note);
        }
    }
    Ok(notes)
}

pub(super) fn restore_referenced_notes(
    extracted: &dom_smoothie::Extracted,
    mut notes: HashMap<String, RetainedNote>,
    source_url: Option<&Url>,
    base: Option<&Url>,
) -> Result<(), WebImportError> {
    if extracted.document.tree.len() > MAX_NODES {
        return Err(WebImportError::TooComplex);
    }
    check_dom(extracted.root())?;
    let mut present: HashSet<_> = extracted
        .selection()
        .select("[id]")
        .nodes()
        .iter()
        .filter_map(|node| node.attr("id").map(|id| id.to_string()))
        .collect();
    let mut pending: VecDeque<_> = note_targets(&extracted.selection(), source_url, base).into();
    while let Some(id) = pending.pop_front() {
        if present.contains(&id) {
            continue;
        }
        let Some(note) = notes.remove(&id) else {
            continue;
        };
        if extracted
            .document
            .tree
            .len()
            .saturating_add(note.nodes.len())
            .saturating_add(1)
            > MAX_NODES
            || note.depth.saturating_add(2) > MAX_DEPTH
        {
            return Err(WebImportError::TooComplex);
        }
        let wrapper = extracted.document.tree.new_element("div");
        let mut copied: Vec<NodeId> = Vec::with_capacity(note.nodes.len());
        for retained in note.nodes {
            let node = extracted.document.tree.create_node(retained.data);
            let parent = retained.parent.map_or(wrapper, |index| {
                extracted.document.tree.get(&copied[index]).unwrap()
            });
            parent.append_child(&node);
            copied.push(node);
        }
        let selection = Selection::from(wrapper);
        present.extend(
            selection
                .select("[id]")
                .nodes()
                .iter()
                .filter_map(|node| node.attr("id").map(|id| id.to_string())),
        );
        pending.extend(note_targets(&selection, source_url, base));
        extracted.root().append_child(&wrapper);
    }
    check_dom(extracted.root())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restoration_checks_total_arena_capacity_before_copying() {
        let original = Document::from(
            "<p><a role='doc-noteref' href='#note'>1</a></p><aside id='note'><p>Evidence</p></aside>",
        );
        let notes = capture_referenced_notes(&original, None, None).unwrap();
        let document = Document::from("<p><a role='doc-noteref' href='#note'>1</a></p>");
        while document.tree.len() < MAX_NODES - 1 {
            document.tree.new_element("span");
        }
        let root = document.root().id;
        let extracted = dom_smoothie::Extracted {
            document,
            root,
            metadata: Default::default(),
        };
        let before = extracted.document.tree.len();
        assert!(matches!(
            restore_referenced_notes(&extracted, notes, None, None),
            Err(WebImportError::TooComplex)
        ));
        assert_eq!(extracted.document.tree.len(), before);
    }
}
