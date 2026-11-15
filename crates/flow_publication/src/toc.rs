//! Publication outlines with typed document targets.
use crate::publication::PublicationTarget;
use std::collections::HashSet;

/// One publication outline entry with a format-neutral destination.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationNavigationEntry {
    pub label: String,
    pub target: PublicationTarget,
    pub children: Vec<PublicationNavigationEntry>,
}

/// Identity within one immutable publication outline, never a document text offset.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OutlineId(usize);

#[derive(Clone, Debug)]
pub struct OutlineNode {
    pub identity: OutlineId,
    pub label: String,
    pub target: PublicationTarget,
    pub parent: Option<OutlineId>,
    pub children: Vec<OutlineId>,
    pub depth: usize,
}

#[derive(Clone, Debug, Default)]
pub struct PublicationOutline {
    nodes: Vec<OutlineNode>,
    roots: Vec<OutlineId>,
}

impl PublicationOutline {
    pub fn new(entries: &[PublicationNavigationEntry]) -> Self {
        fn append(
            tree: &mut PublicationOutline,
            entries: &[PublicationNavigationEntry],
            parent: Option<OutlineId>,
            depth: usize,
        ) {
            for entry in entries {
                let identity = OutlineId(tree.nodes.len());
                tree.nodes.push(OutlineNode {
                    identity,
                    label: entry.label.clone(),
                    target: entry.target.clone(),
                    parent,
                    children: Vec::new(),
                    depth,
                });
                if let Some(parent) = parent {
                    tree.nodes[parent.0].children.push(identity);
                } else {
                    tree.roots.push(identity);
                }
                append(tree, &entry.children, Some(identity), depth + 1);
            }
        }
        let mut tree = Self::default();
        append(&mut tree, entries, None, 0);
        tree
    }

    pub fn nodes(&self) -> &[OutlineNode] {
        &self.nodes
    }
    pub fn node(&self, id: OutlineId) -> Option<&OutlineNode> {
        self.nodes.get(id.0)
    }
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn node_for_target(&self, target: &PublicationTarget) -> Option<OutlineId> {
        self.nodes
            .iter()
            .find(|node| &node.target == target)
            .map(|node| node.identity)
    }

    pub fn first_match(&self, query: &str) -> Option<OutlineId> {
        let query = query.to_lowercase();
        self.nodes
            .iter()
            .find(|node| node.label.to_lowercase().contains(&query))
            .map(|node| node.identity)
    }

    pub fn visible(&self, collapsed: &HashSet<OutlineId>, query: &str) -> Vec<OutlineId> {
        if query.is_empty() {
            let mut visible = Vec::new();
            for node in &self.nodes {
                let mut parent = node.parent;
                let mut hidden = false;
                while let Some(id) = parent {
                    if collapsed.contains(&id) {
                        hidden = true;
                        break;
                    }
                    parent = self.nodes[id.0].parent;
                }
                if !hidden {
                    visible.push(node.identity);
                }
            }
            visible
        } else {
            let query = query.to_lowercase();
            let mut included = HashSet::new();
            for node in &self.nodes {
                if node.label.to_lowercase().contains(&query) {
                    let mut current = Some(node.identity);
                    while let Some(id) = current {
                        included.insert(id);
                        current = self.nodes[id.0].parent;
                    }
                }
            }
            self.nodes
                .iter()
                .filter(|node| included.contains(&node.identity))
                .map(|node| node.identity)
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publication::DocumentKey;
    use flow_document::model::flow::AnchorId;

    #[test]
    fn outlines_keep_hierarchy_fragments_and_typed_identity() {
        let first = PublicationTarget::document(DocumentKey::new("part").unwrap());
        let second = PublicationTarget {
            document: DocumentKey::new("chapter").unwrap(),
            anchor: Some(AnchorId::new("start")),
        };
        let entries = vec![PublicationNavigationEntry {
            label: "Part".into(),
            target: first,
            children: vec![PublicationNavigationEntry {
                label: "Chapter".into(),
                target: second.clone(),
                children: Vec::new(),
            }],
        }];
        let tree = PublicationOutline::new(&entries);
        let child = tree.nodes()[1].identity;
        assert_eq!(tree.nodes().len(), 2);
        assert_eq!(tree.nodes()[1].depth, 1);
        assert_eq!(tree.nodes()[0].children, vec![child]);
        assert_eq!(tree.node_for_target(&second), Some(child));
        assert_eq!(
            tree.node(child)
                .unwrap()
                .target
                .anchor
                .as_ref()
                .unwrap()
                .as_str(),
            "start"
        );
        assert_eq!(
            tree.node_for_target(&PublicationTarget::document(second.document)),
            None
        );
        let collapsed = HashSet::from([tree.nodes()[0].identity]);
        assert_eq!(tree.visible(&collapsed, "").len(), 1);
        assert_eq!(tree.visible(&collapsed, "chapter").len(), 2);
        assert_eq!(tree.first_match("CHAPTER"), Some(child));
    }
}
