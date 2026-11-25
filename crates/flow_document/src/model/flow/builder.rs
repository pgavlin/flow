//! Validating construction and canonical finalization for flow IR.

use std::collections::{BTreeMap, HashSet};
use std::ops::Range;

use thiserror::Error;

use super::finalize::finalize_text;
use super::{
    Affinity, Anchor, AnchorId, AnchorTarget, CellKind, ChildGrammar, FlowDocument,
    LanguageOverride, LogicalText, Node, NodeId, NodeKind, Origin, OriginId, OriginSpan, Placement,
    Resource, ResourceId, Semantics, SourceArtifactDescriptor, SourceArtifactId, SourceDigest,
    StyleInput, StyleInputId, TextOffset, TextPosition, TextRange,
};

const MAX_NODES: usize = u32::MAX as usize;
const MAX_TABLE_ROWS: usize = 65_536;
const MAX_TABLE_CELLS: usize = 1_000_000;
const MAX_TABLE_COLUMNS: u32 = 4_096;
const MAX_TABLE_PLACEMENT_STEPS: usize = 8_000_000;

/// Mutable structural constructor for one flow document.
///
/// IDs returned before finalization identify draft slots. Finalization validates
/// the complete ownership graph and remaps those slots into deterministic
/// semantic preorder.
pub struct FlowBuilder {
    nodes: Vec<DraftNode>,
    resources: Vec<Resource>,
    resource_base: Option<String>,
    anchors: BTreeMap<AnchorId, DraftAnchor>,
    source_artifacts: Vec<SourceArtifactDescriptor>,
    source_buffers: Vec<Box<str>>,
    style_inputs: Vec<StyleInput>,
    origins: Vec<Origin>,
    origin_spans: Vec<DraftOriginSpan>,
    component_parents: Vec<usize>,
    component_ranks: Vec<u8>,
}

#[derive(Clone, Debug)]
pub(super) struct DraftNode {
    pub(super) parent: Option<NodeId>,
    pub(super) children: Vec<NodeId>,
    pub(super) kind: NodeKind,
    pub(super) semantics: Semantics,
    pub(super) literal: Option<Box<str>>,
    style_input: StyleInputId,
    origin: Option<OriginId>,
}

#[derive(Clone, Copy, Debug)]
struct DraftAnchor {
    node: NodeId,
    origin: Option<OriginId>,
}

#[derive(Clone, Debug)]
struct DraftOriginSpan {
    node: NodeId,
    relative: Range<usize>,
    origin: OriginId,
    source_range: Option<Range<usize>>,
}

/// Structural construction or validation failure.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum BuildError {
    #[error("optional metadata field {field} must not contain an empty string")]
    EmptyOptionalMetadata { field: &'static str },

    /// An operation referenced a draft ID outside this builder.
    #[error("node ID {node:?} is outside this builder")]
    InvalidNode { node: NodeId },

    /// The node table cannot be represented by the compact ID type.
    #[error("the document exceeds the node limit")]
    NodeLimit,

    /// Literal text must be supplied through `FlowBuilder::add_text`.
    #[error("text nodes require an explicit draft literal")]
    TextLiteralRequired,

    /// Canonical text length overflowed the resident string coordinate space.
    #[error("canonical text is too large for one resident flow document")]
    CanonicalTextTooLarge,

    /// An exact anchor name was inserted more than once.
    #[error("duplicate anchor {anchor:?}")]
    DuplicateAnchor { anchor: AnchorId },

    #[error("style input ID {style:?} is outside this builder")]
    InvalidStyleInput { style: StyleInputId },

    #[error("origin ID {origin:?} is outside this builder")]
    InvalidOrigin { origin: OriginId },

    #[error("source artifact ID {artifact:?} is outside this builder")]
    InvalidSourceArtifact { artifact: SourceArtifactId },

    #[error("the supplied source bytes do not match the descriptor digest")]
    SourceDigestMismatch,

    #[error("source range {range:?} is invalid for artifact {artifact:?}")]
    InvalidSourceRange {
        artifact: SourceArtifactId,
        range: Range<usize>,
    },

    #[error("logical origin span {range:?} is invalid for node {node:?}")]
    InvalidOriginSpan { node: NodeId, range: Range<usize> },

    /// A child already belongs to another parent.
    #[error("node {child:?} already has parent {parent:?}")]
    AlreadyParented { child: NodeId, parent: NodeId },

    /// An append would make a node its own ancestor.
    #[error("appending {child:?} beneath {parent:?} would create a cycle")]
    Cycle { parent: NodeId, child: NodeId },

    /// A child's kind is not accepted by its parent's broad or specialized grammar.
    #[error("node {child:?} is not a valid direct child of {parent:?}")]
    InvalidChild { parent: NodeId, child: NodeId },

    /// A complete specialized child sequence violates its grammar.
    #[error("node {node:?} has invalid specialized child grammar: {violation:?}")]
    SpecializedGrammar {
        node: NodeId,
        violation: GrammarViolation,
    },

    /// A link contains another link at any depth.
    #[error("link {link:?} contains nested link {nested:?}")]
    NestedLink { link: NodeId, nested: NodeId },

    /// Finalization did not find exactly one document node.
    #[error("expected exactly one document root, found {count}")]
    DocumentRootCount { count: usize },

    /// The sole document node was attached beneath another node.
    #[error("document root {root:?} has a parent")]
    RootHasParent { root: NodeId },

    /// A non-root node is not owned by the root tree.
    #[error("node {node:?} is not reachable from the document root")]
    UnreachableNode { node: NodeId },

    /// A table cell span cannot fit within its row group or safety limits.
    #[error("table cell {cell:?} has an invalid normalized span")]
    InvalidTableSpan { cell: NodeId },

    /// A table header reference is invalid, duplicated, non-header, or foreign.
    #[error("table cell {cell:?} has invalid explicit header {header:?}")]
    InvalidTableHeader { cell: NodeId, header: NodeId },

    /// Semantic metadata repeats a non-inherited role on one node.
    #[error("node {node:?} contains a duplicate semantic role")]
    DuplicateSemanticRole { node: NodeId },

    /// A node references a resource outside this builder's catalog.
    #[error("node {node:?} has invalid resource {resource:?}")]
    InvalidResource { node: NodeId, resource: ResourceId },

    /// A ruby annotation level does not form a normalized grid.
    #[error("ruby node {node:?} has an invalid normalized annotation grid")]
    InvalidRubyGrid { node: NodeId },

    /// A bounded structural operation exceeded its documented safety limit.
    #[error("{resource:?} exceeds the structural resource limit")]
    ResourceLimit { resource: StructuralResource },
}

/// Specialized child-sequence invariant that was violated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GrammarViolation {
    EmptyList,
    DefinitionListSequence,
    MultipleFigureCaptions,
    TableSequence,
    EmptyRowGroup,
    EmptyRow,
    DisclosureSummaryPosition,
    EmptyRuby,
    RubySegmentSequence,
    EmptyRubyBaseLevel,
    RubyAnnotationSequence,
}

/// Bounded resource whose limit was exceeded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StructuralResource {
    TableRows,
    TableCells,
    TableColumns,
    TablePlacementWork,
    Resources,
    SourceArtifacts,
    StyleInputs,
    Origins,
    OriginSpans,
}

impl Default for FlowBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl FlowBuilder {
    /// Creates an empty draft builder.
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            resources: Vec::new(),
            resource_base: None,
            anchors: BTreeMap::new(),
            source_artifacts: Vec::new(),
            source_buffers: Vec::new(),
            style_inputs: vec![StyleInput::default_input()],
            origins: Vec::new(),
            origin_spans: Vec::new(),
            component_parents: Vec::new(),
            component_ranks: Vec::new(),
        }
    }

    /// Registers a document-local resource and returns its draft identity.
    pub fn add_resource(&mut self, resource: Resource) -> Result<ResourceId, BuildError> {
        reject_empty_optional("resource.media_type", &resource.media_type)?;
        if self.resources.len() >= u32::MAX as usize {
            return Err(BuildError::ResourceLimit {
                resource: StructuralResource::Resources,
            });
        }
        let id = ResourceId::from_index(self.resources.len() as u32);
        self.resources.push(resource);
        Ok(id)
    }

    /// Sets the source-format base used to resolve relative resources.
    pub fn set_resource_base(&mut self, base: Option<String>) {
        self.resource_base = base;
    }

    /// Adds an exact source revision after verifying its descriptor digest.
    pub fn add_source_artifact(
        &mut self,
        descriptor: SourceArtifactDescriptor,
        source: impl Into<String>,
    ) -> Result<SourceArtifactId, BuildError> {
        let source = source.into().into_boxed_str();
        reject_empty_optional("source_artifact.media_type", &descriptor.media_type)?;
        if !descriptor.revision.verifies(&source) {
            return Err(BuildError::SourceDigestMismatch);
        }
        if self.source_artifacts.len() >= u32::MAX as usize {
            return Err(BuildError::ResourceLimit {
                resource: StructuralResource::SourceArtifacts,
            });
        }
        let id = SourceArtifactId::from_index(self.source_artifacts.len() as u32);
        self.source_artifacts.push(descriptor);
        self.source_buffers.push(source);
        Ok(id)
    }

    /// Restores an already dependency-verified source descriptor for cache
    /// validation. The placeholder preserves source-range bounds without
    /// retaining publication source text in the semantic artifact.
    #[cfg(feature = "render")]
    pub(crate) fn add_cached_source_artifact(
        &mut self,
        descriptor: SourceArtifactDescriptor,
        source_len: usize,
    ) -> Result<SourceArtifactId, BuildError> {
        reject_empty_optional("source_artifact.media_type", &descriptor.media_type)?;
        if self.source_artifacts.len() >= u32::MAX as usize {
            return Err(BuildError::ResourceLimit {
                resource: StructuralResource::SourceArtifacts,
            });
        }
        let id = SourceArtifactId::from_index(self.source_artifacts.len() as u32);
        self.source_artifacts.push(descriptor);
        self.source_buffers
            .push("\0".repeat(source_len).into_boxed_str());
        Ok(id)
    }

    /// Adds source bytes and computes their exact BLAKE3 revision.
    pub fn add_source(
        &mut self,
        source: impl Into<String>,
        identity: Option<super::DocumentReference>,
        media_type: Option<String>,
    ) -> Result<SourceArtifactId, BuildError> {
        let source = source.into();
        let descriptor = SourceArtifactDescriptor {
            identity,
            revision: SourceDigest::for_text(&source),
            media_type,
        };
        self.add_source_artifact(descriptor, source)
    }

    /// Interns an immutable style input in document-local order.
    pub fn intern_style_input(&mut self, style: StyleInput) -> Result<StyleInputId, BuildError> {
        if let Some(index) = self
            .style_inputs
            .iter()
            .position(|candidate| candidate == &style)
        {
            return Ok(StyleInputId::from_index(index as u32));
        }
        if self.style_inputs.len() >= u32::MAX as usize {
            return Err(BuildError::ResourceLimit {
                resource: StructuralResource::StyleInputs,
            });
        }
        let id = StyleInputId::from_index(self.style_inputs.len() as u32);
        self.style_inputs.push(style);
        Ok(id)
    }

    /// Adds an unattached node and returns its draft identity.
    pub fn add_node(&mut self, kind: NodeKind) -> Result<NodeId, BuildError> {
        if matches!(kind, NodeKind::Text) {
            return Err(BuildError::TextLiteralRequired);
        }
        self.insert_draft(kind, None)
    }

    /// Adds a text leaf whose literal has already undergone importer whitespace normalization.
    pub fn add_text(&mut self, literal: impl Into<String>) -> Result<NodeId, BuildError> {
        self.insert_draft(NodeKind::Text, Some(literal.into().into_boxed_str()))
    }

    /// Registers an exact anchor whose final position is derived from its node.
    pub fn add_anchor(&mut self, anchor: AnchorId, node: NodeId) -> Result<(), BuildError> {
        self.add_anchor_with_origin(anchor, node, None)
    }

    /// Registers an exact anchor and the source construct that supplied it.
    pub fn add_anchor_with_origin(
        &mut self,
        anchor: AnchorId,
        node: NodeId,
        origin: Option<OriginId>,
    ) -> Result<(), BuildError> {
        self.node(node)?;
        if let Some(origin) = origin {
            self.origin(origin)?;
        }
        if self.anchors.contains_key(&anchor) {
            return Err(BuildError::DuplicateAnchor { anchor });
        }
        self.anchors.insert(anchor, DraftAnchor { node, origin });
        Ok(())
    }

    fn insert_draft(
        &mut self,
        kind: NodeKind,
        literal: Option<Box<str>>,
    ) -> Result<NodeId, BuildError> {
        validate_optional_metadata(&kind)?;
        if self.nodes.len() >= MAX_NODES {
            return Err(BuildError::NodeLimit);
        }
        let index = self.nodes.len();
        let id = NodeId::from_index(index as u32);
        self.nodes.push(DraftNode {
            parent: None,
            children: Vec::new(),
            kind,
            semantics: Semantics::default(),
            literal,
            style_input: StyleInputId::DEFAULT,
            origin: None,
        });
        self.component_parents.push(index);
        self.component_ranks.push(0);
        Ok(id)
    }

    /// Replaces semantic metadata on a draft node.
    pub fn set_semantics(&mut self, node: NodeId, semantics: Semantics) -> Result<(), BuildError> {
        self.node_mut(node)?.semantics = semantics;
        Ok(())
    }

    /// Assigns an interned style input to a draft node.
    pub fn set_style_input(&mut self, node: NodeId, style: StyleInputId) -> Result<(), BuildError> {
        if self.style_inputs.get(style.index()).is_none() {
            return Err(BuildError::InvalidStyleInput { style });
        }
        self.node_mut(node)?.style_input = style;
        Ok(())
    }

    /// Adds validated provenance to the document-local origin catalog.
    pub fn add_origin(&mut self, origin: Origin) -> Result<OriginId, BuildError> {
        self.validate_origin(&origin)?;
        if self.origins.len() >= u32::MAX as usize {
            return Err(BuildError::ResourceLimit {
                resource: StructuralResource::Origins,
            });
        }
        let id = OriginId::from_index(self.origins.len() as u32);
        self.origins.push(origin);
        Ok(id)
    }

    /// Assigns provenance to a draft node.
    pub fn set_origin(&mut self, node: NodeId, origin: OriginId) -> Result<(), BuildError> {
        self.origin(origin)?;
        self.node_mut(node)?.origin = Some(origin);
        Ok(())
    }

    /// Adds a fine-grained mapping relative to a node's finalized canonical range.
    pub fn add_origin_span(
        &mut self,
        node: NodeId,
        relative: Range<usize>,
        origin: OriginId,
        source_range: Option<Range<usize>>,
    ) -> Result<(), BuildError> {
        self.node(node)?;
        let origin_record = self.origin(origin)?;
        if let Some(range) = &source_range {
            let source =
                self.origin_source(origin_record)
                    .ok_or_else(|| BuildError::InvalidOriginSpan {
                        node,
                        range: relative.clone(),
                    })?;
            self.validate_source_range(source, range)?;
        }
        if self.origin_spans.len() >= u32::MAX as usize {
            return Err(BuildError::ResourceLimit {
                resource: StructuralResource::OriginSpans,
            });
        }
        self.origin_spans.push(DraftOriginSpan {
            node,
            relative,
            origin,
            source_range,
        });
        Ok(())
    }

    /// Attaches one unowned child after checking direct grammar and cycles.
    pub fn append(&mut self, parent: NodeId, child: NodeId) -> Result<(), BuildError> {
        let parent_kind = &self.node(parent)?.kind;
        let child_kind = &self.node(child)?.kind;
        if !accepts_direct_child(parent_kind, child_kind) {
            return Err(BuildError::InvalidChild { parent, child });
        }
        if let Some(existing) = self.node(child)?.parent {
            return Err(BuildError::AlreadyParented {
                child,
                parent: existing,
            });
        }
        let parent_component = self.find_component(parent.index());
        let child_component = self.find_component(child.index());
        if parent_component == child_component {
            return Err(BuildError::Cycle { parent, child });
        }

        self.union_components(parent_component, child_component);
        self.node_mut(child)?.parent = Some(parent);
        self.node_mut(parent)?.children.push(child);
        Ok(())
    }

    /// Validates, normalizes, derives canonical text, and freezes the document.
    pub fn finish(mut self) -> Result<FlowDocument, BuildError> {
        let root = self.validate_ownership()?;
        if self.nodes[root.index()].semantics.language == LanguageOverride::Inherit {
            self.nodes[root.index()].semantics.language = LanguageOverride::Unknown;
        }
        self.validate_grammars(root)?;
        self.validate_references(root)?;
        self.normalize_tables(root)?;
        self.validate_ruby(root)?;
        self.freeze(root)
    }

    fn validate_origin(&self, origin: &Origin) -> Result<(), BuildError> {
        match origin {
            Origin::Markdown {
                source,
                source_range,
            } => self.validate_source_range(*source, source_range),
            Origin::Xhtml {
                source,
                serialized_range,
                ..
            }
            | Origin::Html {
                source,
                serialized_range,
                ..
            } => {
                self.source_artifact(*source)?;
                if let Some(range) = serialized_range {
                    self.validate_source_range(*source, range)?;
                }
                Ok(())
            }
            Origin::Generated { related, .. } => {
                if let Some(related) = related {
                    self.origin(*related)?;
                }
                Ok(())
            }
        }
    }

    fn validate_source_range(
        &self,
        source: SourceArtifactId,
        range: &Range<usize>,
    ) -> Result<(), BuildError> {
        let text = self.source_buffer(source)?;
        if range.start > range.end
            || range.end > text.len()
            || !text.is_char_boundary(range.start)
            || !text.is_char_boundary(range.end)
        {
            return Err(BuildError::InvalidSourceRange {
                artifact: source,
                range: range.clone(),
            });
        }
        Ok(())
    }

    fn origin_source(&self, origin: &Origin) -> Option<SourceArtifactId> {
        match origin {
            Origin::Markdown { source, .. }
            | Origin::Xhtml { source, .. }
            | Origin::Html { source, .. } => Some(*source),
            Origin::Generated { .. } => None,
        }
    }

    fn find_component(&mut self, mut index: usize) -> usize {
        let mut root = index;
        while self.component_parents[root] != root {
            root = self.component_parents[root];
        }
        while index != root {
            let parent = self.component_parents[index];
            self.component_parents[index] = root;
            index = parent;
        }
        root
    }

    fn union_components(&mut self, first: usize, second: usize) {
        match self.component_ranks[first].cmp(&self.component_ranks[second]) {
            std::cmp::Ordering::Less => self.component_parents[first] = second,
            std::cmp::Ordering::Greater => self.component_parents[second] = first,
            std::cmp::Ordering::Equal => {
                self.component_parents[second] = first;
                self.component_ranks[first] += 1;
            }
        }
    }

    fn source_artifact(
        &self,
        id: SourceArtifactId,
    ) -> Result<&SourceArtifactDescriptor, BuildError> {
        self.source_artifacts
            .get(id.index())
            .ok_or(BuildError::InvalidSourceArtifact { artifact: id })
    }

    fn source_buffer(&self, id: SourceArtifactId) -> Result<&str, BuildError> {
        self.source_buffers
            .get(id.index())
            .map(AsRef::as_ref)
            .ok_or(BuildError::InvalidSourceArtifact { artifact: id })
    }

    fn origin(&self, id: OriginId) -> Result<&Origin, BuildError> {
        self.origins
            .get(id.index())
            .ok_or(BuildError::InvalidOrigin { origin: id })
    }

    fn node(&self, id: NodeId) -> Result<&DraftNode, BuildError> {
        self.nodes
            .get(id.index())
            .ok_or(BuildError::InvalidNode { node: id })
    }

    fn node_mut(&mut self, id: NodeId) -> Result<&mut DraftNode, BuildError> {
        self.nodes
            .get_mut(id.index())
            .ok_or(BuildError::InvalidNode { node: id })
    }

    fn validate_ownership(&self) -> Result<NodeId, BuildError> {
        let documents = self
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| {
                matches!(node.kind, NodeKind::Document).then_some(NodeId::from_index(index as u32))
            })
            .collect::<Vec<_>>();
        if documents.len() != 1 {
            return Err(BuildError::DocumentRootCount {
                count: documents.len(),
            });
        }
        let root = documents[0];
        if self.node(root)?.parent.is_some() {
            return Err(BuildError::RootHasParent { root });
        }

        let reachable = self.preorder(root)?;
        if reachable.len() != self.nodes.len() {
            let seen = reachable.into_iter().collect::<HashSet<_>>();
            let node = (0..self.nodes.len())
                .map(|index| NodeId::from_index(index as u32))
                .find(|id| !seen.contains(id))
                .expect("node counts differ");
            return Err(BuildError::UnreachableNode { node });
        }
        Ok(root)
    }

    fn validate_grammars(&self, root: NodeId) -> Result<(), BuildError> {
        for id in self.preorder(root)? {
            let node = self.node(id)?;
            for &child in &node.children {
                if !accepts_direct_child(&node.kind, &self.node(child)?.kind) {
                    return Err(BuildError::InvalidChild { parent: id, child });
                }
            }
            if node.kind.child_grammar() == ChildGrammar::Specialized {
                self.validate_specialized(id, node)?;
            }
        }
        self.validate_no_nested_links(root)
    }

    fn validate_references(&self, root: NodeId) -> Result<(), BuildError> {
        for id in self.preorder(root)? {
            let node = &self.nodes[id.index()];
            let mut roles = HashSet::new();
            if node.semantics.roles.iter().any(|role| !roles.insert(role)) {
                return Err(BuildError::DuplicateSemanticRole { node: id });
            }
            if let NodeKind::Image(image) = &node.kind
                && self.resources.get(image.resource.index()).is_none()
            {
                return Err(BuildError::InvalidResource {
                    node: id,
                    resource: image.resource,
                });
            }
        }
        Ok(())
    }

    fn validate_specialized(&self, id: NodeId, node: &DraftNode) -> Result<(), BuildError> {
        let children = node
            .children
            .iter()
            .map(|&child| &self.nodes[child.index()].kind)
            .collect::<Vec<_>>();
        let violation = match &node.kind {
            NodeKind::List(_) if children.is_empty() => Some(GrammarViolation::EmptyList),
            NodeKind::DefinitionList if !valid_definition_list(&children) => {
                Some(GrammarViolation::DefinitionListSequence)
            }
            NodeKind::Figure
                if children
                    .iter()
                    .filter(|kind| matches!(kind, NodeKind::Caption))
                    .count()
                    > 1 =>
            {
                Some(GrammarViolation::MultipleFigureCaptions)
            }
            NodeKind::Table(_) if !valid_table_sequence(&children) => {
                Some(GrammarViolation::TableSequence)
            }
            NodeKind::RowGroup(_) if children.is_empty() => Some(GrammarViolation::EmptyRowGroup),
            NodeKind::Row if children.is_empty() => Some(GrammarViolation::EmptyRow),
            NodeKind::Disclosure(_)
                if children
                    .iter()
                    .enumerate()
                    .any(|(index, kind)| matches!(kind, NodeKind::Summary) && index != 0) =>
            {
                Some(GrammarViolation::DisclosureSummaryPosition)
            }
            NodeKind::Ruby if children.is_empty() => Some(GrammarViolation::EmptyRuby),
            NodeKind::RubySegment if !valid_ruby_segment(&children) => {
                Some(GrammarViolation::RubySegmentSequence)
            }
            NodeKind::RubyBaseLevel if children.is_empty() => {
                Some(GrammarViolation::EmptyRubyBaseLevel)
            }
            NodeKind::RubyAnnotationLevel if !valid_ruby_annotation_level(&children) => {
                Some(GrammarViolation::RubyAnnotationSequence)
            }
            _ => None,
        };
        if let Some(violation) = violation {
            Err(BuildError::SpecializedGrammar {
                node: id,
                violation,
            })
        } else {
            Ok(())
        }
    }

    fn validate_no_nested_links(&self, root: NodeId) -> Result<(), BuildError> {
        let mut stack = vec![(root, None)];
        while let Some((id, containing_link)) = stack.pop() {
            let node = self.node(id)?;
            let next_link = if matches!(node.kind, NodeKind::Link(_)) {
                if let Some(link) = containing_link {
                    return Err(BuildError::NestedLink { link, nested: id });
                }
                Some(id)
            } else {
                containing_link
            };
            stack.extend(node.children.iter().rev().map(|&child| (child, next_link)));
        }
        Ok(())
    }

    fn normalize_tables(&mut self, root: NodeId) -> Result<(), BuildError> {
        let tables = self
            .preorder(root)?
            .into_iter()
            .filter(|&id| matches!(self.nodes[id.index()].kind, NodeKind::Table(_)))
            .collect::<Vec<_>>();
        for table in tables {
            self.normalize_table(table)?;
        }
        Ok(())
    }

    fn normalize_table(&mut self, table: NodeId) -> Result<(), BuildError> {
        let mut declared_columns = 0u32;
        for group in self.nodes[table.index()]
            .children
            .iter()
            .copied()
            .filter(|&id| matches!(self.nodes[id.index()].kind, NodeKind::ColumnGroup(_)))
        {
            let children = &self.nodes[group.index()].children;
            let group_columns = if children.is_empty() {
                match &self.nodes[group.index()].kind {
                    NodeKind::ColumnGroup(metadata) => metadata.span.get(),
                    _ => unreachable!(),
                }
            } else {
                children
                    .iter()
                    .try_fold(0u32, |count, child| {
                        let span = match &self.nodes[child.index()].kind {
                            NodeKind::Column(metadata) => metadata.span.get(),
                            _ => unreachable!(),
                        };
                        count.checked_add(span)
                    })
                    .ok_or(BuildError::ResourceLimit {
                        resource: StructuralResource::TableColumns,
                    })?
            };
            declared_columns =
                declared_columns
                    .checked_add(group_columns)
                    .ok_or(BuildError::ResourceLimit {
                        resource: StructuralResource::TableColumns,
                    })?;
            if declared_columns > MAX_TABLE_COLUMNS {
                return Err(BuildError::ResourceLimit {
                    resource: StructuralResource::TableColumns,
                });
            }
        }

        // Count rows and cells without allocating attacker-sized temporary
        // collections. Only after both limits pass do we collect bounded IDs for
        // normalization and header validation.
        let mut total_rows = 0usize;
        let mut total_cells = 0usize;
        for group in self.nodes[table.index()]
            .children
            .iter()
            .copied()
            .filter(|&id| matches!(self.nodes[id.index()].kind, NodeKind::RowGroup(_)))
        {
            let rows = &self.nodes[group.index()].children;
            total_rows = total_rows.saturating_add(rows.len());
            if total_rows > MAX_TABLE_ROWS {
                return Err(BuildError::ResourceLimit {
                    resource: StructuralResource::TableRows,
                });
            }
            for &row in rows {
                total_cells = total_cells.saturating_add(self.nodes[row.index()].children.len());
                if total_cells > MAX_TABLE_CELLS {
                    return Err(BuildError::ResourceLimit {
                        resource: StructuralResource::TableCells,
                    });
                }
            }
        }
        let row_groups = self.nodes[table.index()]
            .children
            .iter()
            .copied()
            .filter(|&id| matches!(self.nodes[id.index()].kind, NodeKind::RowGroup(_)))
            .collect::<Vec<_>>();
        let table_cells = row_groups
            .iter()
            .flat_map(|&group| self.nodes[group.index()].children.iter().copied())
            .flat_map(|row| self.nodes[row.index()].children.iter().copied())
            .collect::<Vec<_>>();
        debug_assert_eq!(table_cells.len(), total_cells);
        let table_cell_set = table_cells.iter().copied().collect::<HashSet<_>>();
        // `Table::column_count` is finalized metadata. The internal staged
        // builder accepts `NodeKind`, but must not treat that draft field as a
        // third source of declared width: only column declarations and placed
        // cell edges determine the finalized count.
        let mut max_column = declared_columns;
        if max_column > MAX_TABLE_COLUMNS {
            return Err(BuildError::ResourceLimit {
                resource: StructuralResource::TableColumns,
            });
        }
        let mut work = 0usize;

        for group in row_groups {
            let rows = self.nodes[group.index()].children.clone();
            let mut reservations: Vec<(u32, u32, usize)> = Vec::new();
            for (row_index, row) in rows.iter().copied().enumerate() {
                charge_table_work(&mut work, reservations.len())?;
                reservations.retain(|(_, _, row_end)| *row_end > row_index);
                let cells = self.nodes[row.index()].children.clone();
                let mut cursor = 0u32;
                for cell in cells {
                    let (column_span, row_span) = match &self.nodes[cell.index()].kind {
                        NodeKind::Cell(metadata) => {
                            (metadata.column_span.get(), metadata.row_span.get() as usize)
                        }
                        _ => unreachable!(),
                    };
                    if row_span > rows.len() - row_index {
                        return Err(BuildError::InvalidTableSpan { cell });
                    }
                    if column_span > MAX_TABLE_COLUMNS {
                        return Err(BuildError::ResourceLimit {
                            resource: StructuralResource::TableColumns,
                        });
                    }

                    loop {
                        // Charge the candidate itself and a conservative upper
                        // bound for the linear reservation scan before doing it.
                        charge_table_work(&mut work, 1 + reservations.len())?;
                        let end =
                            cursor
                                .checked_add(column_span)
                                .ok_or(BuildError::ResourceLimit {
                                    resource: StructuralResource::TableColumns,
                                })?;
                        if end > MAX_TABLE_COLUMNS {
                            return Err(BuildError::ResourceLimit {
                                resource: StructuralResource::TableColumns,
                            });
                        }
                        if let Some((_, occupied_end, _)) = reservations
                            .iter()
                            .find(|(start, occupied_end, _)| cursor < *occupied_end && end > *start)
                        {
                            cursor = *occupied_end;
                            continue;
                        }
                        if let NodeKind::Cell(metadata) = &mut self.nodes[cell.index()].kind {
                            metadata.column_start = cursor;
                        }
                        max_column = max_column.max(end);
                        if row_span > 1 {
                            reservations.push((cursor, end, row_index + row_span));
                        }
                        cursor = end;
                        break;
                    }
                }
            }
        }

        for &cell in &table_cells {
            let headers = match &self.nodes[cell.index()].kind {
                NodeKind::Cell(metadata) => metadata.explicit_headers.as_deref(),
                _ => unreachable!(),
            };
            if let Some(headers) = headers {
                let mut seen = HashSet::new();
                for &header in headers {
                    let valid = table_cell_set.contains(&header)
                        && seen.insert(header)
                        && matches!(
                            self.nodes.get(header.index()).map(|node| &node.kind),
                            Some(NodeKind::Cell(metadata)) if metadata.kind == CellKind::Header
                        );
                    if !valid {
                        return Err(BuildError::InvalidTableHeader { cell, header });
                    }
                }
            }
        }

        if let NodeKind::Table(metadata) = &mut self.nodes[table.index()].kind {
            metadata.column_count =
                std::num::NonZeroU32::new(max_column).expect("table is nonempty");
        }
        Ok(())
    }

    fn validate_ruby(&self, root: NodeId) -> Result<(), BuildError> {
        for id in self.preorder(root)? {
            if !matches!(self.nodes[id.index()].kind, NodeKind::RubySegment) {
                continue;
            }
            let children = &self.nodes[id.index()].children;
            let base_level = children[0];
            let base_count = self.nodes[base_level.index()].children.len() as u64;
            for &level in &children[1..] {
                let mut covered = 0u64;
                for &annotation in &self.nodes[level.index()].children {
                    if let NodeKind::RubyAnnotation(metadata) = &self.nodes[annotation.index()].kind
                    {
                        covered = covered.saturating_add(u64::from(metadata.base_span.get()));
                    }
                }
                if covered != base_count {
                    return Err(BuildError::InvalidRubyGrid { node: level });
                }
            }
        }
        Ok(())
    }

    fn finalize_origin_spans(
        &self,
        text: &str,
        ranges: &[TextRange],
    ) -> Result<Vec<OriginSpan>, BuildError> {
        let mut spans = Vec::with_capacity(self.origin_spans.len());
        for draft in &self.origin_spans {
            self.origin(draft.origin)?;
            let node_range = ranges[draft.node.index()];
            let node_start = node_range.start.as_usize();
            let node_len = node_range
                .end
                .as_usize()
                .checked_sub(node_start)
                .expect("finalized node ranges are ordered");
            let start = node_start.checked_add(draft.relative.start);
            let end = node_start.checked_add(draft.relative.end);
            let valid = draft.relative.start <= draft.relative.end
                && draft.relative.end <= node_len
                && start.is_some_and(|offset| text.is_char_boundary(offset))
                && end.is_some_and(|offset| text.is_char_boundary(offset));
            if !valid {
                return Err(BuildError::InvalidOriginSpan {
                    node: draft.node,
                    range: draft.relative.clone(),
                });
            }
            spans.push(OriginSpan {
                logical: TextRange::new(
                    TextOffset::new(start.expect("validated start")),
                    TextOffset::new(end.expect("validated end")),
                ),
                origin: draft.origin,
                source_range: draft.source_range.clone(),
            });
        }
        spans.sort_by(|left, right| {
            let left_source_len = left
                .source_range
                .as_ref()
                .map_or(usize::MAX, |range| range.end - range.start);
            let right_source_len = right
                .source_range
                .as_ref()
                .map_or(usize::MAX, |range| range.end - range.start);
            (
                left.logical.start,
                left.logical.end,
                left_source_len,
                left.origin,
            )
                .cmp(&(
                    right.logical.start,
                    right.logical.end,
                    right_source_len,
                    right.origin,
                ))
        });
        Ok(spans)
    }

    fn freeze(self, root: NodeId) -> Result<FlowDocument, BuildError> {
        let order = self.preorder(root)?;
        let finalized_text = finalize_text(&self.nodes, root)?;
        let origin_spans =
            self.finalize_origin_spans(&finalized_text.text, &finalized_text.ranges)?;
        let mut remap = vec![None; self.nodes.len()];
        for (new_index, old_id) in order.iter().copied().enumerate() {
            remap[old_id.index()] = Some(NodeId::from_index(new_index as u32));
        }

        let nodes = order
            .iter()
            .copied()
            .map(|old_id| {
                let draft = &self.nodes[old_id.index()];
                let mut kind = draft.kind.clone();
                if let NodeKind::Cell(cell) = &mut kind
                    && let Some(headers) = &mut cell.explicit_headers
                {
                    for header in headers {
                        *header = remap[header.index()].expect("validated header is reachable");
                    }
                }
                Node {
                    parent: draft
                        .parent
                        .map(|id| remap[id.index()].expect("validated parent is reachable")),
                    children: draft
                        .children
                        .iter()
                        .map(|id| remap[id.index()].expect("validated child is reachable"))
                        .collect(),
                    kind,
                    text_range: finalized_text.ranges[old_id.index()],
                    style_input: draft.style_input,
                    semantics: draft.semantics.clone(),
                    origin: draft.origin,
                }
            })
            .collect();

        let anchors = self
            .anchors
            .into_iter()
            .map(|(id, draft)| {
                let node = remap[draft.node.index()].expect("anchored node is reachable");
                let text_range = finalized_text.ranges[draft.node.index()];
                let affinity = if text_range.is_empty() {
                    finalized_text.empty_affinities[draft.node.index()]
                } else {
                    Affinity::Downstream
                };
                (
                    id,
                    Anchor {
                        target: AnchorTarget {
                            node,
                            position: TextPosition::new(text_range.start, affinity),
                        },
                        origin: draft.origin,
                    },
                )
            })
            .collect();

        Ok(FlowDocument {
            text: LogicalText::from_boxed_str(finalized_text.text),
            nodes,
            root: remap[root.index()].expect("validated root is reachable"),
            anchors,
            resource_base: self.resource_base,
            resources: self.resources,
            source_artifacts: self.source_artifacts,
            style_inputs: self.style_inputs,
            origins: self.origins,
            origin_spans,
        })
    }

    fn preorder(&self, root: NodeId) -> Result<Vec<NodeId>, BuildError> {
        self.node(root)?;
        let mut order = Vec::new();
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            order.push(id);
            stack.extend(self.node(id)?.children.iter().rev().copied());
        }
        Ok(order)
    }
}

fn validate_optional_metadata(kind: &NodeKind) -> Result<(), BuildError> {
    match kind {
        NodeKind::Preformatted(metadata) => {
            reject_empty_optional("preformatted.language", &metadata.language)
        }
        NodeKind::LocationMarker(metadata) => {
            reject_empty_optional("location_marker.label", &metadata.label)
        }
        NodeKind::Image(metadata) => reject_empty_optional("image.title", &metadata.title),
        NodeKind::UnsupportedObject(metadata) => {
            reject_empty_optional("unsupported_object.title", &metadata.title)
        }
        NodeKind::InlineSemantic(metadata) => {
            reject_empty_optional("inline_semantic.value", &metadata.value)
        }
        NodeKind::Link(metadata) => reject_empty_optional("link.title", &metadata.title),
        _ => Ok(()),
    }
}

fn reject_empty_optional(field: &'static str, value: &Option<String>) -> Result<(), BuildError> {
    if value.as_deref() == Some("") {
        Err(BuildError::EmptyOptionalMetadata { field })
    } else {
        Ok(())
    }
}

fn charge_table_work(work: &mut usize, amount: usize) -> Result<(), BuildError> {
    *work = work.checked_add(amount).ok_or(BuildError::ResourceLimit {
        resource: StructuralResource::TablePlacementWork,
    })?;
    if *work > MAX_TABLE_PLACEMENT_STEPS {
        return Err(BuildError::ResourceLimit {
            resource: StructuralResource::TablePlacementWork,
        });
    }
    Ok(())
}

fn accepts_direct_child(parent: &NodeKind, child: &NodeKind) -> bool {
    match parent.child_grammar() {
        ChildGrammar::Flow => child.is_flow_child(),
        ChildGrammar::Phrasing => child.is_phrasing_child(),
        ChildGrammar::None => false,
        ChildGrammar::Specialized => match parent {
            NodeKind::List(_) => matches!(child, NodeKind::ListItem(_)),
            NodeKind::DefinitionList => matches!(child, NodeKind::Term | NodeKind::Definition),
            NodeKind::Figure => matches!(child, NodeKind::Caption) || child.is_flow_child(),
            NodeKind::Table(_) => matches!(
                child,
                NodeKind::TableCaption | NodeKind::ColumnGroup(_) | NodeKind::RowGroup(_)
            ),
            NodeKind::ColumnGroup(_) => matches!(child, NodeKind::Column(_)),
            NodeKind::RowGroup(_) => matches!(child, NodeKind::Row),
            NodeKind::Row => matches!(child, NodeKind::Cell(_)),
            NodeKind::Disclosure(_) => matches!(child, NodeKind::Summary) || child.is_flow_child(),
            NodeKind::Ruby => matches!(child, NodeKind::RubySegment),
            NodeKind::RubySegment => matches!(
                child,
                NodeKind::RubyBaseLevel | NodeKind::RubyAnnotationLevel
            ),
            NodeKind::RubyBaseLevel => matches!(child, NodeKind::RubyBase),
            NodeKind::RubyAnnotationLevel => {
                matches!(child, NodeKind::RubyAnnotation(_) | NodeKind::RubyFallback)
            }
            NodeKind::UnsupportedObject(object) => match object.placement {
                Placement::Flow => child.is_flow_child(),
                Placement::Inline => child.is_phrasing_child(),
            },
            _ => unreachable!("all specialized node kinds are covered"),
        },
    }
}

fn valid_definition_list(children: &[&NodeKind]) -> bool {
    if children.is_empty() {
        return false;
    }
    let mut index = 0;
    while index < children.len() {
        let term_start = index;
        while index < children.len() && matches!(children[index], NodeKind::Term) {
            index += 1;
        }
        if index == term_start {
            return false;
        }
        let definition_start = index;
        while index < children.len() && matches!(children[index], NodeKind::Definition) {
            index += 1;
        }
        if index == definition_start {
            return false;
        }
    }
    true
}

fn valid_table_sequence(children: &[&NodeKind]) -> bool {
    let mut index = usize::from(matches!(children.first(), Some(NodeKind::TableCaption)));
    while matches!(children.get(index), Some(NodeKind::ColumnGroup(_))) {
        index += 1;
    }
    let row_group_start = index;
    while matches!(children.get(index), Some(NodeKind::RowGroup(_))) {
        index += 1;
    }
    index == children.len() && index > row_group_start
}

fn valid_ruby_segment(children: &[&NodeKind]) -> bool {
    matches!(children.first(), Some(NodeKind::RubyBaseLevel))
        && children.len() >= 2
        && children[1..]
            .iter()
            .all(|kind| matches!(kind, NodeKind::RubyAnnotationLevel))
}

fn valid_ruby_annotation_level(children: &[&NodeKind]) -> bool {
    if !children
        .iter()
        .any(|kind| matches!(kind, NodeKind::RubyAnnotation(_)))
    {
        return false;
    }
    !children
        .windows(2)
        .any(|pair| matches!(pair, [NodeKind::RubyFallback, NodeKind::RubyFallback]))
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;
    use crate::model::flow::{
        BreakOpportunity, Cell, Column, ColumnGroup, Destination, Disclosure, Image,
        ImageAlternative, InlineRole, InlineSemantic, KnownRole, LanguageTag, Link, LinkContent,
        LinkRelation, List, ListItem, ListKind, LocationMarker, LocationMarkerKind, MediaKind,
        Preformatted, PreformattedKind, RowGroup, RubyAnnotation, SemanticRole, Table,
        UnsupportedObject, UnsupportedReason,
    };

    fn one() -> NonZeroU32 {
        NonZeroU32::new(1).unwrap()
    }

    fn add(builder: &mut FlowBuilder, kind: NodeKind) -> NodeId {
        builder.add_node(kind).unwrap()
    }

    fn append(builder: &mut FlowBuilder, parent: NodeId, child: NodeId) {
        builder.append(parent, child).unwrap();
    }

    fn finish_error(builder: FlowBuilder) -> BuildError {
        builder.finish().unwrap_err()
    }

    fn paragraph(builder: &mut FlowBuilder) -> NodeId {
        let paragraph = add(builder, NodeKind::Paragraph);
        let opportunity = add(
            builder,
            NodeKind::BreakOpportunity(BreakOpportunity {
                hyphen_when_broken: false,
            }),
        );
        append(builder, paragraph, opportunity);
        paragraph
    }

    fn cell(kind: CellKind, column_span: u32, row_span: u32) -> NodeKind {
        NodeKind::Cell(Cell {
            kind,
            column_start: u32::MAX,
            column_span: NonZeroU32::new(column_span).unwrap(),
            row_span: NonZeroU32::new(row_span).unwrap(),
            scope: None,
            explicit_headers: None,
        })
    }

    fn table_with_header_reference(
        target_kind: CellKind,
        duplicate: bool,
    ) -> (FlowBuilder, NodeId, NodeId) {
        let mut builder = FlowBuilder::new();
        let root = add(&mut builder, NodeKind::Document);
        let table = add(
            &mut builder,
            NodeKind::Table(Table {
                column_count: one(),
            }),
        );
        let group = add(&mut builder, NodeKind::RowGroup(RowGroup::Body));
        let row = add(&mut builder, NodeKind::Row);
        let target = add(&mut builder, cell(target_kind, 1, 1));
        let data = add(&mut builder, cell(CellKind::Data, 1, 1));
        if let NodeKind::Cell(metadata) = &mut builder.nodes[data.index()].kind {
            metadata.explicit_headers = Some(if duplicate {
                vec![target, target]
            } else {
                vec![target]
            });
        }
        append(&mut builder, row, target);
        append(&mut builder, row, data);
        append(&mut builder, group, row);
        append(&mut builder, table, group);
        append(&mut builder, root, table);
        (builder, data, target)
    }

    fn unsupported(placement: Placement) -> NodeKind {
        NodeKind::UnsupportedObject(UnsupportedObject {
            placement,
            media_kind: super::super::MediaKind::Unknown,
            title: None,
            size_hint: None,
            activation: None,
            reason: UnsupportedReason::NoNativePresenter,
        })
    }

    #[test]
    fn finalizes_in_deterministic_preorder_and_remaps_references() {
        let mut builder = FlowBuilder::new();
        let detached_paragraph = paragraph(&mut builder);
        let root = add(&mut builder, NodeKind::Document);
        let section = add(&mut builder, NodeKind::Section);
        append(&mut builder, root, section);
        append(&mut builder, section, detached_paragraph);

        let document = builder.finish().unwrap();
        let order = document.preorder(document.root()).collect::<Vec<_>>();
        assert_eq!(order.len(), 4);
        assert!(matches!(
            document.get_node(order[0]).unwrap().kind,
            NodeKind::Document
        ));
        assert!(matches!(
            document.get_node(order[1]).unwrap().kind,
            NodeKind::Section
        ));
        assert!(matches!(
            document.get_node(order[2]).unwrap().kind,
            NodeKind::Paragraph
        ));
        assert_eq!(document.parent(order[2]), Some(order[1]));
        assert_eq!(document.children(order[2]), Some(&[order[3]][..]));
    }

    #[test]
    fn accepts_every_specialized_grammar() {
        let mut builder = FlowBuilder::new();
        let root = add(&mut builder, NodeKind::Document);

        let list = add(
            &mut builder,
            NodeKind::List(List {
                kind: ListKind::Unordered,
                start: 1,
                reversed: false,
            }),
        );
        let item = add(&mut builder, NodeKind::ListItem(ListItem { ordinal: None }));
        append(&mut builder, list, item);
        let item_content = paragraph(&mut builder);
        append(&mut builder, item, item_content);
        append(&mut builder, root, list);

        let definitions = add(&mut builder, NodeKind::DefinitionList);
        for kind in [NodeKind::Term, NodeKind::Term, NodeKind::Definition] {
            let node = add(&mut builder, kind);
            append(&mut builder, definitions, node);
        }
        append(&mut builder, root, definitions);

        let figure = add(&mut builder, NodeKind::Figure);
        let caption = add(&mut builder, NodeKind::Caption);
        let caption_content = paragraph(&mut builder);
        append(&mut builder, caption, caption_content);
        append(&mut builder, figure, caption);
        let figure_content = paragraph(&mut builder);
        append(&mut builder, figure, figure_content);
        append(&mut builder, root, figure);

        let table = add(
            &mut builder,
            NodeKind::Table(Table {
                column_count: one(),
            }),
        );
        let columns = add(
            &mut builder,
            NodeKind::ColumnGroup(ColumnGroup { span: one() }),
        );
        let column = add(&mut builder, NodeKind::Column(Column { span: one() }));
        append(&mut builder, columns, column);
        append(&mut builder, table, columns);
        let group = add(&mut builder, NodeKind::RowGroup(RowGroup::Body));
        let row = add(&mut builder, NodeKind::Row);
        let data = add(&mut builder, cell(CellKind::Data, 1, 1));
        let data_content = paragraph(&mut builder);
        append(&mut builder, data, data_content);
        append(&mut builder, row, data);
        append(&mut builder, group, row);
        append(&mut builder, table, group);
        append(&mut builder, root, table);

        let disclosure = add(
            &mut builder,
            NodeKind::Disclosure(Disclosure {
                initially_open: true,
            }),
        );
        let summary = add(&mut builder, NodeKind::Summary);
        append(&mut builder, disclosure, summary);
        let disclosure_content = paragraph(&mut builder);
        append(&mut builder, disclosure, disclosure_content);
        append(&mut builder, root, disclosure);

        let ruby = add(&mut builder, NodeKind::Ruby);
        let segment = add(&mut builder, NodeKind::RubySegment);
        let bases = add(&mut builder, NodeKind::RubyBaseLevel);
        let base = add(&mut builder, NodeKind::RubyBase);
        append(&mut builder, bases, base);
        append(&mut builder, segment, bases);
        let annotations = add(&mut builder, NodeKind::RubyAnnotationLevel);
        let annotation = add(
            &mut builder,
            NodeKind::RubyAnnotation(RubyAnnotation { base_span: one() }),
        );
        append(&mut builder, annotations, annotation);
        append(&mut builder, segment, annotations);
        append(&mut builder, ruby, segment);
        let wrapper = paragraph(&mut builder);
        append(&mut builder, wrapper, ruby);
        append(&mut builder, root, wrapper);

        let unsupported_flow = add(&mut builder, unsupported(Placement::Flow));
        let fallback = paragraph(&mut builder);
        append(&mut builder, unsupported_flow, fallback);
        append(&mut builder, root, unsupported_flow);
        let unsupported_inline = add(&mut builder, unsupported(Placement::Inline));
        let inline_wrapper = paragraph(&mut builder);
        append(&mut builder, inline_wrapper, unsupported_inline);
        append(&mut builder, root, inline_wrapper);

        builder.finish().unwrap();
    }

    #[test]
    fn rejects_wrong_children_duplicate_parents_and_cycles() {
        let mut builder = FlowBuilder::new();
        let root = add(&mut builder, NodeKind::Document);
        let paragraph = add(&mut builder, NodeKind::Paragraph);
        let section = add(&mut builder, NodeKind::Section);
        assert_eq!(
            builder.append(paragraph, section),
            Err(BuildError::InvalidChild {
                parent: paragraph,
                child: section
            })
        );
        append(&mut builder, root, section);
        append(&mut builder, section, paragraph);
        assert_eq!(
            builder.append(root, paragraph),
            Err(BuildError::AlreadyParented {
                child: paragraph,
                parent: section
            })
        );
        assert_eq!(
            builder.append(paragraph, root),
            Err(BuildError::InvalidChild {
                parent: paragraph,
                child: root
            })
        );
    }

    #[test]
    fn rejects_cycle_when_kinds_are_otherwise_compatible() {
        let mut builder = FlowBuilder::new();
        let first = add(&mut builder, NodeKind::Flow);
        let second = add(&mut builder, NodeKind::Flow);
        append(&mut builder, first, second);
        assert_eq!(
            builder.append(second, first),
            Err(BuildError::Cycle {
                parent: second,
                child: first
            })
        );
    }

    #[test]
    fn requires_one_reachable_document_root() {
        assert_eq!(
            finish_error(FlowBuilder::new()),
            BuildError::DocumentRootCount { count: 0 }
        );

        let mut builder = FlowBuilder::new();
        add(&mut builder, NodeKind::Document);
        add(&mut builder, NodeKind::Document);
        assert_eq!(
            finish_error(builder),
            BuildError::DocumentRootCount { count: 2 }
        );

        let mut builder = FlowBuilder::new();
        let root = add(&mut builder, NodeKind::Document);
        let orphan = add(&mut builder, NodeKind::Flow);
        assert_eq!(
            finish_error(builder),
            BuildError::UnreachableNode { node: orphan }
        );
        let _ = root;
    }

    #[test]
    fn validates_specialized_sequences_and_nested_links() {
        let mut builder = FlowBuilder::new();
        let root = add(&mut builder, NodeKind::Document);
        let list = add(
            &mut builder,
            NodeKind::List(List {
                kind: ListKind::Unordered,
                start: 1,
                reversed: false,
            }),
        );
        append(&mut builder, root, list);
        assert_eq!(
            finish_error(builder),
            BuildError::SpecializedGrammar {
                node: list,
                violation: GrammarViolation::EmptyList
            }
        );

        let mut builder = FlowBuilder::new();
        let root = add(&mut builder, NodeKind::Document);
        let outer = add(
            &mut builder,
            NodeKind::Link(Link {
                content: LinkContent::Phrasing,
                destination: super::super::Destination::External("a".into()),
                relation: LinkRelation::Ordinary,
                title: None,
            }),
        );
        let inner = add(
            &mut builder,
            NodeKind::Link(Link {
                content: LinkContent::Phrasing,
                destination: super::super::Destination::External("b".into()),
                relation: LinkRelation::Ordinary,
                title: None,
            }),
        );
        append(&mut builder, outer, inner);
        let paragraph = add(&mut builder, NodeKind::Paragraph);
        append(&mut builder, paragraph, outer);
        append(&mut builder, root, paragraph);
        assert_eq!(
            finish_error(builder),
            BuildError::NestedLink {
                link: outer,
                nested: inner
            }
        );
    }

    #[test]
    fn normalizes_sparse_table_placement_and_remaps_headers() {
        let mut builder = FlowBuilder::new();
        // Create referenced cells before their ancestors so final preorder IDs
        // differ from draft insertion IDs.
        let spanning_header = add(&mut builder, cell(CellKind::Header, 1, 2));
        let second_data = add(&mut builder, cell(CellKind::Data, 1, 1));
        let root = add(&mut builder, NodeKind::Document);
        let table = add(
            &mut builder,
            NodeKind::Table(Table {
                column_count: NonZeroU32::new(99).unwrap(),
            }),
        );
        let columns = add(
            &mut builder,
            NodeKind::ColumnGroup(ColumnGroup {
                span: NonZeroU32::new(3).unwrap(),
            }),
        );
        append(&mut builder, table, columns);
        let group = add(&mut builder, NodeKind::RowGroup(RowGroup::Body));
        let first_row = add(&mut builder, NodeKind::Row);
        let first_data = add(&mut builder, cell(CellKind::Data, 1, 1));
        append(&mut builder, first_row, spanning_header);
        append(&mut builder, first_row, first_data);
        let second_row = add(&mut builder, NodeKind::Row);
        if let NodeKind::Cell(metadata) = &mut builder.nodes[second_data.index()].kind {
            metadata.explicit_headers = Some(vec![spanning_header]);
        }
        append(&mut builder, second_row, second_data);
        append(&mut builder, group, first_row);
        append(&mut builder, group, second_row);
        append(&mut builder, table, group);
        append(&mut builder, root, table);

        let document = builder.finish().unwrap();
        let cells = document
            .preorder(document.root())
            .filter(|&id| matches!(document.get_node(id).unwrap().kind, NodeKind::Cell(_)))
            .collect::<Vec<_>>();
        let metadata = cells
            .iter()
            .map(|&id| match &document.get_node(id).unwrap().kind {
                NodeKind::Cell(cell) => cell,
                _ => unreachable!(),
            })
            .collect::<Vec<_>>();
        assert_eq!(metadata[0].column_start, 0);
        assert_eq!(metadata[1].column_start, 1);
        assert_eq!(metadata[2].column_start, 1);
        assert_eq!(metadata[2].explicit_headers, Some(vec![cells[0]]));
        let table = document
            .preorder(document.root())
            .find_map(|id| match &document.get_node(id).unwrap().kind {
                NodeKind::Table(table) => Some(table),
                _ => None,
            })
            .unwrap();
        assert_eq!(table.column_count.get(), 3);
    }

    #[test]
    fn rejects_duplicate_semantic_roles() {
        let mut builder = FlowBuilder::new();
        let root = add(&mut builder, NodeKind::Document);
        builder
            .set_semantics(
                root,
                Semantics {
                    roles: vec![
                        SemanticRole::Known(KnownRole::Main),
                        SemanticRole::Known(KnownRole::Main),
                    ],
                    ..Semantics::default()
                },
            )
            .unwrap();
        assert_eq!(
            finish_error(builder),
            BuildError::DuplicateSemanticRole { node: root }
        );
    }

    #[test]
    fn validates_document_local_resource_references() {
        let mut builder = FlowBuilder::new();
        let root = add(&mut builder, NodeKind::Document);
        let resource = builder
            .add_resource(Resource {
                identity: "cover.png".into(),
                media_type: Some("image/png".into()),
            })
            .unwrap();
        let image = add(
            &mut builder,
            NodeKind::Image(Image {
                resource,
                alternative: ImageAlternative::Missing,
                title: None,
                intrinsic_size: None,
            }),
        );
        append(&mut builder, root, image);
        let document = builder.finish().unwrap();
        assert_eq!(
            document.get_resource(resource).unwrap().identity,
            "cover.png"
        );

        let mut builder = FlowBuilder::new();
        let root = add(&mut builder, NodeKind::Document);
        let invalid = ResourceId::from_index(0);
        let image = add(
            &mut builder,
            NodeKind::Image(Image {
                resource: invalid,
                alternative: ImageAlternative::Missing,
                title: None,
                intrinsic_size: None,
            }),
        );
        append(&mut builder, root, image);
        assert_eq!(
            finish_error(builder),
            BuildError::InvalidResource {
                node: image,
                resource: invalid
            }
        );
    }

    #[test]
    fn rejects_duplicate_and_non_header_references() {
        let (builder, cell, header) = table_with_header_reference(CellKind::Header, true);
        assert_eq!(
            finish_error(builder),
            BuildError::InvalidTableHeader { cell, header }
        );

        let (builder, cell, non_header) = table_with_header_reference(CellKind::Data, false);
        assert_eq!(
            finish_error(builder),
            BuildError::InvalidTableHeader {
                cell,
                header: non_header
            }
        );
    }

    #[test]
    fn rejects_invalid_table_spans_headers_and_limits() {
        let mut builder = FlowBuilder::new();
        let root = add(&mut builder, NodeKind::Document);
        let table = add(
            &mut builder,
            NodeKind::Table(Table {
                column_count: one(),
            }),
        );
        let group = add(&mut builder, NodeKind::RowGroup(RowGroup::Body));
        let row = add(&mut builder, NodeKind::Row);
        let bad = add(&mut builder, cell(CellKind::Data, 1, 2));
        append(&mut builder, row, bad);
        append(&mut builder, group, row);
        append(&mut builder, table, group);
        append(&mut builder, root, table);
        assert_eq!(
            finish_error(builder),
            BuildError::InvalidTableSpan { cell: bad }
        );

        let mut builder = FlowBuilder::new();
        let root = add(&mut builder, NodeKind::Document);
        let table = add(
            &mut builder,
            NodeKind::Table(Table {
                column_count: one(),
            }),
        );
        let group = add(&mut builder, NodeKind::RowGroup(RowGroup::Body));
        let row = add(&mut builder, NodeKind::Row);
        let data = add(&mut builder, cell(CellKind::Data, MAX_TABLE_COLUMNS + 1, 1));
        append(&mut builder, row, data);
        append(&mut builder, group, row);
        append(&mut builder, table, group);
        append(&mut builder, root, table);
        assert_eq!(
            finish_error(builder),
            BuildError::ResourceLimit {
                resource: StructuralResource::TableColumns
            }
        );

        let mut builder = FlowBuilder::new();
        let root = add(&mut builder, NodeKind::Document);
        let first_table = add(
            &mut builder,
            NodeKind::Table(Table {
                column_count: one(),
            }),
        );
        let first_group = add(&mut builder, NodeKind::RowGroup(RowGroup::Body));
        let first_row = add(&mut builder, NodeKind::Row);
        let header = add(&mut builder, cell(CellKind::Header, 1, 1));
        append(&mut builder, first_row, header);
        append(&mut builder, first_group, first_row);
        append(&mut builder, first_table, first_group);
        append(&mut builder, root, first_table);
        let second_table = add(
            &mut builder,
            NodeKind::Table(Table {
                column_count: one(),
            }),
        );
        let second_group = add(&mut builder, NodeKind::RowGroup(RowGroup::Body));
        let second_row = add(&mut builder, NodeKind::Row);
        let data = add(&mut builder, cell(CellKind::Data, 1, 1));
        if let NodeKind::Cell(metadata) = &mut builder.nodes[data.index()].kind {
            metadata.explicit_headers = Some(vec![header]);
        }
        append(&mut builder, second_row, data);
        append(&mut builder, second_group, second_row);
        append(&mut builder, second_table, second_group);
        append(&mut builder, root, second_table);
        assert_eq!(
            finish_error(builder),
            BuildError::InvalidTableHeader { cell: data, header }
        );
    }

    #[test]
    fn accepts_independently_partitioned_ruby_levels() {
        let mut builder = FlowBuilder::new();
        let root = add(&mut builder, NodeKind::Document);
        let paragraph = add(&mut builder, NodeKind::Paragraph);
        let ruby = add(&mut builder, NodeKind::Ruby);
        let segment = add(&mut builder, NodeKind::RubySegment);
        let bases = add(&mut builder, NodeKind::RubyBaseLevel);
        for _ in 0..3 {
            let base = add(&mut builder, NodeKind::RubyBase);
            append(&mut builder, bases, base);
        }
        append(&mut builder, segment, bases);

        for spans in [[2, 1], [1, 2]] {
            let level = add(&mut builder, NodeKind::RubyAnnotationLevel);
            for (index, span) in spans.into_iter().enumerate() {
                let annotation = add(
                    &mut builder,
                    NodeKind::RubyAnnotation(RubyAnnotation {
                        base_span: NonZeroU32::new(span).unwrap(),
                    }),
                );
                append(&mut builder, level, annotation);
                if index == 0 {
                    let fallback = add(&mut builder, NodeKind::RubyFallback);
                    append(&mut builder, level, fallback);
                }
            }
            append(&mut builder, segment, level);
        }
        append(&mut builder, ruby, segment);
        append(&mut builder, paragraph, ruby);
        append(&mut builder, root, paragraph);
        builder.finish().unwrap();
    }

    #[test]
    fn rejects_empty_optional_metadata() {
        let cases = [
            (
                NodeKind::Preformatted(Preformatted {
                    language: Some(String::new()),
                    content_kind: PreformattedKind::SourceCode,
                }),
                "preformatted.language",
            ),
            (
                NodeKind::LocationMarker(LocationMarker {
                    kind: LocationMarkerKind::Other,
                    label: Some(String::new()),
                }),
                "location_marker.label",
            ),
            (
                NodeKind::Image(Image {
                    resource: ResourceId::from_index(0),
                    alternative: ImageAlternative::Decorative,
                    title: Some(String::new()),
                    intrinsic_size: None,
                }),
                "image.title",
            ),
            (
                NodeKind::UnsupportedObject(UnsupportedObject {
                    placement: Placement::Flow,
                    media_kind: MediaKind::Unknown,
                    title: Some(String::new()),
                    size_hint: None,
                    activation: None,
                    reason: UnsupportedReason::NoNativePresenter,
                }),
                "unsupported_object.title",
            ),
            (
                NodeKind::InlineSemantic(InlineSemantic {
                    role: InlineRole::Abbreviation,
                    value: Some(String::new()),
                }),
                "inline_semantic.value",
            ),
            (
                NodeKind::Link(Link {
                    content: LinkContent::Phrasing,
                    destination: Destination::External("https://example.com".into()),
                    relation: LinkRelation::Ordinary,
                    title: Some(String::new()),
                }),
                "link.title",
            ),
        ];
        for (kind, field) in cases {
            assert_eq!(
                FlowBuilder::new().add_node(kind),
                Err(BuildError::EmptyOptionalMetadata { field })
            );
        }

        let mut builder = FlowBuilder::new();
        assert_eq!(
            builder.add_resource(Resource {
                identity: "image".into(),
                media_type: Some(String::new()),
            }),
            Err(BuildError::EmptyOptionalMetadata {
                field: "resource.media_type"
            })
        );
        assert_eq!(
            builder.add_source_artifact(
                SourceArtifactDescriptor {
                    identity: None,
                    revision: SourceDigest::for_text("source"),
                    media_type: Some(String::new()),
                },
                "source",
            ),
            Err(BuildError::EmptyOptionalMetadata {
                field: "source_artifact.media_type"
            })
        );
    }

    #[test]
    fn resolves_root_language_without_external_context() {
        let mut builder = FlowBuilder::new();
        add(&mut builder, NodeKind::Document);
        let document = builder.finish().unwrap();
        assert_eq!(
            document
                .get_node(document.root())
                .unwrap()
                .semantics
                .language,
            LanguageOverride::Unknown
        );

        let mut builder = FlowBuilder::new();
        let root = add(&mut builder, NodeKind::Document);
        let language = LanguageTag::parse("en-US").unwrap();
        builder
            .set_semantics(
                root,
                Semantics {
                    language: LanguageOverride::Tag(language.clone()),
                    ..Semantics::default()
                },
            )
            .unwrap();
        let document = builder.finish().unwrap();
        assert_eq!(
            document
                .get_node(document.root())
                .unwrap()
                .semantics
                .language,
            LanguageOverride::Tag(language)
        );
    }

    #[test]
    fn constructs_deep_trees_without_ancestor_scans() {
        let mut builder = FlowBuilder::new();
        let root = add(&mut builder, NodeKind::Document);
        let paragraph = add(&mut builder, NodeKind::Paragraph);
        append(&mut builder, root, paragraph);
        let mut parent = paragraph;
        for _ in 0..50_000 {
            let child = add(&mut builder, NodeKind::Inline);
            append(&mut builder, parent, child);
            parent = child;
        }
        builder.finish().unwrap();
    }

    #[test]
    fn rejects_incomplete_ruby_grid() {
        let mut builder = FlowBuilder::new();
        let root = add(&mut builder, NodeKind::Document);
        let paragraph = add(&mut builder, NodeKind::Paragraph);
        let ruby = add(&mut builder, NodeKind::Ruby);
        let segment = add(&mut builder, NodeKind::RubySegment);
        let bases = add(&mut builder, NodeKind::RubyBaseLevel);
        for _ in 0..2 {
            let base = add(&mut builder, NodeKind::RubyBase);
            append(&mut builder, bases, base);
        }
        let annotations = add(&mut builder, NodeKind::RubyAnnotationLevel);
        let annotation = add(
            &mut builder,
            NodeKind::RubyAnnotation(RubyAnnotation { base_span: one() }),
        );
        append(&mut builder, annotations, annotation);
        append(&mut builder, segment, bases);
        append(&mut builder, segment, annotations);
        append(&mut builder, ruby, segment);
        append(&mut builder, paragraph, ruby);
        append(&mut builder, root, paragraph);
        assert_eq!(
            finish_error(builder),
            BuildError::InvalidRubyGrid { node: annotations }
        );
    }
}
