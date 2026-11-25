//! Internal structural HTML lowering shared by source adapters. This module does not
//! select a document root, parse author styles, or recognize format extensions.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;
use std::sync::Arc;

use super::source::SourceSyntax;
use super::tree::{Element, NodeProvenance, SourceNodeId, SourceNodeKind, SourceTree};
use crate::model::flow::{
    AnchorId, AuthorStyle, BaseDirection, BidiMode, BidiState, BuildError, Cell, CellKind,
    Destination, Diagnostic, DiagnosticKind, DiagnosticSeverity, Disclosure, DocumentReference,
    FlowBuilder, GeneratedReason, HeaderScope, Heading, HeadingLevel, Image, ImageAlternative,
    ImportOutcome, InlineRole, InlineSemantic, IntrinsicSize, LanguageOverride, LanguageTag, Link,
    LinkContent, LinkRelation, List, ListItem, ListKind, Math, MathDisplay, MathNotation,
    MediaKind, NodeId, NodeKind, NonEmptyText, Origin, OriginId, Placement, PositiveCssPixels,
    Preformatted, PreformattedKind, Resource, ResourceId, RetainedSourceBundle,
    RetainedSourceError, RowGroup, RubyAnnotation, SemanticRole, SemanticStyleKey, Semantics,
    SourceArtifactId, SourceDigest, SourceRetention, StyleInput, StyleInputId, Table,
    UnsupportedObject, UnsupportedReason,
};
use thiserror::Error;

const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";
const MAX_TABLE_COLUMNS: u32 = 4_096;

/// Hooks selected explicitly by each format adapter, never public import API.
pub(in crate::formats) trait LoweringPolicy {
    /// Adapter-owned interpretation of deferred image alternatives.
    fn responsive_image(
        &self,
        _element: &Element,
        _builder: &mut FlowBuilder,
    ) -> Result<Option<crate::model::flow::ResponsiveImage>, BuildError> {
        Ok(None)
    }

    /// Lower the whole preformatted container as literal phrasing, retaining
    /// semantics and targets rather than flattening its first code child.
    fn preformatted_all_children(&self) -> bool {
        false
    }

    /// Preserve inline as well as block media fallback content.
    fn media_flow_fallback(&self) -> bool {
        false
    }

    /// Preserve arbitrary figure flow instead of the legacy image/paragraph subset.
    fn preserve_figure_flow(&self) -> bool {
        false
    }

    /// Adapter-selected flow container with ordinary flow children.
    fn flow_container(&self, _element: &Element) -> Option<NodeKind> {
        None
    }

    /// Adapter-selected phrasing semantics, in addition to the shared baseline.
    fn inline_semantic(&self, _element: &Element) -> Option<InlineSemantic> {
        None
    }

    /// Legacy adapters applied element metadata to both an atom and its spacing
    /// wrapper. New adapters can keep metadata solely on the semantic atom.
    fn atomic_spacing_metadata(&self) -> bool {
        true
    }

    fn inline_atom(&self, _element: &Element) -> Option<NodeKind> {
        None
    }

    fn ruby(&self, _element: &Element) -> bool {
        false
    }

    fn link_content(&self, _element: &Element, _has_flow_children: bool) -> LinkContent {
        LinkContent::Phrasing
    }

    fn link_relation(&self, _element: &Element) -> LinkRelation {
        LinkRelation::Ordinary
    }

    fn roles(&self, _element: &Element) -> Vec<SemanticRole> {
        Vec::new()
    }

    /// Interpret a zero row span as extending through its current row group.
    fn zero_row_span_to_end(&self) -> bool {
        false
    }

    fn cell_scope(&self, _element: &Element) -> Option<HeaderScope> {
        None
    }

    fn cell_headers(&self, _element: &Element) -> Option<Vec<String>> {
        None
    }

    fn bidi(&self, _element: &Element) -> Option<BidiState> {
        None
    }

    fn title(&self, _element: &Element) -> Option<String> {
        None
    }

    fn math_notation(&self, _element: &Element) -> MathNotation {
        MathNotation::Tex
    }

    fn math_metadata(&self, _element: &Element) -> Option<(String, MathDisplay)> {
        None
    }

    /// Only HTML (or unqualified fragment) names have HTML structural meaning.
    /// Legacy XML adapters explicitly opt into their historical local-name policy.
    fn element_name<'a>(&self, element: &'a Element) -> Option<&'a str> {
        matches!(
            element.name().namespace(),
            None | Some("http://www.w3.org/1999/xhtml")
        )
        .then(|| element.name().local())
    }

    fn code_metadata(&self, _element: &Element) -> Preformatted {
        Preformatted {
            language: None,
            content_kind: PreformattedKind::SourceCode,
        }
    }
}

#[derive(Debug, Error)]
pub(in crate::formats) enum LoweringError {
    #[error(transparent)]
    Build(#[from] BuildError),
    #[error(transparent)]
    RetainedSource(#[from] RetainedSourceError),
}

struct Lowerer<'a> {
    tree: &'a SourceTree,
    builder: FlowBuilder,
    source_id: SourceArtifactId,
    origins: Vec<Option<OriginId>>,
    styles: HashMap<(Option<SemanticStyleKey>, Option<BidiState>, AuthorStyle), StyleInputId>,
    author_styles: &'a [AuthorStyle],
    claimed_anchors: HashSet<String>,
    resources: HashMap<String, ResourceId>,
    diagnostics: Vec<Diagnostic>,
    policy: &'a dyn LoweringPolicy,
    syntax: SourceSyntax,
}

#[derive(Default)]
struct RubySegmentPlan {
    bases: Vec<Vec<SourceNodeId>>,
    levels: Vec<Vec<SourceNodeId>>,
}

#[derive(Default)]
struct InlineWhitespace {
    literal: bool,
    has_output: bool,
    at_boundary: bool,
    pending_space: bool,
}

pub(in crate::formats) struct LoweringOptions {
    pub resource_base: Option<String>,
    pub identity: Option<DocumentReference>,
    pub retention: SourceRetention,
    pub syntax: SourceSyntax,
}

pub(in crate::formats) fn lower_tree(
    source: &str,
    tree: SourceTree,
    author_styles: &[AuthorStyle],
    options: LoweringOptions,
    content_root: SourceNodeId,
    policy: &dyn LoweringPolicy,
) -> Result<ImportOutcome, LoweringError> {
    let mut builder = FlowBuilder::new();
    builder.set_resource_base(options.resource_base);
    let source_id = builder.add_source(
        source.to_owned(),
        options.identity,
        Some(options.syntax.media_type().into()),
    )?;
    let node_count = tree.nodes().len();
    let mut lowerer = Lowerer {
        tree: &tree,
        builder,
        source_id,
        origins: vec![None; node_count],
        styles: HashMap::new(),
        author_styles,
        claimed_anchors: HashSet::new(),
        resources: HashMap::new(),
        diagnostics: Vec::new(),
        policy,
        syntax: options.syntax,
    };
    let root = lowerer.node(NodeKind::Document, content_root, None)?;
    lowerer.append_flow_children(root, content_root)?;

    let revision = SourceDigest::for_text(source);
    let document = lowerer.builder.finish()?;
    let retained_sources = if options.retention == SourceRetention::Retain {
        let mut retained = RetainedSourceBundle::default();
        retained.insert(revision, Arc::from(source))?;
        Some(retained)
    } else {
        None
    };
    Ok(ImportOutcome {
        document,
        diagnostics: lowerer.diagnostics,
        retained_sources,
    })
}

impl Lowerer<'_> {
    fn append_flow_children(
        &mut self,
        parent: NodeId,
        source_parent: SourceNodeId,
    ) -> Result<(), LoweringError> {
        let children = self.tree.get(source_parent).unwrap().children().to_vec();
        self.append_flow_nodes(parent, children)
    }

    fn append_flow_nodes(
        &mut self,
        parent: NodeId,
        children: Vec<SourceNodeId>,
    ) -> Result<(), LoweringError> {
        let mut paragraph = None;
        let mut whitespace = InlineWhitespace {
            at_boundary: true,
            ..InlineWhitespace::default()
        };

        for source in children {
            if self.is_orphan_table_part(source) {
                paragraph = None;
                whitespace = InlineWhitespace {
                    at_boundary: true,
                    ..InlineWhitespace::default()
                };
                let origin = self.origin(source)?;
                self.diagnostics.push(Diagnostic {
                    severity: DiagnosticSeverity::Warning,
                    kind: DiagnosticKind::InvalidChild,
                    origin: Some(origin),
                    message: "orphan table structure was unwrapped in reading order".into(),
                });
                self.append_unwrapped_table_children(parent, source)?;
                continue;
            }
            if self.is_block(source) {
                paragraph = None;
                whitespace = InlineWhitespace {
                    at_boundary: true,
                    ..InlineWhitespace::default()
                };
                if let Some(node) = self.lower_flow(source)? {
                    self.builder.append(parent, node)?;
                }
                continue;
            }

            if self.source_is_ignorable_whitespace(source) && paragraph.is_none() {
                continue;
            }
            let paragraph_node = if let Some(paragraph) = paragraph {
                paragraph
            } else {
                let generated = self.builder.add_node(NodeKind::Paragraph)?;
                let related = self.origin(source)?;
                let origin = self.builder.add_origin(Origin::Generated {
                    reason: crate::model::flow::GeneratedReason::AnonymousParagraph,
                    related: Some(related),
                })?;
                self.builder.set_origin(generated, origin)?;
                self.builder.append(parent, generated)?;
                paragraph = Some(generated);
                generated
            };
            if let Some(inline) = self.lower_inline(source, &mut whitespace)? {
                self.builder.append(paragraph_node, inline)?;
            }
        }
        Ok(())
    }

    fn lower_flow(&mut self, source: SourceNodeId) -> Result<Option<NodeId>, LoweringError> {
        let Some(name) = self.element_name(source) else {
            return Ok(None);
        };
        if let Some((tex, MathDisplay::Block)) = self.math_metadata(source) {
            return self
                .node(
                    NodeKind::Math(Math {
                        notation: self.policy.math_notation(self.element(source).unwrap()),
                        source: tex,
                        display: MathDisplay::Block,
                    }),
                    source,
                    None,
                )
                .map(Some);
        }
        if self.is_flow_link(source) {
            let element = self.element(source).unwrap();
            let link = Link {
                content: LinkContent::Flow,
                destination: destination(element.attribute(None, "href").unwrap()),
                relation: self.policy.link_relation(element),
                title: self.policy.title(element),
            };
            let node = self.node(NodeKind::Link(link), source, Some(SemanticStyleKey::Link))?;
            self.append_flow_children(node, source)?;
            return Ok(Some(node));
        }
        if let Some(kind) = self.policy.flow_container(self.element(source).unwrap()) {
            let node = self.node(kind, source, None)?;
            self.append_flow_children(node, source)?;
            return Ok(Some(node));
        }
        let node = match name {
            "p" => {
                let node = self.node(NodeKind::Paragraph, source, None)?;
                self.append_inline_children(node, source)?;
                node
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = HeadingLevel::new(name.as_bytes()[1] - b'0').unwrap();
                let node = self.node(
                    NodeKind::Heading(Heading { level }),
                    source,
                    Some(SemanticStyleKey::Heading),
                )?;
                self.append_inline_children(node, source)?;
                node
            }
            "hr" => self.node(
                NodeKind::ThematicBreak,
                source,
                Some(SemanticStyleKey::ThematicBreak),
            )?,
            "blockquote" => {
                let node = self.node(
                    NodeKind::Quotation,
                    source,
                    Some(SemanticStyleKey::Quotation),
                )?;
                self.append_flow_children(node, source)?;
                node
            }
            "ul" => self.lower_list(source, ListKind::Unordered)?,
            "ol" => self.lower_list(source, ListKind::Ordered)?,
            "dl" => self.lower_definition_list(source)?,
            "pre" | "code" => self.lower_preformatted(source)?,
            "figure" => self.lower_figure(source)?,
            "table" => self.lower_table(source)?,
            "details" => self.lower_disclosure(source)?,
            "iframe" => self.lower_media(source, MediaKind::Iframe)?,
            "audio" => self.lower_media(source, MediaKind::Audio)?,
            "video" => self.lower_media(source, MediaKind::Video)?,
            _ => {
                let node = self.node(NodeKind::Flow, source, None)?;
                self.append_flow_children(node, source)?;
                node
            }
        };
        Ok(Some(node))
    }

    fn lower_list(
        &mut self,
        source: SourceNodeId,
        kind: ListKind,
    ) -> Result<NodeId, LoweringError> {
        let element = self.element(source).unwrap();
        let start = if kind == ListKind::Ordered {
            element
                .attribute(None, "start")
                .and_then(|value| value.parse().ok())
                .unwrap_or(1)
        } else {
            1
        };
        let reversed = kind == ListKind::Ordered && element.attribute(None, "reversed").is_some();
        let style = match kind {
            ListKind::Ordered => SemanticStyleKey::OrderedList,
            ListKind::Unordered => SemanticStyleKey::UnorderedList,
        };
        let list = self.node(
            NodeKind::List(List {
                kind,
                start,
                reversed,
            }),
            source,
            Some(style),
        )?;
        for child in self.tree.get(source).unwrap().children().to_vec() {
            if self.element_name(child) != Some("li") {
                continue;
            }
            let ordinal = self
                .element(child)
                .and_then(|element| element.attribute(None, "value"))
                .and_then(|value| value.parse().ok());
            let item = self.node(NodeKind::ListItem(ListItem { ordinal }), child, None)?;
            self.append_flow_children(item, child)?;
            self.builder.append(list, item)?;
        }
        Ok(list)
    }

    fn lower_definition_list(&mut self, source: SourceNodeId) -> Result<NodeId, LoweringError> {
        let list = self.node(NodeKind::DefinitionList, source, None)?;
        for child in self.tree.get(source).unwrap().children().to_vec() {
            let kind = match self.element_name(child) {
                Some("dt") => NodeKind::Term,
                Some("dd") => NodeKind::Definition,
                _ => continue,
            };
            let entry = self.node(kind.clone(), child, None)?;
            if matches!(kind, NodeKind::Term) {
                let mut whitespace = InlineWhitespace {
                    at_boundary: true,
                    ..InlineWhitespace::default()
                };
                for term_child in self.tree.get(child).unwrap().children().to_vec() {
                    if self.element_name(term_child) == Some("p") {
                        for inline_child in self.tree.get(term_child).unwrap().children().to_vec() {
                            if let Some(inline) =
                                self.lower_inline(inline_child, &mut whitespace)?
                            {
                                self.builder.append(entry, inline)?;
                            }
                        }
                    } else if let Some(inline) = self.lower_inline(term_child, &mut whitespace)? {
                        self.builder.append(entry, inline)?;
                    }
                }
            } else {
                self.append_flow_children(entry, child)?;
            }
            self.builder.append(list, entry)?;
        }
        Ok(list)
    }

    fn lower_preformatted(&mut self, source: SourceNodeId) -> Result<NodeId, LoweringError> {
        let code = if self.element_name(source) == Some("code") {
            Some(source)
        } else {
            self.tree
                .get(source)
                .unwrap()
                .children()
                .iter()
                .copied()
                .find(|child| self.element_name(*child) == Some("code"))
        };
        let metadata = code.and_then(|code| self.element(code)).map_or(
            Preformatted {
                language: None,
                content_kind: PreformattedKind::PlainText,
            },
            |element| self.policy.code_metadata(element),
        );
        let node = self.node(
            NodeKind::Preformatted(metadata),
            source,
            Some(SemanticStyleKey::Preformatted),
        )?;
        if self.element_name(source) == Some("code") {
            let origin = self.origin(source)?;
            self.diagnostics.push(Diagnostic {
                severity: DiagnosticSeverity::Note,
                kind: DiagnosticKind::SynthesizedWrapper,
                origin: Some(origin),
                message: "root-level code was normalized as a preformatted block".into(),
            });
        }
        if self.policy.preformatted_all_children() {
            let mut whitespace = InlineWhitespace {
                literal: true,
                at_boundary: true,
                ..Default::default()
            };
            for child in self.tree.get(source).unwrap().children().to_vec() {
                if let Some(child) = self.lower_inline(child, &mut whitespace)? {
                    self.builder.append(node, child)?;
                }
            }
            return Ok(node);
        }
        let text_source = code.unwrap_or(source);
        let mut value = String::new();
        self.collect_text(text_source, &mut value);
        let text = self.builder.add_text(value)?;
        let origin = self.origin(text_source)?;
        self.builder.set_origin(text, origin)?;
        self.builder.append(node, text)?;
        Ok(node)
    }

    fn lower_disclosure(&mut self, source: SourceNodeId) -> Result<NodeId, LoweringError> {
        let initially_open = self
            .element(source)
            .unwrap()
            .attribute(None, "open")
            .is_some();
        let disclosure = self.node(
            NodeKind::Disclosure(Disclosure { initially_open }),
            source,
            None,
        )?;
        let children = self.tree.get(source).unwrap().children().to_vec();
        if let Some(summary_source) = children
            .iter()
            .copied()
            .find(|child| self.element_name(*child) == Some("summary"))
        {
            let summary = self.node(NodeKind::Summary, summary_source, None)?;
            self.append_phrasing_unwrapping_paragraphs(summary, summary_source)?;
            self.builder.append(disclosure, summary)?;
        }
        for child in children {
            if self.element_name(child) == Some("summary") {
                continue;
            }
            if self.is_block(child) {
                if let Some(child) = self.lower_flow(child)? {
                    self.builder.append(disclosure, child)?;
                }
            } else if !self.source_is_ignorable_whitespace(child) {
                let paragraph = self.generated_node(
                    NodeKind::Paragraph,
                    child,
                    GeneratedReason::AnonymousParagraph,
                )?;
                let mut whitespace = InlineWhitespace {
                    at_boundary: true,
                    ..InlineWhitespace::default()
                };
                if let Some(inline) = self.lower_inline(child, &mut whitespace)? {
                    self.builder.append(paragraph, inline)?;
                }
                self.builder.append(disclosure, paragraph)?;
            }
        }
        Ok(disclosure)
    }

    fn append_phrasing_unwrapping_paragraphs(
        &mut self,
        parent: NodeId,
        source: SourceNodeId,
    ) -> Result<(), LoweringError> {
        let mut whitespace = InlineWhitespace {
            at_boundary: true,
            ..InlineWhitespace::default()
        };
        for child in self.tree.get(source).unwrap().children().to_vec() {
            if self.element_name(child) == Some("p") {
                for inline_child in self.tree.get(child).unwrap().children().to_vec() {
                    if let Some(inline) = self.lower_inline(inline_child, &mut whitespace)? {
                        self.builder.append(parent, inline)?;
                    }
                }
            } else if let Some(inline) = self.lower_inline(child, &mut whitespace)? {
                self.builder.append(parent, inline)?;
            }
        }
        Ok(())
    }

    fn lower_media(
        &mut self,
        source: SourceNodeId,
        media_kind: MediaKind,
    ) -> Result<NodeId, LoweringError> {
        let element = self.element(source).unwrap();
        let title = element.attribute(None, "title").map(str::to_owned);
        let width = element.attribute(None, "width").map(str::to_owned);
        let height = element.attribute(None, "height").map(str::to_owned);
        let direct_source = element.attribute(None, "src").map(str::to_owned);
        let children = self.tree.get(source).unwrap().children().to_vec();
        let child_source = children.iter().find_map(|child| {
            (self.element_name(*child) == Some("source"))
                .then(|| {
                    self.element(*child)
                        .and_then(|element| element.attribute(None, "src"))
                        .map(str::to_owned)
                })
                .flatten()
        });
        let activation = direct_source
            .or(child_source)
            .map(|source| destination(&source));
        let size_hint = match (
            parse_positive_dimension(width.as_deref()),
            parse_positive_dimension(height.as_deref()),
        ) {
            (Some(width), Some(height)) => Some(IntrinsicSize { width, height }),
            _ => None,
        };
        let object = self.node(
            NodeKind::UnsupportedObject(UnsupportedObject {
                placement: Placement::Flow,
                media_kind,
                title: title.clone(),
                size_hint,
                activation,
                reason: UnsupportedReason::NoNativePresenter,
            }),
            source,
            None,
        )?;
        let origin = self.origin(source)?;
        self.diagnostics.push(Diagnostic {
            severity: DiagnosticSeverity::Warning,
            kind: DiagnosticKind::UnsupportedMedia,
            origin: Some(origin),
            message: format!("{media_kind:?} content requires a host presenter"),
        });

        let mut has_fallback = false;
        let mut fallback = Vec::new();
        for child in children {
            match self.element_name(child) {
                Some("source") => continue,
                Some("track") => {
                    let origin = self.origin(child)?;
                    self.diagnostics.push(Diagnostic {
                        severity: DiagnosticSeverity::Note,
                        kind: DiagnosticKind::DroppedNonTextualMetadata,
                        origin: Some(origin),
                        message: "media track metadata is not retained by the placeholder".into(),
                    });
                    continue;
                }
                _ => {}
            }
            if self.policy.media_flow_fallback() {
                if !self.source_is_ignorable_whitespace(child) {
                    fallback.push(child);
                }
            } else if self.is_block(child)
                && let Some(child) = self.lower_flow(child)?
            {
                self.builder.append(object, child)?;
                has_fallback = true;
            }
        }
        if self.policy.media_flow_fallback() {
            has_fallback = !fallback.is_empty();
            self.append_flow_nodes(object, fallback)?;
        }
        if !has_fallback && let Some(title) = title {
            let paragraph = self.generated_node(
                NodeKind::Paragraph,
                source,
                GeneratedReason::UnsupportedFallback,
            )?;
            let text = self.builder.add_text(title)?;
            self.builder.set_origin(text, origin)?;
            self.builder.append(paragraph, text)?;
            self.builder.append(object, paragraph)?;
        }
        Ok(object)
    }

    fn lower_table(&mut self, source: SourceNodeId) -> Result<NodeId, LoweringError> {
        let children = self.tree.get(source).unwrap().children().to_vec();
        let caption = children
            .iter()
            .copied()
            .find(|child| self.element_name(*child) == Some("caption"));
        let mut groups = Vec::<(Option<SourceNodeId>, RowGroup, Vec<SourceNodeId>)>::new();
        let mut direct_rows = Vec::new();
        for child in &children {
            let kind = match self.element_name(*child) {
                Some("thead") => Some(RowGroup::Head),
                Some("tbody") => Some(RowGroup::Body),
                Some("tfoot") => Some(RowGroup::Foot),
                Some("tr") => {
                    direct_rows.push(*child);
                    None
                }
                _ => None,
            };
            if let Some(kind) = kind {
                let rows = self
                    .tree
                    .get(*child)
                    .unwrap()
                    .children()
                    .iter()
                    .copied()
                    .filter(|row| self.element_name(*row) == Some("tr"))
                    .collect::<Vec<_>>();
                if !rows.is_empty() {
                    groups.push((Some(*child), kind, rows));
                }
            }
        }
        if !direct_rows.is_empty() {
            groups.push((None, RowGroup::Body, direct_rows));
        }

        let table = self.node(
            NodeKind::Table(Table {
                column_count: NonZeroU32::new(1).unwrap(),
            }),
            source,
            Some(SemanticStyleKey::Table),
        )?;
        if let Some(caption) = caption {
            let caption_node = self.node(NodeKind::TableCaption, caption, None)?;
            self.append_flow_children(caption_node, caption)?;
            self.builder.append(table, caption_node)?;
        }
        let mut header_targets = HashMap::new();
        let mut pending_headers = Vec::new();
        for (group_source, kind, rows) in groups {
            let group = if let Some(group_source) = group_source {
                self.node(NodeKind::RowGroup(kind), group_source, None)?
            } else {
                self.generated_node(
                    NodeKind::RowGroup(kind),
                    source,
                    GeneratedReason::TableNormalization,
                )?
            };
            let row_count = rows.len();
            for (row_index, row_source) in rows.into_iter().enumerate() {
                let cells = self
                    .tree
                    .get(row_source)
                    .unwrap()
                    .children()
                    .iter()
                    .copied()
                    .filter(|cell| matches!(self.element_name(*cell), Some("th" | "td")))
                    .collect::<Vec<_>>();
                if cells.is_empty() {
                    continue;
                }
                let row = self.node(NodeKind::Row, row_source, None)?;
                for cell_source in cells {
                    let is_header = self.element_name(cell_source) == Some("th");
                    let column_span = self.table_span(cell_source, "colspan", MAX_TABLE_COLUMNS)?;
                    let remaining_rows = u32::try_from(row_count - row_index).unwrap_or(u32::MAX);
                    let row_span = self.table_span(cell_source, "rowspan", remaining_rows)?;
                    let metadata = Cell {
                        kind: if is_header {
                            CellKind::Header
                        } else {
                            CellKind::Data
                        },
                        column_start: 0,
                        column_span,
                        row_span,
                        scope: self
                            .policy
                            .cell_scope(self.element(cell_source).unwrap())
                            .or_else(|| {
                                (is_header && kind == RowGroup::Head).then_some(HeaderScope::Column)
                            }),
                        explicit_headers: None,
                    };
                    let cell = self.node(NodeKind::Cell(metadata.clone()), cell_source, None)?;
                    let element = self.element(cell_source).unwrap();
                    if let Some(id) = element.attribute(None, "id") {
                        header_targets
                            .entry(id.to_owned())
                            .or_insert(is_header.then_some(cell));
                    }
                    if let Some(headers) = self.policy.cell_headers(element) {
                        pending_headers.push((cell, cell_source, headers, metadata));
                    }
                    self.append_flow_children(cell, cell_source)?;
                    self.builder.append(row, cell)?;
                }
                self.builder.append(group, row)?;
            }
            self.builder.append(table, group)?;
        }
        for (cell, source, names, mut metadata) in pending_headers {
            let mut headers = Vec::new();
            let mut seen = HashSet::new();
            for name in names {
                if let Some(header) = header_targets.get(&name).copied().flatten()
                    && header != cell
                    && seen.insert(header)
                {
                    headers.push(header);
                } else {
                    let origin = self.origin(source)?;
                    self.diagnostics.push(Diagnostic {
                        severity: DiagnosticSeverity::Warning,
                        kind: DiagnosticKind::InvalidTableHeaderReference,
                        origin: Some(origin),
                        message: format!("table header reference '{name}' was missing, self-referencing, or repeated and was ignored"),
                    });
                }
            }
            metadata.explicit_headers = Some(headers);
            self.builder.set_kind(cell, NodeKind::Cell(metadata))?;
        }
        Ok(table)
    }

    fn table_span(
        &mut self,
        source: SourceNodeId,
        attribute: &str,
        maximum: u32,
    ) -> Result<NonZeroU32, LoweringError> {
        let raw = self
            .element(source)
            .and_then(|element| element.attribute(None, attribute))
            .map(str::to_owned);
        let parsed = raw.as_deref().and_then(|value| value.parse::<u32>().ok());
        let interpreted = parsed.map(|value| {
            if attribute == "rowspan" && value == 0 && self.policy.zero_row_span_to_end() {
                maximum.max(1)
            } else {
                value
            }
        });
        let requested = interpreted.unwrap_or(1);
        let normalized = requested.clamp(1, maximum.max(1));
        if let Some(raw) = raw
            && interpreted != Some(normalized)
        {
            let origin = self.origin(source)?;
            self.diagnostics.push(Diagnostic {
                severity: DiagnosticSeverity::Warning,
                kind: DiagnosticKind::InvalidSpan,
                origin: Some(origin),
                message: format!("{attribute} value '{raw}' was normalized to {normalized}"),
            });
        }
        Ok(NonZeroU32::new(normalized).unwrap())
    }

    fn lower_figure(&mut self, source: SourceNodeId) -> Result<NodeId, LoweringError> {
        let figure = self.node(NodeKind::Figure, source, None)?;
        if self.policy.preserve_figure_flow() {
            let mut pending = Vec::new();
            let mut caption_seen = false;
            for child in self.tree.get(source).unwrap().children().to_vec() {
                if self.element_name(child) == Some("figcaption") {
                    self.append_flow_nodes(figure, std::mem::take(&mut pending))?;
                    let kind = if caption_seen {
                        let origin = self.origin(child)?;
                        self.diagnostics.push(Diagnostic {
                            severity: DiagnosticSeverity::Warning,
                            kind: DiagnosticKind::InvalidChild,
                            origin: Some(origin),
                            message: "additional figure caption was retained as ordinary flow"
                                .into(),
                        });
                        NodeKind::Flow
                    } else {
                        NodeKind::Caption
                    };
                    let caption = self.node(kind, child, None)?;
                    self.append_flow_children(caption, child)?;
                    self.builder.append(figure, caption)?;
                    caption_seen = true;
                } else {
                    pending.push(child);
                }
            }
            self.append_flow_nodes(figure, pending)?;
            return Ok(figure);
        }
        for child in self.tree.get(source).unwrap().children().to_vec() {
            let lowered = match self.element_name(child) {
                Some("figcaption") => {
                    let caption = self.node(NodeKind::Caption, child, None)?;
                    self.append_flow_children(caption, child)?;
                    Some(caption)
                }
                Some("img") => Some(self.lower_image(child)?),
                Some(_) if self.is_block(child) => self.lower_flow(child)?,
                _ => None,
            };
            if let Some(lowered) = lowered {
                self.builder.append(figure, lowered)?;
            }
        }
        Ok(figure)
    }

    fn lower_image(&mut self, source: SourceNodeId) -> Result<NodeId, LoweringError> {
        let SourceNodeKind::Element(element) = self.tree.get(source).unwrap().kind() else {
            unreachable!()
        };
        let identity = element
            .attribute(None, "src")
            .unwrap_or_default()
            .to_owned();
        let title = self.policy.title(element);
        let responsive = self.policy.responsive_image(element, &mut self.builder)?;
        let alternative = element.attribute(None, "alt").map(str::to_owned);
        let width = element.attribute(None, "width").map(str::to_owned);
        let height = element.attribute(None, "height").map(str::to_owned);
        let alternative = match alternative.as_deref() {
            Some("") => ImageAlternative::Decorative,
            Some(value) => ImageAlternative::Text(NonEmptyText::new(value).unwrap()),
            None => {
                let origin = self.origin(source)?;
                self.diagnostics.push(Diagnostic {
                    severity: DiagnosticSeverity::Warning,
                    kind: DiagnosticKind::MissingImageAlternative,
                    origin: Some(origin),
                    message: "image has no alternative text".into(),
                });
                ImageAlternative::Missing
            }
        };
        let intrinsic_size = match (
            parse_positive_dimension(width.as_deref()),
            parse_positive_dimension(height.as_deref()),
        ) {
            (Some(width), Some(height)) => Some(IntrinsicSize { width, height }),
            _ => None,
        };
        let resource = if let Some(resource) = self.resources.get(&identity) {
            *resource
        } else {
            let resource = self.builder.add_resource(Resource {
                identity: identity.clone(),
                media_type: None,
            })?;
            self.resources.insert(identity, resource);
            resource
        };
        self.node(
            NodeKind::Image(Image {
                resource,
                alternative,
                title,
                intrinsic_size,
                responsive,
            }),
            source,
            None,
        )
    }

    fn collect_text(&self, source: SourceNodeId, output: &mut String) {
        match self.tree.get(source).unwrap().kind() {
            SourceNodeKind::Text(value) | SourceNodeKind::Cdata(value) => output.push_str(value),
            _ => {
                for child in self.tree.get(source).unwrap().children() {
                    self.collect_text(*child, output);
                }
            }
        }
    }

    fn append_inline_children(
        &mut self,
        parent: NodeId,
        source_parent: SourceNodeId,
    ) -> Result<(), LoweringError> {
        self.append_inline_children_with_mode(parent, source_parent, false)
    }

    fn append_inline_children_with_mode(
        &mut self,
        parent: NodeId,
        source_parent: SourceNodeId,
        literal: bool,
    ) -> Result<(), LoweringError> {
        let mut whitespace = InlineWhitespace {
            literal,
            at_boundary: true,
            ..InlineWhitespace::default()
        };
        for child in self.tree.get(source_parent).unwrap().children().to_vec() {
            if let Some(node) = self.lower_inline(child, &mut whitespace)? {
                self.builder.append(parent, node)?;
            }
        }
        Ok(())
    }

    fn lower_inline(
        &mut self,
        source: SourceNodeId,
        whitespace: &mut InlineWhitespace,
    ) -> Result<Option<NodeId>, LoweringError> {
        let kind = self.tree.get(source).unwrap().kind().clone();
        match kind {
            SourceNodeKind::Text(value) | SourceNodeKind::Cdata(value) => {
                let normalized = if whitespace.literal {
                    if !value.is_empty() {
                        whitespace.has_output = true;
                        whitespace.at_boundary = value.ends_with('\n');
                    }
                    value
                } else {
                    normalize_inline_text(&value, whitespace)
                };
                if normalized.is_empty() {
                    return Ok(None);
                }
                let node = self.builder.add_text(normalized)?;
                let origin = self.origin(source)?;
                self.builder.set_origin(node, origin)?;
                Ok(Some(node))
            }
            SourceNodeKind::Comment(_)
            | SourceNodeKind::ProcessingInstruction { .. }
            | SourceNodeKind::Document => Ok(None),
            SourceNodeKind::Element(element) => {
                let name = self
                    .policy
                    .element_name(&element)
                    .unwrap_or_default()
                    .to_owned();
                if let Some((tex, display)) = self.math_metadata(source) {
                    let math = self.node(
                        NodeKind::Math(Math {
                            notation: self.policy.math_notation(&element),
                            source: tex,
                            display,
                        }),
                        source,
                        None,
                    )?;
                    return self
                        .inline_atomic_with_pending(source, math, whitespace)
                        .map(Some);
                }
                if let Some(kind) = self.policy.inline_atom(&element) {
                    let invisible = matches!(kind, NodeKind::BreakOpportunity(_));
                    let atom = self.node(kind, source, None)?;
                    if invisible {
                        return Ok(Some(atom));
                    }
                    return self
                        .inline_atomic_with_pending(source, atom, whitespace)
                        .map(Some);
                }
                if self.policy.ruby(&element) {
                    if let Some(ruby) = self.lower_ruby(source, whitespace.literal)? {
                        return self
                            .inline_atomic_with_pending(source, ruby, whitespace)
                            .map(Some);
                    }
                    return self.lower_transparent_inline(source, whitespace);
                }
                if name == "img" {
                    let image = self.lower_image(source)?;
                    return self
                        .inline_atomic_with_pending(source, image, whitespace)
                        .map(Some);
                }
                if name == "br" {
                    whitespace.pending_space = false;
                    whitespace.at_boundary = true;
                    whitespace.has_output = true;
                    return self.node(NodeKind::HardBreak, source, None).map(Some);
                }

                let (mut kind, style) = match name.as_str() {
                    "a" => {
                        let Some(href) = element.attribute(None, "href") else {
                            return self.lower_transparent_inline(source, whitespace);
                        };
                        (
                            NodeKind::Link(Link {
                                content: LinkContent::Phrasing,
                                destination: destination(href),
                                relation: self.policy.link_relation(&element),
                                title: self.policy.title(&element),
                            }),
                            Some(SemanticStyleKey::Link),
                        )
                    }
                    "em" => inline(InlineRole::Emphasis, None),
                    "strong" => inline(InlineRole::Strong, None),
                    "code" => inline(InlineRole::Code, None),
                    "sub" => inline(InlineRole::Subscript, None),
                    "sup" => inline(InlineRole::Superscript, None),
                    "abbr" => inline(InlineRole::Abbreviation, None),
                    "cite" => inline(InlineRole::Citation, None),
                    "q" => inline(InlineRole::Quotation, None),
                    "mark" => inline(InlineRole::Mark, None),
                    "time" => inline(
                        InlineRole::Time,
                        element.attribute(None, "datetime").map(str::to_owned),
                    ),
                    _ => (NodeKind::Inline, None),
                };
                if let Some(semantic) = self.policy.inline_semantic(&element) {
                    kind = NodeKind::InlineSemantic(semantic);
                }
                let node = self.node(kind, source, style)?;
                let literal_block = whitespace.literal && name != "code" && self.is_block(source);
                if literal_block {
                    self.append_literal_break(node, source, whitespace)?;
                }
                for child in self.tree.get(source).unwrap().children().to_vec() {
                    if let Some(child) = self.lower_inline(child, whitespace)? {
                        self.builder.append(node, child)?;
                    }
                }
                if literal_block {
                    self.append_literal_break(node, source, whitespace)?;
                }
                Ok(Some(node))
            }
        }
    }

    fn append_literal_break(
        &mut self,
        parent: NodeId,
        source: SourceNodeId,
        whitespace: &mut InlineWhitespace,
    ) -> Result<(), LoweringError> {
        if whitespace.has_output && !whitespace.at_boundary {
            let text = self.builder.add_text("\n")?;
            let origin = self.origin(source)?;
            self.builder.set_origin(text, origin)?;
            self.builder.append(parent, text)?;
            whitespace.at_boundary = true;
        }
        Ok(())
    }

    /// Build a normalized ruby grid only when coverage is unambiguous. Malformed
    /// grids remain readable phrasing with a diagnostic, never invalid IR.
    fn lower_ruby(
        &mut self,
        source: SourceNodeId,
        literal: bool,
    ) -> Result<Option<NodeId>, LoweringError> {
        let mut plans = Vec::new();
        let mut plan = RubySegmentPlan::default();
        let mut implicit_base = false;
        let mut valid = true;
        for child in self.tree.get(source).unwrap().children().iter().copied() {
            match self.element_name(child) {
                Some("rt" | "rp") => {
                    if plan.levels.is_empty() {
                        plan.levels.push(Vec::new());
                    }
                    plan.levels[0].push(child);
                    implicit_base = false;
                }
                Some("rtc") => {
                    let level = self
                        .tree
                        .get(child)
                        .unwrap()
                        .children()
                        .iter()
                        .copied()
                        .filter(|child| !self.source_is_ignorable_whitespace(*child))
                        .collect::<Vec<_>>();
                    valid &= level
                        .iter()
                        .all(|child| matches!(self.element_name(*child), Some("rt" | "rp")));
                    plan.levels.push(level);
                    implicit_base = false;
                }
                _ if self.source_is_ignorable_whitespace(child) => {
                    if implicit_base && plan.levels.is_empty() {
                        plan.bases.last_mut().unwrap().push(child);
                    }
                }
                _ => {
                    if !plan.levels.is_empty() {
                        plans.push(std::mem::take(&mut plan));
                    }
                    if self.element_name(child) == Some("rb") {
                        plan.bases.push(vec![child]);
                        implicit_base = false;
                    } else if implicit_base {
                        plan.bases.last_mut().unwrap().push(child);
                    } else {
                        plan.bases.push(vec![child]);
                        implicit_base = true;
                    }
                }
            }
        }
        if !plan.bases.is_empty() || !plan.levels.is_empty() {
            plans.push(plan);
        }
        valid &= !plans.is_empty()
            && plans.iter().all(|plan| {
                !plan.bases.is_empty()
                    && plan.levels.iter().all(|level| {
                        let mut coverage = 0usize;
                        let mut previous_fallback = false;
                        let mut has_annotation = false;
                        for child in level {
                            if self.element_name(*child) == Some("rp") {
                                if previous_fallback {
                                    return false;
                                }
                                previous_fallback = true;
                            } else {
                                previous_fallback = false;
                                has_annotation = true;
                                let span = self
                                    .element(*child)
                                    .unwrap()
                                    .attribute(None, "rbspan")
                                    .map_or(Some(1), |span| span.parse::<usize>().ok());
                                let Some(span) =
                                    span.filter(|span| *span > 0 && *span <= plan.bases.len())
                                else {
                                    return false;
                                };
                                coverage += span;
                            }
                        }
                        has_annotation && coverage == plan.bases.len()
                    })
            });
        if !valid {
            let origin = self.origin(source)?;
            self.diagnostics.push(Diagnostic {
                severity: DiagnosticSeverity::Warning,
                kind: DiagnosticKind::InvalidRubyStructure,
                origin: Some(origin),
                message:
                    "ruby with ambiguous base/annotation coverage was retained as readable phrasing"
                        .into(),
            });
            return Ok(None);
        }
        let ruby = self.node(NodeKind::Ruby, source, None)?;
        for plan in plans {
            let segment = self.structural_wrapper(NodeKind::RubySegment, source)?;
            let bases = self.structural_wrapper(NodeKind::RubyBaseLevel, source)?;
            for base_sources in plan.bases {
                let base_source = base_sources[0];
                let explicit = self.element_name(base_source) == Some("rb");
                let base = if explicit {
                    self.node(NodeKind::RubyBase, base_source, None)?
                } else {
                    self.structural_wrapper(NodeKind::RubyBase, base_source)?
                };
                if explicit {
                    self.append_inline_children_with_mode(base, base_source, literal)?;
                } else {
                    let mut whitespace = InlineWhitespace {
                        literal,
                        at_boundary: true,
                        ..Default::default()
                    };
                    for child in base_sources {
                        if let Some(child) = self.lower_inline(child, &mut whitespace)? {
                            self.builder.append(base, child)?;
                        }
                    }
                }
                self.builder.append(bases, base)?;
            }
            self.builder.append(segment, bases)?;
            for level in plan.levels {
                let annotations = self.structural_wrapper(NodeKind::RubyAnnotationLevel, source)?;
                for child in level {
                    let kind = if self.element_name(child) == Some("rp") {
                        NodeKind::RubyFallback
                    } else {
                        let span = self
                            .element(child)
                            .unwrap()
                            .attribute(None, "rbspan")
                            .and_then(|span| span.parse().ok())
                            .unwrap_or(1);
                        NodeKind::RubyAnnotation(RubyAnnotation {
                            base_span: NonZeroU32::new(span).unwrap(),
                        })
                    };
                    let annotation = self.node(kind, child, None)?;
                    self.append_inline_children_with_mode(annotation, child, literal)?;
                    self.builder.append(annotations, annotation)?;
                }
                self.builder.append(segment, annotations)?;
            }
            self.builder.append(ruby, segment)?;
        }
        Ok(Some(ruby))
    }

    /// Structural wrappers share source provenance but must not claim its
    /// anchors or apply its element metadata repeatedly.
    fn structural_wrapper(
        &mut self,
        kind: NodeKind,
        source: SourceNodeId,
    ) -> Result<NodeId, LoweringError> {
        let node = self.builder.add_node(kind)?;
        let origin = self.origin(source)?;
        self.builder.set_origin(node, origin)?;
        Ok(node)
    }

    fn inline_atomic_with_pending(
        &mut self,
        source: SourceNodeId,
        atomic: NodeId,
        whitespace: &mut InlineWhitespace,
    ) -> Result<NodeId, LoweringError> {
        let node = if whitespace.pending_space {
            let wrapper = if self.policy.atomic_spacing_metadata() {
                self.node(NodeKind::Inline, source, None)?
            } else {
                self.structural_wrapper(NodeKind::Inline, source)?
            };
            let space = self.builder.add_text(" ")?;
            let origin = self.origin(source)?;
            self.builder.set_origin(space, origin)?;
            self.builder.append(wrapper, space)?;
            self.builder.append(wrapper, atomic)?;
            wrapper
        } else {
            atomic
        };
        whitespace.pending_space = false;
        whitespace.has_output = true;
        whitespace.at_boundary = false;
        Ok(node)
    }

    fn lower_transparent_inline(
        &mut self,
        source: SourceNodeId,
        whitespace: &mut InlineWhitespace,
    ) -> Result<Option<NodeId>, LoweringError> {
        let node = self.node(NodeKind::Inline, source, None)?;
        for child in self.tree.get(source).unwrap().children().to_vec() {
            if let Some(child) = self.lower_inline(child, whitespace)? {
                self.builder.append(node, child)?;
            }
        }
        Ok(Some(node))
    }

    fn generated_node(
        &mut self,
        kind: NodeKind,
        related_source: SourceNodeId,
        reason: GeneratedReason,
    ) -> Result<NodeId, LoweringError> {
        let node = self.builder.add_node(kind)?;
        let related = self.origin(related_source)?;
        let origin = self.builder.add_origin(Origin::Generated {
            reason,
            related: Some(related),
        })?;
        self.builder.set_origin(node, origin)?;
        Ok(node)
    }

    fn node(
        &mut self,
        kind: NodeKind,
        source: SourceNodeId,
        semantic_style: Option<SemanticStyleKey>,
    ) -> Result<NodeId, LoweringError> {
        let node = self.builder.add_node(kind)?;
        let origin = self.origin(source)?;
        self.builder.set_origin(node, origin)?;
        self.apply_metadata(node, source, semantic_style, origin)?;
        Ok(node)
    }

    fn apply_metadata(
        &mut self,
        node: NodeId,
        source: SourceNodeId,
        semantic_style: Option<SemanticStyleKey>,
        origin: OriginId,
    ) -> Result<(), LoweringError> {
        let SourceNodeKind::Element(element) = self.tree.get(source).unwrap().kind() else {
            if let Some(style) = semantic_style {
                let style = self.style(Some(style), None, AuthorStyle::default())?;
                self.builder.set_style_input(node, style)?;
            }
            return Ok(());
        };
        let language = element
            .attribute(None, "lang")
            .or_else(|| element.attribute(Some(XML_NAMESPACE), "lang"));
        let mut semantics = Semantics {
            roles: self.policy.roles(element),
            ..Semantics::default()
        };
        if let Some(language) = language {
            semantics.language = if language.is_empty() {
                LanguageOverride::Unknown
            } else if let Ok(language) = LanguageTag::parse(language) {
                LanguageOverride::Tag(language)
            } else {
                self.diagnostics.push(Diagnostic {
                    severity: DiagnosticSeverity::Warning,
                    kind: DiagnosticKind::InvalidLanguage,
                    origin: Some(origin),
                    message: format!("invalid language tag '{language}' was ignored"),
                });
                LanguageOverride::Inherit
            };
        }
        if semantics != Semantics::default() {
            self.builder.set_semantics(node, semantics)?;
        }

        let bidi = self.policy.bidi(element).or_else(|| {
            element
                .attribute(None, "dir")
                .and_then(|direction| match direction {
                    "ltr" => Some(BidiState {
                        direction: BaseDirection::LeftToRight,
                        mode: BidiMode::Normal,
                    }),
                    "rtl" => Some(BidiState {
                        direction: BaseDirection::RightToLeft,
                        mode: BidiMode::Normal,
                    }),
                    "auto" => Some(BidiState::default()),
                    _ => None,
                })
        });
        let author_style = self.author_styles[source.index()].clone();
        if semantic_style.is_some() || bidi.is_some() || !author_style.is_empty() {
            let style = self.style(semantic_style, bidi, author_style)?;
            self.builder.set_style_input(node, style)?;
        }

        if let Some(anchor) = element.attribute(None, "id") {
            if self.claimed_anchors.insert(anchor.to_owned()) {
                self.builder
                    .add_anchor_with_origin(AnchorId::new(anchor), node, Some(origin))?;
            } else {
                self.diagnostics.push(Diagnostic {
                    severity: DiagnosticSeverity::Warning,
                    kind: DiagnosticKind::DuplicateAnchor,
                    origin: Some(origin),
                    message: format!("duplicate anchor '{anchor}' ignored; the first target wins"),
                });
            }
        }
        Ok(())
    }

    fn style(
        &mut self,
        semantic: Option<SemanticStyleKey>,
        bidi: Option<BidiState>,
        author: AuthorStyle,
    ) -> Result<StyleInputId, BuildError> {
        if let Some(style) = self.styles.get(&(semantic, bidi, author.clone())) {
            return Ok(*style);
        }
        let mut input = match (semantic, bidi) {
            (Some(semantic), _) => StyleInput::semantic(semantic),
            (None, Some(bidi)) => StyleInput::source_bidi(bidi),
            (None, None) => StyleInput::default(),
        };
        if semantic.is_some()
            && let Some(bidi) = bidi
        {
            input = input.with_bidi(bidi);
        }
        input = input.with_author(author.clone());
        let style = self.builder.intern_style_input(input)?;
        self.styles.insert((semantic, bidi, author), style);
        Ok(style)
    }

    fn origin(&mut self, source: SourceNodeId) -> Result<OriginId, BuildError> {
        if let Some(origin) = self.origins[source.index()] {
            return Ok(origin);
        }
        // Generated provenance only points backwards, so origin chains are
        // acyclic. Resolve iteratively to avoid recursion on transformed trees.
        let mut pending = Vec::new();
        let mut cursor = source;
        while self.origins[cursor.index()].is_none() {
            let node = self.tree.get(cursor).unwrap();
            match &node.provenance {
                NodeProvenance::Original { identity } => {
                    let origin =
                        self.syntax
                            .origin(self.source_id, cursor, node, identity.as_ref());
                    self.origins[cursor.index()] = Some(self.builder.add_origin(origin)?);
                }
                NodeProvenance::Generated { reason, related } => {
                    pending.push((cursor, *reason, *related));
                    cursor = *related;
                }
            }
        }
        for (id, reason, related) in pending.into_iter().rev() {
            let origin = self.builder.add_origin(Origin::Generated {
                reason,
                related: self.origins[related.index()],
            })?;
            self.origins[id.index()] = Some(origin);
        }
        Ok(self.origins[source.index()].unwrap())
    }

    fn append_unwrapped_table_children(
        &mut self,
        parent: NodeId,
        source: SourceNodeId,
    ) -> Result<(), LoweringError> {
        let mut leaves = Vec::new();
        self.collect_unwrapped_table_children(source, &mut leaves);
        let mut paragraph = None;
        let mut whitespace = InlineWhitespace {
            at_boundary: true,
            ..InlineWhitespace::default()
        };
        for leaf in leaves {
            if self.is_block(leaf) {
                paragraph = None;
                whitespace = InlineWhitespace {
                    at_boundary: true,
                    ..InlineWhitespace::default()
                };
                if let Some(block) = self.lower_flow(leaf)? {
                    self.builder.append(parent, block)?;
                }
            } else if !self.source_is_ignorable_whitespace(leaf) {
                let paragraph_node = if let Some(paragraph) = paragraph {
                    paragraph
                } else {
                    let generated = self.generated_node(
                        NodeKind::Paragraph,
                        leaf,
                        GeneratedReason::TableNormalization,
                    )?;
                    self.builder.append(parent, generated)?;
                    whitespace = InlineWhitespace {
                        at_boundary: true,
                        ..InlineWhitespace::default()
                    };
                    paragraph = Some(generated);
                    generated
                };
                if let Some(inline) = self.lower_inline(leaf, &mut whitespace)? {
                    self.builder.append(paragraph_node, inline)?;
                }
            }
        }
        Ok(())
    }

    fn collect_unwrapped_table_children(
        &self,
        source: SourceNodeId,
        output: &mut Vec<SourceNodeId>,
    ) {
        for child in self.tree.get(source).unwrap().children() {
            if self.is_table_internal(*child) {
                self.collect_unwrapped_table_children(*child, output);
            } else {
                output.push(*child);
            }
        }
    }

    fn is_orphan_table_part(&self, source: SourceNodeId) -> bool {
        if !self.is_table_internal(source) {
            return false;
        }
        let mut parent = self.tree.get(source).and_then(|node| node.parent());
        while let Some(ancestor) = parent {
            if self.element_name(ancestor) == Some("table") {
                return false;
            }
            parent = self.tree.get(ancestor).and_then(|node| node.parent());
        }
        true
    }

    fn is_table_internal(&self, source: SourceNodeId) -> bool {
        matches!(
            self.element_name(source),
            Some("thead" | "tbody" | "tfoot" | "tr" | "td" | "th")
        )
    }

    fn math_metadata(&self, source: SourceNodeId) -> Option<(String, MathDisplay)> {
        self.policy.math_metadata(self.element(source)?)
    }

    fn element(&self, source: SourceNodeId) -> Option<&Element> {
        match self.tree.get(source)?.kind() {
            SourceNodeKind::Element(element) => Some(element),
            _ => None,
        }
    }

    fn element_name(&self, source: SourceNodeId) -> Option<&str> {
        self.element(source)
            .and_then(|element| self.policy.element_name(element))
    }

    fn is_flow_link(&self, source: SourceNodeId) -> bool {
        if self.element_name(source) != Some("a") {
            return false;
        }
        let element = self.element(source).unwrap();
        element.attribute(None, "href").is_some()
            && self.policy.link_content(
                element,
                self.tree
                    .get(source)
                    .unwrap()
                    .children()
                    .iter()
                    // Root-level code has a legacy preformatted convenience,
                    // but code inside a transparent link remains phrasing.
                    .any(|child| {
                        self.element_name(*child) != Some("code") && self.is_block(*child)
                    }),
            ) == LinkContent::Flow
    }

    fn is_block(&self, source: SourceNodeId) -> bool {
        if self.is_flow_link(source) {
            return true;
        }
        if self
            .element(source)
            .is_some_and(|element| self.policy.flow_container(element).is_some())
        {
            return true;
        }
        if self
            .math_metadata(source)
            .is_some_and(|(_, display)| display == MathDisplay::Block)
        {
            return true;
        }
        self.element_name(source).is_some_and(|name| {
            matches!(
                name,
                "html"
                    | "body"
                    | "div"
                    | "section"
                    | "article"
                    | "main"
                    | "header"
                    | "footer"
                    | "aside"
                    | "nav"
                    | "address"
                    | "h1"
                    | "h2"
                    | "h3"
                    | "h4"
                    | "h5"
                    | "h6"
                    | "p"
                    | "blockquote"
                    | "pre"
                    | "code"
                    | "hr"
                    | "figure"
                    | "ul"
                    | "ol"
                    | "dl"
                    | "table"
                    | "details"
                    | "iframe"
                    | "audio"
                    | "video"
            )
        })
    }

    fn source_is_ignorable_whitespace(&self, source: SourceNodeId) -> bool {
        match self.tree.get(source).unwrap().kind() {
            SourceNodeKind::Text(value) | SourceNodeKind::Cdata(value) => {
                value.chars().all(is_collapsible_whitespace)
            }
            SourceNodeKind::Comment(_) | SourceNodeKind::ProcessingInstruction { .. } => true,
            _ => false,
        }
    }
}

fn parse_positive_dimension(value: Option<&str>) -> Option<PositiveCssPixels> {
    value?.parse().ok().and_then(PositiveCssPixels::new)
}

fn inline(role: InlineRole, value: Option<String>) -> (NodeKind, Option<SemanticStyleKey>) {
    let style = match role {
        InlineRole::Emphasis => Some(SemanticStyleKey::Emphasis),
        InlineRole::Strong => Some(SemanticStyleKey::Strong),
        InlineRole::Code => Some(SemanticStyleKey::InlineCode),
        _ => None,
    };
    (
        NodeKind::InlineSemantic(InlineSemantic { role, value }),
        style,
    )
}

fn normalize_inline_text(value: &str, state: &mut InlineWhitespace) -> String {
    let mut output = String::new();
    for character in value.chars() {
        if is_collapsible_whitespace(character) {
            if state.has_output && !state.at_boundary {
                state.pending_space = true;
            }
            continue;
        }
        if state.pending_space {
            output.push(' ');
            state.pending_space = false;
        }
        output.push(character);
        state.has_output = true;
        state.at_boundary = false;
    }
    output
}

fn is_collapsible_whitespace(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\n' | '\r' | '\u{c}')
}

fn destination(raw: &str) -> Destination {
    if let Some(fragment) = raw.strip_prefix('#') {
        return Destination::Local(AnchorId::new(percent_decode(fragment)));
    }
    if has_uri_scheme(raw) {
        return Destination::External(raw.to_owned());
    }
    let (path, anchor) = raw.split_once('#').map_or((raw, None), |(path, anchor)| {
        (path, Some(AnchorId::new(percent_decode(anchor))))
    });
    Destination::Document {
        document: DocumentReference::Relative(path.to_owned()),
        anchor,
    }
}

fn has_uri_scheme(value: &str) -> bool {
    let Some((scheme, _)) = value.split_once(':') else {
        return false;
    };
    let mut characters = scheme.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
}

pub(in crate::formats) fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2]))
        {
            output.push(high * 16 + low);
            index += 3;
            continue;
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_lowering_policy_does_not_interpret_format_extensions() {
        struct NoExtensions;
        impl LoweringPolicy for NoExtensions {}

        let source = "<p>Before <span data-tex='x' class='katex'>visible fallback</span>.</p><pre><code class='language-mermaid'>graph TD\n  One --&gt; Two</code></pre>";
        let tree = crate::formats::xhtml::parse(source.as_bytes()).unwrap();
        let root = tree.root();
        let styles = vec![AuthorStyle::default(); tree.nodes().len()];
        let outcome = lower_tree(
            source,
            tree,
            &styles,
            LoweringOptions {
                resource_base: None,
                identity: None,
                retention: SourceRetention::Discard,
                syntax: SourceSyntax::Xhtml,
            },
            root,
            &NoExtensions,
        )
        .unwrap();
        let kinds = outcome
            .document
            .preorder(outcome.document.root())
            .map(|id| &outcome.document.get_node(id).unwrap().kind)
            .collect::<Vec<_>>();
        assert!(!kinds.iter().any(|kind| matches!(kind, NodeKind::Math(_))));
        assert!(
            kinds
                .iter()
                .any(|kind| matches!(kind, NodeKind::Preformatted(code)
            if code.content_kind == PreformattedKind::SourceCode
                && code.language.is_none()))
        );
        assert_eq!(
            outcome.document.text().as_str(),
            "Before visible fallback.\n\ngraph TD\n  One --> Two"
        );
    }
}
