//! Markdown importer for flow's semantic flow document model.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;
use std::sync::Arc;

use markdown::mdast::Node as MdNode;
use thiserror::Error;

use crate::model::flow::*;

#[derive(Clone, Debug, Default)]
pub struct MarkdownOptions {
    pub resource_base: Option<String>,
    pub identity: Option<DocumentReference>,
    pub retention: SourceRetention,
}

#[derive(Debug, Error)]
pub enum MarkdownError {
    #[error("could not parse Markdown: {0}")]
    Markdown(String),
    #[error(transparent)]
    Build(#[from] BuildError),
    #[error(transparent)]
    RetainedSource(#[from] RetainedSourceError),
    #[error("Markdown input exceeds the flow importer complexity limit")]
    TooComplex,
}

#[derive(Clone)]
struct Definition {
    url: String,
    title: Option<String>,
}

#[derive(Clone)]
struct Footnote {
    number: usize,
    defined: bool,
}

struct Importer<'a> {
    builder: FlowBuilder,
    source: &'a str,
    source_id: SourceArtifactId,
    definitions: HashMap<String, Definition>,
    footnotes: HashMap<String, Footnote>,
    reference_counts: HashMap<String, usize>,
    heading_counts: HashMap<String, usize>,
    reserved_note_anchors: HashSet<String>,
    claimed_anchors: HashSet<String>,
    footnote_depth: usize,
    styles: HashMap<SemanticStyleKey, StyleInputId>,
    diagnostics: Vec<Diagnostic>,
}

pub fn parse(source: &str, options: MarkdownOptions) -> Result<ImportOutcome, MarkdownError> {
    let mut parse_options = markdown::ParseOptions::gfm();
    parse_options.constructs.math_flow = true;
    parse_options.constructs.math_text = true;
    let root = markdown::to_mdast(source, &parse_options).map_err(MarkdownError::Markdown)?;
    validate_complexity(&root)?;

    let mut builder = FlowBuilder::new();
    builder.set_resource_base(options.resource_base);
    let source_id = builder.add_source(
        source.to_owned(),
        options.identity,
        Some("text/markdown".into()),
    )?;
    let definitions = collect_definitions(&root);
    let footnotes = collect_footnotes(&root);
    let reserved_note_anchors = collect_note_anchors(&root, &footnotes);
    let mut importer = Importer {
        builder,
        source,
        source_id,
        definitions,
        footnotes,
        reference_counts: HashMap::new(),
        heading_counts: HashMap::new(),
        reserved_note_anchors,
        claimed_anchors: HashSet::new(),
        footnote_depth: 0,
        styles: HashMap::new(),
        diagnostics: Vec::new(),
    };
    let document_root = importer.node(NodeKind::Document, &root)?;
    let children = match &root {
        MdNode::Root(root) => &root.children,
        _ => unreachable!(),
    };
    for child in children {
        if let Some(lowered) = importer.lower_flow(child)? {
            importer.builder.append(document_root, lowered)?;
        }
    }
    let revision = SourceDigest::for_text(source);
    let document = importer.builder.finish()?;
    let retained_sources = if options.retention == SourceRetention::Retain {
        let mut retained = RetainedSourceBundle::default();
        retained.insert(revision, Arc::from(source))?;
        Some(retained)
    } else {
        None
    };
    Ok(ImportOutcome {
        document,
        diagnostics: importer.diagnostics,
        retained_sources,
    })
}

impl Importer<'_> {
    fn lower_flow(&mut self, source: &MdNode) -> Result<Option<NodeId>, MarkdownError> {
        let node = match source {
            MdNode::Paragraph(paragraph) => {
                if let [image @ (MdNode::Image(_) | MdNode::ImageReference(_))] =
                    paragraph.children.as_slice()
                {
                    return self.lower_inline(image);
                }
                let node = self.node(NodeKind::Paragraph, source)?;
                self.append_inlines(node, &paragraph.children)?;
                node
            }
            MdNode::Heading(heading) => {
                let level = HeadingLevel::new(heading.depth).unwrap_or(HeadingLevel::H6);
                let node = self.node(NodeKind::Heading(Heading { level }), source)?;
                self.style(node, SemanticStyleKey::Heading)?;
                self.append_inlines(node, &heading.children)?;
                let base = self.heading_slug(&heading.children);
                let count = self.heading_counts.entry(base.clone()).or_default();
                let anchor = loop {
                    let candidate = if *count == 0 {
                        base.clone()
                    } else {
                        format!("{base}-{count}")
                    };
                    *count += 1;
                    if !self.reserved_note_anchors.contains(&candidate)
                        && !self.claimed_anchors.contains(&candidate)
                    {
                        break candidate;
                    }
                };
                let origin = self.builder_origin(source)?;
                self.add_anchor_first_wins(AnchorId::new(anchor), node, Some(origin))?;
                node
            }
            MdNode::BlockQuote(quotation) => {
                let node = self.node(NodeKind::Quotation, source)?;
                self.style(node, SemanticStyleKey::Quotation)?;
                self.append_flows(node, &quotation.children)?;
                node
            }
            MdNode::List(list) => return self.lower_list(source, list).map(Some),
            MdNode::Code(code) => {
                let node = self.node(
                    NodeKind::Preformatted(Preformatted {
                        language: code.lang.clone().filter(|value| !value.is_empty()),
                        content_kind: PreformattedKind::SourceCode,
                    }),
                    source,
                )?;
                self.style(node, SemanticStyleKey::Preformatted)?;
                let text = self.code_text(&code.value, source)?;
                self.builder.append(node, text)?;
                node
            }
            MdNode::Math(math) => self.node_with_span(
                NodeKind::Math(Math {
                    notation: MathNotation::Tex,
                    source: math.value.clone(),
                    display: MathDisplay::Block,
                }),
                math.value.len(),
                source,
                self.value_range(source, &math.value),
            )?,
            MdNode::ThematicBreak(_) => {
                let node = self.node_with_span(
                    NodeKind::ThematicBreak,
                    '\u{fffc}'.len_utf8(),
                    source,
                    None,
                )?;
                self.style(node, SemanticStyleKey::ThematicBreak)?;
                node
            }
            MdNode::Table(table) => return self.lower_table(source, table).map(Some),
            MdNode::FootnoteDefinition(definition) => {
                return self.lower_footnote_definition(source, definition).map(Some);
            }
            MdNode::Definition(_) => return Ok(None),
            MdNode::Html(_) => {
                self.diagnostic(
                    source,
                    DiagnosticSeverity::Warning,
                    DiagnosticKind::UnsupportedElement,
                    "raw HTML is omitted by the Markdown flow importer",
                )?;
                return Ok(None);
            }
            MdNode::Yaml(_) | MdNode::Toml(_) => {
                self.diagnostic(
                    source,
                    DiagnosticSeverity::Note,
                    DiagnosticKind::DroppedNonTextualMetadata,
                    "front matter is not readable flow content",
                )?;
                return Ok(None);
            }
            other => {
                if let Some(children) = other.children() {
                    let flow = self.node(NodeKind::Flow, source)?;
                    self.append_flows(flow, children)?;
                    flow
                } else {
                    self.diagnostic(
                        source,
                        DiagnosticSeverity::Warning,
                        DiagnosticKind::UnsupportedElement,
                        "unsupported Markdown construct was omitted",
                    )?;
                    return Ok(None);
                }
            }
        };
        Ok(Some(node))
    }

    fn lower_list(
        &mut self,
        source: &MdNode,
        list: &markdown::mdast::List,
    ) -> Result<NodeId, MarkdownError> {
        let kind = if list.ordered {
            ListKind::Ordered
        } else {
            ListKind::Unordered
        };
        let start = i64::from(list.start.unwrap_or(1));
        let node = self.node(
            NodeKind::List(List {
                kind,
                start,
                reversed: false,
            }),
            source,
        )?;
        self.style(
            node,
            if list.ordered {
                SemanticStyleKey::OrderedList
            } else {
                SemanticStyleKey::UnorderedList
            },
        )?;
        for child in &list.children {
            let MdNode::ListItem(item) = child else {
                continue;
            };
            let checkbox = item.checked.map(|checked| {
                if checked {
                    CheckboxState::Checked
                } else {
                    CheckboxState::Unchecked
                }
            });
            let item_node = self.node(NodeKind::ListItem(ListItem { ordinal: None }), child)?;
            for (index, item_child) in item.children.iter().enumerate() {
                let lowered = if index == 0
                    && let (Some(state), MdNode::Paragraph(paragraph)) = (checkbox, item_child)
                {
                    Some(self.lower_task_paragraph(child, item_child, paragraph, state)?)
                } else {
                    self.lower_flow(item_child)?
                };
                if let Some(lowered) = lowered {
                    self.builder.append(item_node, lowered)?;
                }
            }
            self.builder.append(node, item_node)?;
        }
        Ok(node)
    }

    fn lower_task_paragraph(
        &mut self,
        item_source: &MdNode,
        paragraph_source: &MdNode,
        paragraph: &markdown::mdast::Paragraph,
        state: CheckboxState,
    ) -> Result<NodeId, MarkdownError> {
        let paragraph_node = self.node(NodeKind::Paragraph, paragraph_source)?;
        let checkbox_range = self
            .task_marker_range(item_source)
            .unwrap_or_else(|| source_range(item_source).unwrap_or(0..0));
        let origin = self.builder.add_origin(Origin::Markdown {
            source: self.source_id,
            source_range: checkbox_range,
        })?;
        let checkbox = self
            .builder
            .add_node(NodeKind::Checkbox(Checkbox { state }))?;
        self.builder.set_origin(checkbox, origin)?;
        self.style(checkbox, SemanticStyleKey::CheckboxMarker)?;
        self.builder.append(paragraph_node, checkbox)?;
        self.append_inlines(paragraph_node, &paragraph.children)?;
        Ok(paragraph_node)
    }

    fn task_marker_range(&self, item: &MdNode) -> Option<std::ops::Range<usize>> {
        let item_range = source_range(item)?;
        let first_line_end = self.source[item_range.clone()]
            .find(['\r', '\n'])
            .map_or(item_range.end, |offset| item_range.start + offset);
        let line = self.source.get(item_range.start..first_line_end)?;
        let offset = line
            .as_bytes()
            .windows(3)
            .position(|window| window == b"[ ]" || window == b"[x]" || window == b"[X]")?;
        let start = item_range.start + offset;
        Some(start..start + 3)
    }

    fn lower_table(
        &mut self,
        source: &MdNode,
        table: &markdown::mdast::Table,
    ) -> Result<NodeId, MarkdownError> {
        let columns = table
            .children
            .iter()
            .filter_map(|row| row.children())
            .map(Vec::len)
            .max()
            .unwrap_or(1)
            .max(1);
        let column_count = NonZeroU32::new(u32::try_from(columns).unwrap_or(u32::MAX)).unwrap();
        let table_node = self.node(NodeKind::Table(Table { column_count }), source)?;
        self.style(table_node, SemanticStyleKey::Table)?;
        let mut head_group = None;
        let mut body_group = None;
        for (row_index, row_source) in table.children.iter().enumerate() {
            let MdNode::TableRow(row) = row_source else {
                continue;
            };
            let group_slot = if row_index == 0 {
                &mut head_group
            } else {
                &mut body_group
            };
            let group = if let Some(group) = *group_slot {
                group
            } else {
                let group = self.generated_node(
                    NodeKind::RowGroup(if row_index == 0 {
                        RowGroup::Head
                    } else {
                        RowGroup::Body
                    }),
                    row_source,
                    GeneratedReason::TableNormalization,
                )?;
                self.builder.append(table_node, group)?;
                *group_slot = Some(group);
                group
            };
            let row_node = self.node(NodeKind::Row, row_source)?;
            for (column_index, cell_source) in row.children.iter().enumerate() {
                let MdNode::TableCell(cell_source_data) = cell_source else {
                    continue;
                };
                let cell = self.node(
                    NodeKind::Cell(Cell {
                        kind: if row_index == 0 {
                            CellKind::Header
                        } else {
                            CellKind::Data
                        },
                        column_start: 0,
                        column_span: NonZeroU32::new(1).unwrap(),
                        row_span: NonZeroU32::new(1).unwrap(),
                        scope: (row_index == 0).then_some(HeaderScope::Column),
                        explicit_headers: None,
                    }),
                    cell_source,
                )?;
                if let Some(alignment) =
                    table
                        .align
                        .get(column_index)
                        .and_then(|alignment| match alignment {
                            markdown::mdast::AlignKind::Left => Some(TextAlignment::Left),
                            markdown::mdast::AlignKind::Center => Some(TextAlignment::Center),
                            markdown::mdast::AlignKind::Right => Some(TextAlignment::Right),
                            markdown::mdast::AlignKind::None => None,
                        })
                {
                    let style = self
                        .builder
                        .intern_style_input(StyleInput::table_alignment(alignment))?;
                    self.builder.set_style_input(cell, style)?;
                }
                let paragraph = self.generated_node(
                    NodeKind::Paragraph,
                    cell_source,
                    GeneratedReason::TableNormalization,
                )?;
                self.append_inlines(paragraph, &cell_source_data.children)?;
                self.builder.append(cell, paragraph)?;
                self.builder.append(row_node, cell)?;
            }
            self.builder.append(group, row_node)?;
        }
        Ok(table_node)
    }

    fn lower_footnote_definition(
        &mut self,
        source: &MdNode,
        definition: &markdown::mdast::FootnoteDefinition,
    ) -> Result<NodeId, MarkdownError> {
        let node = self.node(NodeKind::Section, source)?;
        self.builder.set_semantics(
            node,
            Semantics {
                roles: vec![SemanticRole::Known(KnownRole::Footnote)],
                ..Semantics::default()
            },
        )?;
        self.style(node, SemanticStyleKey::Footnote)?;
        self.footnote_depth += 1;
        let lowered = self.append_flows(node, &definition.children);
        self.footnote_depth -= 1;
        lowered?;
        let origin = self.builder_origin(source)?;
        self.add_anchor_first_wins(
            AnchorId::new(format!("fn-{}", definition.identifier)),
            node,
            Some(origin),
        )?;
        Ok(node)
    }

    fn append_flows(&mut self, parent: NodeId, children: &[MdNode]) -> Result<(), MarkdownError> {
        for child in children {
            if let Some(node) = self.lower_flow(child)? {
                self.builder.append(parent, node)?;
            }
        }
        Ok(())
    }

    fn append_inlines(&mut self, parent: NodeId, children: &[MdNode]) -> Result<(), MarkdownError> {
        for child in children {
            if let Some(node) = self.lower_inline(child)? {
                self.builder.append(parent, node)?;
            }
        }
        Ok(())
    }

    fn lower_inline(&mut self, source: &MdNode) -> Result<Option<NodeId>, MarkdownError> {
        let node = match source {
            MdNode::Text(text) => {
                if source_range(source)
                    .and_then(|range| self.source.get(range))
                    .is_some_and(contains_unresolved_footnote)
                {
                    self.diagnostic(
                        source,
                        DiagnosticSeverity::Warning,
                        DiagnosticKind::InvalidDestination,
                        "footnote reference has no definition",
                    )?;
                }
                let normalized = text
                    .value
                    .replace("\r\n", "\n")
                    .replace('\r', "\n")
                    .replace('\n', " ");
                self.text(&normalized, source, Some(text.value.as_str()))?
            }
            MdNode::Break(_) => self.node_with_span(NodeKind::HardBreak, 1, source, None)?,
            MdNode::InlineCode(code) => {
                let node = self.node(
                    NodeKind::InlineSemantic(InlineSemantic {
                        role: InlineRole::Code,
                        value: None,
                    }),
                    source,
                )?;
                self.style(node, SemanticStyleKey::InlineCode)?;
                let text = self.text(&code.value, source, Some(&code.value))?;
                self.builder.append(node, text)?;
                node
            }
            MdNode::InlineMath(math) => self.node_with_span(
                NodeKind::Math(Math {
                    notation: MathNotation::Tex,
                    source: math.value.clone(),
                    display: MathDisplay::Inline,
                }),
                math.value.len(),
                source,
                self.value_range(source, &math.value),
            )?,
            MdNode::Emphasis(emphasis) => self.inline_wrapper(
                source,
                InlineRole::Emphasis,
                SemanticStyleKey::Emphasis,
                &emphasis.children,
            )?,
            MdNode::Strong(strong) => self.inline_wrapper(
                source,
                InlineRole::Strong,
                SemanticStyleKey::Strong,
                &strong.children,
            )?,
            MdNode::Delete(delete) => self.inline_wrapper(
                source,
                InlineRole::Deletion,
                SemanticStyleKey::Deletion,
                &delete.children,
            )?,
            MdNode::Link(link) => self.link(
                source,
                &link.url,
                link.title.clone(),
                &link.children,
                self.link_relation(&link.url),
            )?,
            MdNode::LinkReference(link) => {
                let Some(definition) = self.definitions.get(&link.identifier).cloned() else {
                    self.diagnostic(
                        source,
                        DiagnosticSeverity::Warning,
                        DiagnosticKind::InvalidDestination,
                        "link reference has no definition",
                    )?;
                    let wrapper = self.node(NodeKind::Inline, source)?;
                    self.append_inlines(wrapper, &link.children)?;
                    return Ok(Some(wrapper));
                };
                self.link(
                    source,
                    &definition.url,
                    definition.title,
                    &link.children,
                    self.link_relation(&definition.url),
                )?
            }
            MdNode::Image(image) => {
                self.image(source, &image.url, &image.alt, image.title.clone())?
            }
            MdNode::ImageReference(image) => {
                let definition = self.definitions.get(&image.identifier).cloned();
                if definition.is_none() {
                    self.diagnostic(
                        source,
                        DiagnosticSeverity::Warning,
                        DiagnosticKind::InvalidDestination,
                        "image reference has no definition",
                    )?;
                }
                let (url, title) = definition
                    .map(|definition| (definition.url, definition.title))
                    .unwrap_or_else(|| (image.identifier.clone(), None));
                self.image(source, &url, &image.alt, title)?
            }
            MdNode::FootnoteReference(reference) => {
                return self
                    .lower_footnote_reference(source, &reference.identifier)
                    .map(Some);
            }
            MdNode::Html(_) => {
                self.diagnostic(
                    source,
                    DiagnosticSeverity::Warning,
                    DiagnosticKind::UnsupportedElement,
                    "raw inline HTML is omitted",
                )?;
                return Ok(None);
            }
            other => {
                if let Some(children) = other.children() {
                    let wrapper = self.node(NodeKind::Inline, source)?;
                    self.append_inlines(wrapper, children)?;
                    wrapper
                } else {
                    self.diagnostic(
                        source,
                        DiagnosticSeverity::Warning,
                        DiagnosticKind::UnsupportedElement,
                        "unsupported inline Markdown construct was omitted",
                    )?;
                    return Ok(None);
                }
            }
        };
        Ok(Some(node))
    }

    fn inline_wrapper(
        &mut self,
        source: &MdNode,
        role: InlineRole,
        style: SemanticStyleKey,
        children: &[MdNode],
    ) -> Result<NodeId, MarkdownError> {
        let node = self.node(
            NodeKind::InlineSemantic(InlineSemantic { role, value: None }),
            source,
        )?;
        self.style(node, style)?;
        self.append_inlines(node, children)?;
        Ok(node)
    }

    fn link(
        &mut self,
        source: &MdNode,
        raw_destination: &str,
        title: Option<String>,
        children: &[MdNode],
        relation: LinkRelation,
    ) -> Result<NodeId, MarkdownError> {
        let (destination, malformed) = destination(raw_destination);
        if malformed {
            self.diagnostic(
                source,
                DiagnosticSeverity::Warning,
                DiagnosticKind::InvalidDestination,
                "link destination contains malformed percent encoding",
            )?;
        }
        let node = self.node(
            NodeKind::Link(Link {
                content: LinkContent::Phrasing,
                destination,
                relation,
                title: title.filter(|value| !value.is_empty()),
            }),
            source,
        )?;
        self.style(node, SemanticStyleKey::Link)?;
        self.append_inlines(node, children)?;
        Ok(node)
    }

    fn image(
        &mut self,
        source: &MdNode,
        url: &str,
        alt: &str,
        title: Option<String>,
    ) -> Result<NodeId, MarkdownError> {
        let resource = self.builder.add_resource(Resource {
            identity: url.to_owned(),
            media_type: None,
        })?;
        let alternative = if alt.is_empty() {
            ImageAlternative::Decorative
        } else {
            ImageAlternative::Text(NonEmptyText::new(alt).expect("nonempty alternative"))
        };
        self.node_with_span(
            NodeKind::Image(Image {
                resource,
                alternative,
                title: title.filter(|value| !value.is_empty()),
                responsive: None,
                intrinsic_size: None,
            }),
            alt.len(),
            source,
            (!alt.is_empty())
                .then(|| self.value_range(source, alt))
                .flatten(),
        )
    }

    fn lower_footnote_reference(
        &mut self,
        source: &MdNode,
        identifier: &str,
    ) -> Result<NodeId, MarkdownError> {
        let footnote = self.footnotes.get(identifier).cloned();
        let label = footnote.as_ref().map_or_else(
            || identifier.to_owned(),
            |footnote| footnote.number.to_string(),
        );
        if !footnote.as_ref().is_some_and(|footnote| footnote.defined) {
            self.diagnostic(
                source,
                DiagnosticSeverity::Warning,
                DiagnosticKind::InvalidDestination,
                "footnote reference has no definition",
            )?;
            return self.generated_text(&label, source, GeneratedReason::FootnoteLabel);
        }
        let count = self
            .reference_counts
            .entry(identifier.to_owned())
            .or_default();
        let return_name = if *count == 0 {
            format!("fnref-{identifier}")
        } else {
            format!("fnref-{identifier}-{}", *count + 1)
        };
        *count += 1;
        let return_anchor = AnchorId::new(return_name);
        let link = self.node(
            NodeKind::Link(Link {
                content: LinkContent::Phrasing,
                destination: Destination::Local(AnchorId::new(format!("fn-{identifier}"))),
                relation: LinkRelation::NoteReference {
                    return_anchor: Some(return_anchor.clone()),
                },
                title: None,
            }),
            source,
        )?;
        self.style(link, SemanticStyleKey::Link)?;
        let superscript = self.generated_node(
            NodeKind::InlineSemantic(InlineSemantic {
                role: InlineRole::Superscript,
                value: None,
            }),
            source,
            GeneratedReason::FootnoteLabel,
        )?;
        let label_node = self.generated_text(&label, source, GeneratedReason::FootnoteLabel)?;
        self.builder.append(superscript, label_node)?;
        self.builder.append(link, superscript)?;
        let origin = self.builder_origin(source)?;
        self.add_anchor_first_wins(return_anchor, link, Some(origin))?;
        Ok(link)
    }

    fn node(&mut self, kind: NodeKind, source: &MdNode) -> Result<NodeId, MarkdownError> {
        let node = self.builder.add_node(kind)?;
        let origin = self.builder_origin(source)?;
        self.builder.set_origin(node, origin)?;
        Ok(node)
    }

    fn node_with_span(
        &mut self,
        kind: NodeKind,
        canonical_len: usize,
        source: &MdNode,
        source_range: Option<std::ops::Range<usize>>,
    ) -> Result<NodeId, MarkdownError> {
        let origin = self.builder_origin(source)?;
        let node = self.builder.add_node(kind)?;
        self.builder.set_origin(node, origin)?;
        if canonical_len > 0 {
            self.builder
                .add_origin_span(node, 0..canonical_len, origin, source_range)?;
        }
        Ok(node)
    }

    fn generated_node(
        &mut self,
        kind: NodeKind,
        related_source: &MdNode,
        reason: GeneratedReason,
    ) -> Result<NodeId, MarkdownError> {
        let related = self.builder_origin(related_source)?;
        let origin = self.builder.add_origin(Origin::Generated {
            reason,
            related: Some(related),
        })?;
        let node = self.builder.add_node(kind)?;
        self.builder.set_origin(node, origin)?;
        Ok(node)
    }

    fn text(
        &mut self,
        value: &str,
        source: &MdNode,
        source_value: Option<&str>,
    ) -> Result<NodeId, MarkdownError> {
        let node = self.builder.add_text(value)?;
        let origin = self.builder_origin(source)?;
        self.builder.set_origin(node, origin)?;
        if !value.is_empty() {
            let source_range = source_value
                .filter(|needle| *needle == value)
                .and_then(|needle| self.value_range(source, needle));
            self.builder
                .add_origin_span(node, 0..value.len(), origin, source_range)?;
        }
        Ok(node)
    }

    fn code_text(&mut self, value: &str, source: &MdNode) -> Result<NodeId, MarkdownError> {
        let origin = self.builder_origin(source)?;
        let node = self.builder.add_text(value)?;
        self.builder.set_origin(node, origin)?;
        let outer = source_range(source).unwrap_or(0..0);
        let raw = self.source.get(outer.clone()).unwrap_or_default();
        let mut raw_lines = Vec::new();
        let mut raw_offset = outer.start;
        for line in raw.split_inclusive('\n') {
            let content = line.strip_suffix('\n').unwrap_or(line);
            let content = content.strip_suffix('\r').unwrap_or(content);
            raw_lines.push((raw_offset, content));
            raw_offset += line.len();
        }
        let opening = raw_lines
            .first()
            .map(|(_, line)| line.trim_start())
            .unwrap_or_default();
        let fenced = opening.starts_with("```") || opening.starts_with("~~~");
        let candidates = if fenced && raw_lines.len() >= 2 {
            &raw_lines[1..raw_lines.len() - 1]
        } else {
            &raw_lines[..]
        };
        let value_lines = value.split('\n').collect::<Vec<_>>();
        let mut logical_cursor = 0;
        for (index, value_line) in value_lines.iter().enumerate() {
            if let Some((line_start, raw_line)) = candidates.get(index).copied()
                && !value_line.is_empty()
                && let Some(column) = raw_line.find(value_line)
            {
                let source_start = line_start + column;
                self.builder.add_origin_span(
                    node,
                    logical_cursor..logical_cursor + value_line.len(),
                    origin,
                    Some(source_start..source_start + value_line.len()),
                )?;
            }
            logical_cursor += value_line.len();
            if index + 1 < value_lines.len() {
                logical_cursor += 1;
            }
        }
        Ok(node)
    }

    fn generated_text(
        &mut self,
        value: &str,
        related_source: &MdNode,
        reason: GeneratedReason,
    ) -> Result<NodeId, MarkdownError> {
        let related = self.builder_origin(related_source)?;
        let origin = self.builder.add_origin(Origin::Generated {
            reason,
            related: Some(related),
        })?;
        let node = self.builder.add_text(value)?;
        self.builder.set_origin(node, origin)?;
        if !value.is_empty() {
            self.builder
                .add_origin_span(node, 0..value.len(), origin, None)?;
        }
        Ok(node)
    }

    fn builder_origin(&mut self, source: &MdNode) -> Result<OriginId, MarkdownError> {
        let range = source_range(source).unwrap_or(0..0);
        Ok(self.builder.add_origin(Origin::Markdown {
            source: self.source_id,
            source_range: range,
        })?)
    }

    fn value_range(&self, node: &MdNode, value: &str) -> Option<std::ops::Range<usize>> {
        let outer = source_range(node)?;
        (self.source.get(outer.clone())? == value).then_some(outer)
    }

    fn style(&mut self, node: NodeId, key: SemanticStyleKey) -> Result<(), MarkdownError> {
        let style = if let Some(style) = self.styles.get(&key) {
            *style
        } else {
            let style = self.builder.intern_style_input(StyleInput::semantic(key))?;
            self.styles.insert(key, style);
            style
        };
        self.builder.set_style_input(node, style)?;
        Ok(())
    }

    fn link_relation(&self, raw: &str) -> LinkRelation {
        if self.footnote_depth > 0
            && raw.strip_prefix('#').is_some_and(|fragment| {
                let anchor = percent_decode(fragment).0.to_lowercase();
                anchor.starts_with("fnref-") && self.reserved_note_anchors.contains(&anchor)
            })
        {
            LinkRelation::NoteReturn
        } else {
            LinkRelation::Ordinary
        }
    }

    fn heading_slug(&self, children: &[MdNode]) -> String {
        let mut visible = String::new();
        let mut stack = children.iter().rev().collect::<Vec<_>>();
        while let Some(node) = stack.pop() {
            match node {
                MdNode::Text(text) => visible.push_str(
                    &text
                        .value
                        .replace("\r\n", "\n")
                        .replace('\r', "\n")
                        .replace('\n', " "),
                ),
                MdNode::InlineCode(code) => visible.push_str(&code.value),
                MdNode::InlineMath(math) => visible.push_str(&math.value),
                MdNode::Image(image) => visible.push_str(&image.alt),
                MdNode::ImageReference(image) => visible.push_str(&image.alt),
                MdNode::FootnoteReference(reference) => {
                    if let Some(footnote) = self.footnotes.get(&reference.identifier) {
                        visible.push_str(&footnote.number.to_string());
                    } else {
                        visible.push_str(&reference.identifier);
                    }
                }
                MdNode::Break(_) => visible.push('\n'),
                _ => {
                    if let Some(children) = node.children() {
                        stack.extend(children.iter().rev());
                    }
                }
            }
        }
        slugify(&visible)
    }

    fn add_anchor_first_wins(
        &mut self,
        anchor: AnchorId,
        node: NodeId,
        origin: Option<OriginId>,
    ) -> Result<(), MarkdownError> {
        match self
            .builder
            .add_anchor_with_origin(anchor.clone(), node, origin)
        {
            Ok(()) => {
                self.claimed_anchors.insert(anchor.as_str().to_owned());
                Ok(())
            }
            Err(BuildError::DuplicateAnchor { .. }) => {
                self.diagnostics.push(Diagnostic {
                    severity: DiagnosticSeverity::Warning,
                    kind: DiagnosticKind::DuplicateAnchor,
                    origin,
                    message: format!(
                        "duplicate anchor {:?}; the first target was retained",
                        anchor.as_str()
                    ),
                });
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    fn diagnostic(
        &mut self,
        source: &MdNode,
        severity: DiagnosticSeverity,
        kind: DiagnosticKind,
        message: &str,
    ) -> Result<(), MarkdownError> {
        let origin = self.builder_origin(source)?;
        self.diagnostics.push(Diagnostic {
            severity,
            kind,
            origin: Some(origin),
            message: message.into(),
        });
        Ok(())
    }
}

const MAX_MARKDOWN_NODES: usize = 1_000_000;
const MAX_MARKDOWN_DEPTH: usize = 256;

fn validate_complexity(root: &MdNode) -> Result<(), MarkdownError> {
    let mut visited = 0usize;
    let mut stack = vec![(root, 0usize)];
    while let Some((node, depth)) = stack.pop() {
        visited = visited.checked_add(1).ok_or(MarkdownError::TooComplex)?;
        if visited > MAX_MARKDOWN_NODES || depth > MAX_MARKDOWN_DEPTH {
            return Err(MarkdownError::TooComplex);
        }
        if let Some(children) = node.children() {
            stack.extend(children.iter().map(|child| (child, depth + 1)));
        }
    }
    Ok(())
}

fn contains_unresolved_footnote(value: &str) -> bool {
    value.match_indices("[^").any(|(start, _)| {
        (start == 0 || value.as_bytes()[start - 1] != b'\\') && value[start + 2..].contains(']')
    })
}

fn source_range(node: &MdNode) -> Option<std::ops::Range<usize>> {
    node.position()
        .map(|position| position.start.offset..position.end.offset)
}

fn collect_definitions(root: &MdNode) -> HashMap<String, Definition> {
    fn visit(node: &MdNode, output: &mut HashMap<String, Definition>) {
        if let MdNode::Definition(definition) = node {
            output
                .entry(definition.identifier.clone())
                .or_insert_with(|| Definition {
                    url: definition.url.clone(),
                    title: definition.title.clone(),
                });
        }
        if let Some(children) = node.children() {
            for child in children {
                visit(child, output);
            }
        }
    }
    let mut definitions = HashMap::new();
    visit(root, &mut definitions);
    definitions
}

fn collect_footnotes(root: &MdNode) -> HashMap<String, Footnote> {
    fn visit_definitions(
        node: &MdNode,
        definitions: &mut HashSet<String>,
        order: &mut Vec<String>,
    ) {
        if let MdNode::FootnoteDefinition(definition) = node
            && definitions.insert(definition.identifier.clone())
        {
            order.push(definition.identifier.clone());
        }
        if let Some(children) = node.children() {
            for child in children {
                visit_definitions(child, definitions, order);
            }
        }
    }
    fn visit_references(node: &MdNode, order: &mut Vec<String>, seen: &mut HashSet<String>) {
        if let MdNode::FootnoteReference(reference) = node
            && seen.insert(reference.identifier.clone())
        {
            order.push(reference.identifier.clone());
        }
        if let Some(children) = node.children() {
            for child in children {
                visit_references(child, order, seen);
            }
        }
    }
    let mut definitions = HashSet::new();
    let mut definition_order = Vec::new();
    let mut order = Vec::new();
    let mut seen = HashSet::new();
    visit_definitions(root, &mut definitions, &mut definition_order);
    visit_references(root, &mut order, &mut seen);
    for identifier in definition_order {
        if seen.insert(identifier.clone()) {
            order.push(identifier);
        }
    }
    order
        .into_iter()
        .enumerate()
        .map(|(index, identifier)| {
            let defined = definitions.contains(&identifier);
            (
                identifier,
                Footnote {
                    number: index + 1,
                    defined,
                },
            )
        })
        .collect()
}

fn collect_note_anchors(root: &MdNode, footnotes: &HashMap<String, Footnote>) -> HashSet<String> {
    let mut anchors = footnotes
        .iter()
        .filter(|(_, footnote)| footnote.defined)
        .map(|(identifier, _)| format!("fn-{identifier}"))
        .collect::<HashSet<_>>();
    let mut counts = HashMap::<String, usize>::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if let MdNode::FootnoteReference(reference) = node
            && footnotes
                .get(&reference.identifier)
                .is_some_and(|footnote| footnote.defined)
        {
            let count = counts.entry(reference.identifier.clone()).or_default();
            *count += 1;
            anchors.insert(if *count == 1 {
                format!("fnref-{}", reference.identifier)
            } else {
                format!("fnref-{}-{count}", reference.identifier)
            });
        }
        if let Some(children) = node.children() {
            stack.extend(children);
        }
    }
    anchors
}

fn slugify(value: &str) -> String {
    value
        .chars()
        .filter_map(|character| {
            if character.is_alphanumeric() || character == '-' || character == '_' {
                Some(character.to_lowercase().collect::<String>())
            } else if character.is_whitespace() {
                Some("-".to_owned())
            } else {
                None
            }
        })
        .collect()
}

fn destination(raw: &str) -> (Destination, bool) {
    if let Some(fragment) = raw.strip_prefix('#') {
        let (fragment, malformed) = percent_decode(fragment);
        return (
            Destination::Local(AnchorId::new(fragment.to_lowercase())),
            malformed,
        );
    }
    if has_uri_scheme(raw) {
        return (Destination::External(raw.to_owned()), false);
    }
    let mut malformed = false;
    let (path, anchor) = raw.split_once('#').map_or((raw, None), |(path, anchor)| {
        let (anchor, invalid) = percent_decode(anchor);
        malformed = invalid;
        (path, Some(AnchorId::new(anchor.to_lowercase())))
    });
    (
        Destination::Document {
            document: DocumentReference::Relative(path.to_owned()),
            anchor,
        },
        malformed,
    )
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

fn percent_decode(value: &str) -> (String, bool) {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut malformed = false;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 < bytes.len()
                && let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2]))
            {
                output.push(high * 16 + low);
                index += 3;
                continue;
            }
            malformed = true;
        }
        output.push(bytes[index]);
        index += 1;
    }
    match String::from_utf8(output) {
        Ok(decoded) => (decoded, malformed),
        Err(error) => (String::from_utf8_lossy(error.as_bytes()).into_owned(), true),
    }
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

    fn import(source: &str) -> ImportOutcome {
        super::parse(source, MarkdownOptions::default()).unwrap()
    }

    fn kinds(document: &FlowDocument) -> Vec<&NodeKind> {
        document
            .preorder(document.root())
            .map(|id| &document.get_node(id).unwrap().kind)
            .collect()
    }

    #[test]
    fn lowers_task_syntax_to_empty_text_checkboxes_with_exact_origins() {
        let source = "- ordinary\n- [ ] open\n- [X] done\n- prefix [x] literal\n";
        let outcome = parse(source, MarkdownOptions::default()).unwrap();
        let document = &outcome.document;
        assert_eq!(
            document.text().as_str(),
            "ordinary\n\nopen\n\ndone\n\nprefix [x] literal"
        );

        let checkboxes = document
            .preorder(document.root())
            .filter_map(|id| {
                let node = document.get_node(id).unwrap();
                let NodeKind::Checkbox(checkbox) = &node.kind else {
                    return None;
                };
                Some((id, checkbox.state, node))
            })
            .collect::<Vec<_>>();
        assert_eq!(checkboxes.len(), 2);
        assert_eq!(checkboxes[0].1, CheckboxState::Unchecked);
        assert_eq!(checkboxes[1].1, CheckboxState::Checked);
        for (index, (id, _, node)) in checkboxes.iter().enumerate() {
            assert!(node.text_range.is_empty());
            let expected = if index == 0 {
                source.find("[ ]").unwrap()
            } else {
                source.find("[X]").unwrap()
            };
            assert!(matches!(
                document.get_origin(node.origin.unwrap()),
                Some(Origin::Markdown { source_range, .. })
                    if source_range == &(expected..expected + 3)
            ));
            let styles = document.resolve_styles(StyleContextKey::default());
            let style = styles
                .get_computed_style(styles.style_for_node(*id).unwrap())
                .unwrap();
            assert!(style.checkbox_marker());
        }

        let plain = parse("- open\n", MarkdownOptions::default()).unwrap();
        let task = parse("- [ ] open\n", MarkdownOptions::default()).unwrap();
        assert_eq!(plain.document.text(), task.document.text());
    }

    #[test]
    fn lowers_the_markdown_contract_tree() {
        let source = "> 3. **Read** this.[^n]\n>    - [x] carefully\n>\n> [^n]: A note.";
        let outcome = import(source);
        let document = &outcome.document;
        assert_eq!(
            document.text().as_str(),
            "Read this.1\n\ncarefully\n\nA note."
        );
        assert!(outcome.diagnostics.is_empty());
        let kinds = kinds(document);
        assert!(kinds.iter().any(|kind| matches!(kind, NodeKind::Quotation)));
        assert!(kinds.iter().any(|kind| matches!(
            kind,
            NodeKind::List(List {
                kind: ListKind::Ordered,
                start: 3,
                ..
            })
        )));
        assert!(
            kinds
                .iter()
                .any(|kind| matches!(kind, NodeKind::ListItem(ListItem { ordinal: None, .. })))
        );
        assert!(kinds.iter().any(|kind| matches!(
            kind,
            NodeKind::Checkbox(Checkbox {
                state: CheckboxState::Checked,
            })
        )));
        assert!(kinds.iter().any(|kind| matches!(
            kind,
            NodeKind::InlineSemantic(InlineSemantic {
                role: InlineRole::Strong,
                ..
            })
        )));
        assert!(document.anchor(&AnchorId::new("fn-n")).is_some());
        assert!(document.anchor(&AnchorId::new("fnref-n")).is_some());
        let note = document.anchor(&AnchorId::new("fn-n")).unwrap().target.node;
        assert_eq!(
            document.get_node(note).unwrap().semantics.roles,
            vec![SemanticRole::Known(KnownRole::Footnote)]
        );
    }

    #[test]
    fn discard_retention_keeps_only_source_descriptors() {
        let source = "# Visible\n\nsource-only sentinel <!-- retained nowhere -->";
        let outcome = super::parse(
            source,
            MarkdownOptions {
                retention: SourceRetention::Discard,
                ..MarkdownOptions::default()
            },
        )
        .unwrap();

        assert!(outcome.retained_sources.is_none());
        assert_eq!(outcome.document.source_artifacts().len(), 1);
        assert!(
            !outcome
                .document
                .text()
                .as_str()
                .contains("retained nowhere")
        );
    }

    #[test]
    fn lowers_markdown_vocabulary_resources_styles_and_origins() {
        let source = "# Title\n\nsoft\nline *em* **strong** ~~gone~~ `code` [link][d] ![alt](image.png) $x$\\\nnext\n\n> quote\n\n1. [ ] task\n\n```rust\nfn main() {}\n```\n\n***\n\n| Name | Count |\n| --- | ---: |\n| One | 1 |\n\n[d]: guide.md#Part";
        let outcome = super::parse(
            source,
            MarkdownOptions {
                resource_base: Some("book/".into()),
                identity: Some(DocumentReference::Relative("chapter.md".into())),
                retention: SourceRetention::Retain,
            },
        )
        .unwrap();
        let document = &outcome.document;
        assert_eq!(document.resource_base(), Some("book/"));
        assert!(
            document
                .text()
                .as_str()
                .contains("soft line em strong gone code link alt x\nnext")
        );
        assert!(document.text().as_str().contains("Name\tCount\nOne\t1"));
        assert!(document.anchor(&AnchorId::new("title")).is_some());
        assert_eq!(document.resources().len(), 1);
        assert_eq!(
            document
                .get_resource(document.resources().next().unwrap().0)
                .unwrap()
                .identity,
            "image.png"
        );
        assert!(document.style_inputs().len() > 1);
        assert!(document.origins().len() > 1);
        assert!(!document.origin_spans().is_empty());
        assert!(
            document.preorder(document.root()).all(|id| document
                .get_node(id)
                .unwrap()
                .origin
                .is_some())
        );
        let artifact = document.source_artifacts().next().unwrap().1;
        let retained = outcome.retained_sources.as_ref().unwrap();
        assert_eq!(retained.get(&artifact.revision), Some(source));

        let kinds = kinds(document);
        for expected in [
            SemanticStyleKey::Heading,
            SemanticStyleKey::Quotation,
            SemanticStyleKey::OrderedList,
            SemanticStyleKey::CheckboxMarker,
            SemanticStyleKey::Preformatted,
            SemanticStyleKey::Table,
            SemanticStyleKey::Emphasis,
            SemanticStyleKey::Strong,
            SemanticStyleKey::Deletion,
            SemanticStyleKey::InlineCode,
            SemanticStyleKey::Link,
        ] {
            assert!(
                document
                    .style_inputs()
                    .any(|(_, style)| style.semantic_key() == Some(expected))
            );
        }
        assert!(kinds.iter().any(|kind| matches!(kind, NodeKind::Math(Math { source, display: MathDisplay::Inline, .. }) if source == "x")));
        assert!(kinds.iter().any(|kind| matches!(kind, NodeKind::HardBreak)));
        assert!(kinds.iter().any(|kind| matches!(kind, NodeKind::Table(_))));
        assert!(document.preorder(document.root()).any(|id| {
            let node = document.get_node(id).unwrap();
            matches!(node.kind, NodeKind::Cell(_))
                && document
                    .get_style_input(node.style_input)
                    .is_some_and(|style| style.text_alignment() == Some(TextAlignment::Right))
        }));
    }

    #[test]
    fn note_anchors_are_reserved_and_return_relations_are_contextual() {
        let outcome = import(
            "# fn-n\n\n# fn-n-1\n\n[ordinary](#fnref-n) note[^n]\n\n[^n]: body [back](#fnref-n)",
        );
        let document = &outcome.document;
        assert!(document.anchor(&AnchorId::new("fn-n-1")).is_some());
        assert!(document.anchor(&AnchorId::new("fn-n-1-1")).is_some());
        let definition = document.anchor(&AnchorId::new("fn-n")).unwrap().target.node;
        assert_eq!(
            document.get_node(definition).unwrap().semantics.roles,
            vec![SemanticRole::Known(KnownRole::Footnote)]
        );
        let relations = document
            .preorder(document.root())
            .filter_map(|id| match &document.get_node(id).unwrap().kind {
                NodeKind::Link(link) => Some(link.relation.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(relations.contains(&LinkRelation::Ordinary));
        assert!(relations.contains(&LinkRelation::NoteReturn));
        assert!(
            relations
                .iter()
                .any(|relation| matches!(relation, LinkRelation::NoteReference { .. }))
        );
    }

    #[test]
    fn repeated_and_missing_footnotes_have_stable_semantics() {
        let outcome = import("a[^n] b[^n] c[^missing]\n\n[^n]: note [back](#fnref-n)");
        let document = &outcome.document;
        assert_eq!(document.text().as_str(), "a1 b1 c[^missing]\n\nnote back");
        for anchor in ["fn-n", "fnref-n", "fnref-n-2"] {
            assert!(
                document.anchor(&AnchorId::new(anchor)).is_some(),
                "{anchor}"
            );
        }
        assert!(
            outcome
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.kind == DiagnosticKind::InvalidDestination)
        );
        let generated_labels = document
            .origins()
            .filter(|(_, origin)| {
                matches!(
                    origin,
                    Origin::Generated {
                        reason: GeneratedReason::FootnoteLabel,
                        ..
                    }
                )
            })
            .count();
        assert!(generated_labels >= 3);
        assert!(document.preorder(document.root()).any(|id| matches!(
            &document.get_node(id).unwrap().kind,
            NodeKind::Link(Link {
                relation: LinkRelation::NoteReturn,
                ..
            })
        )));
    }

    #[test]
    fn duplicate_source_anchors_keep_the_first_target() {
        let outcome = import("[^n]: first\n\n[^n]: second");
        assert_eq!(outcome.document.text().as_str(), "first\n\nsecond");
        assert_eq!(
            outcome
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.kind == DiagnosticKind::DuplicateAnchor)
                .count(),
            1
        );
        let anchor = outcome.document.anchor(&AnchorId::new("fn-n")).unwrap();
        assert_eq!(anchor.target.position.offset, TextOffset::ZERO);
    }

    #[test]
    fn source_mappings_are_exact_or_explicitly_coarse() {
        let source = "A &amp; \\*literal\\*\r\nnext\n\n```rust\nrust\ncode\n```";
        let outcome = import(source);
        let document = &outcome.document;
        assert_eq!(document.text().as_str(), "A & *literal* next\n\nrust\ncode");

        let code = document
            .preorder(document.root())
            .find(|id| {
                matches!(
                    document.get_node(*id).unwrap().kind,
                    NodeKind::Preformatted(_)
                )
            })
            .unwrap();
        let code_range = document.get_node(code).unwrap().text_range;
        let code_spans = document
            .origin_spans()
            .iter()
            .filter(|span| {
                code_range.start <= span.logical.start && span.logical.end <= code_range.end
            })
            .filter(|span| span.source_range.is_some())
            .collect::<Vec<_>>();
        assert_eq!(code_spans.len(), 2);
        for span in code_spans {
            let logical = document.get_text(span.logical).unwrap();
            let source_range = span.source_range.clone().unwrap();
            assert_eq!(&source[source_range], logical);
        }
        assert!(document.origin_spans().iter().any(|span| {
            document.get_text(span.logical) == Some("A & *literal* next")
                && span.source_range.is_none()
        }));

        let transformed = import("hard  \nline\n\n***");
        for id in transformed.document.preorder(transformed.document.root()) {
            let node = transformed.document.get_node(id).unwrap();
            if matches!(node.kind, NodeKind::HardBreak | NodeKind::ThematicBreak) {
                let origin = node.origin.unwrap();
                assert!(
                    transformed
                        .document
                        .origin_spans()
                        .iter()
                        .filter(|span| span.origin == origin)
                        .all(|span| span.source_range.is_none())
                );
            }
        }
    }

    #[test]
    fn fragment_decoding_is_utf8_safe_and_diagnoses_malformed_sequences() {
        let (valid, malformed) = destination("#caf%C3%A9");
        assert_eq!(valid, Destination::Local(AnchorId::new("café")));
        assert!(!malformed);
        let (unicode, malformed) = destination("#%aé");
        assert!(matches!(unicode, Destination::Local(_)));
        assert!(malformed);

        let outcome = import("[bad](#bad%zz)");
        assert!(
            outcome
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.kind == DiagnosticKind::InvalidDestination })
        );
        for uri in ["https:opaque", "urn:isbn:9780140328721"] {
            assert_eq!(destination(uri), (Destination::External(uri.into()), false));
        }
    }

    #[test]
    fn heading_slugs_include_atomic_alternatives_and_footnote_labels() {
        let image = import("# ![Landscape](view.png)");
        assert_eq!(image.document.text().as_str(), "Landscape");
        assert!(image.document.anchor(&AnchorId::new("landscape")).is_some());

        let footnote = import("# Note[^n]\n\n[^n]: definition");
        assert_eq!(footnote.document.text().as_str(), "Note1\n\ndefinition");
        assert!(footnote.document.anchor(&AnchorId::new("note1")).is_some());
    }

    #[test]
    fn excessive_markdown_depth_is_rejected_before_lowering() {
        let source = format!("{}deep", "> ".repeat(MAX_MARKDOWN_DEPTH + 8));
        assert!(matches!(
            super::parse(&source, MarkdownOptions::default()),
            Err(MarkdownError::TooComplex)
        ));
    }

    #[test]
    fn table_structure_is_normalized_semantically() {
        let outcome = import("| A | B |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |");
        let document = &outcome.document;
        let groups = document
            .preorder(document.root())
            .filter_map(|id| match document.get_node(id).unwrap().kind {
                NodeKind::RowGroup(group) => Some(group),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(groups, vec![RowGroup::Head, RowGroup::Body]);
        let cells = document
            .preorder(document.root())
            .filter_map(|id| match &document.get_node(id).unwrap().kind {
                NodeKind::Cell(cell) => Some((cell.kind, cell.column_start)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            cells,
            vec![
                (CellKind::Header, 0),
                (CellKind::Header, 1),
                (CellKind::Data, 0),
                (CellKind::Data, 1),
                (CellKind::Data, 0),
                (CellKind::Data, 1),
            ]
        );
    }

    #[test]
    fn markdown_fixtures_have_flow_semantic_equivalents() {
        for (source, expected) in [
            ("1. one\n2. two\n\n> quoted", "one\n\ntwo\n\nquoted"),
            (
                "- [x] done\n- [ ] pending\n- ordinary",
                "done\n\npending\n\nordinary",
            ),
            ("![A landscape](images/view.png \"A title\")", "A landscape"),
            ("A café résumé.", "A café résumé."),
            ("soft\nline\n\nhard  \nbreak", "soft line\n\nhard\nbreak"),
            (
                "# One\n\nA **bold** line.\n\n| X | Y |\n|---|---|\n| A | B |\n\n```\ncode\n```",
                "One\n\nA bold line.\n\nX\tY\nA\tB\n\ncode",
            ),
        ] {
            assert_eq!(
                import(source).document.text().as_str(),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn heading_slugs_references_images_math_and_malformed_input_are_deterministic() {
        let outcome = import(
            "# Same\n\n# Same\n\n[ok][d] [missing][x] ![][img]\n\n<div>ignored</div>\n\n[d]: https://example.com\n[img]: image.png\n\n$$\ny = 2\n$$",
        );
        let document = &outcome.document;
        assert!(document.anchor(&AnchorId::new("same")).is_some());
        assert!(document.anchor(&AnchorId::new("same-1")).is_some());
        assert!(
            outcome
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.kind == DiagnosticKind::UnsupportedElement })
        );
        assert!(document.preorder(document.root()).any(|id| matches!(
            &document.get_node(id).unwrap().kind,
            NodeKind::Image(Image {
                alternative: ImageAlternative::Decorative,
                ..
            })
        )));
        assert!(document.preorder(document.root()).any(|id| matches!(
            &document.get_node(id).unwrap().kind,
            NodeKind::Math(Math { display: MathDisplay::Block, source, .. }) if source == "y = 2"
        )));
    }
}
