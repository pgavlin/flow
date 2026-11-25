use crate::{
    model::{TextRange, flow::FlowDocument},
    navigation::{AnchorLookup, NavigationIdentity, NavigationTarget, flow_heading_target},
};
use std::collections::HashSet;
#[derive(Clone, Debug)]
pub struct TocNode {
    pub identity: NavigationIdentity,
    pub range: TextRange,
    pub label: String,
    pub heading_level: u8,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub depth: usize,
}

#[derive(Clone, Debug, Default)]
pub struct TocTree {
    nodes: Vec<TocNode>,
    roots: Vec<usize>,
}

impl TocTree {
    pub fn new(document: &FlowDocument) -> Self {
        let anchors = AnchorLookup::new(document);
        Self::from_heading_targets(
            document
                .preorder(document.root())
                .filter_map(|id| flow_heading_target(document, id, &anchors)),
        )
    }

    fn from_heading_targets(targets: impl IntoIterator<Item = NavigationTarget>) -> Self {
        let mut nodes = Vec::<TocNode>::new();
        let mut roots = Vec::new();
        let mut ancestors = Vec::<usize>::new();

        for target in targets {
            let level = target.heading_level.unwrap_or(1);
            while ancestors
                .last()
                .is_some_and(|index| nodes[*index].heading_level >= level)
            {
                ancestors.pop();
            }
            let parent = ancestors.last().copied();
            let index = nodes.len();
            nodes.push(TocNode {
                identity: target.identity(),
                range: target.range,
                label: target.label.clone(),
                heading_level: level,
                parent,
                children: Vec::new(),
                depth: ancestors.len(),
            });
            if let Some(parent) = parent {
                nodes[parent].children.push(index);
            } else {
                roots.push(index);
            }
            ancestors.push(index);
        }

        Self { nodes, roots }
    }

    pub fn nodes(&self) -> &[TocNode] {
        &self.nodes
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn resolve(&self, identity: &NavigationIdentity) -> Option<usize> {
        if let Some(exact) = self
            .nodes
            .iter()
            .position(|node| node.identity == *identity)
        {
            return Some(exact);
        }
        if let Some(identity_flow) = &identity.flow {
            if !identity_flow.anchors.is_empty() {
                let mut candidates = self.nodes.iter().enumerate().filter(|(_, node)| {
                    node.identity.kind == identity.kind
                        && node.identity.flow.as_ref().is_some_and(|node_flow| {
                            node_flow
                                .anchors
                                .iter()
                                .any(|anchor| identity_flow.anchors.contains(anchor))
                        })
                });
                let (index, _) = candidates.next()?;
                return candidates.next().is_none().then_some(index);
            }
            let mut candidates = self.nodes.iter().enumerate().filter(|(_, node)| {
                node.identity.kind == identity.kind && node.label == identity.label
            });
            let (index, _) = candidates.next()?;
            return candidates.next().is_none().then_some(index);
        }
        self.nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.identity.kind == identity.kind && node.label == identity.label)
            .min_by_key(|(_, node)| node.range.start.abs_diff(identity.start))
            .map(|(index, _)| index)
    }

    pub fn section_for_text(&self, offset: usize) -> Option<usize> {
        self.nodes
            .iter()
            .rposition(|node| node.range.start <= offset)
            .or_else(|| (!self.nodes.is_empty()).then_some(0))
    }

    pub fn first_match(&self, query: &str) -> Option<usize> {
        let query = query.to_lowercase();
        self.nodes
            .iter()
            .position(|node| node.label.to_lowercase().contains(&query))
    }

    pub fn visible(&self, collapsed: &HashSet<NavigationIdentity>, query: &str) -> Vec<usize> {
        if query.is_empty() {
            let mut visible = Vec::new();
            for root in &self.roots {
                self.push_visible(*root, collapsed, &mut visible);
            }
            return visible;
        }

        let query = query.to_lowercase();
        let mut included = vec![false; self.nodes.len()];
        for (index, node) in self.nodes.iter().enumerate() {
            if node.label.to_lowercase().contains(&query) {
                let mut current = Some(index);
                while let Some(ancestor) = current {
                    included[ancestor] = true;
                    current = self.nodes[ancestor].parent;
                }
            }
        }
        (0..self.nodes.len())
            .filter(|index| included[*index])
            .collect()
    }

    fn push_visible(
        &self,
        index: usize,
        collapsed: &HashSet<NavigationIdentity>,
        visible: &mut Vec<usize>,
    ) {
        visible.push(index);
        let node = &self.nodes[index];
        if collapsed.contains(&node.identity) {
            return;
        }
        for child in &node.children {
            self.push_visible(*child, collapsed, visible);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::markdown as flow_markdown;

    fn tree(source: &str) -> TocTree {
        let document = flow_markdown::parse(source, Default::default())
            .unwrap()
            .document;
        TocTree::new(&document)
    }

    #[test]
    fn constructs_hierarchy_from_heading_levels() {
        let tree = tree("# A\n\n### B\n\n## C\n\n# D");

        assert_eq!(
            tree.nodes()
                .iter()
                .map(|node| node.depth)
                .collect::<Vec<_>>(),
            vec![0, 1, 1, 0]
        );
        assert_eq!(tree.nodes()[0].children, vec![1, 2]);
        assert_eq!(tree.nodes()[3].parent, None);
    }

    #[test]
    fn collapse_hides_descendants() {
        let tree = tree("# A\n\n## B\n\n### C\n\n# D");
        let collapsed = HashSet::from([tree.nodes()[0].identity.clone()]);

        assert_eq!(tree.visible(&collapsed, ""), vec![0, 3]);
    }

    #[test]
    fn filtering_selects_the_first_direct_match() {
        let tree = tree("# Parent\n\n## Alpha match\n\n## Beta match");

        assert_eq!(tree.first_match("MATCH"), Some(1));
        assert_eq!(tree.first_match("missing"), None);
    }

    #[test]
    fn filtering_keeps_ancestor_context_and_ignores_collapse() {
        let tree = tree("# Parent\n\n## Other\n\n### Match me\n\n# Unrelated");
        let collapsed = HashSet::from([tree.nodes()[0].identity.clone()]);

        assert_eq!(tree.visible(&collapsed, "MATCH"), vec![0, 1, 2]);
    }

    #[test]
    fn constructs_flow_outline_from_heading_semantics_and_canonical_labels() {
        let document = flow_markdown::parse(
            "# Parent *styled* ![mark](mark.png)\n\n### Nested\n\n## Sibling\n\n# Last",
            Default::default(),
        )
        .unwrap()
        .document;
        let tree = TocTree::new(&document);

        assert_eq!(
            tree.nodes()
                .iter()
                .map(|node| node.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Parent styled mark", "Nested", "Sibling", "Last"]
        );
        assert_eq!(
            tree.nodes()
                .iter()
                .map(|node| node.depth)
                .collect::<Vec<_>>(),
            vec![0, 1, 1, 0]
        );
        assert!(
            tree.nodes()[0]
                .identity
                .flow
                .as_ref()
                .unwrap()
                .anchors
                .iter()
                .any(|anchor| anchor.as_str() == "parent-styled-mark")
        );
    }

    #[test]
    fn flow_outline_recovers_heading_identity_by_authoritative_anchor() {
        let original = flow_markdown::parse("# Target", Default::default())
            .unwrap()
            .document;
        let identity = TocTree::new(&original).nodes()[0].identity.clone();
        let edited = flow_markdown::parse("intro\n\n# Target", Default::default())
            .unwrap()
            .document;
        let tree = TocTree::new(&edited);

        let recovered = tree.resolve(&identity).unwrap();
        assert_eq!(tree.nodes()[recovered].label, "Target");
        assert_ne!(
            tree.nodes()[recovered].identity.flow.as_ref().unwrap().node,
            identity.flow.as_ref().unwrap().node
        );
    }
}
