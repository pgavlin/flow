//! Shared syntactic tree. XHTML's public tree names are compatibility re-exports.

use std::ops::Range;
use thiserror::Error;

use crate::model::flow::{GeneratedReason, SourceNodeIdentity};

/// Stable identity of a node in one temporary source tree.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceNodeId(u32);

impl SourceNodeId {
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// A qualified name with its namespace resolved by the syntax frontend.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ExpandedName {
    pub(in crate::formats) qualified: String,
    pub(in crate::formats) prefix: Option<String>,
    pub(in crate::formats) local: String,
    pub(in crate::formats) namespace: Option<String>,
}

impl ExpandedName {
    pub(in crate::formats) fn new(
        qualified: String,
        local: String,
        prefix: Option<String>,
        namespace: Option<String>,
    ) -> Self {
        Self {
            qualified,
            local,
            prefix,
            namespace,
        }
    }

    pub fn qualified(&self) -> &str {
        &self.qualified
    }

    pub fn prefix(&self) -> Option<&str> {
        self.prefix.as_deref()
    }

    pub fn local(&self) -> &str {
        &self.local
    }

    pub fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref()
    }
}

/// One decoded attribute in source order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Attribute {
    pub(in crate::formats) name: ExpandedName,
    pub(in crate::formats) value: String,
    pub(in crate::formats) source_range: Option<Range<usize>>,
}

impl Attribute {
    pub(in crate::formats) fn new(
        name: ExpandedName,
        value: String,
        source_range: Option<Range<usize>>,
    ) -> Self {
        Self {
            name,
            value,
            source_range,
        }
    }

    /// Original coordinates, if known. Never offsets into a reserialization.
    pub fn serialized_range(&self) -> Option<Range<usize>> {
        self.source_range.clone()
    }

    pub fn name(&self) -> &ExpandedName {
        &self.name
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    /// Range of the containing start tag. `quick-xml` does not expose a
    /// narrower attribute range, but retaining this range still gives repairs
    /// a deterministic source location.
    ///
    /// # Panics
    /// Panics for an internally constructed attribute without original coordinates.
    /// Shared HTML consumers should use `serialized_range` instead.
    pub fn source_range(&self) -> Range<usize> {
        self.source_range
            .clone()
            .expect("XML parser supplies serialized ranges")
    }
}

/// Element data retained before validation, repair, and semantic lowering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Element {
    pub(in crate::formats) name: ExpandedName,
    pub(in crate::formats) attributes: Vec<Attribute>,
}

impl Element {
    pub(in crate::formats) fn new(name: ExpandedName, attributes: Vec<Attribute>) -> Self {
        Self { name, attributes }
    }

    pub fn name(&self) -> &ExpandedName {
        &self.name
    }

    pub fn attributes(&self) -> &[Attribute] {
        &self.attributes
    }

    pub fn attribute(&self, namespace: Option<&str>, local: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|attribute| {
                attribute.name.local() == local && attribute.name.namespace() == namespace
            })
            .map(Attribute::value)
    }
}

/// Syntactic node kinds retained by the temporary source tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceNodeKind {
    Document,
    Element(Element),
    Text(String),
    Cdata(String),
    Comment(String),
    ProcessingInstruction { target: String, value: String },
}

/// One arena node in the temporary source tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceNode {
    parent: Option<SourceNodeId>,
    pub(in crate::formats) children: Vec<SourceNodeId>,
    pub(in crate::formats) kind: SourceNodeKind,
    pub(in crate::formats) provenance: NodeProvenance,
    depth: usize,
    pub(in crate::formats) source_range: Option<Range<usize>>,
}

impl SourceNode {
    /// Original coordinates, if known. Implied or transformed nodes may lack them.
    pub fn serialized_range(&self) -> Option<Range<usize>> {
        self.source_range.clone()
    }

    pub fn parent(&self) -> Option<SourceNodeId> {
        self.parent
    }

    pub fn children(&self) -> &[SourceNodeId] {
        &self.children
    }

    pub fn kind(&self) -> &SourceNodeKind {
        &self.kind
    }

    /// Compatibility accessor for XML-parsed nodes, which always have ranges.
    /// Shared HTML consumers must use `serialized_range` instead.
    ///
    /// # Panics
    /// Panics if an internally constructed node has no original coordinates.
    pub fn source_range(&self) -> Range<usize> {
        self.source_range
            .clone()
            .expect("XML parser supplies serialized ranges")
    }
}

/// A flow-owned syntactic tree, independent of HTML5 or XML lexical policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceTree {
    pub(in crate::formats) nodes: Vec<SourceNode>,
    root: SourceNodeId,
    limits: TreeLimits,
    text_bytes: usize,
}

impl SourceTree {
    pub fn root(&self) -> SourceNodeId {
        self.root
    }

    pub fn get(&self, id: SourceNodeId) -> Option<&SourceNode> {
        self.nodes.get(id.index())
    }

    pub fn get_mut(&mut self, id: SourceNodeId) -> Option<&mut SourceNode> {
        self.nodes.get_mut(id.index())
    }

    pub fn nodes(&self) -> impl ExactSizeIterator<Item = (SourceNodeId, &SourceNode)> {
        self.nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (SourceNodeId(index as u32), node))
    }

    pub(in crate::formats) fn new(
        source_range: Option<Range<usize>>,
        limits: TreeLimits,
    ) -> Result<Self, TreeError> {
        if limits.max_nodes == 0 {
            return Err(TreeError::TooComplex);
        }
        if source_range
            .as_ref()
            .is_some_and(|range| range.start > range.end)
        {
            return Err(TreeError::InvalidSource);
        }
        Ok(Self {
            nodes: vec![SourceNode {
                parent: None,
                children: Vec::new(),
                kind: SourceNodeKind::Document,
                source_range,
                depth: 0,
                provenance: NodeProvenance::Original { identity: None },
            }],
            root: SourceNodeId(0),
            limits,
            text_bytes: 0,
        })
    }

    /// Append in reading order. References can only address existing nodes,
    /// so neither topology nor generated provenance can acquire cycles.
    pub(in crate::formats) fn append(
        &mut self,
        parent: SourceNodeId,
        kind: SourceNodeKind,
        source_range: Option<Range<usize>>,
        provenance: NodeProvenance,
    ) -> Result<SourceNodeId, TreeError> {
        let parent_node = self.get(parent).ok_or(TreeError::InvalidSource)?;
        let depth = parent_node
            .depth
            .checked_add(1)
            .ok_or(TreeError::TooComplex)?;
        if source_range
            .as_ref()
            .is_some_and(|range| range.start > range.end)
            || matches!(kind, SourceNodeKind::Document)
            || matches!(&provenance, NodeProvenance::Generated { related, .. } if self.get(*related).is_none())
        {
            return Err(TreeError::InvalidSource);
        }
        let bytes = match &kind {
            SourceNodeKind::Element(element) => {
                if element.attributes.len() > self.limits.max_attributes {
                    return Err(TreeError::TooComplex);
                }
                element
                    .attributes
                    .iter()
                    .try_fold(
                        element.name.qualified.len()
                            + element.name.namespace.as_ref().map_or(0, String::len),
                        |sum, attr| {
                            sum.checked_add(attr.name.qualified.len())
                                .and_then(|n| {
                                    n.checked_add(
                                        attr.name.namespace.as_ref().map_or(0, String::len),
                                    )
                                })
                                .and_then(|n| n.checked_add(attr.value.len()))
                        },
                    )
                    .ok_or(TreeError::TooComplex)?
            }
            SourceNodeKind::Text(value)
            | SourceNodeKind::Cdata(value)
            | SourceNodeKind::Comment(value) => value.len(),
            SourceNodeKind::ProcessingInstruction { target, value } => target
                .len()
                .checked_add(value.len())
                .ok_or(TreeError::TooComplex)?,
            SourceNodeKind::Document => unreachable!(),
        };
        let text_bytes = self
            .text_bytes
            .checked_add(bytes)
            .ok_or(TreeError::TooComplex)?;
        if self.nodes.len() >= self.limits.max_nodes
            || self.nodes.len() >= u32::MAX as usize
            || depth > self.limits.max_depth
            || text_bytes > self.limits.max_text_bytes
        {
            return Err(TreeError::TooComplex);
        }
        let id = SourceNodeId(self.nodes.len() as u32);
        self.nodes.push(SourceNode {
            parent: Some(parent),
            children: Vec::new(),
            kind,
            source_range,
            provenance,
            depth,
        });
        self.nodes[parent.index()].children.push(id);
        self.text_bytes = text_bytes;
        Ok(id)
    }

    /// XML lexical parser compatibility path.
    pub(in crate::formats) fn push(
        &mut self,
        parent: SourceNodeId,
        kind: SourceNodeKind,
        range: Range<usize>,
    ) -> Result<SourceNodeId, TreeError> {
        self.append(
            parent,
            kind,
            Some(range),
            NodeProvenance::Original { identity: None },
        )
    }
}

/// Checked while adapting an external DOM. Parser integrations must also bound
/// their own input and DOM allocations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::formats) struct TreeLimits {
    pub max_nodes: usize,
    pub max_depth: usize,
    pub max_attributes: usize,
    pub max_text_bytes: usize,
}

impl Default for TreeLimits {
    fn default() -> Self {
        Self {
            max_nodes: 1_000_000,
            max_depth: 256,
            max_attributes: 4_096,
            max_text_bytes: 32 * 1024 * 1024,
        }
    }
}

impl TreeLimits {
    /// XML's lexical parser already enforces its historical limits.
    pub(in crate::formats) fn xml() -> Self {
        Self {
            max_nodes: 1_000_000,
            max_depth: usize::MAX,
            max_attributes: usize::MAX,
            max_text_bytes: usize::MAX,
        }
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub(in crate::formats) enum TreeError {
    #[error("HTML source tree exceeds its construction limits")]
    TooComplex,
    #[error("invalid source node or source coordinates")]
    InvalidSource,
}

/// Provenance survives DOM rewriting independently of allocation identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::formats) enum NodeProvenance {
    Original {
        identity: Option<SourceNodeIdentity>,
    },
    #[allow(
        dead_code,
        reason = "Constructed HTML DOMs can include generated nodes"
    )]
    Generated {
        reason: GeneratedReason,
        related: SourceNodeId,
    },
}
