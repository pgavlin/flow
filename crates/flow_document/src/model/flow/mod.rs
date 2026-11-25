//! flow IR v1's immutable semantic vocabulary.
//!
//! Input formats lower directly into this authoritative semantic vocabulary;
//! viewer consumers derive presentation and interaction indexes from it.

mod builder;
#[cfg(test)]
mod canonical_tests;
#[cfg(test)]
mod catalog_tests;
mod catalogs;
mod finalize;
mod ids;
mod node;
mod styles;
mod text;

pub use builder::{BuildError, FlowBuilder, GrammarViolation, StructuralResource};
pub use catalogs::*;
pub use ids::*;
pub use node::*;
pub use styles::*;
pub use text::*;

use std::collections::BTreeMap;

/// An immutable, independently loadable semantic flow document.
///
/// All owning storage is private. Document-local IDs are resolved through checked
/// queries and cannot be fabricated from raw integers by public callers.
#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
pub struct FlowDocument {
    text: LogicalText,
    nodes: Vec<Node>,
    root: NodeId,
    anchors: BTreeMap<AnchorId, Anchor>,
    resource_base: Option<String>,
    resources: Vec<Resource>,
    source_artifacts: Vec<SourceArtifactDescriptor>,
    style_inputs: Vec<StyleInput>,
    origins: Vec<Origin>,
    origin_spans: Vec<OriginSpan>,
}

impl FlowDocument {
    /// Returns the sole document-root identity.
    pub const fn root(&self) -> NodeId {
        self.root
    }

    /// Returns the immutable canonical logical text.
    pub const fn text(&self) -> &LogicalText {
        &self.text
    }

    /// Looks up a same-document node, returning `None` when out of range.
    pub fn get_node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id.index())
    }

    #[cfg(feature = "render")]
    pub(crate) fn nodes(&self) -> impl ExactSizeIterator<Item = (NodeId, &Node)> {
        self.nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (NodeId::from_index(index as u32), node))
    }

    /// Resolves a style input, returning `None` when out of range.
    pub fn get_style_input(&self, id: StyleInputId) -> Option<&StyleInput> {
        self.style_inputs.get(id.index())
    }

    /// Iterates style inputs in document-local catalog order.
    pub fn style_inputs(&self) -> impl ExactSizeIterator<Item = (StyleInputId, &StyleInput)> {
        self.style_inputs
            .iter()
            .enumerate()
            .map(|(index, style)| (StyleInputId::from_index(index as u32), style))
    }

    /// Resolves provenance, returning `None` when out of range.
    pub fn get_origin(&self, id: OriginId) -> Option<&Origin> {
        self.origins.get(id.index())
    }

    /// Iterates origins in document-local catalog order.
    pub fn origins(&self) -> impl ExactSizeIterator<Item = (OriginId, &Origin)> {
        self.origins
            .iter()
            .enumerate()
            .map(|(index, origin)| (OriginId::from_index(index as u32), origin))
    }

    /// Resolves a resource, returning `None` when out of range.
    pub fn get_resource(&self, id: ResourceId) -> Option<&Resource> {
        self.resources.get(id.index())
    }

    /// Returns the source-format base used to resolve relative resources.
    pub fn resource_base(&self) -> Option<&str> {
        self.resource_base.as_deref()
    }

    /// Iterates resources in catalog order.
    pub fn resources(&self) -> impl ExactSizeIterator<Item = (ResourceId, &Resource)> {
        self.resources
            .iter()
            .enumerate()
            .map(|(index, resource)| (ResourceId::from_index(index as u32), resource))
    }

    /// Resolves a source-revision descriptor, returning `None` when out of range.
    pub fn get_source_artifact(&self, id: SourceArtifactId) -> Option<&SourceArtifactDescriptor> {
        self.source_artifacts.get(id.index())
    }

    /// Iterates source descriptors in catalog order.
    pub fn source_artifacts(
        &self,
    ) -> impl ExactSizeIterator<Item = (SourceArtifactId, &SourceArtifactDescriptor)> {
        self.source_artifacts
            .iter()
            .enumerate()
            .map(|(index, source)| (SourceArtifactId::from_index(index as u32), source))
    }

    /// Returns fine-grained logical-to-source mappings in logical range order.
    pub fn origin_spans(&self) -> &[OriginSpan] {
        &self.origin_spans
    }

    /// Returns the node's parent, or `None` for the root or an invalid ID.
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.get_node(id).and_then(|node| node.parent)
    }

    /// Returns child identities, or `None` for an invalid ID.
    pub fn children(&self, id: NodeId) -> Option<&[NodeId]> {
        self.get_node(id).map(|node| node.children.as_slice())
    }

    /// Iterates from the immediate parent toward the document root.
    pub fn ancestors(&self, id: NodeId) -> impl DoubleEndedIterator<Item = NodeId> + '_ {
        let mut ancestors = Vec::new();
        let mut cursor = self.parent(id);
        while let Some(parent) = cursor {
            ancestors.push(parent);
            cursor = self.parent(parent);
        }
        ancestors.into_iter()
    }

    /// Traverses a subtree in semantic preorder, including `root`.
    pub fn preorder(&self, root: NodeId) -> impl DoubleEndedIterator<Item = NodeId> + '_ {
        let mut order = Vec::new();
        let mut stack = self.get_node(root).map(|_| vec![root]).unwrap_or_default();
        while let Some(id) = stack.pop() {
            order.push(id);
            if let Some(children) = self.children(id) {
                stack.extend(children.iter().rev().copied());
            }
        }
        order.into_iter()
    }

    /// Returns the next node in document preorder.
    pub fn following(&self, id: NodeId) -> Option<NodeId> {
        self.get_node(id)?;
        let next = id.index().checked_add(1)?;
        self.nodes
            .get(next)
            .map(|_| NodeId::from_index(next as u32))
    }

    /// Returns the previous node in document preorder.
    pub fn preceding(&self, id: NodeId) -> Option<NodeId> {
        self.get_node(id)?;
        let previous = id.index().checked_sub(1)?;
        Some(NodeId::from_index(previous as u32))
    }

    /// Resolves an exact, case-sensitive anchor identity.
    pub fn anchor(&self, id: &AnchorId) -> Option<&Anchor> {
        self.anchors.get(id)
    }

    /// Iterates anchors in exact-name order.
    pub fn anchors(&self) -> impl ExactSizeIterator<Item = (&AnchorId, &Anchor)> {
        self.anchors.iter()
    }

    /// Iterates anchors whose targets identify `node`.
    pub fn anchors_for(&self, node: NodeId) -> impl Iterator<Item = (&AnchorId, &Anchor)> {
        self.anchors
            .iter()
            .filter(move |(_, anchor)| anchor.target.node == node)
    }

    /// Slices canonical text when the range is ordered, in bounds, and on UTF-8 boundaries.
    pub fn get_text(&self, range: TextRange) -> Option<&str> {
        self.text.get(range)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU32;

    struct TestDocumentBuilder {
        text: String,
        nodes: Vec<Node>,
        resources: Vec<Resource>,
    }

    impl TestDocumentBuilder {
        fn new(text: &str) -> Self {
            Self {
                text: text.into(),
                nodes: Vec::new(),
                resources: Vec::new(),
            }
        }

        fn add_node(&mut self, kind: NodeKind, parent: Option<NodeId>, range: TextRange) -> NodeId {
            let id = NodeId::from_index(self.nodes.len() as u32);
            self.nodes.push(Node {
                parent,
                children: Vec::new(),
                kind,
                text_range: range,
                style_input: StyleInputId::DEFAULT,
                semantics: Semantics::default(),
                origin: None,
            });
            if let Some(parent) = parent {
                self.nodes[parent.index()].children.push(id);
            }
            id
        }

        fn add_resource(&mut self, identity: &str) -> ResourceId {
            let id = ResourceId::from_index(self.resources.len() as u32);
            self.resources.push(Resource {
                identity: identity.into(),
                media_type: None,
            });
            id
        }

        fn invalid_node_id(&self) -> NodeId {
            NodeId::from_index(self.nodes.len() as u32)
        }

        fn finish(self, root: NodeId) -> FlowDocument {
            FlowDocument {
                text: LogicalText::from_boxed_str(self.text.into_boxed_str()),
                nodes: self.nodes,
                root,
                anchors: BTreeMap::new(),
                resource_base: None,
                resources: self.resources,
                source_artifacts: Vec::new(),
                style_inputs: vec![StyleInput::default_input()],
                origins: Vec::new(),
                origin_spans: Vec::new(),
            }
        }
    }

    fn range(start: usize, end: usize) -> TextRange {
        TextRange::new(TextOffset::new(start), TextOffset::new(end))
    }

    fn one() -> NonZeroU32 {
        NonZeroU32::new(1).unwrap()
    }

    fn destination() -> Destination {
        Destination::External("https://example.com".into())
    }

    type ClassificationCase = (NodeKind, ChildGrammar, bool, bool, LogicalTextRule);

    fn classification_cases(resource: ResourceId, header: NodeId) -> Vec<ClassificationCase> {
        use ChildGrammar::{Flow, None as NoChildren, Phrasing, Specialized};
        use LogicalTextRule::{
            AtomicAlternative, BreakOpportunity as BreakOpportunityText, Empty, FlowChildren,
            HardBreak as HardBreakText, InlineChildren, ObjectReplacement, TableCells, TableRoot,
            TableRows, TextLiteral,
        };

        let list = || List {
            kind: ListKind::Ordered,
            start: 1,
            reversed: false,
        };
        let link = |content| {
            NodeKind::Link(Link {
                content,
                destination: destination(),
                relation: LinkRelation::Ordinary,
                title: None,
            })
        };
        let math = |display| {
            NodeKind::Math(Math {
                notation: MathNotation::Tex,
                source: "x".into(),
                display,
            })
        };
        let unsupported = |placement| {
            NodeKind::UnsupportedObject(UnsupportedObject {
                placement,
                media_kind: MediaKind::Unknown,
                title: None,
                size_hint: None,
                activation: Some(destination()),
                reason: UnsupportedReason::NoNativePresenter,
            })
        };

        vec![
            (NodeKind::Document, Flow, false, false, FlowChildren),
            (NodeKind::Flow, Flow, true, false, FlowChildren),
            (NodeKind::Section, Flow, true, false, FlowChildren),
            (NodeKind::Paragraph, Phrasing, true, false, InlineChildren),
            (
                NodeKind::Heading(Heading {
                    level: HeadingLevel::H1,
                }),
                Phrasing,
                true,
                false,
                InlineChildren,
            ),
            (NodeKind::Quotation, Flow, true, false, FlowChildren),
            (
                NodeKind::List(list()),
                Specialized,
                true,
                false,
                FlowChildren,
            ),
            (
                NodeKind::ListItem(ListItem { ordinal: None }),
                Flow,
                false,
                false,
                FlowChildren,
            ),
            (
                NodeKind::DefinitionList,
                Specialized,
                true,
                false,
                FlowChildren,
            ),
            (NodeKind::Term, Phrasing, false, false, InlineChildren),
            (NodeKind::Definition, Flow, false, false, FlowChildren),
            (
                NodeKind::Preformatted(Preformatted {
                    language: None,
                    content_kind: PreformattedKind::PlainText,
                }),
                Phrasing,
                true,
                false,
                InlineChildren,
            ),
            (NodeKind::Figure, Specialized, true, false, FlowChildren),
            (NodeKind::Caption, Flow, false, false, FlowChildren),
            (
                NodeKind::Table(Table {
                    column_count: one(),
                }),
                Specialized,
                true,
                false,
                TableRoot,
            ),
            (NodeKind::TableCaption, Flow, false, false, FlowChildren),
            (
                NodeKind::ColumnGroup(ColumnGroup { span: one() }),
                Specialized,
                false,
                false,
                Empty,
            ),
            (
                NodeKind::Column(Column { span: one() }),
                NoChildren,
                false,
                false,
                Empty,
            ),
            (
                NodeKind::RowGroup(RowGroup::Body),
                Specialized,
                false,
                false,
                TableRows,
            ),
            (NodeKind::Row, Specialized, false, false, TableCells),
            (
                NodeKind::Cell(Cell {
                    kind: CellKind::Header,
                    column_start: 0,
                    column_span: one(),
                    row_span: one(),
                    scope: None,
                    explicit_headers: Some(vec![header]),
                }),
                Flow,
                false,
                false,
                FlowChildren,
            ),
            (
                NodeKind::Disclosure(Disclosure {
                    initially_open: false,
                }),
                Specialized,
                true,
                false,
                FlowChildren,
            ),
            (NodeKind::Summary, Phrasing, false, false, InlineChildren),
            (
                NodeKind::ThematicBreak,
                NoChildren,
                true,
                false,
                ObjectReplacement,
            ),
            (
                NodeKind::LocationMarker(LocationMarker {
                    kind: LocationMarkerKind::Other,
                    label: None,
                }),
                NoChildren,
                true,
                true,
                Empty,
            ),
            (
                NodeKind::Image(Image {
                    resource,
                    alternative: ImageAlternative::Missing,
                    title: None,
                    responsive: None,
                    intrinsic_size: None,
                }),
                NoChildren,
                true,
                true,
                AtomicAlternative,
            ),
            (
                math(MathDisplay::Inline),
                NoChildren,
                false,
                true,
                AtomicAlternative,
            ),
            (
                math(MathDisplay::Block),
                NoChildren,
                true,
                false,
                AtomicAlternative,
            ),
            (
                unsupported(Placement::Inline),
                Specialized,
                false,
                true,
                AtomicAlternative,
            ),
            (
                unsupported(Placement::Flow),
                Specialized,
                true,
                false,
                AtomicAlternative,
            ),
            (NodeKind::Inline, Phrasing, false, true, InlineChildren),
            (
                NodeKind::InlineSemantic(InlineSemantic {
                    role: InlineRole::Emphasis,
                    value: None,
                }),
                Phrasing,
                false,
                true,
                InlineChildren,
            ),
            (
                link(LinkContent::Phrasing),
                Phrasing,
                false,
                true,
                InlineChildren,
            ),
            (link(LinkContent::Flow), Flow, true, false, FlowChildren),
            (NodeKind::Ruby, Specialized, false, true, InlineChildren),
            (
                NodeKind::RubySegment,
                Specialized,
                false,
                false,
                InlineChildren,
            ),
            (
                NodeKind::RubyBaseLevel,
                Specialized,
                false,
                false,
                InlineChildren,
            ),
            (
                NodeKind::RubyAnnotationLevel,
                Specialized,
                false,
                false,
                InlineChildren,
            ),
            (NodeKind::RubyBase, Phrasing, false, false, InlineChildren),
            (
                NodeKind::RubyAnnotation(RubyAnnotation { base_span: one() }),
                Phrasing,
                false,
                false,
                InlineChildren,
            ),
            (
                NodeKind::RubyFallback,
                Phrasing,
                false,
                false,
                InlineChildren,
            ),
            (NodeKind::Text, NoChildren, false, true, TextLiteral),
            (NodeKind::HardBreak, NoChildren, false, true, HardBreakText),
            (
                NodeKind::BreakOpportunity(BreakOpportunity {
                    hyphen_when_broken: false,
                }),
                NoChildren,
                false,
                true,
                BreakOpportunityText,
            ),
            (
                NodeKind::Checkbox(Checkbox {
                    state: CheckboxState::Unchecked,
                }),
                NoChildren,
                false,
                true,
                Empty,
            ),
        ]
    }

    #[test]
    fn validates_local_payloads() {
        assert_eq!(HeadingLevel::new(1), Some(HeadingLevel::H1));
        assert_eq!(HeadingLevel::new(6), Some(HeadingLevel::H6));
        assert_eq!(HeadingLevel::new(0), None);
        assert_eq!(HeadingLevel::new(7), None);

        assert!(NonEmptyText::new("").is_err());
        assert_eq!(NonEmptyText::new(" ").unwrap().as_str(), " ");

        for invalid in [0.0, -1.0, f32::INFINITY, f32::NEG_INFINITY, f32::NAN] {
            assert!(PositiveCssPixels::new(invalid).is_none());
        }
        assert_eq!(PositiveCssPixels::new(1.5).unwrap().get(), 1.5);

        assert_eq!(LanguageTag::parse("EN-latn-us").unwrap().as_str(), "en-US");
        assert!(LanguageTag::parse("").is_err());
        assert!(LanguageTag::parse("not_a_tag").is_err());
    }

    #[test]
    fn every_node_kind_matches_the_classification_table() {
        let mut builder = TestDocumentBuilder::new("");
        let header = builder.add_node(NodeKind::Text, None, range(0, 0));
        let resource = builder.add_resource("image.png");
        let cases = classification_cases(resource, header);

        // The 42 variants plus the alternate data-dependent forms of Link,
        // Math, and UnsupportedObject must each have an asserted row.
        assert_eq!(cases.len(), 45);
        for (kind, grammar, flow, phrasing, text_rule) in cases {
            assert_eq!(kind.child_grammar(), grammar, "child grammar for {kind:?}");
            assert_eq!(kind.is_flow_child(), flow, "flow category for {kind:?}");
            assert_eq!(
                kind.is_phrasing_child(),
                phrasing,
                "phrasing category for {kind:?}"
            );
            assert_eq!(
                kind.logical_text_rule(),
                text_rule,
                "logical text rule for {kind:?}"
            );
        }
    }

    #[test]
    fn test_builder_is_the_only_raw_id_factory() {
        let mut builder = TestDocumentBuilder::new("hello");
        let root = builder.add_node(NodeKind::Document, None, range(0, 5));
        let paragraph = builder.add_node(NodeKind::Paragraph, Some(root), range(0, 5));
        let text = builder.add_node(NodeKind::Text, Some(paragraph), range(0, 5));
        let document = builder.finish(root);

        assert_eq!(document.root(), root);
        assert_eq!(document.children(root), Some(&[paragraph][..]));
        assert_eq!(document.parent(text), Some(paragraph));
        assert_eq!(
            document.preorder(root).collect::<Vec<_>>(),
            vec![root, paragraph, text]
        );
        assert_eq!(
            document.ancestors(text).collect::<Vec<_>>(),
            vec![paragraph, root]
        );
        assert_eq!(document.following(root), Some(paragraph));
        assert_eq!(document.preceding(text), Some(paragraph));
    }

    #[test]
    fn checked_lookups_reject_out_of_bounds_ids_and_ranges() {
        let mut builder = TestDocumentBuilder::new("é");
        let root = builder.add_node(NodeKind::Document, None, range(0, 2));
        let invalid = builder.invalid_node_id();
        let document = builder.finish(root);

        assert!(document.get_node(invalid).is_none());
        assert!(document.children(invalid).is_none());
        assert!(
            document
                .get_style_input(StyleInputId::from_index(1))
                .is_none()
        );
        assert!(document.get_resource(ResourceId::from_index(0)).is_none());
        assert!(document.get_origin(OriginId::from_index(0)).is_none());
        assert!(
            document
                .get_source_artifact(SourceArtifactId::from_index(0))
                .is_none()
        );
        assert_eq!(document.get_text(range(0, 2)), Some("é"));
        assert!(document.get_text(range(1, 2)).is_none());
        assert!(document.get_text(range(2, 1)).is_none());
    }

    #[test]
    fn default_style_input_is_catalog_entry_zero() {
        let mut builder = TestDocumentBuilder::new("");
        let root = builder.add_node(NodeKind::Document, None, range(0, 0));
        let document = builder.finish(root);
        assert!(document.get_style_input(StyleInputId::DEFAULT).is_some());
        assert_eq!(
            document.get_node(root).unwrap().style_input,
            StyleInputId::DEFAULT
        );
    }
}
