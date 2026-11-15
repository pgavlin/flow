use std::sync::Arc;
use text_layout::{Item, LineWidths};
use unicode_segmentation::UnicodeSegmentation;

use super::{
    Atom, AtomicLayout, AtomicLayoutKind, BlockBoxLayout, BlockLayout, BorderLayout, Break,
    ComposedBlock, ComposedDocument, ContinuousLayout, FontFamily, FontWeight, GeneratedAction,
    LayoutBlockKind, LayoutOptions, LineLayout, RunLayout, SyntaxKind, TableLayout, TableRuleKind,
    TableRuleLayout, TableSizingDiagnostics, TextMeasurer, TextStyle, WordBreaking, highlight_code,
    line_box, line_breaks, make_atom, materialize_lines, push_atomic_item, push_break_opportunity,
    push_text_items, set_flow_link,
    table::{
        CellMeasure, TableSizingInput, TrackConstraint, automatic_table_width,
        compute_intrinsic_grid, distribute_assignable_width, distribute_emergency_width,
        distribute_span_minima,
    },
    terminate, terminate_at,
};
use crate::model::{
    TextRange,
    flow::{
        EffectiveFontFamily, EffectiveFontWeight, EffectiveTextBreaking, EffectiveVerticalAlign,
        FlowDocument, NodeId, NodeKind, StyleSnapshot,
    },
};

#[derive(Clone, Copy, Debug)]
enum TextBlockKind {
    Paragraph,
    Heading,
    Preformatted,
    Table,
    Atomic,
    Rule,
}

#[derive(Clone, Debug)]
struct MarkerGroup {
    segments: Vec<String>,
    superscript: bool,
}

#[derive(Clone, Debug)]
struct FootnotePresentation {
    label: String,
    first_reference: Option<crate::model::flow::AnchorId>,
}

#[derive(Clone, Debug)]
struct HangingColumn {
    samples: Arc<[String]>,
    compact: bool,
}

#[derive(Clone, Debug, Default)]
struct Presentation {
    hanging_columns: Vec<HangingColumn>,
    list_depth: usize,
    quote_bars: Vec<(usize, u16, bool)>,
    marker: Option<Vec<MarkerGroup>>,
    marker_level: Option<usize>,
    marker_checkboxes: Vec<NodeId>,
}

#[derive(Clone, Debug)]
struct TextBlock {
    node: NodeId,
    range: TextRange,
    kind: TextBlockKind,
    inherited_link: Option<crate::model::flow::Destination>,
    presentation: Presentation,
    trailing_action: Option<GeneratedAction>,
}

#[allow(dead_code)]
pub(crate) fn layout_flow_document<M: TextMeasurer>(
    document: &FlowDocument,
    styles: &StyleSnapshot,
    measurer: &M,
    options: &LayoutOptions,
) -> ContinuousLayout {
    compose_flow_document(document, styles, measurer, options).into_continuous_layout()
}

pub(crate) fn compose_flow_document<M: TextMeasurer>(
    document: &FlowDocument,
    styles: &StyleSnapshot,
    measurer: &M,
    options: &LayoutOptions,
) -> ComposedDocument {
    let mut result = ComposedDocument {
        width: options.width,
        leading_spacing: options.block_spacing,
        minimum_height: options.block_spacing * 2.0,
        ..ComposedDocument::default()
    };
    let note_markers = footnote_markers(document);
    let units = text_blocks_from(document, styles, document.root(), &note_markers);
    let column_widths = measure_column_widths(&units, measurer, options);

    for unit in units {
        let computed = styles
            .style_for_node(unit.node)
            .and_then(|style| styles.get_computed_style(style))
            .expect("style snapshots cover every finalized node");
        let margin_before = computed
            .margin_top()
            .map_or(0.0, |margin| {
                margin.resolve(options.body_size, options.width)
            })
            .max(0.0);
        let margin_after = computed
            .margin_bottom()
            .map_or(options.block_spacing, |margin| {
                margin.resolve(options.body_size, options.width).max(0.0)
            });
        let trailing_action = unit.trailing_action.clone();
        let mut block = match unit.kind {
            TextBlockKind::Paragraph | TextBlockKind::Heading => layout_text_block(
                document,
                styles,
                unit,
                &column_widths,
                measurer,
                0.0,
                FlowLayoutContext {
                    options,
                    force_emergency_breaking: false,
                },
            ),
            TextBlockKind::Preformatted => layout_preformatted(
                document,
                styles,
                unit,
                &column_widths,
                measurer,
                options,
                0.0,
            ),
            TextBlockKind::Table => layout_table(
                document,
                styles,
                unit,
                &column_widths,
                measurer,
                options,
                0.0,
                &note_markers,
            ),
            TextBlockKind::Atomic => layout_atomic_block(
                document,
                styles,
                unit,
                &column_widths,
                measurer,
                options,
                0.0,
            ),
            TextBlockKind::Rule => {
                layout_rule(styles, unit, &column_widths, measurer, options, 0.0)
            }
        };
        if let Some(action) = trailing_action {
            append_generated_action(&mut block, action, measurer, options);
        }
        block.y = 0.0;
        result.blocks.push(ComposedBlock {
            layout: block,
            margin_before,
            margin_after,
        });
    }
    result
}

fn text_blocks_from(
    document: &FlowDocument,
    styles: &StyleSnapshot,
    root: NodeId,
    note_markers: &std::collections::HashMap<NodeId, FootnotePresentation>,
) -> Vec<TextBlock> {
    #[derive(Clone)]
    enum Event {
        Enter(NodeId, Option<crate::model::flow::Destination>),
        ExitList,
        ExitMarker,
        ExitQuote,
    }
    struct ListState {
        kind: crate::model::flow::ListKind,
        next: i64,
        reversed: bool,
        column_samples: Arc<[String]>,
    }
    struct MarkerState {
        group: MarkerGroup,
        column: HangingColumn,
        checkbox: Option<NodeId>,
        emitted: bool,
    }

    let mut blocks = Vec::new();
    let mut stack = vec![Event::Enter(root, None)];
    let mut lists = Vec::<ListState>::new();
    let mut markers = Vec::<MarkerState>::new();
    let mut quote_bars = Vec::<(usize, u16, bool)>::new();
    while let Some(event) = stack.pop() {
        let (id, inherited_link) = match event {
            Event::ExitList => {
                lists.pop();
                continue;
            }
            Event::ExitMarker => {
                markers.pop();
                continue;
            }
            Event::ExitQuote => {
                quote_bars.pop();
                continue;
            }
            Event::Enter(id, link) => (id, link),
        };
        let node = document
            .get_node(id)
            .expect("finalized flow trees contain valid node identities");
        let inherited_link = match &node.kind {
            NodeKind::Link(link) => Some(link.destination.clone()),
            _ => inherited_link,
        };
        let kind = match node.kind {
            NodeKind::Paragraph => Some(TextBlockKind::Paragraph),
            NodeKind::Heading(_) => Some(TextBlockKind::Heading),
            NodeKind::Preformatted(_) => Some(TextBlockKind::Preformatted),
            NodeKind::Table(_) => Some(TextBlockKind::Table),
            NodeKind::Image(_) | NodeKind::Math(_) => Some(TextBlockKind::Atomic),
            NodeKind::UnsupportedObject(_) if !has_readable_fallback(document, node) => {
                Some(TextBlockKind::Atomic)
            }
            NodeKind::ThematicBreak => Some(TextBlockKind::Rule),
            _ => None,
        };
        if let Some(kind) = kind {
            let marker_level = markers.iter().position(|marker| !marker.emitted);
            let marker_checkboxes = markers
                .iter()
                .filter(|marker| !marker.emitted)
                .filter_map(|marker| marker.checkbox)
                .collect();
            let marker = markers
                .iter_mut()
                .filter(|marker| !marker.emitted)
                .map(|marker| {
                    marker.emitted = true;
                    marker.group.clone()
                })
                .collect::<Vec<_>>();
            blocks.push(TextBlock {
                node: id,
                range: text_range(node.text_range),
                kind,
                inherited_link,
                presentation: Presentation {
                    hanging_columns: markers.iter().map(|marker| marker.column.clone()).collect(),
                    list_depth: lists.len(),
                    quote_bars: quote_bars.clone(),
                    marker: (!marker.is_empty()).then_some(marker),
                    marker_level,
                    marker_checkboxes,
                },
                trailing_action: None,
            });
            continue;
        }

        match &node.kind {
            NodeKind::List(list) => {
                lists.push(ListState {
                    kind: list.kind,
                    next: list.start,
                    reversed: list.reversed,
                    column_samples: list_marker_samples(document, styles, node, *list),
                });
                stack.push(Event::ExitList);
                stack.extend(
                    node.children
                        .iter()
                        .rev()
                        .copied()
                        .map(|child| Event::Enter(child, inherited_link.clone())),
                );
            }
            NodeKind::ListItem(item) => {
                let list = lists
                    .last_mut()
                    .expect("validated list items have a list parent");
                let ordinal = item.ordinal.unwrap_or(list.next);
                list.next = if list.reversed {
                    ordinal.saturating_sub(1)
                } else {
                    ordinal.saturating_add(1)
                };
                let base = match list.kind {
                    crate::model::flow::ListKind::Ordered => format!("{ordinal}."),
                    crate::model::flow::ListKind::Unordered => "•".to_owned(),
                };
                let checkbox = leading_marker_checkbox(document, styles, id);
                let segments = checkbox.map_or_else(
                    || vec![base],
                    |(_, state)| vec![checkbox_glyph(state).to_owned()],
                );
                markers.push(MarkerState {
                    group: MarkerGroup {
                        segments,
                        superscript: false,
                    },
                    column: HangingColumn {
                        samples: list.column_samples.clone(),
                        compact: false,
                    },
                    checkbox: checkbox.map(|(id, _)| id),
                    emitted: false,
                });
                stack.push(Event::ExitMarker);
                stack.extend(
                    node.children
                        .iter()
                        .rev()
                        .copied()
                        .map(|child| Event::Enter(child, inherited_link.clone())),
                );
            }
            NodeKind::Quotation => {
                let quote_depth = u16::try_from(quote_bars.len())
                    .unwrap_or(u16::MAX)
                    .saturating_add(1);
                quote_bars.push((markers.len(), quote_depth, !lists.is_empty()));
                stack.push(Event::ExitQuote);
                stack.extend(
                    node.children
                        .iter()
                        .rev()
                        .copied()
                        .map(|child| Event::Enter(child, inherited_link.clone())),
                );
            }
            NodeKind::Section if is_footnote(node) => {
                let label = note_markers
                    .get(&id)
                    .map_or_else(|| "Note".to_owned(), |note| note.label.clone());
                markers.push(MarkerState {
                    group: MarkerGroup {
                        segments: vec![label.clone()],
                        superscript: true,
                    },
                    column: HangingColumn {
                        samples: vec![label].into(),
                        compact: true,
                    },
                    checkbox: None,
                    emitted: false,
                });
                stack.push(Event::ExitMarker);
                stack.extend(
                    node.children
                        .iter()
                        .rev()
                        .copied()
                        .map(|child| Event::Enter(child, inherited_link.clone())),
                );
            }
            NodeKind::UnsupportedObject(_) => {
                stack.extend(
                    node.children
                        .iter()
                        .rev()
                        .copied()
                        .map(|child| Event::Enter(child, inherited_link.clone())),
                );
            }
            _ => stack.extend(
                node.children
                    .iter()
                    .rev()
                    .copied()
                    .map(|child| Event::Enter(child, inherited_link.clone())),
            ),
        }
    }
    for (definition, note) in note_markers {
        let Some(fallback) = note.first_reference.clone() else {
            continue;
        };
        if let Some(block) = blocks.iter_mut().rev().find(|block| {
            block.node == *definition || document.ancestors(block.node).any(|id| id == *definition)
        }) {
            block.trailing_action = Some(GeneratedAction::NoteReturn {
                definition: *definition,
                fallback,
            });
        }
    }
    blocks
}

fn has_readable_fallback(document: &FlowDocument, node: &crate::model::flow::Node) -> bool {
    node.children.iter().any(|child| {
        document
            .get_node(*child)
            .is_some_and(|child| child.text_range.start != child.text_range.end)
    })
}

fn inherit_flow_link(
    lines: &mut [LineLayout],
    atomic_objects: &mut [AtomicLayout],
    destination: Option<&crate::model::flow::Destination>,
) {
    let Some(destination) = destination else {
        return;
    };
    for run in lines.iter_mut().flat_map(|line| &mut line.runs) {
        run.flow_link.get_or_insert_with(|| destination.clone());
    }
    for object in atomic_objects {
        object.flow_link.get_or_insert_with(|| destination.clone());
    }
}

fn apply_run_flow_links<M: TextMeasurer>(
    runs: &mut Vec<RunLayout>,
    links: &[(TextRange, crate::model::flow::Destination)],
    measurer: &M,
) {
    let mut linked = Vec::new();
    let mut previous_end: Option<f32> = None;
    let mut x = runs.first().map_or(0.0, |run| run.x);
    for run in std::mem::take(runs) {
        if let Some(previous_end) = previous_end {
            x += (run.x - previous_end).max(0.0);
        }
        previous_end = Some(run.x + run.width);
        if run.range.end.saturating_sub(run.range.start) != run.text.len() {
            let mut run = run;
            run.x = x;
            x += run.width;
            linked.push(run);
            continue;
        }
        let mut boundaries = vec![run.range.start, run.range.end];
        for (range, _) in links {
            if range.start < run.range.end && run.range.start < range.end {
                boundaries.push(range.start.max(run.range.start));
                boundaries.push(range.end.min(run.range.end));
            }
        }
        boundaries.sort_unstable();
        boundaries.dedup();
        for endpoints in boundaries.windows(2) {
            let range = TextRange {
                start: endpoints[0],
                end: endpoints[1],
            };
            if range.start == range.end {
                continue;
            }
            let text =
                run.text[range.start - run.range.start..range.end - run.range.start].to_owned();
            let width = measurer.measure(&text, &run.style).width;
            let mut segment = run.clone();
            segment.text = text;
            segment.range = range;
            segment.x = x;
            segment.width = width;
            segment.flow_link = links
                .iter()
                .find(|(link, _)| link.start <= range.start && range.end <= link.end)
                .map(|(_, destination)| destination.clone());
            linked.push(segment);
            x += width;
        }
    }
    *runs = linked;
}

struct FlowLinkMap {
    text: Vec<(TextRange, crate::model::flow::Destination)>,
    atomic: std::collections::HashMap<NodeId, crate::model::flow::Destination>,
}

fn collect_flow_links(
    document: &FlowDocument,
    root: NodeId,
    inherited: Option<crate::model::flow::Destination>,
) -> FlowLinkMap {
    let mut result = FlowLinkMap {
        text: Vec::new(),
        atomic: std::collections::HashMap::new(),
    };
    let mut stack = vec![(root, inherited)];
    while let Some((id, inherited)) = stack.pop() {
        let node = document
            .get_node(id)
            .expect("finalized link traversal retains valid identities");
        let active = match &node.kind {
            NodeKind::Link(link) => Some(link.destination.clone()),
            _ => inherited,
        };
        if let Some(destination) = &active {
            match node.kind {
                NodeKind::Text => result
                    .text
                    .push((text_range(node.text_range), destination.clone())),
                NodeKind::Image(_) | NodeKind::Math(_) | NodeKind::ThematicBreak => {
                    result.atomic.insert(id, destination.clone());
                }
                NodeKind::UnsupportedObject(_) if !has_readable_fallback(document, node) => {
                    result.atomic.insert(id, destination.clone());
                }
                _ => {}
            }
        }
        stack.extend(
            node.children
                .iter()
                .rev()
                .copied()
                .map(|child| (child, active.clone())),
        );
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn push_linked_text_items<M: TextMeasurer>(
    items: &mut Vec<super::LayoutItem>,
    text: &str,
    range: TextRange,
    links: &[(TextRange, crate::model::flow::Destination)],
    link_index: &mut usize,
    style: TextStyle,
    measurer: &M,
    word_breaking: WordBreaking,
    options: &LayoutOptions,
) {
    while links
        .get(*link_index)
        .is_some_and(|(link, _)| link.end <= range.start)
    {
        *link_index += 1;
    }
    let mut cursor = range.start;
    while let Some((link, destination)) = links.get(*link_index) {
        if link.start >= range.end {
            break;
        }
        let linked_start = link.start.max(range.start).max(cursor);
        let linked_end = link.end.min(range.end);
        if cursor < linked_start {
            let first = items.len();
            push_text_items(
                items,
                &text[cursor - range.start..linked_start - range.start],
                TextRange {
                    start: cursor,
                    end: linked_start,
                },
                style.clone(),
                measurer,
                word_breaking,
                options,
            );
            set_flow_link(&mut items[first..], None);
        }
        if linked_start < linked_end {
            let first = items.len();
            push_text_items(
                items,
                &text[linked_start - range.start..linked_end - range.start],
                TextRange {
                    start: linked_start,
                    end: linked_end,
                },
                style.clone(),
                measurer,
                word_breaking,
                options,
            );
            set_flow_link(&mut items[first..], Some(destination));
            cursor = linked_end;
        }
        *link_index += 1;
    }
    if cursor < range.end {
        push_text_items(
            items,
            &text[cursor - range.start..],
            TextRange {
                start: cursor,
                end: range.end,
            },
            style,
            measurer,
            word_breaking,
            options,
        );
    }
}

fn checkbox_glyph(state: crate::model::flow::CheckboxState) -> &'static str {
    match state {
        crate::model::flow::CheckboxState::Unchecked => "☐",
        crate::model::flow::CheckboxState::Checked => "☑",
    }
}

fn leading_marker_checkbox(
    document: &FlowDocument,
    styles: &StyleSnapshot,
    item: NodeId,
) -> Option<(NodeId, crate::model::flow::CheckboxState)> {
    let item = document.get_node(item)?;
    let paragraph = document.get_node(*item.children.first()?)?;
    if !matches!(paragraph.kind, NodeKind::Paragraph) {
        return None;
    }
    for child in &paragraph.children {
        let node = document.get_node(*child)?;
        match node.kind {
            NodeKind::LocationMarker(_) | NodeKind::BreakOpportunity(_) => continue,
            NodeKind::Checkbox(checkbox) => {
                let style = styles
                    .style_for_node(*child)
                    .and_then(|style| styles.get_computed_style(style))?;
                return style.checkbox_marker().then_some((*child, checkbox.state));
            }
            _ => return None,
        }
    }
    None
}

fn list_marker_samples(
    document: &FlowDocument,
    styles: &StyleSnapshot,
    node: &crate::model::flow::Node,
    list: crate::model::flow::List,
) -> Arc<[String]> {
    let mut next = list.start;
    node.children
        .iter()
        .filter_map(|id| {
            let NodeKind::ListItem(item) = document.get_node(*id).map(|node| &node.kind)? else {
                return None;
            };
            let ordinal = item.ordinal.unwrap_or(next);
            next = if list.reversed {
                ordinal.saturating_sub(1)
            } else {
                ordinal.saturating_add(1)
            };
            let base = match list.kind {
                crate::model::flow::ListKind::Ordered => format!("{ordinal}."),
                crate::model::flow::ListKind::Unordered => "•".to_owned(),
            };
            Some(
                leading_marker_checkbox(document, styles, *id)
                    .map_or(base, |(_, state)| checkbox_glyph(state).to_owned()),
            )
        })
        .collect::<Vec<_>>()
        .into()
}

fn is_footnote(node: &crate::model::flow::Node) -> bool {
    node.semantics.roles.iter().any(|role| {
        matches!(
            role,
            crate::model::flow::SemanticRole::Known(
                crate::model::flow::KnownRole::Footnote | crate::model::flow::KnownRole::Endnote
            )
        )
    })
}

fn footnote_markers(
    document: &FlowDocument,
) -> std::collections::HashMap<NodeId, FootnotePresentation> {
    let mut markers = std::collections::HashMap::new();
    for id in document.preorder(document.root()) {
        let Some(node) = document.get_node(id) else {
            continue;
        };
        let NodeKind::Link(link) = &node.kind else {
            continue;
        };
        let crate::model::flow::LinkRelation::NoteReference { return_anchor } = &link.relation
        else {
            continue;
        };
        let crate::model::flow::Destination::Local(target) = &link.destination else {
            continue;
        };
        let Some(definition) = document.anchor(target).map(|anchor| anchor.target.node) else {
            continue;
        };
        if let Some(label) = document.get_text(node.text_range)
            && !label.is_empty()
        {
            markers
                .entry(definition)
                .or_insert_with(|| FootnotePresentation {
                    label: label.to_owned(),
                    first_reference: return_anchor.clone(),
                });
        }
    }
    markers
}

struct MarkerGeometry {
    segments: Vec<(String, super::TextMetrics, f32, TextStyle)>,
}

struct PresentationGeometry {
    content_left: f32,
    marker: Option<MarkerGeometry>,
    quote_bars: Vec<f32>,
}

fn generated_marker_style(options: &LayoutOptions) -> TextStyle {
    TextStyle {
        family: FontFamily::Proportional,
        size: options.body_size,
        weight: FontWeight::Normal,
        italic: false,
        strikethrough: false,
        link: false,
        initial_letter: None,
        baseline_shift: 0.0,
        letter_spacing: 0.0,
        bidi: crate::model::flow::BidiState::default(),
        bidi_boundary: None,
        shape_direction: crate::model::flow::BaseDirection::Auto,
    }
}

fn generated_superscript_style(options: &LayoutOptions) -> TextStyle {
    let mut style = generated_marker_style(options);
    style.size *= options.superscript_scale;
    style.baseline_shift = options.body_size * options.superscript_shift;
    style
}

fn column_key(column: &HangingColumn) -> usize {
    Arc::as_ptr(&column.samples) as *const () as usize
}

fn measure_column_widths<M: TextMeasurer>(
    units: &[TextBlock],
    measurer: &M,
    options: &LayoutOptions,
) -> std::collections::HashMap<usize, f32> {
    let style = generated_marker_style(options);
    let mut widths = std::collections::HashMap::new();
    for column in units
        .iter()
        .flat_map(|unit| &unit.presentation.hanging_columns)
    {
        widths.entry(column_key(column)).or_insert_with(|| {
            let marker_style = if column.compact {
                generated_superscript_style(options)
            } else {
                style.clone()
            };
            let space = measurer.measure(" ", &marker_style).width;
            let width = column
                .samples
                .iter()
                .map(|sample| measurer.measure(sample, &marker_style).width)
                .fold(0.0_f32, f32::max);
            if column.compact {
                width + space
            } else {
                options.indent.max(width + space)
            }
        });
    }
    widths
}

fn presentation_geometry<M: TextMeasurer>(
    presentation: &Presentation,
    column_widths: &std::collections::HashMap<usize, f32>,
    measurer: &M,
    options: &LayoutOptions,
) -> PresentationGeometry {
    let quote_left = options.indent
        * f32::from(u16::try_from(presentation.quote_bars.len()).unwrap_or(u16::MAX));
    let list_outer_inset = if presentation.list_depth == 0 {
        0.0
    } else {
        options.indent
    };
    let marker_style = generated_marker_style(options);
    let columns = presentation
        .hanging_columns
        .iter()
        .map(|column| {
            column_widths
                .get(&column_key(column))
                .copied()
                .unwrap_or(options.indent)
        })
        .collect::<Vec<_>>();
    let hanging_width = columns.iter().sum::<f32>();
    let marker = presentation.marker.as_ref().map(|groups| {
        let start = presentation.marker_level.unwrap_or(columns.len());
        let mut segments = Vec::new();
        for (group_index, group) in groups.iter().enumerate() {
            let style = if group.superscript {
                generated_superscript_style(options)
            } else {
                marker_style.clone()
            };
            let marker_space = measurer.measure(" ", &style).width;
            let mut x = list_outer_inset
                + quote_left
                + columns.iter().take(start + group_index).sum::<f32>();
            for text in &group.segments {
                let metrics = measurer.measure(text, &style);
                segments.push((text.clone(), metrics, x, style.clone()));
                x += metrics.width + marker_space;
            }
        }
        MarkerGeometry { segments }
    });
    let quote_bars = presentation
        .quote_bars
        .iter()
        .map(|(hanging_depth, quote_depth, inside_list)| {
            columns.iter().take(*hanging_depth).sum::<f32>()
                + if *inside_list { list_outer_inset } else { 0.0 }
                + options.indent * f32::from(*quote_depth)
                - options.quote_bar_offset
        })
        .collect();
    PresentationGeometry {
        content_left: list_outer_inset + quote_left + hanging_width,
        marker,
        quote_bars,
    }
}

fn insert_marker(
    lines: &mut [LineLayout],
    marker: Option<MarkerGeometry>,
    range: TextRange,
    options: &LayoutOptions,
) {
    let Some(marker) = marker else {
        return;
    };
    let Some((first, remaining)) = lines.split_first_mut() else {
        return;
    };
    let mut height = 0.0_f32;
    let mut baseline = 0.0_f32;
    for (_, metrics, _, style) in &marker.segments {
        let (segment_height, segment_baseline) =
            line_box(*metrics, style.size, options.line_spacing);
        height = height.max(segment_height);
        baseline = baseline.max(segment_baseline);
    }
    if height > first.height {
        let growth = height - first.height;
        first.height = height;
        for line in remaining {
            line.y += growth;
        }
    }
    first.baseline = first.baseline.max(baseline);
    for (index, (text, metrics, x, style)) in marker.segments.into_iter().enumerate() {
        first.runs.insert(
            index,
            RunLayout {
                text,
                range: TextRange {
                    start: range.start,
                    end: range.start,
                },
                style: style.clone(),
                syntax: SyntaxKind::Plain,
                flow_link: None,
                generated_action: None,
                x,
                width: metrics.width,
            },
        );
    }
}

fn append_generated_action<M: TextMeasurer>(
    block: &mut BlockLayout,
    action: GeneratedAction,
    measurer: &M,
    options: &LayoutOptions,
) {
    let style = generated_superscript_style(options);
    let metrics = measurer.measure("↑", &style);
    let spacing = measurer.measure(" ", &style).width;
    let position = block.range.end;
    if let Some(line) = block.lines.last_mut() {
        let content_end = line
            .runs
            .iter()
            .map(|run| run.x + run.width)
            .fold(0.0_f32, f32::max);
        let x = content_end + spacing;
        if x + metrics.width <= options.width {
            line.runs.push(RunLayout {
                text: "↑".to_owned(),
                range: TextRange {
                    start: position,
                    end: position,
                },
                style,
                syntax: SyntaxKind::Plain,
                flow_link: None,
                generated_action: Some(action),
                x,
                width: metrics.width,
            });
            return;
        }
    }

    let base_style = generated_marker_style(options);
    let line_metrics = measurer.measure("M", &base_style);
    let (height, baseline) = line_box(line_metrics, base_style.size, options.line_spacing);
    let y = block.y + block.height;
    let x = block
        .lines
        .last()
        .and_then(|line| line.runs.first())
        .map_or_else(
            || block.table.as_ref().map_or(block.rule_x, |table| table.x),
            |run| run.x,
        );
    block.lines.push(LineLayout {
        range: TextRange {
            start: position,
            end: position,
        },
        y,
        height,
        baseline,
        runs: vec![RunLayout {
            text: "↑".to_owned(),
            range: TextRange {
                start: position,
                end: position,
            },
            style,
            syntax: SyntaxKind::Plain,
            flow_link: None,
            generated_action: Some(action),
            x,
            width: metrics.width,
        }],
    });
    block.height += height;
}

#[derive(Clone, Debug)]
struct PendingAtomic {
    node: NodeId,
    range: TextRange,
    width: f32,
    height: f32,
    flow_link: Option<crate::model::flow::Destination>,
    kind: AtomicLayoutKind,
}

fn pending_atomic<M: TextMeasurer>(
    document: &FlowDocument,
    node_id: NodeId,
    node: &crate::model::flow::Node,
    style: &TextStyle,
    computed: &crate::model::flow::ComputedStyle,
    measurer: &M,
    available_width: f32,
) -> PendingAtomic {
    let range = text_range(node.text_range);
    let mut flow_link = None;
    let (mut width, mut height, kind) = match &node.kind {
        NodeKind::Image(image) => {
            let resource = document
                .get_resource(image.resource)
                .expect("validated images retain valid resource identities");
            let intrinsic = image
                .intrinsic_size
                .map(|size| (size.width.get(), size.height.get()));
            let metrics = measurer.measure_image_resource(
                &resource.identity,
                document.resource_base(),
                intrinsic,
            );
            let alt = document
                .get_text(node.text_range)
                .unwrap_or_default()
                .to_owned();
            let missing = !metrics.width.is_finite()
                || metrics.width <= 0.0
                || !metrics.height.is_finite()
                || metrics.height <= 0.0;
            let (width, height) = if missing {
                let label_width = measurer.measure(&alt, style).width + style.size;
                (
                    label_width
                        .max(style.size * 3.0)
                        .min(available_width.max(1.0)),
                    style.size * 2.0,
                )
            } else {
                (metrics.width, metrics.height)
            };
            (
                width,
                height,
                AtomicLayoutKind::Image {
                    key: metrics.key,
                    alt,
                },
            )
        }
        NodeKind::Math(math) => {
            let metrics = measurer.measure(&math.source, style);
            (
                (metrics.width + style.size).max(style.size * 2.0),
                (metrics.ascent + metrics.descent + style.size * 0.5).max(style.size),
                AtomicLayoutKind::Placeholder {
                    label: math.source.clone(),
                },
            )
        }
        NodeKind::ThematicBreak => (
            available_width,
            (style.size * 0.25).max(1.0),
            AtomicLayoutKind::ThematicBreak,
        ),
        NodeKind::UnsupportedObject(object) => {
            flow_link = object.activation.clone();
            let label = object
                .title
                .clone()
                .unwrap_or_else(|| format!("unsupported {:?}", object.media_kind).to_lowercase());
            let measured = measurer.measure(&label, style);
            let hinted = object
                .size_hint
                .map(|size| (size.width.get(), size.height.get()));
            (
                hinted.map_or((measured.width + style.size).max(style.size * 3.0), |v| v.0),
                hinted.map_or(style.size * 2.0, |v| v.1),
                AtomicLayoutKind::Placeholder { label },
            )
        }
        _ => unreachable!("only atomic nodes produce atomic layout"),
    };
    if !width.is_finite() || width <= 0.0 || !height.is_finite() || height <= 0.0 {
        width = style.size * 3.0;
        height = style.size * 2.0;
    }
    let intrinsic_ratio = width / height;
    let resolve_dimension = |value: Option<crate::model::flow::CssLength>| {
        value.map(|value| value.resolve(style.size, available_width).max(1.0))
    };
    match (
        resolve_dimension(computed.width()),
        resolve_dimension(computed.height()),
    ) {
        (Some(css_width), Some(css_height)) => {
            width = css_width;
            height = css_height;
        }
        (Some(css_width), None) => {
            width = css_width;
            height = css_width / intrinsic_ratio;
        }
        (None, Some(css_height)) => {
            height = css_height;
            width = css_height * intrinsic_ratio;
        }
        (None, None) => {}
    }
    if let Some(maximum) = resolve_dimension(computed.max_width())
        && width > maximum
    {
        let scale = maximum / width;
        width = maximum;
        height *= scale;
    }
    if let Some(maximum) = resolve_dimension(computed.max_height())
        && height > maximum
    {
        let scale = maximum / height;
        height = maximum;
        width *= scale;
    }
    if let Some(minimum) = resolve_dimension(computed.min_width())
        && width < minimum
    {
        let scale = minimum / width;
        width = minimum;
        height *= scale;
    }
    if let Some(minimum) = resolve_dimension(computed.min_height())
        && height < minimum
    {
        let scale = minimum / height;
        height = minimum;
        width *= scale;
    }
    let scale = (available_width.max(1.0) / width).min(1.0);
    PendingAtomic {
        node: node_id,
        range,
        width: width * scale,
        height: height * scale,
        flow_link,
        kind,
    }
}

fn atomic_is_rtl(bidi: crate::model::flow::BidiState, text: &str) -> bool {
    match bidi.direction {
        crate::model::flow::BaseDirection::LeftToRight => false,
        crate::model::flow::BaseDirection::RightToLeft => true,
        crate::model::flow::BaseDirection::Auto => unicode_bidi::BidiInfo::new(text, None)
            .paragraphs
            .first()
            .is_some_and(|paragraph| paragraph.level.is_rtl()),
    }
}

fn extract_atomic_objects(
    lines: &mut [LineLayout],
    pending: Vec<PendingAtomic>,
) -> Vec<AtomicLayout> {
    let mut buckets = std::collections::HashMap::<
        (usize, usize, u32),
        std::collections::VecDeque<PendingAtomic>,
    >::new();
    for spec in pending {
        buckets
            .entry((spec.range.start, spec.range.end, spec.width.to_bits()))
            .or_default()
            .push_back(spec);
    }
    let mut objects = Vec::new();
    for line in lines {
        let mut index = 0;
        while index < line.runs.len() {
            if !line.runs[index].text.is_empty() {
                index += 1;
                continue;
            }
            let run = line.runs.remove(index);
            let key = (run.range.start, run.range.end, run.width.to_bits());
            let Some(spec) = buckets.get_mut(&key).and_then(|specs| specs.pop_front()) else {
                continue;
            };
            let mut bidi = run.style.bidi;
            bidi.direction = run.style.shape_direction;
            let rtl = matches!(
                bidi.direction,
                crate::model::flow::BaseDirection::RightToLeft
            );
            objects.push(AtomicLayout {
                node: Some(spec.node),
                range: spec.range,
                x: run.x,
                y: line.y + (line.height - spec.height).max(0.0) / 2.0,
                width: spec.width,
                height: spec.height,
                leading_x: if rtl { run.x + spec.width } else { run.x },
                trailing_x: if rtl { run.x } else { run.x + spec.width },
                bidi,
                flow_link: spec.flow_link,
                kind: spec.kind,
            });
        }
    }
    debug_assert!(buckets.values().all(|specs| specs.is_empty()));
    objects
}

#[derive(Clone, Copy)]
struct FlowLayoutContext<'a> {
    options: &'a LayoutOptions,
    force_emergency_breaking: bool,
}

fn collect_text_block_items<M: TextMeasurer>(
    document: &FlowDocument,
    styles: &StyleSnapshot,
    unit: &TextBlock,
    measurer: &M,
    options: &LayoutOptions,
    available_width: f32,
    force_emergency_breaking: bool,
) -> (Vec<super::LayoutItem>, Vec<PendingAtomic>) {
    let mut items = Vec::new();
    let mut pending_atomics = Vec::new();
    let mut stack = vec![(
        unit.node,
        None::<crate::model::flow::BidiState>,
        None::<usize>,
        unit.inherited_link.clone(),
    )];
    while let Some((id, inherited_boundary, inherited_boundary_id, inherited_link)) = stack.pop() {
        let node = document
            .get_node(id)
            .expect("layout traversal starts from a finalized node");
        let computed = styles
            .style_for_node(id)
            .and_then(|style| styles.get_computed_style(style))
            .expect("style snapshots cover every finalized node");
        let establishes_boundary = computed.bidi().mode != crate::model::flow::BidiMode::Normal;
        let boundary = establishes_boundary
            .then_some(computed.bidi())
            .or(inherited_boundary);
        let boundary_id = if establishes_boundary {
            Some(id.layout_index())
        } else {
            inherited_boundary_id
        };
        let inherited_link = match &node.kind {
            NodeKind::Link(link) => Some(link.destination.clone()),
            _ => inherited_link,
        };
        match node.kind {
            NodeKind::Text => {
                let Some(text) = document.get_text(node.text_range) else {
                    continue;
                };
                let mut style = text_style(computed, options, unit.kind);
                if let Some(boundary) = boundary {
                    style.bidi = boundary;
                }
                style.bidi_boundary = boundary_id;
                let first_item = items.len();
                push_text_items(
                    &mut items,
                    text,
                    text_range(node.text_range),
                    style,
                    measurer,
                    if computed.text_breaking() == EffectiveTextBreaking::InlineCode {
                        WordBreaking::InlineCode(available_width)
                    } else if force_emergency_breaking {
                        WordBreaking::Emergency(available_width)
                    } else if matches!(unit.kind, TextBlockKind::Heading) {
                        WordBreaking::Normal
                    } else {
                        WordBreaking::Hyphenate
                    },
                    options,
                );
                set_flow_link(&mut items[first_item..], inherited_link.as_ref());
            }
            NodeKind::HardBreak => {
                terminate_at(&mut items, Break::Line, Some(text_range(node.text_range)))
            }
            NodeKind::BreakOpportunity(opportunity) => {
                let mut style = text_style(computed, options, unit.kind);
                if let Some(boundary) = boundary {
                    style.bidi = boundary;
                }
                style.bidi_boundary = boundary_id;
                let first_item = items.len();
                push_break_opportunity(
                    &mut items,
                    text_range(node.text_range),
                    opportunity.hyphen_when_broken,
                    style,
                    measurer,
                );
                set_flow_link(&mut items[first_item..], inherited_link.as_ref());
            }
            NodeKind::Checkbox(checkbox) => {
                if unit.presentation.marker_checkboxes.contains(&id) {
                    continue;
                }
                let mut style = text_style(computed, options, unit.kind);
                if let Some(boundary) = boundary {
                    style.bidi = boundary;
                }
                style.bidi_boundary = boundary_id;
                push_text_items(
                    &mut items,
                    checkbox_glyph(checkbox.state),
                    text_range(node.text_range),
                    style,
                    measurer,
                    WordBreaking::Normal,
                    options,
                );
            }
            NodeKind::Image(_) | NodeKind::Math(_) => {
                let mut style = text_style(computed, options, unit.kind);
                if let Some(boundary) = boundary {
                    style.bidi = boundary;
                }
                style.bidi_boundary = boundary_id;
                let mut atomic = pending_atomic(
                    document,
                    id,
                    node,
                    &style,
                    computed,
                    measurer,
                    available_width,
                );
                if inherited_link.is_some() {
                    atomic.flow_link = inherited_link.clone();
                }
                push_atomic_item(&mut items, atomic.range, style, atomic.width, atomic.height);
                let last = items.len().saturating_sub(1);
                set_flow_link(&mut items[last..], inherited_link.as_ref());
                pending_atomics.push(atomic);
            }
            NodeKind::UnsupportedObject(_) if !has_readable_fallback(document, node) => {
                let mut style = text_style(computed, options, unit.kind);
                if let Some(boundary) = boundary {
                    style.bidi = boundary;
                }
                style.bidi_boundary = boundary_id;
                let mut atomic = pending_atomic(
                    document,
                    id,
                    node,
                    &style,
                    computed,
                    measurer,
                    available_width,
                );
                if inherited_link.is_some() {
                    atomic.flow_link = inherited_link.clone();
                }
                push_atomic_item(&mut items, atomic.range, style, atomic.width, atomic.height);
                let last = items.len().saturating_sub(1);
                set_flow_link(&mut items[last..], inherited_link.as_ref());
                pending_atomics.push(atomic);
            }
            _ => stack.extend(
                node.children
                    .iter()
                    .rev()
                    .copied()
                    .map(|child| (child, boundary, boundary_id, inherited_link.clone())),
            ),
        }
    }
    (items, pending_atomics)
}

#[derive(Clone, Copy)]
struct ResolvedAuthorBox {
    padding: [f32; 4],
    borders: [BorderLayout; 4],
    content_width: f32,
    minimum_height: f32,
}

impl ResolvedAuthorBox {
    fn horizontal(self) -> f32 {
        self.padding[1] + self.padding[3] + self.borders[1].width + self.borders[3].width
    }

    fn vertical(self) -> f32 {
        self.padding[0] + self.padding[2] + self.borders[0].width + self.borders[2].width
    }

    fn content_x(self, x: f32) -> f32 {
        x + self.borders[3].width + self.padding[3]
    }

    fn content_y(self, y: f32) -> f32 {
        y + self.borders[0].width + self.padding[0]
    }

    fn layout(self, x: f32) -> Option<BlockBoxLayout> {
        let visible = self.borders.iter().any(|border| border.width > 0.0);
        (visible || self.padding.iter().any(|padding| *padding > 0.0)).then_some(BlockBoxLayout {
            x,
            width: self.content_width + self.horizontal(),
            borders: self.borders,
        })
    }
}

fn resolve_author_box(
    computed: &crate::model::flow::ComputedStyle,
    em: f32,
    percentage_basis: f32,
    available_outer_width: f32,
) -> ResolvedAuthorBox {
    let resolve = |value: Option<crate::model::flow::CssLength>| {
        value
            .map_or(0.0, |value| value.resolve(em, percentage_basis))
            .max(0.0)
    };
    let padding = [
        resolve(computed.padding_top()),
        resolve(computed.padding_right()),
        resolve(computed.padding_bottom()),
        resolve(computed.padding_left()),
    ];
    let borders = computed.borders().map(|border| {
        border.map_or(
            BorderLayout {
                width: 0.0,
                color: None,
            },
            |border| BorderLayout {
                width: border.width.resolve(em, percentage_basis).max(0.0),
                color: border
                    .color
                    .map(|color| [color.red, color.green, color.blue]),
            },
        )
    });
    let horizontal = padding[1] + padding[3] + borders[1].width + borders[3].width;
    let available_content = (available_outer_width - horizontal).max(1.0);
    let dimension = |value: Option<crate::model::flow::CssLength>| {
        value.map(|value| value.resolve(em, percentage_basis).max(0.0))
    };
    let mut content_width = dimension(computed.width()).unwrap_or(available_content);
    if let Some(maximum) = dimension(computed.max_width()) {
        content_width = content_width.min(maximum);
    }
    if let Some(minimum) = dimension(computed.min_width()) {
        content_width = content_width.max(minimum);
    }
    // Native layout favors readable reflow over author-requested horizontal
    // overflow. Intrinsic table overflow remains governed by its own policy.
    content_width = content_width.min(available_content).max(1.0);
    let minimum_height = dimension(computed.height())
        .into_iter()
        .chain(dimension(computed.min_height()))
        .fold(0.0_f32, f32::max);
    ResolvedAuthorBox {
        padding,
        borders,
        content_width,
        minimum_height,
    }
}

fn layout_text_block<M: TextMeasurer>(
    document: &FlowDocument,
    styles: &StyleSnapshot,
    unit: TextBlock,
    column_widths: &std::collections::HashMap<usize, f32>,
    measurer: &M,
    y: f32,
    context: FlowLayoutContext<'_>,
) -> BlockLayout {
    let base_options = context.options;
    let computed = styles
        .style_for_node(unit.node)
        .and_then(|style| styles.get_computed_style(style))
        .expect("style snapshots cover every finalized node");
    let mut adjusted_options;
    let options = if let Some(line_height) = computed.line_height_per_mille() {
        adjusted_options = base_options.clone();
        adjusted_options.line_spacing = line_height as f32 / 1_000.0;
        &adjusted_options
    } else {
        base_options
    };
    let block_style = text_style(computed, options, unit.kind);
    let mut geometry = presentation_geometry(&unit.presentation, column_widths, measurer, options);
    let presentation_left = geometry.content_left;
    let mut margin_left = computed.margin_left().map_or(0.0, |margin| {
        margin.resolve(block_style.size, options.width)
    });
    let mut margin_right = computed.margin_right().map_or(0.0, |margin| {
        margin.resolve(block_style.size, options.width)
    });
    geometry.content_left += margin_left;
    let mut box_x = geometry.content_left;
    let mut author_box = resolve_author_box(
        computed,
        block_style.size,
        options.width,
        (options.width - box_x - margin_right).max(1.0),
    );
    geometry.content_left = author_box.content_x(box_x);
    let mut available_width = author_box.content_width;
    let mut content_y = author_box.content_y(y);
    let (mut items, mut pending_atomics) = collect_text_block_items(
        document,
        styles,
        &unit,
        measurer,
        options,
        available_width,
        context.force_emergency_breaking,
    );
    let longest_unbreakable = items
        .iter()
        .filter_map(|item| match item {
            Item::Box { width, .. } => Some(*width),
            _ => None,
        })
        .fold(0.0_f32, f32::max);
    let mut required = (longest_unbreakable - available_width).max(0.0);
    if required > 0.01 && computed.width().is_none() {
        let from_left = required.min(margin_left.max(0.0));
        margin_left -= from_left;
        required -= from_left;
        let from_right = required.min(margin_right.max(0.0));
        margin_right -= from_right;
        if from_left > 0.0 || from_right > 0.0 {
            geometry.content_left = presentation_left + margin_left;
            box_x = geometry.content_left;
            author_box = resolve_author_box(
                computed,
                block_style.size,
                options.width,
                (options.width - box_x - margin_right).max(1.0),
            );
            geometry.content_left = author_box.content_x(box_x);
            available_width = author_box.content_width;
            content_y = author_box.content_y(y);
            (items, pending_atomics) = collect_text_block_items(
                document,
                styles,
                &unit,
                measurer,
                options,
                available_width,
                context.force_emergency_breaking,
            );
        }
    }
    let text_indent = computed.text_indent().map_or(0.0, |indent| {
        indent.resolve(block_style.size, available_width)
    });
    if text_indent > 0.0 {
        let mut indent = make_atom(
            "",
            TextRange {
                start: unit.range.start,
                end: unit.range.start,
            },
            block_style.clone(),
            measurer,
        );
        indent.metrics.width = text_indent;
        items.insert(
            0,
            Item::Box {
                width: text_indent,
                data: indent,
            },
        );
    }
    terminate(&mut items, Break::Paragraph);
    let alignment = computed.text_alignment();
    let justify = alignment.is_none()
        || matches!(alignment, Some(crate::model::flow::TextAlignment::Justify));
    let first_content = items
        .iter()
        .position(|item| matches!(item, Item::Box { data, .. } if !data.text.is_empty()));
    if let (Some(index), Some(first_letter)) = (first_content, computed.first_letter()) {
        split_first_letter_item(&mut items, index, first_letter, measurer);
    }
    let initial = first_content.filter(
        |index| matches!(&items[*index], Item::Box { data, .. } if !data.atomic && data.style.initial_letter.is_some()),
    );
    let initial_sink = initial
        .and_then(|index| match &items[index] {
            Item::Box { data, .. } => data.style.initial_letter.map(|value| value.sink),
            _ => None,
        })
        .unwrap_or(0);
    let mut lines = if let Some(index) = initial {
        let Item::Box { data: cap, .. } = items.remove(index) else {
            unreachable!()
        };
        materialize_initial_letter_lines(
            items,
            cap,
            available_width,
            geometry.content_left,
            content_y,
            &block_style,
            text_indent.max(0.0),
            measurer,
            options,
            justify,
        )
    } else {
        let breaks = line_breaks(&items, available_width);
        materialize_lines(
            items,
            breaks,
            geometry.content_left,
            content_y,
            options,
            justify,
        )
    };
    let mut empty_line_position = unit.range.start;
    for line in &mut lines {
        if line.runs.is_empty() && line.range == TextRange::default() {
            line.range = TextRange {
                start: empty_line_position,
                end: empty_line_position,
            };
        }
        empty_line_position = empty_line_position.max(line.range.end);
    }
    if text_indent < 0.0
        && let Some(first) = lines.first_mut()
    {
        for run in &mut first.runs {
            run.x += text_indent;
        }
    }
    if let Some(alignment) = alignment {
        for line in &mut lines {
            let used = line
                .runs
                .iter()
                .map(|run| run.x + run.width - geometry.content_left)
                .fold(0.0_f32, f32::max);
            let remaining = (available_width - used).max(0.0);
            let shift = match alignment {
                crate::model::flow::TextAlignment::Left
                | crate::model::flow::TextAlignment::Justify => 0.0,
                crate::model::flow::TextAlignment::Center => remaining / 2.0,
                crate::model::flow::TextAlignment::Right => remaining,
            };
            for run in &mut line.runs {
                run.x += shift;
            }
        }
    }
    if lines.is_empty() {
        let metrics = measurer.measure("M", &block_style);
        let (height, baseline) = line_box(metrics, block_style.size, options.line_spacing);
        lines.push(LineLayout {
            range: unit.range,
            y: content_y,
            height,
            baseline,
            runs: Vec::new(),
        });
    }
    insert_marker(&mut lines, geometry.marker, unit.range, options);
    let mut atomic_objects = extract_atomic_objects(&mut lines, pending_atomics);
    inherit_flow_link(
        &mut lines,
        &mut atomic_objects,
        unit.inherited_link.as_ref(),
    );
    let content_height: f32 = lines.iter().map(|line| line.height).sum();
    let height = (content_height + author_box.vertical()).max(author_box.minimum_height);
    BlockLayout {
        node: Some(unit.node),
        kind: if matches!(unit.kind, TextBlockKind::Heading) {
            LayoutBlockKind::Heading
        } else {
            LayoutBlockKind::Paragraph
        },
        range: unit.range,
        y,
        height,
        lines,
        atomic_objects,
        table: None,
        rule: false,
        rule_x: 0.0,
        rule_width: 0.0,
        quoted: !unit.presentation.quote_bars.is_empty(),
        quote_bars: geometry.quote_bars,
        box_layout: author_box.layout(box_x),
        break_before: computed.break_before(),
        break_after: computed.break_after(),
        break_inside_avoid: computed.break_inside_avoid(),
        widows: computed.widows(),
        orphans: computed
            .orphans()
            .max(initial_sink.min(u16::from(u8::MAX)) as u8),
    }
}

fn split_first_letter_item<M: TextMeasurer>(
    items: &mut Vec<super::LayoutItem>,
    index: usize,
    initial: crate::model::flow::InitialLetter,
    measurer: &M,
) {
    let Item::Box { data, .. } = &items[index] else {
        return;
    };
    if data.atomic || data.style.initial_letter.is_some() || data.text.is_empty() {
        return;
    }
    let original = data.clone();
    let mut initial_end = 0;
    let mut initial_has_letter = false;
    for (offset, grapheme) in original.text.grapheme_indices(true) {
        initial_end = offset + grapheme.len();
        if grapheme.chars().any(char::is_alphanumeric) {
            initial_has_letter = true;
            break;
        }
    }
    if initial_end == 0 {
        return;
    }

    let mut cap_text = original.text[..initial_end].to_owned();
    let mut cap_end = original.range.start.saturating_add(initial_end);
    let mut consumed_through = index;
    let mut trailing: Option<Atom> = None;
    if initial_has_letter {
        if initial_end < original.text.len() {
            let mut remainder = make_atom(
                &original.text[initial_end..],
                TextRange {
                    start: cap_end,
                    end: original.range.end,
                },
                original.style.clone(),
                measurer,
            );
            remainder.flow_link = original.flow_link.clone();
            trailing = Some(remainder);
        }
    } else if let Some((next_index, next)) =
        items
            .iter()
            .enumerate()
            .skip(index + 1)
            .find_map(|(next_index, item)| match item {
                Item::Box { data, .. } if !data.atomic && !data.text.is_empty() => {
                    Some((next_index, data.clone()))
                }
                _ => None,
            })
    {
        let next_end = next
            .text
            .grapheme_indices(true)
            .find_map(|(offset, grapheme)| {
                grapheme
                    .chars()
                    .any(char::is_alphanumeric)
                    .then_some(offset + grapheme.len())
            })
            .unwrap_or_else(|| next.text.len());
        cap_text.push_str(&next.text[..next_end]);
        cap_end = next.range.start.saturating_add(next_end);
        consumed_through = next_index;
        if next_end < next.text.len() {
            let mut remainder = make_atom(
                &next.text[next_end..],
                TextRange {
                    start: cap_end,
                    end: next.range.end,
                },
                next.style,
                measurer,
            );
            remainder.flow_link = next.flow_link;
            trailing = Some(remainder);
        }
    }
    if cap_end > original.range.end && consumed_through == index {
        return;
    }

    let mut cap_style = original.style;
    cap_style.initial_letter = Some(initial);
    let mut cap = make_atom(
        &cap_text,
        TextRange {
            start: original.range.start,
            end: cap_end,
        },
        cap_style,
        measurer,
    );
    cap.flow_link = original.flow_link;
    items[index] = Item::Box {
        width: cap.metrics.width,
        data: cap,
    };
    if consumed_through > index {
        items.drain(index + 1..=consumed_through);
    }
    if let Some(remainder) = trailing {
        items.insert(
            index + 1,
            Item::Box {
                width: remainder.metrics.width,
                data: remainder,
            },
        );
    }
}

fn materialize_initial_letter_lines<M: TextMeasurer>(
    items: Vec<super::LayoutItem>,
    mut cap: Atom,
    available_width: f32,
    left: f32,
    top: f32,
    block_style: &TextStyle,
    origin_indent: f32,
    measurer: &M,
    options: &LayoutOptions,
    justify: bool,
) -> Vec<LineLayout> {
    let initial = cap.style.initial_letter.expect("initial letter marker");
    let normal_metrics = measurer.measure("M", block_style);
    let (normal_line_height, _) = line_box(normal_metrics, block_style.size, options.line_spacing);
    if !initial.legacy_float {
        let target_height = (initial.size_per_mille as f32 / 1_000.0 - 1.0) * normal_line_height
            + normal_metrics.ascent;
        if cap.metrics.ascent > 0.0 {
            cap.style.size *= target_height.max(normal_metrics.ascent) / cap.metrics.ascent;
            cap.metrics = measurer.measure(&cap.text, &cap.style);
        }
    }
    let margin_left = initial
        .margin_left
        .map_or(0.0, |value| value.resolve(cap.style.size, available_width));
    let margin_right = initial.margin_right.map_or_else(
        || {
            if initial.legacy_float {
                // Legacy Kindle drop-cap recipes commonly omit an inline-end
                // margin. Keep a small reader gap so following text does not
                // visually collide with the enlarged glyph.
                cap.style.size * 0.08
            } else {
                0.0
            }
        },
        |value| value.resolve(cap.style.size, available_width),
    );
    let margin_top = initial
        .margin_top
        .map_or(0.0, |value| value.resolve(cap.style.size, available_width));
    let margin_bottom = initial
        .margin_bottom
        .map_or(0.0, |value| value.resolve(cap.style.size, available_width));
    let vertical_extra = ((margin_top.max(0.0) + margin_bottom.max(0.0))
        / normal_line_height.max(1.0))
    .ceil() as usize;
    let sink = initial_letter_sink(initial, margin_bottom, normal_line_height);
    let requested_lines = sink.saturating_add(vertical_extra);
    let inset = (cap.metrics.width + margin_left + margin_right)
        .max(0.0)
        .min(available_width * 0.75);
    let narrow_width = (available_width - inset).max(options.body_size * 8.0);
    let breaks = super::line_breaks_with_widths(
        &items,
        InitialLetterLineWidths {
            narrow: narrow_width,
            full: available_width,
            affected_lines: requested_lines,
        },
    );
    let mut lines = materialize_lines(items, breaks, left, top, options, justify);
    if lines.is_empty() {
        return lines;
    }

    let wrapped_lines = requested_lines.min(lines.len());
    for line in &mut lines[..wrapped_lines] {
        for run in &mut line.runs {
            run.x += inset;
        }
    }

    let target = sink.saturating_sub(1).min(lines.len() - 1);
    let first_baseline = lines[0].y + lines[0].baseline;
    let target_baseline = lines[target].y + lines[target].baseline;
    let baseline_margin_top = if initial.legacy_float {
        // Legacy floated-cap recipes use a negative top margin to compensate
        // for the enlarged inline box's font leading. The initial-letter model
        // already aligns using measured ink metrics, so applying that negative
        // margin again raises the glyph above its intended sink baseline.
        margin_top.max(0.0)
    } else {
        margin_top
    };
    cap.style.baseline_shift += first_baseline - (target_baseline + baseline_margin_top);
    cap.style.initial_letter = None;
    lines[0].range.start = lines[0].range.start.min(cap.range.start);
    lines[0].range.end = lines[0].range.end.max(cap.range.end);
    lines[0].runs.insert(
        0,
        RunLayout {
            text: cap.text,
            range: cap.range,
            style: cap.style,
            syntax: SyntaxKind::Plain,
            flow_link: cap.flow_link,
            generated_action: None,
            x: left + origin_indent + margin_left,
            width: cap.metrics.width,
        },
    );
    lines
}

#[derive(Clone, Copy)]
struct InitialLetterLineWidths {
    narrow: f32,
    full: f32,
    affected_lines: usize,
}

impl LineWidths for InitialLetterLineWidths {
    fn line_width(&self, line: usize) -> f32 {
        if line < self.affected_lines {
            self.narrow
        } else {
            self.full
        }
    }

    fn uniform_at(&self) -> Option<usize> {
        Some(self.affected_lines)
    }
}

fn initial_letter_sink(
    initial: crate::model::flow::InitialLetter,
    margin_bottom: f32,
    normal_line_height: f32,
) -> usize {
    let sink = usize::from(initial.sink.max(1));
    if !initial.legacy_float || margin_bottom >= 0.0 {
        return sink;
    }
    // A negative bottom margin in the legacy floated-cap recipe shortens the
    // float's exclusion box. Account for that authored adjustment rather than
    // wrapping an otherwise empty extra line beside the glyph.
    (sink as f32 + margin_bottom / normal_line_height.max(1.0))
        .round()
        .max(1.0) as usize
}

fn layout_atomic_block<M: TextMeasurer>(
    document: &FlowDocument,
    styles: &StyleSnapshot,
    unit: TextBlock,
    column_widths: &std::collections::HashMap<usize, f32>,
    measurer: &M,
    options: &LayoutOptions,
    y: f32,
) -> BlockLayout {
    let mut block = layout_text_block(
        document,
        styles,
        unit,
        column_widths,
        measurer,
        y,
        FlowLayoutContext {
            options,
            force_emergency_breaking: false,
        },
    );
    block.kind = LayoutBlockKind::Image;
    block
}

fn split_preformatted_bidi_runs<M: TextMeasurer>(
    runs: Vec<RunLayout>,
    measurer: &M,
) -> Vec<RunLayout> {
    let mut result = Vec::new();
    for run in runs {
        if run.text.is_empty()
            || matches!(
                run.style.bidi.mode,
                crate::model::flow::BidiMode::Override
                    | crate::model::flow::BidiMode::IsolateOverride
            )
        {
            result.push(run);
            continue;
        }
        let base_level = match run.style.bidi.direction {
            crate::model::flow::BaseDirection::LeftToRight => Some(unicode_bidi::Level::ltr()),
            crate::model::flow::BaseDirection::RightToLeft => Some(unicode_bidi::Level::rtl()),
            crate::model::flow::BaseDirection::Auto => None,
        };
        let bidi = unicode_bidi::BidiInfo::new(&run.text, base_level);
        let mut start = 0;
        let mut x = run.x;
        while start < run.text.len() {
            let rtl = bidi.levels[start].is_rtl();
            let mut end = start + run.text[start..].chars().next().unwrap().len_utf8();
            while end < run.text.len() && bidi.levels[end].is_rtl() == rtl {
                end += run.text[end..].chars().next().unwrap().len_utf8();
            }
            let mut segment = run.clone();
            segment.text = run.text[start..end].to_owned();
            segment.range = TextRange {
                start: run.range.start + start,
                end: run.range.start + end,
            };
            segment.style.shape_direction = if rtl {
                crate::model::flow::BaseDirection::RightToLeft
            } else {
                crate::model::flow::BaseDirection::LeftToRight
            };
            segment.x = x;
            segment.width = measurer.measure(&segment.text, &segment.style).width;
            x += segment.width;
            result.push(segment);
            start = end;
        }
    }
    result
}

fn layout_preformatted<M: TextMeasurer>(
    document: &FlowDocument,
    styles: &StyleSnapshot,
    unit: TextBlock,
    column_widths: &std::collections::HashMap<usize, f32>,
    measurer: &M,
    options: &LayoutOptions,
    y: f32,
) -> BlockLayout {
    let node = document
        .get_node(unit.node)
        .expect("layout units retain valid flow nodes");
    let NodeKind::Preformatted(preformatted) = &node.kind else {
        unreachable!("preformatted layout units retain their semantic kind");
    };
    let text = document.get_text(node.text_range).unwrap_or_default();
    let computed = styles
        .style_for_node(unit.node)
        .and_then(|style| styles.get_computed_style(style))
        .expect("style snapshots cover every finalized node");
    let style = text_style(computed, options, TextBlockKind::Preformatted);
    let mut geometry = presentation_geometry(&unit.presentation, column_widths, measurer, options);
    let margin_left = computed
        .margin_left()
        .map_or(0.0, |margin| margin.resolve(style.size, options.width));
    let margin_right = computed
        .margin_right()
        .map_or(0.0, |margin| margin.resolve(style.size, options.width));
    geometry.content_left += margin_left;
    let box_x = geometry.content_left;
    let author_box = resolve_author_box(
        computed,
        style.size,
        options.width,
        (options.width - box_x - margin_right).max(1.0),
    );
    geometry.content_left = author_box.content_x(box_x);
    let highlighted = preformatted
        .language
        .as_deref()
        .map(|language| highlight_code(Some(language), text));
    let links = collect_flow_links(document, unit.node, unit.inherited_link.clone());
    let mut cursor = 0;
    let mut cursor_y = author_box.content_y(y);
    let mut lines = Vec::new();

    for (line_index, line) in text.split('\n').enumerate() {
        let has_newline = cursor + line.len() < text.len();
        let range = TextRange {
            start: unit.range.start + cursor,
            end: unit.range.start + cursor + line.len() + usize::from(has_newline),
        };
        let metrics = measurer.measure(if line.is_empty() { " " } else { line }, &style);
        let (height, baseline) = line_box(metrics, style.size, options.line_spacing);
        let code_indent = if unit.presentation.hanging_columns.is_empty() {
            options.code_block_indent
        } else {
            options.nested_code_block_indent
        };
        let mut x = geometry.content_left + options.indent * code_indent;
        let mut runs = Vec::new();
        let spans = highlighted
            .as_ref()
            .and_then(|lines| lines.get(line_index))
            .map(Vec::as_slice)
            .unwrap_or_default();
        if spans.is_empty() && !line.is_empty() {
            let width = measurer.measure(line, &style).width;
            runs.push(RunLayout {
                text: line.to_owned(),
                range: TextRange {
                    start: range.start,
                    end: range.start + line.len(),
                },
                style: style.clone(),
                syntax: SyntaxKind::Plain,
                flow_link: None,
                generated_action: None,
                x,
                width,
            });
        } else {
            for span in spans {
                let Some(segment) = line.get(span.start..span.end) else {
                    continue;
                };
                let width = measurer.measure(segment, &style).width;
                runs.push(RunLayout {
                    text: segment.to_owned(),
                    range: TextRange {
                        start: range.start + span.start,
                        end: range.start + span.end,
                    },
                    style: style.clone(),
                    syntax: span.kind,
                    flow_link: None,
                    generated_action: None,
                    x,
                    width,
                });
                x += width;
            }
        }
        apply_run_flow_links(&mut runs, &links.text, measurer);
        let mut runs = split_preformatted_bidi_runs(runs, measurer);
        super::reorder_bidi_runs(&mut runs);
        lines.push(LineLayout {
            range,
            y: cursor_y,
            height,
            baseline,
            runs,
        });
        cursor_y += height;
        cursor += line.len() + usize::from(has_newline);
    }

    insert_marker(&mut lines, geometry.marker, unit.range, options);
    inherit_flow_link(&mut lines, &mut [], unit.inherited_link.as_ref());
    let content_height: f32 = lines.iter().map(|line| line.height).sum();
    let height = (content_height + author_box.vertical()).max(author_box.minimum_height);
    BlockLayout {
        node: Some(unit.node),
        kind: LayoutBlockKind::Code,
        range: unit.range,
        y,
        height,
        lines,
        atomic_objects: Vec::new(),
        table: None,
        rule: false,
        rule_x: 0.0,
        rule_width: 0.0,
        quoted: !unit.presentation.quote_bars.is_empty(),
        quote_bars: geometry.quote_bars,
        box_layout: author_box.layout(box_x),
        break_before: computed.break_before(),
        break_after: computed.break_after(),
        break_inside_avoid: computed.break_inside_avoid(),
        widows: computed.widows(),
        orphans: computed.orphans(),
    }
}

fn combine_presentation(outer: &Presentation, inner: &Presentation) -> Presentation {
    let mut hanging_columns = outer.hanging_columns.clone();
    hanging_columns.extend(inner.hanging_columns.iter().cloned());
    let list_depth = outer.list_depth.saturating_add(inner.list_depth);
    let mut quote_bars = outer.quote_bars.clone();
    quote_bars.extend(
        inner
            .quote_bars
            .iter()
            .map(|(hanging, depth, inside_list)| {
                (
                    hanging.saturating_add(outer.hanging_columns.len()),
                    depth.saturating_add(u16::try_from(outer.quote_bars.len()).unwrap_or(u16::MAX)),
                    *inside_list || outer.list_depth > 0,
                )
            }),
    );
    let marker_level = if outer.marker.is_some() {
        outer.marker_level
    } else {
        inner
            .marker_level
            .map(|level| level.saturating_add(outer.hanging_columns.len()))
    };
    let marker = match (&outer.marker, &inner.marker) {
        (Some(outer), Some(inner)) => {
            let mut groups = outer.clone();
            groups.extend(inner.iter().cloned());
            Some(groups)
        }
        (Some(marker), None) | (None, Some(marker)) => Some(marker.clone()),
        (None, None) => None,
    };
    let mut marker_checkboxes = outer.marker_checkboxes.clone();
    marker_checkboxes.extend(inner.marker_checkboxes.iter().copied());
    Presentation {
        hanging_columns,
        list_depth,
        quote_bars,
        marker,
        marker_level,
        marker_checkboxes,
    }
}

fn layout_nested_flow<M: TextMeasurer>(
    document: &FlowDocument,
    styles: &StyleSnapshot,
    root: NodeId,
    outer: &Presentation,
    note_markers: &std::collections::HashMap<NodeId, FootnotePresentation>,
    measurer: &M,
    context: FlowLayoutContext<'_>,
) -> Vec<BlockLayout> {
    let options = context.options;
    let mut units = text_blocks_from(document, styles, root, note_markers);
    let mut outer_context = outer.clone();
    for unit in &mut units {
        unit.presentation = combine_presentation(&outer_context, &unit.presentation);
        outer_context.marker = None;
        outer_context.marker_level = None;
        outer_context.marker_checkboxes.clear();
    }
    let column_widths = measure_column_widths(&units, measurer, options);
    let mut blocks = Vec::new();
    let mut y = 0.0;
    for unit in units {
        let trailing_action = unit.trailing_action.clone();
        let mut block = match unit.kind {
            TextBlockKind::Paragraph | TextBlockKind::Heading => {
                layout_text_block(document, styles, unit, &column_widths, measurer, y, context)
            }
            TextBlockKind::Preformatted => {
                layout_preformatted(document, styles, unit, &column_widths, measurer, options, y)
            }
            TextBlockKind::Atomic => {
                layout_atomic_block(document, styles, unit, &column_widths, measurer, options, y)
            }
            TextBlockKind::Rule => layout_rule(styles, unit, &column_widths, measurer, options, y),
            TextBlockKind::Table => layout_nested_table_fallback(
                document,
                styles,
                unit,
                &column_widths,
                measurer,
                options,
                y,
            ),
        };
        if let Some(action) = trailing_action {
            append_generated_action(&mut block, action, measurer, options);
        }
        block.y = y;
        y += block.height + options.block_spacing;
        blocks.push(block);
    }
    blocks
}

fn atomic_descendants(document: &FlowDocument, root: NodeId) -> Vec<NodeId> {
    let mut result = Vec::new();
    let mut stack = document.get_node(root).map_or_else(Vec::new, |node| {
        node.children.iter().rev().copied().collect()
    });
    while let Some(id) = stack.pop() {
        let node = document
            .get_node(id)
            .expect("finalized descendants retain valid identities");
        match node.kind {
            NodeKind::Image(_) | NodeKind::Math(_) | NodeKind::ThematicBreak => result.push(id),
            NodeKind::UnsupportedObject(_) if !has_readable_fallback(document, node) => {
                result.push(id);
            }
            _ => stack.extend(node.children.iter().rev().copied()),
        }
    }
    result
}

fn collect_nested_table_fallback_items<M: TextMeasurer>(
    document: &FlowDocument,
    styles: &StyleSnapshot,
    unit: &TextBlock,
    style: &TextStyle,
    measurer: &M,
    options: &LayoutOptions,
    available: f32,
) -> (Vec<super::LayoutItem>, Vec<PendingAtomic>) {
    let node = document
        .get_node(unit.node)
        .expect("nested table layout units retain valid identities");
    let mut items = Vec::new();
    let mut pending_atomics = Vec::new();
    let links = collect_flow_links(document, unit.node, unit.inherited_link.clone());
    let mut link_index = 0;
    if let Some(text) = document.get_text(node.text_range) {
        let range_start = node.text_range.start.as_usize();
        let mut cursor = 0;
        for atomic_id in atomic_descendants(document, unit.node) {
            let atomic_node = document
                .get_node(atomic_id)
                .expect("atomic descendants retain valid identities");
            let start = atomic_node
                .text_range
                .start
                .as_usize()
                .saturating_sub(range_start)
                .min(text.len());
            let end = atomic_node
                .text_range
                .end
                .as_usize()
                .saturating_sub(range_start)
                .min(text.len());
            if cursor < start {
                push_linked_text_items(
                    &mut items,
                    &text[cursor..start],
                    TextRange {
                        start: unit.range.start + cursor,
                        end: unit.range.start + start,
                    },
                    &links.text,
                    &mut link_index,
                    style.clone(),
                    measurer,
                    WordBreaking::Emergency(available),
                    options,
                );
            }
            let computed = styles
                .style_for_node(atomic_id)
                .and_then(|id| styles.get_computed_style(id))
                .expect("style snapshots cover nested atomic nodes");
            let atomic_style = text_style(computed, options, TextBlockKind::Paragraph);
            let mut atomic = pending_atomic(
                document,
                atomic_id,
                atomic_node,
                &atomic_style,
                computed,
                measurer,
                available,
            );
            if let Some(destination) = links.atomic.get(&atomic_id) {
                atomic.flow_link = Some(destination.clone());
            }
            push_atomic_item(
                &mut items,
                atomic.range,
                atomic_style,
                atomic.width,
                atomic.height,
            );
            pending_atomics.push(atomic);
            cursor = cursor.max(end);
        }
        if cursor < text.len() {
            push_linked_text_items(
                &mut items,
                &text[cursor..],
                TextRange {
                    start: unit.range.start + cursor,
                    end: unit.range.end,
                },
                &links.text,
                &mut link_index,
                style.clone(),
                measurer,
                WordBreaking::Emergency(available),
                options,
            );
        }
    }
    (items, pending_atomics)
}

fn layout_nested_table_fallback<M: TextMeasurer>(
    document: &FlowDocument,
    styles: &StyleSnapshot,
    unit: TextBlock,
    column_widths: &std::collections::HashMap<usize, f32>,
    measurer: &M,
    options: &LayoutOptions,
    y: f32,
) -> BlockLayout {
    let computed = styles
        .style_for_node(unit.node)
        .and_then(|id| styles.get_computed_style(id))
        .expect("style snapshots cover nested tables");
    let style = text_style(computed, options, TextBlockKind::Paragraph);
    let geometry = presentation_geometry(&unit.presentation, column_widths, measurer, options);
    let available = (options.width - geometry.content_left).max(1.0);
    let (mut items, pending_atomics) = collect_nested_table_fallback_items(
        document, styles, &unit, &style, measurer, options, available,
    );
    terminate(&mut items, Break::Paragraph);
    let breaks = line_breaks(&items, available);
    let mut lines = materialize_lines(items, breaks, geometry.content_left, y, options, false);
    if lines.is_empty() || lines.iter().all(|line| line.runs.is_empty()) {
        let metrics = measurer.measure("M", &style);
        let (height, baseline) = line_box(metrics, style.size, options.line_spacing);
        lines.clear();
        lines.push(LineLayout {
            range: unit.range,
            y,
            height,
            baseline,
            runs: Vec::new(),
        });
    }
    insert_marker(&mut lines, geometry.marker, unit.range, options);
    let mut atomic_objects = extract_atomic_objects(&mut lines, pending_atomics);
    inherit_flow_link(
        &mut lines,
        &mut atomic_objects,
        unit.inherited_link.as_ref(),
    );
    let height = lines.iter().map(|line| line.height).sum();
    BlockLayout {
        node: Some(unit.node),
        kind: LayoutBlockKind::Paragraph,
        range: unit.range,
        y,
        height,
        lines,
        atomic_objects,
        table: None,
        rule: false,
        rule_x: 0.0,
        rule_width: 0.0,
        quoted: !unit.presentation.quote_bars.is_empty(),
        quote_bars: geometry.quote_bars,
        box_layout: None,
        break_before: computed.break_before(),
        break_after: computed.break_after(),
        break_inside_avoid: computed.break_inside_avoid(),
        widows: computed.widows(),
        orphans: computed.orphans(),
    }
}

#[derive(Clone, Debug)]
struct SemanticTableRow {
    header: bool,
    cells: Vec<NodeId>,
}

fn semantic_table_rows(
    document: &FlowDocument,
    table: NodeId,
) -> (Option<NodeId>, Vec<SemanticTableRow>, usize) {
    let mut caption = None;
    let mut rows = Vec::new();
    let Some(children) = document.children(table) else {
        return (caption, rows, 0);
    };
    for child in children {
        let Some(node) = document.get_node(*child) else {
            continue;
        };
        match node.kind {
            NodeKind::TableCaption => caption = Some(*child),
            NodeKind::Row => rows.push(SemanticTableRow {
                header: false,
                cells: node.children.clone(),
            }),
            NodeKind::RowGroup(group) => {
                let header = group == crate::model::flow::RowGroup::Head;
                for row in &node.children {
                    let Some(row_node) = document.get_node(*row) else {
                        continue;
                    };
                    rows.push(SemanticTableRow {
                        header,
                        cells: row_node.children.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    let header_count = rows.iter().take_while(|row| row.header).count();
    (caption, rows, header_count)
}

#[derive(Clone, Copy, Debug, Default)]
struct IntrinsicWidths {
    min_content: f32,
    max_content: f32,
    emergency_min: f32,
}

fn marker_right_edge(geometry: &PresentationGeometry) -> f32 {
    geometry.marker.as_ref().map_or(0.0, |marker| {
        marker
            .segments
            .iter()
            .map(|(_, metrics, x, _)| x + metrics.width)
            .fold(0.0_f32, f32::max)
    })
}

fn measure_item_stream(items: &[super::LayoutItem], leading: f32) -> IntrinsicWidths {
    let mut maximum = 0.0_f32;
    let mut natural_line = 0.0_f32;
    let mut minimum = 0.0_f32;
    let mut unbreakable = 0.0_f32;
    let mut emergency_minimum = 0.0_f32;
    let mut emergency_segment = 0.0_f32;

    for item in items {
        match item {
            Item::Box { width, .. } => {
                natural_line += width;
                unbreakable += width;
                emergency_segment += width;
            }
            Item::Glue { width, .. } => {
                natural_line += width;
                minimum = minimum.max(unbreakable);
                unbreakable = 0.0;
                emergency_minimum = emergency_minimum.max(emergency_segment);
                emergency_segment = 0.0;
            }
            Item::Penalty {
                width, cost, data, ..
            } => match data {
                Break::Line | Break::Paragraph => {
                    maximum = maximum.max(natural_line);
                    natural_line = 0.0;
                    minimum = minimum.max(unbreakable);
                    unbreakable = 0.0;
                    emergency_minimum = emergency_minimum.max(emergency_segment);
                    emergency_segment = 0.0;
                }
                Break::Hyphen(_) => {
                    minimum = minimum.max(unbreakable + width);
                    unbreakable = 0.0;
                    emergency_minimum = emergency_minimum.max(emergency_segment + width);
                    emergency_segment = 0.0;
                }
                Break::Emergency if *cost <= 0.0 => {
                    minimum = minimum.max(unbreakable);
                    unbreakable = 0.0;
                    emergency_minimum = emergency_minimum.max(emergency_segment);
                    emergency_segment = 0.0;
                }
                Break::Emergency => {
                    emergency_minimum = emergency_minimum.max(emergency_segment);
                    emergency_segment = 0.0;
                }
            },
        }
    }
    maximum = maximum.max(natural_line);
    minimum = minimum.max(unbreakable);
    emergency_minimum = emergency_minimum.max(emergency_segment);
    IntrinsicWidths {
        min_content: leading + minimum,
        max_content: leading + maximum,
        emergency_min: leading + emergency_minimum,
    }
}

fn intrinsic_flow_cell_width<M: TextMeasurer>(
    document: &FlowDocument,
    styles: &StyleSnapshot,
    cell: NodeId,
    measurer: &M,
    options: &LayoutOptions,
    note_markers: &std::collections::HashMap<NodeId, FootnotePresentation>,
) -> IntrinsicWidths {
    let units = text_blocks_from(document, styles, cell, note_markers);
    let column_widths = measure_column_widths(&units, measurer, options);
    let mut result = IntrinsicWidths::default();

    for unit in units {
        let geometry = presentation_geometry(&unit.presentation, &column_widths, measurer, options);
        let marker_right = marker_right_edge(&geometry);
        let measured = match unit.kind {
            TextBlockKind::Paragraph | TextBlockKind::Heading => {
                let (mut items, _) = collect_text_block_items(
                    document,
                    styles,
                    &unit,
                    measurer,
                    options,
                    f32::MAX / 4.0,
                    false,
                );
                terminate(&mut items, Break::Paragraph);
                let mut measured = measure_item_stream(&items, geometry.content_left);

                let (mut emergency_items, _) =
                    collect_text_block_items(document, styles, &unit, measurer, options, 1.0, true);
                terminate(&mut emergency_items, Break::Paragraph);
                measured.emergency_min =
                    measure_item_stream(&emergency_items, geometry.content_left).emergency_min;
                measured
            }
            TextBlockKind::Preformatted => {
                let node = document
                    .get_node(unit.node)
                    .expect("intrinsic units retain valid nodes");
                let computed = styles
                    .style_for_node(unit.node)
                    .and_then(|style| styles.get_computed_style(style))
                    .expect("style snapshots cover preformatted nodes");
                let style = text_style(computed, options, TextBlockKind::Preformatted);
                let code_indent = if unit.presentation.hanging_columns.is_empty() {
                    options.code_block_indent
                } else {
                    options.nested_code_block_indent
                };
                let leading = geometry.content_left + options.indent * code_indent;
                let width = document
                    .get_text(node.text_range)
                    .unwrap_or_default()
                    .split('\n')
                    .map(|line| measurer.measure(line, &style).width)
                    .fold(0.0_f32, f32::max)
                    + leading;
                IntrinsicWidths {
                    min_content: width,
                    max_content: width,
                    emergency_min: width,
                }
            }
            TextBlockKind::Atomic => {
                let node = document
                    .get_node(unit.node)
                    .expect("intrinsic units retain valid atomic nodes");
                let computed = styles
                    .style_for_node(unit.node)
                    .and_then(|style| styles.get_computed_style(style))
                    .expect("style snapshots cover atomic nodes");
                let style = text_style(computed, options, TextBlockKind::Atomic);
                let atomic = pending_atomic(
                    document,
                    unit.node,
                    node,
                    &style,
                    computed,
                    measurer,
                    f32::MAX / 4.0,
                );
                let emergency_atomic =
                    pending_atomic(document, unit.node, node, &style, computed, measurer, 1.0);
                let width = geometry.content_left + atomic.width;
                IntrinsicWidths {
                    min_content: width,
                    max_content: width,
                    emergency_min: geometry.content_left + emergency_atomic.width,
                }
            }
            TextBlockKind::Table => {
                // Nested tables use a readable source-order fallback. Measure
                // the exact same text-and-atomic item stream at natural and
                // emergency widths rather than treating atomic alt text as
                // painted content.
                let computed = styles
                    .style_for_node(unit.node)
                    .and_then(|style| styles.get_computed_style(style))
                    .expect("style snapshots cover nested tables");
                let style = text_style(computed, options, TextBlockKind::Paragraph);
                let (mut items, _) = collect_nested_table_fallback_items(
                    document,
                    styles,
                    &unit,
                    &style,
                    measurer,
                    options,
                    f32::MAX / 4.0,
                );
                terminate(&mut items, Break::Paragraph);
                let mut measured = measure_item_stream(&items, geometry.content_left);
                let (mut emergency_items, _) = collect_nested_table_fallback_items(
                    document, styles, &unit, &style, measurer, options, 1.0,
                );
                terminate(&mut emergency_items, Break::Paragraph);
                measured.emergency_min =
                    measure_item_stream(&emergency_items, geometry.content_left).emergency_min;
                measured
            }
            TextBlockKind::Rule => IntrinsicWidths::default(),
        };
        result.min_content = result
            .min_content
            .max(measured.min_content.max(marker_right));
        result.max_content = result
            .max_content
            .max(measured.max_content.max(marker_right));
        result.emergency_min = result
            .emergency_min
            .max(measured.emergency_min.max(marker_right));
    }
    result.max_content = result.max_content.max(result.min_content);
    result.min_content = result.min_content.max(result.emergency_min);
    result
}

fn merge_table_quote_bars(bars: &mut Vec<super::TableQuoteBarLayout>, maximum_gap: f32) {
    bars.sort_by(|left, right| {
        left.x
            .total_cmp(&right.x)
            .then_with(|| left.y.total_cmp(&right.y))
    });
    let mut merged = Vec::<super::TableQuoteBarLayout>::new();
    for bar in bars.drain(..) {
        if let Some(previous) = merged.last_mut()
            && (previous.x - bar.x).abs() < 0.01
            && bar.y <= previous.y + previous.height + maximum_gap + 0.01
        {
            previous.height = previous.height.max(bar.y + bar.height - previous.y);
        } else {
            merged.push(bar);
        }
    }
    *bars = merged;
}

#[allow(clippy::too_many_arguments)]
fn layout_table<M: TextMeasurer>(
    document: &FlowDocument,
    styles: &StyleSnapshot,
    unit: TextBlock,
    column_cache: &std::collections::HashMap<usize, f32>,
    measurer: &M,
    options: &LayoutOptions,
    y: f32,
    note_markers: &std::collections::HashMap<NodeId, FootnotePresentation>,
) -> BlockLayout {
    let node = document
        .get_node(unit.node)
        .expect("table layout units retain valid identities");
    let NodeKind::Table(table) = node.kind else {
        unreachable!("table layout units retain their semantic kind");
    };
    let (caption, rows, header_row_count) = semantic_table_rows(document, unit.node);
    let column_count = table.column_count.get() as usize;
    let computed = styles
        .style_for_node(unit.node)
        .and_then(|style| styles.get_computed_style(style))
        .expect("style snapshots cover tables");
    let base_style = text_style(computed, options, TextBlockKind::Table);
    let mut table_presentation = unit.presentation.clone();
    if caption.is_some() {
        table_presentation.marker = None;
        table_presentation.marker_level = None;
    }
    let geometry = presentation_geometry(&table_presentation, column_cache, measurer, options);
    let box_x = geometry.content_left;
    let available_outer_width = (options.width - box_x * 2.0).max(1.0);
    let author_box = resolve_author_box(
        computed,
        base_style.size,
        options.width,
        available_outer_width,
    );
    let table_x = author_box.content_x(box_x);
    let available_width = author_box.content_width;
    let content_y = author_box.content_y(y);
    let horizontal_padding = options.table_horizontal_padding;
    let vertical_padding = options.table_vertical_padding;
    let empty_cell_width = horizontal_padding * 2.0;
    let mut cell_measures = (0..column_count)
        .map(|column| CellMeasure {
            start: column,
            span: 1,
            min_content: empty_cell_width,
            max_content: empty_cell_width,
            emergency_min: empty_cell_width,
        })
        .collect::<Vec<_>>();
    for row in &rows {
        for cell in &row.cells {
            let Some(cell_node) = document.get_node(*cell) else {
                continue;
            };
            let NodeKind::Cell(cell_data) = &cell_node.kind else {
                continue;
            };
            let start = cell_data.column_start as usize;
            let span = cell_data.column_span.get() as usize;
            let end = start.saturating_add(span).min(column_count);
            if start >= end {
                continue;
            }
            let measured =
                intrinsic_flow_cell_width(document, styles, *cell, measurer, options, note_markers);
            let min_content = measured.min_content + horizontal_padding * 2.0;
            cell_measures.push(CellMeasure {
                start,
                span: end - start,
                min_content,
                max_content: (measured.max_content + horizontal_padding * 2.0).max(min_content),
                emergency_min: (measured.emergency_min + horizontal_padding * 2.0).min(min_content),
            });
        }
    }
    let caption_measure = caption.map(|caption| {
        intrinsic_flow_cell_width(document, styles, caption, measurer, options, note_markers)
    });
    let intrinsic = compute_intrinsic_grid(&TableSizingInput {
        tracks: vec![TrackConstraint::default(); column_count],
        cells: cell_measures,
        caption_min: caption_measure.map_or(0.0, |measure| measure.min_content),
        caption_emergency_min: caption_measure.map_or(0.0, |measure| measure.emergency_min),
    })
    .expect("validated flow tables produce valid intrinsic constraints");
    let intrinsic_used_width = if computed.width().is_some() {
        available_width.max(intrinsic.grid_min)
    } else {
        automatic_table_width(&intrinsic, available_width)
            .expect("available table width is finite and nonnegative")
    };
    let fallback_active = intrinsic_used_width > available_width
        && options.table_overflow == super::TableOverflowPolicy::EmergencyFit;
    let column_widths = if fallback_active {
        distribute_emergency_width(&intrinsic, available_width)
            .expect("available table width is finite and nonnegative")
    } else {
        distribute_assignable_width(&intrinsic, intrinsic_used_width)
            .expect("used table width is finite and nonnegative")
    };
    let table_width = column_widths.iter().sum::<f32>();
    let sizing = TableSizingDiagnostics {
        grid_min: intrinsic.grid_min,
        grid_max: intrinsic.grid_max,
        emergency_min: intrinsic.emergency_min,
        used_width: table_width,
        fallback_active,
        overflows_available: table_width > available_width + 0.01,
    };

    let mut lines = Vec::new();
    let mut atomic_objects = Vec::new();
    let mut row_y = content_y;
    if let Some(caption) = caption {
        let mut caption_options = options.clone();
        caption_options.width = table_x + table_width;
        let caption_blocks = layout_nested_flow(
            document,
            styles,
            caption,
            &unit.presentation,
            note_markers,
            measurer,
            FlowLayoutContext {
                options: &caption_options,
                force_emergency_breaking: fallback_active,
            },
        );
        let caption_height = caption_blocks
            .last()
            .map_or(0.0, |block| block.y + block.height);
        row_y += caption_height + options.block_spacing;
        for block in caption_blocks {
            lines.extend(block.lines.into_iter().map(|mut line| {
                line.y += content_y;
                line
            }));
            atomic_objects.extend(block.atomic_objects.into_iter().map(|mut object| {
                object.translate(0.0, content_y);
                object
            }));
        }
    }
    let mut column_x = Vec::with_capacity(column_count + 1);
    column_x.push(table_x);
    for width in &column_widths {
        column_x.push(column_x.last().copied().unwrap_or(table_x) + width);
    }

    #[derive(Clone, Copy)]
    struct CellBox {
        row: usize,
        row_span: usize,
        column: usize,
        column_span: usize,
        x: f32,
        width: f32,
        content_x: f32,
        content_width: f32,
    }
    struct CellLayout {
        bounds: CellBox,
        lines: Vec<LineLayout>,
        rules: Vec<TableRuleLayout>,
        quote_bars: Vec<super::TableQuoteBarLayout>,
        atomic_objects: Vec<AtomicLayout>,
        required_height: f32,
        allows_horizontal_overflow: bool,
    }
    let mut cells = Vec::<CellLayout>::new();
    let minimum_row_height = options.body_size * options.line_spacing + vertical_padding * 2.0;
    let mut row_constraints = Vec::new();
    for (row_index, row) in rows.iter().enumerate() {
        for cell in &row.cells {
            let Some(cell_node) = document.get_node(*cell) else {
                continue;
            };
            let NodeKind::Cell(cell_data) = &cell_node.kind else {
                continue;
            };
            let start = cell_data.column_start as usize;
            let column_span = cell_data.column_span.get() as usize;
            let end = start.saturating_add(column_span).min(column_count);
            if start >= end {
                continue;
            }
            let width = column_widths[start..end].iter().sum::<f32>();
            let content_width = (width - horizontal_padding * 2.0).max(1.0);
            let mut cell_options = options.clone();
            cell_options.width = content_width;
            let cell_blocks = layout_nested_flow(
                document,
                styles,
                *cell,
                &Presentation::default(),
                note_markers,
                measurer,
                FlowLayoutContext {
                    options: &cell_options,
                    force_emergency_breaking: fallback_active,
                },
            );
            let mut cell_lines = Vec::new();
            let mut cell_rules = Vec::new();
            let mut cell_quote_bars = Vec::new();
            let mut cell_atomics = Vec::new();
            let mut content_height = 0.0_f32;
            let mut allows_horizontal_overflow = false;
            for block in cell_blocks {
                content_height = content_height.max(block.y + block.height);
                allows_horizontal_overflow |= block.kind == LayoutBlockKind::Code;
                for mut line in block.lines {
                    for run in &mut line.runs {
                        run.x += column_x[start] + horizontal_padding;
                    }
                    line.y += vertical_padding;
                    cell_lines.push(line);
                }
                let has_thematic_atomic = block
                    .atomic_objects
                    .iter()
                    .any(|object| matches!(object.kind, AtomicLayoutKind::ThematicBreak));
                cell_atomics.extend(block.atomic_objects.into_iter().map(|mut object| {
                    object.translate(column_x[start] + horizontal_padding, vertical_padding);
                    object
                }));
                if let Some(nested_table) = block.table {
                    cell_rules.extend(nested_table.rules.into_iter().map(|mut rule| {
                        rule.x += column_x[start] + horizontal_padding;
                        rule.y += vertical_padding;
                        rule
                    }));
                    cell_quote_bars.extend(nested_table.quote_bars.into_iter().map(|mut bar| {
                        bar.x += column_x[start] + horizontal_padding;
                        bar.y += vertical_padding;
                        bar
                    }));
                }
                if block.rule && !has_thematic_atomic {
                    cell_rules.push(TableRuleLayout {
                        x: column_x[start] + horizontal_padding + block.rule_x,
                        y: vertical_padding + block.y + block.height / 2.0,
                        width: block.rule_width.min(content_width),
                        kind: TableRuleKind::Light,
                    });
                }
                for x in block.quote_bars {
                    cell_quote_bars.push(super::TableQuoteBarLayout {
                        x: column_x[start] + horizontal_padding + x,
                        y: vertical_padding + block.y,
                        height: block.height,
                    });
                }
            }
            merge_table_quote_bars(&mut cell_quote_bars, options.block_spacing);
            if cell_lines.is_empty() {
                let computed = styles
                    .style_for_node(*cell)
                    .and_then(|style| styles.get_computed_style(style))
                    .expect("style snapshots cover empty table cells");
                let style = text_style(computed, options, TextBlockKind::Paragraph);
                let metrics = measurer.measure("M", &style);
                let (height, baseline) = line_box(metrics, style.size, options.line_spacing);
                content_height = height;
                cell_lines.push(LineLayout {
                    range: text_range(cell_node.text_range),
                    y: vertical_padding,
                    height,
                    baseline,
                    runs: Vec::new(),
                });
            }
            let computed = styles
                .style_for_node(*cell)
                .and_then(|style| styles.get_computed_style(style))
                .expect("style snapshots cover table cells");
            let alignment = computed
                .text_alignment()
                .unwrap_or(crate::model::flow::TextAlignment::Left);
            let content_left = column_x[start] + horizontal_padding;
            let mut atomic_lines = Vec::with_capacity(cell_atomics.len());
            let mut used_atomics = vec![0.0_f32; cell_lines.len()];
            for object in &cell_atomics {
                let center = object.y + object.height / 2.0;
                let line_index = cell_lines
                    .partition_point(|line| line.y + line.height <= center)
                    .min(cell_lines.len().saturating_sub(1));
                atomic_lines.push(line_index);
                if let Some(used) = used_atomics.get_mut(line_index) {
                    *used = used.max(object.x + object.width - content_left);
                }
            }
            let mut shifts = Vec::with_capacity(cell_lines.len());
            for (line_index, line) in cell_lines.iter_mut().enumerate() {
                let used_runs = line
                    .runs
                    .iter()
                    .map(|run| run.x + run.width - content_left)
                    .fold(0.0_f32, f32::max);
                let used = used_runs.max(used_atomics[line_index]);
                let shift = match alignment {
                    crate::model::flow::TextAlignment::Left
                    | crate::model::flow::TextAlignment::Justify => 0.0,
                    crate::model::flow::TextAlignment::Center => {
                        (content_width - used).max(0.0) / 2.0
                    }
                    crate::model::flow::TextAlignment::Right => (content_width - used).max(0.0),
                };
                for run in &mut line.runs {
                    run.x += shift;
                }
                shifts.push(shift);
            }
            for (object, line_index) in cell_atomics.iter_mut().zip(atomic_lines) {
                object.translate(shifts.get(line_index).copied().unwrap_or_default(), 0.0);
            }
            let required_height = content_height + vertical_padding * 2.0;
            let row_span = (cell_data.row_span.get() as usize).min(rows.len() - row_index);
            let bounds = CellBox {
                row: row_index,
                row_span,
                column: start,
                column_span: end - start,
                x: column_x[start],
                width,
                content_x: content_left,
                content_width,
            };
            let content_right = bounds.content_x + bounds.content_width;
            allows_horizontal_overflow |=
                cell_lines.iter().flat_map(|line| &line.runs).any(|run| {
                    run.x < bounds.content_x - 0.01 || run.x + run.width > content_right + 0.01
                }) || cell_atomics.iter().any(|object| {
                    object.x < bounds.content_x - 0.01
                        || object.x + object.width > content_right + 0.01
                }) || cell_rules.iter().any(|rule| {
                    rule.x < bounds.content_x - 0.01 || rule.x + rule.width > content_right + 0.01
                }) || cell_quote_bars
                    .iter()
                    .any(|bar| bar.x < bounds.content_x - 0.01 || bar.x > content_right + 0.01);
            if !allows_horizontal_overflow {
                debug_assert!(cell_lines.iter().flat_map(|line| &line.runs).all(|run| {
                    run.x >= bounds.content_x - 0.01 && run.x + run.width <= content_right + 0.01
                }));
            }
            row_constraints.push((row_index, row_span, required_height));
            cells.push(CellLayout {
                bounds,
                lines: cell_lines,
                rules: cell_rules,
                quote_bars: cell_quote_bars,
                atomic_objects: cell_atomics,
                required_height,
                allows_horizontal_overflow,
            });
        }
    }
    let row_heights = if rows.is_empty() {
        Vec::new()
    } else {
        distribute_span_minima(rows.len(), minimum_row_height, &row_constraints)
            .expect("validated flow row spans produce valid height constraints")
    };
    let mut row_offsets = Vec::with_capacity(row_heights.len() + 1);
    row_offsets.push(0.0);
    for height in &row_heights {
        row_offsets.push(row_offsets.last().copied().unwrap_or_default() + height);
    }
    let mut nested_rules = Vec::new();
    let mut nested_quote_bars = Vec::new();
    let mut row_spans = Vec::new();
    for mut cell in cells {
        let offset = row_y + row_offsets[cell.bounds.row];
        for line in &mut cell.lines {
            line.y += offset;
        }
        for rule in &mut cell.rules {
            rule.y += offset;
        }
        for quote in &mut cell.quote_bars {
            quote.y += offset;
        }
        for object in &mut cell.atomic_objects {
            object.translate(0.0, offset);
        }
        if cell.bounds.row_span > 1 {
            row_spans.push((
                cell.bounds.row,
                cell.bounds
                    .row
                    .saturating_add(cell.bounds.row_span)
                    .min(rows.len()),
            ));
        }
        debug_assert_eq!(
            cell.bounds.width,
            column_widths[cell.bounds.column..cell.bounds.column + cell.bounds.column_span]
                .iter()
                .sum::<f32>()
        );
        debug_assert_eq!(cell.bounds.x, column_x[cell.bounds.column]);
        debug_assert!(cell.required_height >= 0.0);
        let _ = cell.allows_horizontal_overflow;
        lines.extend(cell.lines);
        nested_rules.extend(cell.rules);
        nested_quote_bars.extend(cell.quote_bars);
        atomic_objects.extend(cell.atomic_objects);
    }
    let mut rules = vec![TableRuleLayout {
        x: table_x,
        y: row_y,
        width: table_width,
        kind: TableRuleKind::Strong,
    }];
    if header_row_count > 0 {
        rules.push(TableRuleLayout {
            x: table_x,
            y: row_y + row_offsets[header_row_count],
            width: table_width,
            kind: TableRuleKind::Light,
        });
    }
    let table_end = row_y + row_offsets.last().copied().unwrap_or_default();
    rules.push(TableRuleLayout {
        x: table_x,
        y: table_end,
        width: table_width,
        kind: TableRuleKind::Strong,
    });
    rules.extend(nested_rules);
    if geometry.marker.is_some() {
        let marker_line = lines.len();
        let metrics = measurer.measure("M", &base_style);
        let (height, baseline) = line_box(metrics, base_style.size, options.line_spacing);
        lines.push(LineLayout {
            range: TextRange {
                start: unit.range.start,
                end: unit.range.start,
            },
            y: row_y + vertical_padding,
            height,
            baseline,
            runs: Vec::new(),
        });
        insert_marker(
            &mut lines[marker_line..],
            geometry.marker,
            unit.range,
            options,
        );
    }

    inherit_flow_link(
        &mut lines,
        &mut atomic_objects,
        unit.inherited_link.as_ref(),
    );
    let height = (table_end - content_y + author_box.vertical())
        .max(author_box.minimum_height)
        .max(options.block_spacing);
    BlockLayout {
        node: Some(unit.node),
        kind: LayoutBlockKind::Table,
        range: unit.range,
        y,
        height,
        lines,
        atomic_objects,
        table: Some(TableLayout {
            x: table_x,
            width: table_width,
            row_y,
            header_row_count,
            column_widths,
            row_heights,
            row_spans,
            overflow_policy: options.table_overflow,
            sizing,
            rules,
            quote_bars: nested_quote_bars,
        }),
        rule: false,
        rule_x: 0.0,
        rule_width: 0.0,
        quoted: !unit.presentation.quote_bars.is_empty(),
        quote_bars: geometry.quote_bars,
        box_layout: author_box.layout(box_x),
        break_before: computed.break_before(),
        break_after: computed.break_after(),
        break_inside_avoid: computed.break_inside_avoid(),
        widows: computed.widows(),
        orphans: computed.orphans(),
    }
}

fn layout_rule<M: TextMeasurer>(
    styles: &StyleSnapshot,
    unit: TextBlock,
    column_widths: &std::collections::HashMap<usize, f32>,
    measurer: &M,
    options: &LayoutOptions,
    y: f32,
) -> BlockLayout {
    let computed = styles
        .style_for_node(unit.node)
        .and_then(|style| styles.get_computed_style(style))
        .expect("style snapshots cover every finalized node");
    let style = text_style(computed, options, TextBlockKind::Rule);
    let geometry = presentation_geometry(&unit.presentation, column_widths, measurer, options);
    let mut lines = Vec::new();
    if geometry.marker.is_some() {
        let metrics = measurer.measure("M", &style);
        let (height, baseline) = line_box(metrics, style.size, options.line_spacing);
        lines.push(LineLayout {
            range: TextRange {
                start: unit.range.start,
                end: unit.range.start,
            },
            y,
            height,
            baseline,
            runs: Vec::new(),
        });
        insert_marker(&mut lines, geometry.marker, unit.range, options);
    }
    let height = lines
        .iter()
        .map(|line| line.height)
        .sum::<f32>()
        .max(options.block_spacing);
    let rule_x = geometry.content_left;
    let rule_width = (options.width - geometry.content_left * 2.0).max(1.0);
    let rtl = atomic_is_rtl(style.bidi, "\u{fffc}");
    let mut atomic_objects = vec![AtomicLayout {
        node: Some(unit.node),
        range: unit.range,
        x: rule_x,
        y,
        width: rule_width,
        height,
        leading_x: if rtl { rule_x + rule_width } else { rule_x },
        trailing_x: if rtl { rule_x } else { rule_x + rule_width },
        bidi: style.bidi,
        flow_link: None,
        kind: AtomicLayoutKind::ThematicBreak,
    }];
    inherit_flow_link(
        &mut lines,
        &mut atomic_objects,
        unit.inherited_link.as_ref(),
    );
    BlockLayout {
        node: Some(unit.node),
        kind: LayoutBlockKind::Rule,
        range: unit.range,
        y,
        height,
        lines,
        atomic_objects,
        table: None,
        rule: true,
        rule_x,
        rule_width,
        quoted: !unit.presentation.quote_bars.is_empty(),
        quote_bars: geometry.quote_bars,
        box_layout: None,
        break_before: computed.break_before(),
        break_after: computed.break_after(),
        break_inside_avoid: computed.break_inside_avoid(),
        widows: computed.widows(),
        orphans: computed.orphans(),
    }
}

fn text_style(
    computed: &crate::model::flow::ComputedStyle,
    options: &LayoutOptions,
    block_kind: TextBlockKind,
) -> TextStyle {
    let family = match computed.font_family() {
        EffectiveFontFamily::Proportional => FontFamily::Proportional,
        EffectiveFontFamily::SansSerif => FontFamily::SansSerif,
        EffectiveFontFamily::Monospace => FontFamily::Monospace,
        EffectiveFontFamily::Named(name) => FontFamily::Named(name),
    };
    let mut size = options.body_size * computed.font_size_per_mille() as f32 / 1_000.0;
    if matches!(block_kind, TextBlockKind::Preformatted) {
        size *= options.code_block_scale;
    } else if family == FontFamily::Monospace {
        size *= options.inline_code_scale;
    }
    let mut letter_spacing = 0.0;
    if let Some(level) = computed.heading_level() {
        let index = usize::from(level.saturating_sub(1)).min(options.heading_scales.len() - 1);
        if !computed.has_author_font_size() {
            size *= options.heading_scales[index];
        }
        letter_spacing = size * options.heading_tracking[index];
    }
    let baseline_shift = match computed.vertical_align() {
        EffectiveVerticalAlign::Baseline => 0.0,
        EffectiveVerticalAlign::Superscript => {
            size *= options.superscript_scale;
            options.body_size * options.superscript_shift
        }
        EffectiveVerticalAlign::Subscript => {
            size *= options.superscript_scale;
            -options.body_size * options.superscript_shift
        }
    };
    TextStyle {
        family,
        size,
        weight: match computed.font_weight() {
            EffectiveFontWeight::Normal => FontWeight::Normal,
            EffectiveFontWeight::Bold => FontWeight::Bold,
        },
        italic: computed.italic(),
        strikethrough: computed.strikethrough(),
        link: computed.is_link(),
        initial_letter: computed.initial_letter(),
        baseline_shift,
        letter_spacing,
        bidi: computed.bidi(),
        bidi_boundary: None,
        shape_direction: computed.bidi().direction,
    }
}

fn text_range(range: crate::model::flow::TextRange) -> TextRange {
    TextRange {
        start: range.start.as_usize(),
        end: range.end.as_usize(),
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, num::NonZeroU32};

    use super::*;
    use crate::model::flow::{
        AuthorStyle, BaseDirection, BidiMode, BidiState, Cell, CellKind, Checkbox, CheckboxState,
        CssBorder, CssColor, CssLength, Destination as FlowDestination, FlowBuilder, HeaderScope,
        Heading, HeadingLevel, Image, ImageAlternative, InitialLetter, InlineRole, InlineSemantic,
        IntrinsicSize, Link, LinkContent, LinkRelation, List, ListItem, ListKind, Math,
        MathDisplay, MathNotation, MediaKind, NonEmptyText, Placement, PositiveCssPixels,
        Preformatted, PreformattedKind, Resource, RowGroup, SemanticStyleKey, StyleContextKey,
        StyleInput, Table, TextAlignment, UnsupportedObject, UnsupportedReason,
    };

    struct FixedMeasurer;

    impl TextMeasurer for FixedMeasurer {
        fn measure(&self, text: &str, style: &TextStyle) -> super::super::TextMetrics {
            super::super::TextMetrics {
                width: text.chars().count() as f32 * style.size * 0.5,
                ascent: style.size * 0.8,
                descent: style.size * 0.2,
            }
        }
    }

    fn append_paragraph(builder: &mut FlowBuilder, parent: NodeId, text: &str) {
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let text = builder.add_text(text).unwrap();
        builder.append(paragraph, text).unwrap();
        builder.append(parent, paragraph).unwrap();
    }

    #[test]
    fn justified_nonfinal_prose_lines_reach_the_measure() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        append_paragraph(
            &mut builder,
            root,
            "This synthetic publication describes readable content and its structure, including ordinary paragraphs and discretionary hyphenation. It demonstrates automatic justification across several lines without changing the natural spacing of the final line.",
        );
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        for width in [240.0, 320.0, 468.0] {
            let options = LayoutOptions {
                width,
                body_size: 14.0,
                ..LayoutOptions::default()
            };
            let layout = layout_flow_document(&document, &styles, &FixedMeasurer, &options);
            let lines = &layout.blocks[0].lines;
            assert!(lines.len() > 2);
            for line in &lines[..lines.len() - 1] {
                let right = line
                    .runs
                    .iter()
                    .filter(|run| !(run.range.is_empty() && run.text == "-"))
                    .map(|run| run.x + run.width)
                    .fold(0.0_f32, f32::max);
                assert!(
                    (right - width).abs() < 0.01,
                    "non-final line ends at {right}, expected {width}: {line:?}"
                );
            }
            let last = lines.last().unwrap();
            let right = last
                .runs
                .iter()
                .map(|run| run.x + run.width)
                .fold(0.0_f32, f32::max);
            assert!(
                right < width - 1.0,
                "final line should retain natural spacing"
            );
        }
    }

    #[test]
    fn justified_discretionary_hyphens_hang_outside_the_measure() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        append_paragraph(
            &mut builder,
            root,
            "Words before hyphenation demonstrate ordinary paragraph justification repeatedly.",
        );
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let mut hanging_hyphens = 0;
        for width in [60.0, 75.0, 90.0, 105.0, 120.0] {
            let options = LayoutOptions {
                width,
                body_size: 10.0,
                ..LayoutOptions::default()
            };
            let layout = layout_flow_document(&document, &styles, &FixedMeasurer, &options);
            for run in layout
                .blocks
                .iter()
                .flat_map(|block| &block.lines)
                .flat_map(|line| &line.runs)
            {
                if run.range.is_empty() && run.text == "-" {
                    assert!(run.width > 0.0);
                    assert!(run.x <= width + 0.001);
                    if (run.x - width).abs() < 0.001 {
                        hanging_hyphens += 1;
                        assert!(run.x + run.width > width);
                    }
                    continue;
                }
                assert!(
                    run.x + run.width <= width + 0.001,
                    "run exceeds measure {width}: {run:?}"
                );
            }
        }
        assert!(
            hanging_hyphens > 0,
            "exercise at least one hanging discretionary hyphen"
        );
    }

    #[test]
    fn rtl_discretionary_hyphen_hangs_at_the_visual_left_edge() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let style = builder
            .intern_style_input(StyleInput::default().with_bidi(BidiState {
                direction: BaseDirection::RightToLeft,
                mode: BidiMode::Normal,
            }))
            .unwrap();
        builder.set_style_input(paragraph, style).unwrap();
        builder.append(root, paragraph).unwrap();
        let before = builder.add_text("אבגד אבגד אבגדאבגד").unwrap();
        let opportunity = builder
            .add_node(NodeKind::BreakOpportunity(
                crate::model::flow::BreakOpportunity {
                    hyphen_when_broken: true,
                },
            ))
            .unwrap();
        let after = builder.add_text("אבגדאבגד אבגד אבגד").unwrap();
        for node in [before, opportunity, after] {
            builder.append(paragraph, node).unwrap();
        }
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let options = LayoutOptions {
            width: 100.0,
            body_size: 10.0,
            ..LayoutOptions::default()
        };
        let layout = layout_flow_document(&document, &styles, &FixedMeasurer, &options);
        let line = &layout.blocks[0].lines[0];
        let hyphen = &line.runs[0];
        assert_eq!(hyphen.text, "-");
        assert!(hyphen.width > 0.0);
        assert!((hyphen.x + hyphen.width).abs() < 0.01);
        let text = &line.runs[1..];
        assert!(text.iter().all(|run| run.x >= -0.01));
        let right = text
            .iter()
            .map(|run| run.x + run.width)
            .fold(0.0_f32, f32::max);
        assert!((right - options.width).abs() < 0.01);
    }

    fn append_checkbox_paragraph(
        builder: &mut FlowBuilder,
        parent: NodeId,
        text: &str,
        state: CheckboxState,
    ) {
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let checkbox = builder
            .add_node(NodeKind::Checkbox(Checkbox { state }))
            .unwrap();
        let style = builder
            .intern_style_input(StyleInput::semantic(SemanticStyleKey::CheckboxMarker))
            .unwrap();
        builder.set_style_input(checkbox, style).unwrap();
        builder.append(paragraph, checkbox).unwrap();
        let text = builder.add_text(text).unwrap();
        builder.append(paragraph, text).unwrap();
        builder.append(parent, paragraph).unwrap();
    }

    fn append_cell(
        builder: &mut FlowBuilder,
        row: NodeId,
        kind: CellKind,
        column_start: u32,
        column_span: u32,
        row_span: u32,
    ) -> NodeId {
        let cell = builder
            .add_node(NodeKind::Cell(Cell {
                kind,
                column_start,
                column_span: NonZeroU32::new(column_span).unwrap(),
                row_span: NonZeroU32::new(row_span).unwrap(),
                scope: (kind == CellKind::Header).then_some(HeaderScope::Column),
                explicit_headers: None,
            }))
            .unwrap();
        builder.append(row, cell).unwrap();
        cell
    }

    #[test]
    fn composition_is_block_local_and_continuous_placement_translates_geometry() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        append_paragraph(&mut builder, root, "first paragraph wraps across lines");
        let rule = builder.add_node(NodeKind::ThematicBreak).unwrap();
        builder.append(root, rule).unwrap();
        append_paragraph(&mut builder, root, "second paragraph");
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let options = LayoutOptions {
            width: 120.0,
            body_size: 10.0,
            block_spacing: 7.0,
            ..LayoutOptions::default()
        };

        let composed = compose_flow_document(&document, &styles, &FixedMeasurer, &options);
        assert_eq!(composed.blocks.len(), 3);
        assert!(composed.blocks.iter().all(|block| block.layout.y == 0.0));
        let retained = composed.clone();
        let layout = composed.into_continuous_layout();

        assert_eq!(layout.width, options.width);
        assert_eq!(layout.blocks.len(), retained.blocks.len());
        for (local, placed) in retained.blocks.iter().zip(&layout.blocks) {
            assert_eq!(placed.height, local.layout.height);
            assert_eq!(placed.range, local.layout.range);
            assert!(placed.y >= options.block_spacing);
            for (local_line, placed_line) in local.layout.lines.iter().zip(&placed.lines) {
                assert_eq!(placed_line.y, local_line.y + placed.y);
                assert_eq!(placed_line.range, local_line.range);
                assert_eq!(placed_line.runs.len(), local_line.runs.len());
            }
            for (local_object, placed_object) in local
                .layout
                .atomic_objects
                .iter()
                .zip(&placed.atomic_objects)
            {
                assert_eq!(placed_object.y, local_object.y + placed.y);
                assert_eq!(placed_object.range, local_object.range);
            }
        }
        assert_eq!(retained.blocks[0].layout.y, 0.0);
        assert!(layout.blocks[1].y > layout.blocks[0].y);
        assert!(layout.blocks[2].y > layout.blocks[1].y);

        let body_height = 40.0;
        let expected = crate::pagination::paginate(&layout, body_height);
        let stream = crate::pagination::build_page_items(&retained);
        let plan = crate::pagination::break_page_items(&stream, body_height).unwrap();
        assert_eq!(plan.segments.len(), expected.pages.len());
        for (selected, page) in plan.segments.iter().zip(expected.pages) {
            assert!((selected.start - page.start_y).abs() < 0.01);
            assert!((selected.end - page.end_y).abs() < 0.01);
        }
        let paginated =
            crate::pagination::materialize_paginated_layout(&retained, &stream, &plan).unwrap();
        assert_eq!(paginated.pages.len(), plan.segments.len());
        assert!(
            paginated
                .fragments
                .iter()
                .any(|fragment| matches!(fragment, crate::pagination::PageFragment::Rule(_)))
        );
    }

    #[test]
    fn applies_author_padding_borders_and_dimensions_to_text_blocks() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let text = builder.add_text("boxed text").unwrap();
        builder.append(paragraph, text).unwrap();
        builder.append(root, paragraph).unwrap();
        let border = CssBorder {
            width: CssLength::Px(2_000),
            color: Some(CssColor {
                red: 10,
                green: 20,
                blue: 30,
            }),
        };
        let input = builder
            .intern_style_input(StyleInput::default().with_author(AuthorStyle {
                padding_top: Some(CssLength::Em(500)),
                padding_right: Some(CssLength::Em(1_000)),
                padding_bottom: Some(CssLength::Em(500)),
                padding_left: Some(CssLength::Em(1_000)),
                border_top: Some(border),
                border_right: Some(border),
                border_bottom: Some(border),
                border_left: Some(border),
                width: Some(CssLength::Percent(50_000)),
                min_height: Some(CssLength::Em(4_000)),
                ..AuthorStyle::default()
            }))
            .unwrap();
        builder.set_style_input(paragraph, input).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let options = LayoutOptions {
            width: 200.0,
            body_size: 10.0,
            block_spacing: 0.0,
            ..LayoutOptions::default()
        };
        let layout = layout_flow_document(&document, &styles, &FixedMeasurer, &options);
        let block = &layout.blocks[0];
        let box_layout = block.box_layout.as_ref().unwrap();
        assert_eq!(box_layout.width, 124.0);
        assert_eq!(box_layout.borders[0].color, Some([10, 20, 30]));
        assert_eq!(block.lines[0].runs[0].x, 12.0);
        assert!(block.height >= 40.0);
        assert!(block.lines[0].y > block.y);
    }

    #[test]
    fn centered_text_keeps_natural_word_spacing_on_wrapped_lines() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let text = builder.add_text("alpha beta gamma delta epsilon").unwrap();
        builder.append(paragraph, text).unwrap();
        let input = builder
            .intern_style_input(StyleInput::default().with_author(AuthorStyle {
                text_alignment: Some(TextAlignment::Center),
                ..AuthorStyle::default()
            }))
            .unwrap();
        builder.set_style_input(paragraph, input).unwrap();
        builder.append(root, paragraph).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let options = LayoutOptions {
            width: 100.0,
            body_size: 10.0,
            block_spacing: 0.0,
            ..LayoutOptions::default()
        };
        let layout = layout_flow_document(&document, &styles, &FixedMeasurer, &options);
        let first = &layout.blocks[0].lines[0];
        let left = first.runs.first().unwrap().x;
        let right = first
            .runs
            .iter()
            .map(|run| run.x + run.width)
            .fold(0.0_f32, f32::max);

        assert!(layout.blocks[0].lines.len() > 1);
        assert!(left > 0.0);
        assert!(right < options.width);
        assert!((left - (options.width - (right - left)) / 2.0).abs() < 0.01);
    }

    #[test]
    fn text_indent_participates_in_wrapping_and_author_margins_relax_for_long_words() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let indented = builder.add_node(NodeKind::Paragraph).unwrap();
        let text = builder.add_text("alpha beta gamma delta").unwrap();
        builder.append(indented, text).unwrap();
        let input = builder
            .intern_style_input(StyleInput::default().with_author(AuthorStyle {
                text_indent: Some(CssLength::Em(2_000)),
                ..AuthorStyle::default()
            }))
            .unwrap();
        builder.set_style_input(indented, input).unwrap();
        builder.append(root, indented).unwrap();

        let long = builder
            .add_node(NodeKind::Heading(Heading {
                level: HeadingLevel::new(1).unwrap(),
            }))
            .unwrap();
        let text = builder.add_text("prophecy").unwrap();
        builder.append(long, text).unwrap();
        let input = builder
            .intern_style_input(StyleInput::default().with_author(AuthorStyle {
                margin_left: Some(CssLength::Percent(40_000)),
                ..AuthorStyle::default()
            }))
            .unwrap();
        builder.set_style_input(long, input).unwrap();
        builder.append(root, long).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let options = LayoutOptions {
            width: 100.0,
            body_size: 10.0,
            block_spacing: 0.0,
            ..LayoutOptions::default()
        };
        let layout = layout_flow_document(&document, &styles, &FixedMeasurer, &options);

        assert!(layout.blocks[0].lines.len() > 1);
        assert!(layout.blocks[0].lines[0].runs[0].x >= 20.0);
        assert!(
            layout.blocks[0]
                .lines
                .iter()
                .flat_map(|line| &line.runs)
                .all(|run| run.x + run.width <= options.width + 0.01)
        );
        assert_eq!(layout.blocks[1].lines.len(), 1);
        assert!(
            layout.blocks[1]
                .lines
                .iter()
                .flat_map(|line| &line.runs)
                .all(|run| run.x + run.width <= options.width + 0.01)
        );
    }

    #[test]
    fn legacy_initial_letter_bottom_margin_shortens_the_exclusion_box() {
        let legacy = InitialLetter {
            size_per_mille: 3_200,
            sink: 3,
            legacy_float: true,
            margin_top: None,
            margin_right: None,
            margin_bottom: None,
            margin_left: None,
        };
        assert_eq!(initial_letter_sink(legacy, -16.0, 20.0), 2);
        assert_eq!(initial_letter_sink(legacy, 0.0, 20.0), 3);

        let standard = InitialLetter {
            legacy_float: false,
            ..legacy
        };
        assert_eq!(initial_letter_sink(standard, -16.0, 20.0), 3);
    }

    #[test]
    fn legacy_negative_top_margin_preserves_the_sink_baseline() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let initial = builder.add_node(NodeKind::Inline).unwrap();
        let style = builder
            .intern_style_input(StyleInput::default().with_author(AuthorStyle {
                font_size_per_mille: Some(3_200),
                initial_letter: Some(InitialLetter {
                    size_per_mille: 3_200,
                    sink: 3,
                    legacy_float: true,
                    margin_top: Some(CssLength::Em(-323)),
                    margin_right: None,
                    margin_bottom: Some(CssLength::Em(-325)),
                    margin_left: None,
                }),
                ..AuthorStyle::default()
            }))
            .unwrap();
        builder.set_style_input(initial, style).unwrap();
        let letter = builder.add_text("T").unwrap();
        builder.append(initial, letter).unwrap();
        builder.append(paragraph, initial).unwrap();
        let text = builder
            .add_text("he opening paragraph contains enough words to wrap beside a legacy floated initial before returning to the full line width.")
            .unwrap();
        builder.append(paragraph, text).unwrap();
        builder.append(root, paragraph).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let options = LayoutOptions {
            width: 120.0,
            body_size: 10.0,
            block_spacing: 0.0,
            ..LayoutOptions::default()
        };

        let layout = layout_flow_document(&document, &styles, &FixedMeasurer, &options);
        let lines = &layout.blocks[0].lines;
        assert!(lines.len() >= 3);
        let cap = lines[0].runs.iter().find(|run| run.text == "T").unwrap();
        let cap_baseline = lines[0].y + lines[0].baseline - cap.style.baseline_shift;
        let second_baseline = lines[1].y + lines[1].baseline;
        assert!(
            (cap_baseline - second_baseline).abs() < 0.01,
            "cap baseline {cap_baseline}, second baseline {second_baseline}, shift {}",
            cap.style.baseline_shift
        );
    }

    #[test]
    fn marked_initial_letters_drop_across_three_wrapped_lines() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let initial = builder.add_node(NodeKind::Inline).unwrap();
        let style = builder
            .intern_style_input(StyleInput::default().with_author(AuthorStyle {
                initial_letter: Some(InitialLetter {
                    size_per_mille: 3_000,
                    sink: 3,
                    legacy_float: false,
                    margin_top: None,
                    margin_right: None,
                    margin_bottom: None,
                    margin_left: None,
                }),
                ..AuthorStyle::default()
            }))
            .unwrap();
        builder.set_style_input(initial, style).unwrap();
        let letter = builder.add_text("T").unwrap();
        builder.append(initial, letter).unwrap();
        builder.append(paragraph, initial).unwrap();
        let text = builder
            .add_text("he opening paragraph contains enough words to wrap around a dropped initial for three complete lines before returning to the full measure below it.")
            .unwrap();
        builder.append(paragraph, text).unwrap();
        builder.append(root, paragraph).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let options = LayoutOptions {
            width: 120.0,
            body_size: 10.0,
            block_spacing: 0.0,
            ..LayoutOptions::default()
        };

        let layout = layout_flow_document(&document, &styles, &FixedMeasurer, &options);
        let block = &layout.blocks[0];
        let lines = &block.lines;
        assert!(lines.len() >= 4);
        assert_eq!(block.orphans, 3);
        let cap = lines[0].runs.iter().find(|run| run.text == "T").unwrap();
        assert_eq!(cap.x, 0.0);
        assert!(cap.style.baseline_shift < 0.0);
        let cap_baseline = lines[0].y + lines[0].baseline - cap.style.baseline_shift;
        let third_baseline = lines[2].y + lines[2].baseline;
        assert!((cap_baseline - third_baseline).abs() < 0.01);
        for line in &lines[..3] {
            assert!(
                line.runs
                    .iter()
                    .filter(|run| run.text != "T")
                    .all(|run| run.x >= cap.x + cap.width)
            );
        }
        assert!(lines[3].runs.first().unwrap().x < cap.x + cap.width);
        assert!(
            lines[..3]
                .iter()
                .flat_map(|line| &line.runs)
                .all(|run| run.x + run.width <= options.width + 0.01)
        );
    }

    #[test]
    fn pseudo_first_letter_splits_one_typographic_initial_from_a_text_run() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let style = builder
            .intern_style_input(StyleInput::default().with_author(AuthorStyle {
                first_letter: Some(InitialLetter {
                    size_per_mille: 2_000,
                    sink: 2,
                    legacy_float: false,
                    margin_top: None,
                    margin_right: None,
                    margin_bottom: None,
                    margin_left: None,
                }),
                ..AuthorStyle::default()
            }))
            .unwrap();
        builder.set_style_input(paragraph, style).unwrap();
        let text = builder
            .add_text("“Charlie crossed the courtyard while everyone watched in silence.")
            .unwrap();
        builder.append(paragraph, text).unwrap();
        builder.append(root, paragraph).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let options = LayoutOptions {
            width: 120.0,
            body_size: 10.0,
            block_spacing: 0.0,
            ..LayoutOptions::default()
        };

        let layout = layout_flow_document(&document, &styles, &FixedMeasurer, &options);
        let cap = &layout.blocks[0].lines[0].runs[0];
        assert_eq!(cap.text, "“C");
        assert!(
            layout.blocks[0]
                .lines
                .iter()
                .flat_map(|line| &line.runs)
                .any(|run| run.text.starts_with("harlie"))
        );
    }

    #[test]
    fn lays_out_nested_lists_tasks_and_quotations_with_generated_markers() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let quotation = builder.add_node(NodeKind::Quotation).unwrap();
        let list = builder
            .add_node(NodeKind::List(List {
                kind: ListKind::Ordered,
                start: 3,
                reversed: false,
            }))
            .unwrap();
        let first = builder
            .add_node(NodeKind::ListItem(ListItem { ordinal: None }))
            .unwrap();
        append_checkbox_paragraph(
            &mut builder,
            first,
            "first item with enough words to wrap",
            CheckboxState::Checked,
        );
        append_paragraph(&mut builder, first, "continuation");
        let nested = builder
            .add_node(NodeKind::List(List {
                kind: ListKind::Unordered,
                start: 1,
                reversed: false,
            }))
            .unwrap();
        let nested_item = builder
            .add_node(NodeKind::ListItem(ListItem { ordinal: None }))
            .unwrap();
        append_paragraph(&mut builder, nested_item, "nested");
        builder.append(nested, nested_item).unwrap();
        builder.append(first, nested).unwrap();
        let second = builder
            .add_node(NodeKind::ListItem(ListItem { ordinal: Some(7) }))
            .unwrap();
        append_checkbox_paragraph(
            &mut builder,
            second,
            "second item",
            CheckboxState::Unchecked,
        );
        builder.append(list, first).unwrap();
        builder.append(list, second).unwrap();
        builder.append(quotation, list).unwrap();
        append_paragraph(&mut builder, quotation, "quoted tail");
        builder.append(root, quotation).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let options = LayoutOptions {
            width: 150.0,
            indent: 24.0,
            hyphenation: false,
            ..LayoutOptions::default()
        };

        let layout = layout_flow_document(&document, &styles, &FixedMeasurer, &options);

        assert_eq!(layout.blocks.len(), 5);
        let marker = &layout.blocks[0].lines[0].runs[0];
        assert_eq!(marker.text, "☑");
        assert!(marker.range.is_empty());
        assert_eq!(marker.range.start, layout.blocks[0].range.start);
        let first_content_x = layout.blocks[0].lines[0].runs[1].x;
        assert!(layout.blocks[0].lines.len() >= 2);
        assert_eq!(layout.blocks[0].lines[1].runs[0].x, first_content_x);
        assert_eq!(layout.blocks[1].lines[0].runs[0].x, first_content_x);
        assert_eq!(layout.blocks[2].lines[0].runs[0].text, "•");
        assert!(layout.blocks[2].lines[0].runs[1].x > first_content_x);
        assert_eq!(layout.blocks[3].lines[0].runs[0].text, "☐");
        assert_eq!(layout.blocks[4].lines[0].runs[0].text, "quoted");
        for block in &layout.blocks {
            assert!(block.quoted);
            assert_eq!(block.quote_bars.len(), 1);
            assert!(
                block
                    .lines
                    .iter()
                    .flat_map(|line| &line.runs)
                    .any(|run| run.style.italic)
            );
        }
        assert_eq!(layout.blocks[0].quote_bars, layout.blocks[2].quote_bars);
    }

    #[test]
    fn list_markers_keep_an_outer_inset_and_hanging_columns() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let list = builder
            .add_node(NodeKind::List(List {
                kind: ListKind::Unordered,
                start: 1,
                reversed: false,
            }))
            .unwrap();
        let item = builder
            .add_node(NodeKind::ListItem(ListItem { ordinal: None }))
            .unwrap();
        append_paragraph(&mut builder, item, "top");
        append_paragraph(&mut builder, item, "continuation");
        let nested = builder
            .add_node(NodeKind::List(List {
                kind: ListKind::Unordered,
                start: 1,
                reversed: false,
            }))
            .unwrap();
        let nested_item = builder
            .add_node(NodeKind::ListItem(ListItem { ordinal: None }))
            .unwrap();
        append_paragraph(&mut builder, nested_item, "nested");
        builder.append(nested, nested_item).unwrap();
        builder.append(item, nested).unwrap();
        builder.append(list, item).unwrap();
        builder.append(root, list).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let options = LayoutOptions {
            width: 400.0,
            indent: 40.0,
            ..LayoutOptions::default()
        };

        let layout = layout_flow_document(&document, &styles, &FixedMeasurer, &options);

        assert_eq!(layout.blocks[0].lines[0].runs[0].text, "•");
        assert_eq!(layout.blocks[0].lines[0].runs[0].x, 40.0);
        assert_eq!(layout.blocks[0].lines[0].runs[1].x, 80.0);
        assert_eq!(layout.blocks[1].lines[0].runs[0].x, 80.0);
        assert_eq!(layout.blocks[2].lines[0].runs[0].text, "•");
        assert_eq!(layout.blocks[2].lines[0].runs[0].x, 80.0);
        assert_eq!(layout.blocks[2].lines[0].runs[1].x, 120.0);
    }

    #[test]
    fn replaces_markers_per_item_for_mixed_checkbox_siblings() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let list = builder
            .add_node(NodeKind::List(List {
                kind: ListKind::Unordered,
                start: 1,
                reversed: false,
            }))
            .unwrap();
        for (label, state) in [
            ("ordinary", None),
            ("open", Some(CheckboxState::Unchecked)),
            ("done", Some(CheckboxState::Checked)),
        ] {
            let item = builder
                .add_node(NodeKind::ListItem(ListItem { ordinal: None }))
                .unwrap();
            if let Some(state) = state {
                append_checkbox_paragraph(&mut builder, item, label, state);
            } else {
                append_paragraph(&mut builder, item, label);
            }
            builder.append(list, item).unwrap();
        }
        builder.append(root, list).unwrap();
        let document = builder.finish().unwrap();
        assert_eq!(document.text().as_str(), "ordinary\n\nopen\n\ndone");

        let styles = document.resolve_styles(StyleContextKey::default());
        let layout = layout_flow_document(
            &document,
            &styles,
            &FixedMeasurer,
            &LayoutOptions::default(),
        );
        let markers = layout
            .blocks
            .iter()
            .map(|block| block.lines[0].runs[0].text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(markers, ["•", "☐", "☑"]);
    }

    #[test]
    fn paints_nonleading_checkboxes_inline_without_canonical_text() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let list = builder
            .add_node(NodeKind::List(List {
                kind: ListKind::Unordered,
                start: 1,
                reversed: false,
            }))
            .unwrap();
        let item = builder
            .add_node(NodeKind::ListItem(ListItem { ordinal: None }))
            .unwrap();
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let before = builder.add_text("before ").unwrap();
        let checkbox = builder
            .add_node(NodeKind::Checkbox(Checkbox {
                state: CheckboxState::Unchecked,
            }))
            .unwrap();
        let marker_style = builder
            .intern_style_input(StyleInput::semantic(SemanticStyleKey::CheckboxMarker))
            .unwrap();
        builder.set_style_input(checkbox, marker_style).unwrap();
        let after = builder.add_text(" after").unwrap();
        builder.append(paragraph, before).unwrap();
        builder.append(paragraph, checkbox).unwrap();
        builder.append(paragraph, after).unwrap();
        builder.append(item, paragraph).unwrap();
        builder.append(list, item).unwrap();
        builder.append(root, list).unwrap();
        let document = builder.finish().unwrap();
        assert_eq!(document.text().as_str(), "before  after");
        assert!(document.get_node(checkbox).unwrap().text_range.is_empty());

        let styles = document.resolve_styles(StyleContextKey::default());
        let layout = layout_flow_document(
            &document,
            &styles,
            &FixedMeasurer,
            &LayoutOptions::default(),
        );
        let runs = &layout.blocks[0].lines[0].runs;
        let checkbox = runs.iter().find(|run| run.text == "☐").unwrap();
        assert!(checkbox.range.is_empty());
        assert_eq!(runs[0].text, "•");
    }

    #[test]
    fn respects_reversed_ordinals_and_nested_code_and_rule_geometry() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let list = builder
            .add_node(NodeKind::List(List {
                kind: ListKind::Ordered,
                start: 3,
                reversed: true,
            }))
            .unwrap();
        for (index, ordinal) in [None, Some(10), None].into_iter().enumerate() {
            let item = builder
                .add_node(NodeKind::ListItem(ListItem { ordinal }))
                .unwrap();
            if index == 0 {
                let preformatted = builder
                    .add_node(NodeKind::Preformatted(Preformatted {
                        language: None,
                        content_kind: PreformattedKind::PlainText,
                    }))
                    .unwrap();
                let text = builder.add_text("code").unwrap();
                builder.append(preformatted, text).unwrap();
                builder.append(item, preformatted).unwrap();
                let rule = builder.add_node(NodeKind::ThematicBreak).unwrap();
                builder.append(item, rule).unwrap();
            } else if index == 1 {
                let quotation = builder.add_node(NodeKind::Quotation).unwrap();
                append_paragraph(&mut builder, quotation, "quoted item");
                builder.append(item, quotation).unwrap();
            } else {
                append_paragraph(&mut builder, item, "item");
            }
            builder.append(list, item).unwrap();
        }
        builder.append(root, list).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let options = LayoutOptions {
            width: 200.0,
            indent: 20.0,
            code_block_indent: 0.25,
            nested_code_block_indent: 1.5,
            ..LayoutOptions::default()
        };

        let layout = layout_flow_document(&document, &styles, &FixedMeasurer, &options);

        assert_eq!(layout.blocks.len(), 4);
        assert_eq!(layout.blocks[0].lines[0].runs[0].text, "3.");
        let code = layout.blocks[0].lines[0].runs.last().unwrap();
        assert_eq!(code.text, "code");
        assert_eq!(layout.blocks[1].kind, LayoutBlockKind::Rule);
        assert_eq!(
            code.x - layout.blocks[1].rule_x,
            options.indent * options.nested_code_block_indent
        );
        assert_eq!(
            layout.blocks[1].rule_width,
            options.width - layout.blocks[1].rule_x * 2.0
        );
        assert_eq!(layout.blocks[2].lines[0].runs[0].text, "10.");
        assert!(layout.blocks[2].quoted);
        assert_eq!(
            layout.blocks[2].quote_bars,
            vec![layout.blocks[1].rule_x + options.indent - options.quote_bar_offset]
        );
        assert_eq!(layout.blocks[3].lines[0].runs[0].text, "9.");
        assert_eq!(layout.blocks[3].lines[0].runs[1].x, layout.blocks[1].rule_x);
    }

    #[test]
    fn lays_out_markdown_footnote_definitions_with_noncanonical_labels() {
        let outcome = crate::formats::markdown::parse(
            "First[^n], second[^n].\n\n[^n]: definition\n\n[^orphan]: orphan",
            crate::formats::markdown::MarkdownOptions::default(),
        )
        .unwrap();
        let styles = outcome.document.resolve_styles(StyleContextKey::default());

        let layout = layout_flow_document(
            &outcome.document,
            &styles,
            &FixedMeasurer,
            &LayoutOptions::default(),
        );

        let definition = &layout.blocks[layout.blocks.len() - 2];
        let marker = &definition.lines[0].runs[0];
        assert_eq!(marker.text, "1");
        assert!(marker.range.is_empty());
        assert_eq!(marker.range.start, definition.range.start);
        assert_eq!(
            marker.style.size,
            LayoutOptions::default().body_size * LayoutOptions::default().superscript_scale
        );
        assert!(marker.style.baseline_shift > 0.0);
        assert_eq!(definition.lines[0].runs[1].text, "definition");
        let label_gap = definition.lines[0].runs[1].x - marker.x - marker.width;
        assert_eq!(label_gap, FixedMeasurer.measure(" ", &marker.style).width);
        assert!(label_gap < LayoutOptions::default().indent);
        let return_arrow = definition.lines[0].runs.last().unwrap();
        assert_eq!(return_arrow.text, "↑");
        assert!(return_arrow.range.is_empty());
        assert!(return_arrow.style.baseline_shift > 0.0);
        assert!(matches!(
            return_arrow.generated_action,
            Some(GeneratedAction::NoteReturn { .. })
        ));
        let orphan = layout.blocks.last().unwrap();
        assert_eq!(orphan.lines[0].runs[0].text, "Note");
        assert!(orphan.lines[0].runs[0].range.is_empty());
        assert!(
            orphan
                .lines
                .iter()
                .flat_map(|line| &line.runs)
                .all(|run| run.generated_action.is_none())
        );
        assert!(!outcome.document.text().as_str().contains(['[', '↑']));

        let narrow_options = LayoutOptions {
            width: 72.0,
            ..LayoutOptions::default()
        };
        let narrow =
            layout_flow_document(&outcome.document, &styles, &FixedMeasurer, &narrow_options);
        let narrow_definition = &narrow.blocks[narrow.blocks.len() - 2];
        let arrow_line = narrow_definition
            .lines
            .iter()
            .find(|line| line.runs.iter().any(|run| run.generated_action.is_some()))
            .unwrap();
        let arrow = arrow_line
            .runs
            .iter()
            .find(|run| run.generated_action.is_some())
            .unwrap();
        assert!(arrow.x + arrow.width <= narrow_options.width);
        assert_eq!(arrow_line.y, narrow_definition.lines.last().unwrap().y);

        let unresolved = crate::formats::markdown::parse(
            "Missing[^missing].",
            crate::formats::markdown::MarkdownOptions::default(),
        )
        .unwrap();
        let unresolved_styles = unresolved
            .document
            .resolve_styles(StyleContextKey::default());
        let unresolved_layout = layout_flow_document(
            &unresolved.document,
            &unresolved_styles,
            &FixedMeasurer,
            &LayoutOptions::default(),
        );
        assert_eq!(unresolved_layout.blocks.len(), 1);
        assert!(
            unresolved_layout.blocks[0]
                .lines
                .iter()
                .flat_map(|line| &line.runs)
                .all(|run| !run.range.is_empty())
        );
        assert_eq!(unresolved.document.text().as_str(), "Missing[^missing].");
    }

    #[test]
    fn generated_note_return_follows_a_multi_block_definition() {
        let outcome = crate::formats::markdown::parse(
            "Reference[^n].\n\n[^n]: first block\n\n    second block\n",
            crate::formats::markdown::MarkdownOptions::default(),
        )
        .unwrap();
        let styles = outcome.document.resolve_styles(StyleContextKey::default());
        let layout = layout_flow_document(
            &outcome.document,
            &styles,
            &FixedMeasurer,
            &LayoutOptions::default(),
        );
        let actions: Vec<_> = layout
            .blocks
            .iter()
            .enumerate()
            .flat_map(|(block_index, block)| {
                block.lines.iter().flat_map(move |line| {
                    line.runs
                        .iter()
                        .filter(|run| run.generated_action.is_some())
                        .map(move |_| block_index)
                })
            })
            .collect();
        assert_eq!(actions, vec![layout.blocks.len() - 1]);
        let definition = &layout.blocks[layout.blocks.len() - 2];
        let continuation = layout.blocks.last().unwrap();
        assert_eq!(
            definition.lines[0].runs[1].x,
            continuation.lines[0].runs[0].x
        );
        assert_eq!(
            layout
                .blocks
                .last()
                .unwrap()
                .lines
                .last()
                .unwrap()
                .runs
                .last()
                .unwrap()
                .text,
            "↑"
        );
    }

    #[test]
    fn lays_out_headings_preformatted_content_and_thematic_breaks() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let heading = builder
            .add_node(NodeKind::Heading(Heading {
                level: HeadingLevel::H2,
            }))
            .unwrap();
        let heading_text = builder.add_text("flow heading").unwrap();
        builder.append(heading, heading_text).unwrap();
        builder.append(root, heading).unwrap();
        let preformatted = builder
            .add_node(NodeKind::Preformatted(Preformatted {
                language: Some("rust".to_owned()),
                content_kind: PreformattedKind::SourceCode,
            }))
            .unwrap();
        let code = builder.add_text("  let x = 1;\n\nx").unwrap();
        builder.append(preformatted, code).unwrap();
        builder.append(root, preformatted).unwrap();
        let plain = builder
            .add_node(NodeKind::Preformatted(Preformatted {
                language: Some("unknown-language".to_owned()),
                content_kind: PreformattedKind::PlainText,
            }))
            .unwrap();
        let plain_text = builder.add_text("\t  keep  \n").unwrap();
        builder.append(plain, plain_text).unwrap();
        builder.append(root, plain).unwrap();
        let rule = builder.add_node(NodeKind::ThematicBreak).unwrap();
        builder.append(root, rule).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let options = LayoutOptions {
            heading_scales: [2.0, 1.5, 1.25, 1.0, 1.0, 1.0],
            heading_tracking: [0.0, 0.01, 0.0, 0.0, 0.0, 0.0],
            ..LayoutOptions::default()
        };

        let layout = layout_flow_document(&document, &styles, &FixedMeasurer, &options);

        assert_eq!(layout.blocks.len(), 4);
        let heading = &layout.blocks[0];
        assert_eq!(heading.kind, LayoutBlockKind::Heading);
        assert_eq!(heading.lines[0].runs[0].style.size, options.body_size * 1.5);
        assert_eq!(
            heading.lines[0].runs[0].style.letter_spacing,
            options.body_size * 1.5 * 0.01
        );
        assert_eq!(heading.lines[0].runs[0].style.weight, FontWeight::Bold);

        let code = &layout.blocks[1];
        assert_eq!(code.kind, LayoutBlockKind::Code);
        assert_eq!(code.lines.len(), 3);
        assert_eq!(
            code.lines[0]
                .runs
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>(),
            "  let x = 1;"
        );
        assert!(code.lines[1].runs.is_empty());
        assert_eq!(code.lines[0].range.end, code.lines[1].range.start);
        assert_eq!(code.lines[1].range.end, code.lines[2].range.start);
        assert_eq!(code.lines[2].range.end, code.range.end);
        assert!(
            code.lines
                .iter()
                .flat_map(|line| &line.runs)
                .all(|run| run.style.family == FontFamily::Monospace)
        );

        let plain = &layout.blocks[2];
        assert_eq!(plain.lines.len(), 2);
        assert_eq!(plain.lines[0].runs[0].text, "\t  keep  ");
        assert_eq!(plain.lines[0].runs[0].syntax, SyntaxKind::Plain);
        assert_eq!(plain.lines[0].range.end, plain.lines[1].range.start);
        assert_eq!(plain.lines[1].range.start, plain.range.end);
        assert!(plain.lines[1].runs.is_empty());

        let rule = &layout.blocks[3];
        assert_eq!(rule.kind, LayoutBlockKind::Rule);
        assert!(rule.rule);
        assert!(rule.lines.is_empty());
        assert_eq!(rule.atomic_objects.len(), 1);
        assert_eq!(rule.atomic_objects[0].range, rule.range);
        assert!(matches!(
            rule.atomic_objects[0].kind,
            AtomicLayoutKind::ThematicBreak
        ));
        assert_eq!(rule.height, options.block_spacing);
        assert_eq!(rule.y, plain.y + plain.height + options.block_spacing);
    }

    #[test]
    fn headings_do_not_gain_automatic_hyphenation_points() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let heading = builder
            .add_node(NodeKind::Heading(Heading {
                level: HeadingLevel::H1,
            }))
            .unwrap();
        let text = builder.add_text("extraordinarilylongheading").unwrap();
        builder.append(heading, text).unwrap();
        builder.append(root, heading).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let options = LayoutOptions {
            width: 30.0,
            body_size: 10.0,
            hyphenation: true,
            ..LayoutOptions::default()
        };

        let layout = layout_flow_document(&document, &styles, &FixedMeasurer, &options);

        let runs = layout.blocks[0]
            .lines
            .iter()
            .flat_map(|line| &line.runs)
            .collect::<Vec<_>>();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "extraordinarilylongheading");
        assert!(runs.iter().all(|run| run.text != "-"));
    }

    #[test]
    fn lays_out_basic_flow_paragraphs_with_styles_ranges_and_hard_breaks() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let first = builder.add_node(NodeKind::Paragraph).unwrap();
        let strong = builder
            .add_node(NodeKind::InlineSemantic(InlineSemantic {
                role: InlineRole::Strong,
                value: None,
            }))
            .unwrap();
        let hello = builder.add_text("hello world").unwrap();
        let hard_break = builder.add_node(NodeKind::HardBreak).unwrap();
        let code = builder
            .add_node(NodeKind::InlineSemantic(InlineSemantic {
                role: InlineRole::Code,
                value: None,
            }))
            .unwrap();
        let tail = builder.add_text("tail").unwrap();
        builder.append(strong, hello).unwrap();
        builder.append(code, tail).unwrap();
        builder.append(first, strong).unwrap();
        builder.append(first, hard_break).unwrap();
        builder.append(first, code).unwrap();
        builder.append(root, first).unwrap();
        let second = builder.add_node(NodeKind::Paragraph).unwrap();
        let second_text = builder.add_text("second paragraph").unwrap();
        builder.append(second, second_text).unwrap();
        builder.append(root, second).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let options = LayoutOptions {
            width: 200.0,
            hyphenation: false,
            ..LayoutOptions::default()
        };

        let layout = layout_flow_document(&document, &styles, &FixedMeasurer, &options);

        assert_eq!(layout.blocks.len(), 2);
        assert_eq!(layout.blocks[0].node, Some(first));
        assert_eq!(layout.blocks[1].node, Some(second));
        assert_eq!(layout.blocks[0].lines.len(), 2);
        assert_eq!(
            layout.blocks[0].lines[0].range,
            TextRange { start: 0, end: 12 }
        );
        assert_eq!(
            layout.blocks[0].lines[1].range,
            TextRange { start: 12, end: 16 }
        );
        assert_eq!(layout.blocks[0].lines[0].runs[0].text, "hello");
        assert_eq!(
            layout.blocks[0].lines[0].runs[0].style.weight,
            FontWeight::Bold
        );
        assert_eq!(
            layout.blocks[0].lines[1].runs[0].style.family,
            FontFamily::Monospace
        );
        assert_eq!(layout.blocks[0].range, TextRange { start: 0, end: 16 });
        assert_eq!(layout.blocks[1].range, TextRange { start: 18, end: 34 });
        assert_eq!(layout.blocks[1].lines[0].range, layout.blocks[1].range);
    }

    #[test]
    fn lays_out_plain_and_hyphenating_break_opportunities() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        for hyphen_when_broken in [false, true] {
            let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
            let left = builder.add_text("before").unwrap();
            let opportunity = builder
                .add_node(NodeKind::BreakOpportunity(
                    crate::model::flow::BreakOpportunity { hyphen_when_broken },
                ))
                .unwrap();
            let right = builder.add_text("after").unwrap();
            builder.append(paragraph, left).unwrap();
            builder.append(paragraph, opportunity).unwrap();
            builder.append(paragraph, right).unwrap();
            builder.append(root, paragraph).unwrap();
        }
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let options = LayoutOptions {
            width: 31.0,
            body_size: 10.0,
            hyphenation: false,
            ..LayoutOptions::default()
        };

        let layout = layout_flow_document(&document, &styles, &FixedMeasurer, &options);

        assert_eq!(layout.blocks[0].lines.len(), 2);
        assert!(
            layout.blocks[0].lines[0]
                .runs
                .iter()
                .all(|run| run.text != "-")
        );
        assert_eq!(layout.blocks[1].lines.len(), 2);
        let hyphen = layout.blocks[1].lines[0].runs.last().unwrap();
        assert_eq!(hyphen.text, "-");
        assert!(hyphen.range.is_empty());
    }

    #[test]
    fn trailing_hard_break_places_the_empty_line_at_its_canonical_end() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let prefix = builder.add_node(NodeKind::Paragraph).unwrap();
        let prefix_text = builder.add_text("prefix").unwrap();
        builder.append(prefix, prefix_text).unwrap();
        builder.append(root, prefix).unwrap();
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let text = builder.add_text("tail").unwrap();
        let hard_break = builder.add_node(NodeKind::HardBreak).unwrap();
        builder.append(paragraph, text).unwrap();
        builder.append(paragraph, hard_break).unwrap();
        builder.append(root, paragraph).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());

        let layout = layout_flow_document(
            &document,
            &styles,
            &FixedMeasurer,
            &LayoutOptions::default(),
        );

        let block = &layout.blocks[1];
        assert_eq!(block.lines.len(), 2);
        assert_eq!(block.lines[0].range, block.range);
        assert_eq!(
            block.lines[1].range,
            TextRange {
                start: block.range.end,
                end: block.range.end,
            }
        );
    }

    #[test]
    fn wraps_utf8_at_nonzero_canonical_offsets_and_preserves_line_ranges() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        for text in ["prefix", "éé éé"] {
            let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
            let text = builder.add_text(text).unwrap();
            builder.append(paragraph, text).unwrap();
            builder.append(root, paragraph).unwrap();
        }
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let options = LayoutOptions {
            width: 22.0,
            body_size: 10.0,
            hyphenation: false,
            ..LayoutOptions::default()
        };

        let layout = layout_flow_document(&document, &styles, &FixedMeasurer, &options);
        let block = &layout.blocks[1];
        assert!(block.lines.len() >= 2);
        assert_eq!(block.lines.first().unwrap().range.start, block.range.start);
        assert_eq!(block.lines.last().unwrap().range.end, block.range.end);
        for line in &block.lines {
            assert!(line.range.start >= block.range.start);
            assert!(line.range.end <= block.range.end);
            assert!(document.text().as_str().is_char_boundary(line.range.start));
            assert!(document.text().as_str().is_char_boundary(line.range.end));
        }
    }

    #[test]
    fn empty_flow_paragraphs_keep_their_canonical_positions() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let first = builder.add_node(NodeKind::Paragraph).unwrap();
        let middle = builder.add_node(NodeKind::Paragraph).unwrap();
        let text = builder.add_text("body").unwrap();
        let last = builder.add_node(NodeKind::Paragraph).unwrap();
        builder.append(middle, text).unwrap();
        builder.append(root, first).unwrap();
        builder.append(root, middle).unwrap();
        builder.append(root, last).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());

        let layout = layout_flow_document(
            &document,
            &styles,
            &FixedMeasurer,
            &LayoutOptions::default(),
        );

        assert_eq!(layout.blocks.len(), 3);
        assert_eq!(layout.blocks[0].lines[0].range, layout.blocks[0].range);
        assert!(layout.blocks[0].lines[0].runs.is_empty());
        assert_eq!(layout.blocks[2].lines[0].range, layout.blocks[2].range);
        assert!(layout.blocks[2].lines[0].runs.is_empty());
    }

    #[test]
    fn basic_flow_traversal_handles_deep_transparent_trees_iteratively() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        builder.append(root, paragraph).unwrap();
        let mut parent = paragraph;
        for _ in 0..2_048 {
            let inline = builder.add_node(NodeKind::Inline).unwrap();
            builder.append(parent, inline).unwrap();
            parent = inline;
        }
        let text = builder.add_text("deep").unwrap();
        builder.append(parent, text).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());

        let layout = layout_flow_document(
            &document,
            &styles,
            &FixedMeasurer,
            &LayoutOptions::default(),
        );

        assert_eq!(layout.blocks[0].lines[0].runs[0].text, "deep");
    }

    #[test]
    fn lays_out_captioned_spanning_tables_and_nested_cell_flow() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let figure = builder.add_node(NodeKind::Figure).unwrap();
        let table = builder
            .add_node(NodeKind::Table(Table {
                column_count: NonZeroU32::new(3).unwrap(),
            }))
            .unwrap();
        let caption = builder.add_node(NodeKind::TableCaption).unwrap();
        append_paragraph(&mut builder, caption, "caption one");
        append_paragraph(&mut builder, caption, "caption two");
        builder.append(table, caption).unwrap();

        let head = builder
            .add_node(NodeKind::RowGroup(RowGroup::Head))
            .unwrap();
        let head_row = builder.add_node(NodeKind::Row).unwrap();
        let h1 = append_cell(&mut builder, head_row, CellKind::Header, 0, 1, 1);
        append_paragraph(&mut builder, h1, "H1");
        let h2 = append_cell(&mut builder, head_row, CellKind::Header, 1, 2, 1);
        append_paragraph(&mut builder, h2, "H2 span");
        builder.append(head, head_row).unwrap();
        builder.append(table, head).unwrap();

        let body = builder
            .add_node(NodeKind::RowGroup(RowGroup::Body))
            .unwrap();
        let first_row = builder.add_node(NodeKind::Row).unwrap();
        let a = append_cell(&mut builder, first_row, CellKind::Data, 0, 2, 1);
        let flow = builder.add_node(NodeKind::Flow).unwrap();
        append_paragraph(&mut builder, flow, "first flow");
        append_paragraph(&mut builder, flow, "second flow");
        builder.append(a, flow).unwrap();
        let spanning = append_cell(&mut builder, first_row, CellKind::Data, 2, 1, 2);
        append_paragraph(&mut builder, spanning, "row spanning content wraps deeply");
        builder.append(body, first_row).unwrap();

        let second_row = builder.add_node(NodeKind::Row).unwrap();
        let nested = append_cell(&mut builder, second_row, CellKind::Data, 0, 2, 1);
        let quotation = builder.add_node(NodeKind::Quotation).unwrap();
        let list = builder
            .add_node(NodeKind::List(List {
                kind: ListKind::Unordered,
                start: 1,
                reversed: false,
            }))
            .unwrap();
        let item = builder
            .add_node(NodeKind::ListItem(ListItem { ordinal: None }))
            .unwrap();
        append_paragraph(&mut builder, item, "nested cell item");
        builder.append(list, item).unwrap();
        builder.append(quotation, list).unwrap();
        append_paragraph(&mut builder, quotation, "quoted continuation");
        builder.append(nested, quotation).unwrap();
        builder.append(body, second_row).unwrap();

        let third_row = builder.add_node(NodeKind::Row).unwrap();
        let _empty = append_cell(&mut builder, third_row, CellKind::Data, 0, 1, 1);
        let tail = append_cell(&mut builder, third_row, CellKind::Data, 1, 2, 1);
        append_paragraph(&mut builder, tail, "tail");
        let nested_table = builder
            .add_node(NodeKind::Table(Table {
                column_count: NonZeroU32::new(2).unwrap(),
            }))
            .unwrap();
        let nested_body = builder
            .add_node(NodeKind::RowGroup(RowGroup::Body))
            .unwrap();
        let nested_row = builder.add_node(NodeKind::Row).unwrap();
        let nested_x = append_cell(&mut builder, nested_row, CellKind::Data, 0, 1, 1);
        append_paragraph(&mut builder, nested_x, "nested X");
        let nested_y = append_cell(&mut builder, nested_row, CellKind::Data, 1, 1, 1);
        append_paragraph(&mut builder, nested_y, "nested Y");
        builder.append(nested_body, nested_row).unwrap();
        builder.append(nested_table, nested_body).unwrap();
        builder.append(tail, nested_table).unwrap();
        builder.append(body, third_row).unwrap();
        builder.append(table, body).unwrap();
        builder.append(figure, table).unwrap();
        builder.append(root, figure).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let options = LayoutOptions {
            width: 150.0,
            body_size: 10.0,
            line_spacing: 1.0,
            block_spacing: 4.0,
            table_horizontal_padding: 3.0,
            table_vertical_padding: 2.0,
            table_min_column_width: 1.0,
            ..LayoutOptions::default()
        };

        let layout = layout_flow_document(&document, &styles, &FixedMeasurer, &options);

        assert_eq!(layout.blocks.len(), 1);
        let block = &layout.blocks[0];
        let table = block.table.as_ref().unwrap();
        assert_eq!(table.header_row_count, 1);
        assert_eq!(table.row_heights.len(), 4);
        assert_eq!(table.row_spans, vec![(1, 3)]);
        assert!(table.row_y > block.y);
        let caption_one = block
            .lines
            .iter()
            .find(|line| {
                line.runs
                    .iter()
                    .any(|run| run.text == "caption" && run.range.start == 0)
            })
            .unwrap_or(&block.lines[0]);
        let caption_lines = block
            .lines
            .iter()
            .filter(|line| {
                line.runs
                    .iter()
                    .any(|run| run.text == "caption" || run.text == "one" || run.text == "two")
            })
            .collect::<Vec<_>>();
        assert!(caption_lines.len() >= 2);
        assert!(caption_lines.iter().all(|line| line.y < table.row_y));
        assert!(caption_one.y < table.row_y);
        let first_flow_y = block
            .lines
            .iter()
            .find(|line| line.runs.iter().any(|run| run.text == "first"))
            .unwrap()
            .y;
        let second_flow_y = block
            .lines
            .iter()
            .find(|line| line.runs.iter().any(|run| run.text == "second"))
            .unwrap()
            .y;
        assert!(second_flow_y > first_flow_y);
        assert!(
            block
                .lines
                .iter()
                .flat_map(|line| &line.runs)
                .any(|run| run.text == "•" && run.range.is_empty())
        );
        assert_eq!(table.quote_bars.len(), 1);
        let quote = &table.quote_bars[0];
        let quoted_lines = block
            .lines
            .iter()
            .filter(|line| line.runs.iter().any(|run| run.style.italic))
            .collect::<Vec<_>>();
        let first_quoted = quoted_lines.first().unwrap();
        let last_quoted = quoted_lines.last().unwrap();
        assert!(quote.y <= first_quoted.y);
        assert!(quote.y + quote.height >= last_quoted.y + last_quoted.height);
        assert!(
            block
                .lines
                .iter()
                .flat_map(|line| &line.runs)
                .any(|run| run.text == "X")
        );
        assert_eq!(table.rules.len(), 3);
        let empty_range = document
            .preorder(document.root())
            .filter_map(|id| document.get_node(id))
            .find(|node| matches!(node.kind, NodeKind::Cell(_)) && node.text_range.is_empty())
            .unwrap()
            .text_range;
        let empty_line = block
            .lines
            .iter()
            .find(|line| line.runs.is_empty() && line.range == text_range(empty_range))
            .unwrap();
        assert_eq!(
            layout.text_range_at(table.x + 1.0, empty_line.y + 1.0),
            Some(text_range(empty_range))
        );
        assert_eq!(
            layout.y_for_text(empty_range.start.as_usize()),
            Some(empty_line.y)
        );
    }

    #[test]
    fn lays_out_semantic_table_alignment_wrapping_and_pagination() {
        let outcome = crate::formats::markdown::parse(
            "| Left | Right |\n|:-----|------:|\n| alpha beta gamma | 1 |\n| delta epsilon | 22 |\n| zeta eta theta | 333 |\n| iota kappa | 4444 |",
            crate::formats::markdown::MarkdownOptions::default(),
        )
        .unwrap();
        let styles = outcome.document.resolve_styles(StyleContextKey::default());
        let options = LayoutOptions {
            width: 80.0,
            body_size: 10.0,
            line_spacing: 1.0,
            block_spacing: 8.0,
            table_horizontal_padding: 4.0,
            table_vertical_padding: 2.0,
            table_min_column_width: 1.0,
            hyphenation: false,
            ..LayoutOptions::default()
        };

        let layout = layout_flow_document(&outcome.document, &styles, &FixedMeasurer, &options);
        let block = &layout.blocks[0];
        let table = block.table.as_ref().unwrap();
        assert_eq!(block.kind, LayoutBlockKind::Table);
        assert_eq!(table.header_row_count, 1);
        assert_eq!(table.row_heights.len(), 5);
        assert_eq!(table.column_widths.len(), 2);
        assert!(table.column_widths.iter().sum::<f32>() <= options.width + 0.01);
        assert!(table.rules.iter().any(|rule| {
            rule.kind == TableRuleKind::Light
                && (rule.y - (table.row_y + table.row_heights[0])).abs() < 0.01
        }));
        let runs = block
            .lines
            .iter()
            .flat_map(|line| &line.runs)
            .collect::<Vec<_>>();
        let left_header = runs.iter().find(|run| run.text == "Left").unwrap();
        let right_header = runs.iter().find(|run| run.text == "Right").unwrap();
        assert_eq!(left_header.style.weight, FontWeight::Bold);
        assert_eq!(right_header.style.weight, FontWeight::Bold);
        assert!((left_header.x - (table.x + options.table_horizontal_padding)).abs() < 0.01);
        let numeric = runs.iter().find(|run| run.text == "4444").unwrap();
        let right_edge = table.x + table.width - options.table_horizontal_padding;
        assert!(
            (numeric.x + numeric.width - right_edge).abs() < 0.01,
            "numeric={} width={} right={} columns={:?}",
            numeric.x,
            numeric.width,
            right_edge,
            table.column_widths,
        );
        assert!(
            block
                .lines
                .iter()
                .filter(|line| {
                    line.runs
                        .iter()
                        .any(|run| run.text == "alpha" || run.text == "beta" || run.text == "gamma")
                })
                .count()
                >= 2
        );
        for run in runs {
            if !run.range.is_empty() {
                assert_eq!(
                    outcome
                        .document
                        .text()
                        .as_str()
                        .get(run.range.start..run.range.end),
                    Some(run.text.as_str())
                );
            }
        }

        let page_height = options.block_spacing + table.row_heights[0] + table.row_heights[1] + 0.5;
        let pages = crate::pagination::paginate(&layout, page_height);
        assert!(pages.pages.len() >= 2);
        assert!(pages.pages[0].repeated_table_header.is_none());
        assert!(pages.pages.iter().skip(1).any(|page| {
            page.repeated_table_header.is_some_and(|header| {
                (header.source_y - table.row_y).abs() < 0.01
                    && (header.height - table.row_heights[0]).abs() < 0.01
            })
        }));
    }

    #[test]
    fn nested_table_fallback_is_iterative_and_readable() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let mut parent = root;
        for _ in 0..512 {
            let table = builder
                .add_node(NodeKind::Table(Table {
                    column_count: NonZeroU32::new(1).unwrap(),
                }))
                .unwrap();
            let body = builder
                .add_node(NodeKind::RowGroup(RowGroup::Body))
                .unwrap();
            let row = builder.add_node(NodeKind::Row).unwrap();
            let cell = append_cell(&mut builder, row, CellKind::Data, 0, 1, 1);
            builder.append(body, row).unwrap();
            builder.append(table, body).unwrap();
            builder.append(parent, table).unwrap();
            parent = cell;
        }
        let resource = builder
            .add_resource(Resource {
                identity: "deep.png".into(),
                media_type: None,
            })
            .unwrap();
        let image = builder
            .add_node(NodeKind::Image(Image {
                resource,
                alternative: ImageAlternative::Text(NonEmptyText::new("deep image").unwrap()),
                title: None,
                intrinsic_size: Some(IntrinsicSize {
                    width: PositiveCssPixels::new(20.0).unwrap(),
                    height: PositiveCssPixels::new(10.0).unwrap(),
                }),
            }))
            .unwrap();
        builder.append(parent, image).unwrap();
        let rule = builder.add_node(NodeKind::ThematicBreak).unwrap();
        builder.append(parent, rule).unwrap();
        append_paragraph(&mut builder, parent, "deep table text");
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());

        let layout = layout_flow_document(
            &document,
            &styles,
            &FixedMeasurer,
            &LayoutOptions::default(),
        );

        assert_eq!(layout.blocks.len(), 1);
        assert_eq!(layout.blocks[0].atomic_objects.len(), 2);
        assert!(
            layout.blocks[0]
                .atomic_objects
                .iter()
                .any(|object| matches!(object.kind, AtomicLayoutKind::ThematicBreak))
        );
        assert!(
            layout.blocks[0]
                .lines
                .iter()
                .flat_map(|line| &line.runs)
                .any(|run| run.text.contains("table"))
        );
        assert!(
            !layout.blocks[0]
                .lines
                .iter()
                .flat_map(|line| &line.runs)
                .any(|run| run.text.contains("image"))
        );
    }

    #[test]
    fn lays_out_large_span_heavy_table_with_bounded_work() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let table = builder
            .add_node(NodeKind::Table(Table {
                column_count: NonZeroU32::new(64).unwrap(),
            }))
            .unwrap();
        let body = builder
            .add_node(NodeKind::RowGroup(RowGroup::Body))
            .unwrap();
        for _ in 0..256 {
            let row = builder.add_node(NodeKind::Row).unwrap();
            let cell = append_cell(&mut builder, row, CellKind::Data, 0, 64, 1);
            append_paragraph(&mut builder, cell, "wide row");
            builder.append(body, row).unwrap();
        }
        builder.append(table, body).unwrap();
        builder.append(root, table).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());

        let layout = layout_flow_document(
            &document,
            &styles,
            &FixedMeasurer,
            &LayoutOptions {
                width: 320.0,
                ..LayoutOptions::default()
            },
        );

        let table = layout.blocks[0].table.as_ref().unwrap();
        assert_eq!(table.column_widths.len(), 64);
        assert_eq!(table.row_heights.len(), 256);
    }

    #[test]
    fn retains_flow_links_for_text_whitespace_and_atomic_objects() {
        let mut builder = FlowBuilder::new();
        let resource = builder
            .add_resource(Resource {
                identity: "linked.png".into(),
                media_type: None,
            })
            .unwrap();
        let link_style = builder
            .intern_style_input(StyleInput::semantic(SemanticStyleKey::Link))
            .unwrap();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let text_destination = FlowDestination::External("https://example.com/text".into());
        let text_link = builder
            .add_node(NodeKind::Link(Link {
                content: LinkContent::Phrasing,
                destination: text_destination.clone(),
                relation: LinkRelation::Ordinary,
                title: None,
            }))
            .unwrap();
        builder.set_style_input(text_link, link_style).unwrap();
        let text = builder.add_text("linked words").unwrap();
        builder.append(text_link, text).unwrap();
        builder.append(paragraph, text_link).unwrap();
        let adjacent_destination = FlowDestination::External("https://example.com/adjacent".into());
        let adjacent_link = builder
            .add_node(NodeKind::Link(Link {
                content: LinkContent::Phrasing,
                destination: adjacent_destination.clone(),
                relation: LinkRelation::Ordinary,
                title: None,
            }))
            .unwrap();
        builder.set_style_input(adjacent_link, link_style).unwrap();
        let adjacent_text = builder.add_text("next").unwrap();
        builder.append(adjacent_link, adjacent_text).unwrap();
        builder.append(paragraph, adjacent_link).unwrap();
        let image_destination = FlowDestination::Local(crate::model::flow::AnchorId::new("image"));
        let image_link = builder
            .add_node(NodeKind::Link(Link {
                content: LinkContent::Phrasing,
                destination: image_destination.clone(),
                relation: LinkRelation::Ordinary,
                title: None,
            }))
            .unwrap();
        builder.set_style_input(image_link, link_style).unwrap();
        let image = builder
            .add_node(NodeKind::Image(Image {
                resource,
                alternative: ImageAlternative::Text(NonEmptyText::new("linked image").unwrap()),
                title: None,
                intrinsic_size: Some(IntrinsicSize {
                    width: PositiveCssPixels::new(30.0).unwrap(),
                    height: PositiveCssPixels::new(20.0).unwrap(),
                }),
            }))
            .unwrap();
        builder.append(image_link, image).unwrap();
        builder.append(paragraph, image_link).unwrap();
        builder.append(root, paragraph).unwrap();
        let block_destination = FlowDestination::External("https://example.com/block".into());
        let block_link = builder
            .add_node(NodeKind::Link(Link {
                content: LinkContent::Flow,
                destination: block_destination.clone(),
                relation: LinkRelation::Ordinary,
                title: None,
            }))
            .unwrap();
        builder.set_style_input(block_link, link_style).unwrap();
        let linked_paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let linked_text = builder.add_text("block target").unwrap();
        builder.append(linked_paragraph, linked_text).unwrap();
        builder.append(block_link, linked_paragraph).unwrap();
        builder.append(root, block_link).unwrap();
        let code_destination = FlowDestination::External("https://example.com/code".into());
        let preformatted = builder
            .add_node(NodeKind::Preformatted(Preformatted {
                language: None,
                content_kind: PreformattedKind::PlainText,
            }))
            .unwrap();
        let code_link = builder
            .add_node(NodeKind::Link(Link {
                content: LinkContent::Phrasing,
                destination: code_destination.clone(),
                relation: LinkRelation::Ordinary,
                title: None,
            }))
            .unwrap();
        builder.set_style_input(code_link, link_style).unwrap();
        let code_before = builder.add_text("pre ").unwrap();
        let code_text = builder.add_text("abcאבג").unwrap();
        let code_after = builder.add_text(" post").unwrap();
        builder.append(code_link, code_text).unwrap();
        builder.append(preformatted, code_before).unwrap();
        builder.append(preformatted, code_link).unwrap();
        builder.append(preformatted, code_after).unwrap();
        builder.append(root, preformatted).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());

        let layout = layout_flow_document(
            &document,
            &styles,
            &FixedMeasurer,
            &LayoutOptions::default(),
        );

        let linked_runs = layout.blocks[0].lines[0]
            .runs
            .iter()
            .filter(|run| run.flow_link.as_ref() == Some(&text_destination))
            .collect::<Vec<_>>();
        assert_eq!(linked_runs.len(), 2);
        assert!(
            layout.blocks[0].lines[0]
                .runs
                .iter()
                .any(|run| run.text == "next"
                    && run.flow_link.as_ref() == Some(&adjacent_destination))
        );
        let gap_x = linked_runs[0].x + linked_runs[0].width + 0.1;
        assert_eq!(
            layout.flow_link_at(gap_x, layout.blocks[0].lines[0].y + 1.0),
            Some(&text_destination)
        );
        let object = &layout.blocks[0].atomic_objects[0];
        assert_eq!(object.node, Some(image));
        assert_eq!(object.flow_link.as_ref(), Some(&image_destination));
        assert_eq!(
            layout.flow_link_at(object.x + 1.0, object.y + 1.0),
            Some(&image_destination)
        );
        assert!(
            layout.blocks[1]
                .lines
                .iter()
                .flat_map(|line| &line.runs)
                .all(|run| run.flow_link.as_ref() == Some(&block_destination))
        );
        let code_runs = layout.blocks[2]
            .lines
            .iter()
            .flat_map(|line| &line.runs)
            .collect::<Vec<_>>();
        let linked_code_runs = code_runs
            .iter()
            .filter(|run| run.flow_link.as_ref() == Some(&code_destination))
            .collect::<Vec<_>>();
        assert_eq!(linked_code_runs.len(), 2);
        assert!(code_runs.iter().any(|run| run.flow_link.is_none()));
        let code_y = layout.blocks[2].lines[0].y + 1.0;
        assert!(linked_code_runs.iter().all(|run| {
            layout.flow_link_at(run.x + run.width / 2.0, code_y) == Some(&code_destination)
        }));
        assert!(
            code_runs
                .iter()
                .filter(|run| run.flow_link.is_none())
                .all(|run| layout
                    .flow_link_at(run.x + run.width / 2.0, code_y)
                    .is_none())
        );
        assert!(code_runs.iter().any(|run| {
            run.style.shape_direction == crate::model::flow::BaseDirection::RightToLeft
        }));
        assert!(code_runs.windows(2).all(|runs| {
            let [left, right] = runs else { return false };
            (left.x + left.width - right.x).abs() < 0.01
        }));
    }

    #[test]
    fn atomic_geometry_preserves_bidi_edges_ranges_and_boundaries() {
        let mut builder = FlowBuilder::new();
        let resource = builder
            .add_resource(Resource {
                identity: "rtl.png".into(),
                media_type: None,
            })
            .unwrap();
        let rtl = builder
            .intern_style_input(StyleInput::default().with_bidi(BidiState {
                direction: BaseDirection::RightToLeft,
                mode: BidiMode::Isolate,
            }))
            .unwrap();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let boundary = builder.add_node(NodeKind::Inline).unwrap();
        builder.set_style_input(boundary, rtl).unwrap();
        let before = builder.add_text("אב ").unwrap();
        let image = builder
            .add_node(NodeKind::Image(Image {
                resource,
                alternative: ImageAlternative::Text(NonEmptyText::new("תמונה").unwrap()),
                title: None,
                intrinsic_size: Some(IntrinsicSize {
                    width: PositiveCssPixels::new(40.0).unwrap(),
                    height: PositiveCssPixels::new(20.0).unwrap(),
                }),
            }))
            .unwrap();
        builder.append(boundary, before).unwrap();
        builder.append(boundary, image).unwrap();
        builder.append(paragraph, boundary).unwrap();
        builder.append(root, paragraph).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let layout = layout_flow_document(
            &document,
            &styles,
            &FixedMeasurer,
            &LayoutOptions::default(),
        );

        let object = &layout.blocks[0].atomic_objects[0];
        assert_eq!(object.bidi.direction, BaseDirection::RightToLeft);
        assert_eq!(object.bidi.mode, BidiMode::Isolate);
        assert!(object.leading_x > object.trailing_x);
        assert_eq!(
            object.position_at(object.trailing_x),
            crate::model::TextPosition {
                offset: object.range.end,
                affinity: crate::model::Affinity::Upstream,
            }
        );
        assert_eq!(
            object.position_at((object.leading_x + object.trailing_x) / 2.0),
            crate::model::TextPosition {
                offset: object.range.start,
                affinity: crate::model::Affinity::Downstream,
            }
        );
        let interior = crate::model::TextPosition {
            offset: object.range.start + 1,
            affinity: crate::model::Affinity::Downstream,
        };
        assert_eq!(
            object.resolve_position(interior).unwrap().offset,
            object.range.end
        );
        assert_eq!(
            layout.expand_atomic_range(TextRange {
                start: object.range.start + 1,
                end: object.range.end - 1,
            }),
            object.range
        );
        assert!(
            layout.blocks[0]
                .lines
                .iter()
                .flat_map(|line| &line.runs)
                .all(|run| run.style.bidi.mode == BidiMode::Isolate)
        );
    }

    #[test]
    fn missing_images_size_alt_placeholders_to_the_available_measure() {
        struct MissingImageMeasurer;

        impl TextMeasurer for MissingImageMeasurer {
            fn measure(&self, text: &str, style: &TextStyle) -> super::super::TextMetrics {
                FixedMeasurer.measure(text, style)
            }

            fn measure_image_resource(
                &self,
                identity: &str,
                _resource_base: Option<&str>,
                _intrinsic_size: Option<(f32, f32)>,
            ) -> super::super::ImageMetrics {
                super::super::ImageMetrics {
                    key: identity.to_owned(),
                    width: 0.0,
                    height: 0.0,
                }
            }
        }

        let mut builder = FlowBuilder::new();
        let resource = builder
            .add_resource(Resource {
                identity: "missing.png".into(),
                media_type: None,
            })
            .unwrap();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let image = builder
            .add_node(NodeKind::Image(Image {
                resource,
                alternative: ImageAlternative::Text(NonEmptyText::new("Missing diagram").unwrap()),
                title: None,
                intrinsic_size: None,
            }))
            .unwrap();
        builder.append(root, image).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let options = LayoutOptions {
            width: 200.0,
            ..LayoutOptions::default()
        };
        let layout = layout_flow_document(&document, &styles, &MissingImageMeasurer, &options);
        let object = &layout.blocks[0].atomic_objects[0];
        let style = text_style(
            styles
                .style_for_node(image)
                .and_then(|style| styles.get_computed_style(style))
                .unwrap(),
            &options,
            TextBlockKind::Atomic,
        );
        assert_eq!(
            object.width,
            MissingImageMeasurer
                .measure("Missing diagram", &style)
                .width
                + style.size
        );

        let narrow = layout_flow_document(
            &document,
            &styles,
            &MissingImageMeasurer,
            &LayoutOptions {
                width: 50.0,
                ..options
            },
        );
        assert_eq!(narrow.blocks[0].atomic_objects[0].width, 50.0);
    }

    #[test]
    fn images_resolve_document_resources_and_preserve_atomic_ranges() {
        type ResourceCall = (String, Option<String>, Option<(f32, f32)>);
        struct ResourceMeasurer {
            calls: RefCell<Vec<ResourceCall>>,
        }
        impl TextMeasurer for ResourceMeasurer {
            fn measure(&self, text: &str, style: &TextStyle) -> super::super::TextMetrics {
                FixedMeasurer.measure(text, style)
            }

            fn measure_image_resource(
                &self,
                identity: &str,
                resource_base: Option<&str>,
                intrinsic_size: Option<(f32, f32)>,
            ) -> super::super::ImageMetrics {
                self.calls.borrow_mut().push((
                    identity.to_owned(),
                    resource_base.map(str::to_owned),
                    intrinsic_size,
                ));
                super::super::ImageMetrics {
                    key: "resource-key".into(),
                    width: 200.0,
                    height: 100.0,
                }
            }
        }

        let mut builder = FlowBuilder::new();
        builder.set_resource_base(Some("book/".into()));
        let resource = builder
            .add_resource(Resource {
                identity: "images/chart.png".into(),
                media_type: Some("image/png".into()),
            })
            .unwrap();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let image = builder
            .add_node(NodeKind::Image(Image {
                resource,
                alternative: ImageAlternative::Text(NonEmptyText::new("chart").unwrap()),
                title: None,
                intrinsic_size: Some(IntrinsicSize {
                    width: PositiveCssPixels::new(640.0).unwrap(),
                    height: PositiveCssPixels::new(320.0).unwrap(),
                }),
            }))
            .unwrap();
        builder.append(root, image).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let measurer = ResourceMeasurer {
            calls: RefCell::new(Vec::new()),
        };

        let layout = layout_flow_document(
            &document,
            &styles,
            &measurer,
            &LayoutOptions {
                width: 100.0,
                ..LayoutOptions::default()
            },
        );

        assert_eq!(
            measurer.calls.into_inner(),
            vec![(
                "images/chart.png".into(),
                Some("book/".into()),
                Some((640.0, 320.0)),
            )]
        );
        let block = &layout.blocks[0];
        assert_eq!(block.kind, LayoutBlockKind::Image);
        assert_eq!(block.atomic_objects.len(), 1);
        let object = &block.atomic_objects[0];
        assert_eq!(object.range, TextRange { start: 0, end: 5 });
        assert_eq!((object.width, object.height), (100.0, 50.0));
        assert!(block.lines.iter().all(|line| line.runs.is_empty()));
        assert!(matches!(
            &object.kind,
            AtomicLayoutKind::Image { key, alt }
                if key == "resource-key" && alt == "chart"
        ));
    }

    #[test]
    fn inline_code_uses_unicode_and_emergency_breaks_without_hyphens() {
        let outcome = crate::formats::markdown::parse(
            "abcdefghijklmnop\n\n`alpha/beta`\n\n`abcdefghijklmnop`",
            crate::formats::markdown::MarkdownOptions::default(),
        )
        .unwrap();
        let styles = outcome.document.resolve_styles(StyleContextKey::default());
        let options = LayoutOptions {
            width: 30.0,
            body_size: 10.0,
            line_spacing: 1.0,
            block_spacing: 2.0,
            hyphenation: false,
            ..LayoutOptions::default()
        };

        let layout = layout_flow_document(&outcome.document, &styles, &FixedMeasurer, &options);

        assert_eq!(
            layout.blocks[0]
                .lines
                .iter()
                .filter(|line| !line.runs.is_empty())
                .count(),
            1
        );
        assert!(layout.blocks[0].lines[0].runs[0].width > options.width);

        let ordinary = &layout.blocks[1];
        assert_eq!(ordinary.lines.len(), 2);
        assert_eq!(ordinary.lines[0].runs[0].text, "alpha/");
        assert_eq!(ordinary.lines[1].runs[0].text, "beta");

        let emergency = &layout.blocks[2];
        assert!(emergency.lines.len() >= 2);
        assert!(emergency.lines.iter().all(|line| {
            line.runs
                .iter()
                .all(|run| run.x + run.width <= options.width + 0.01)
        }));
        let painted = emergency
            .lines
            .iter()
            .flat_map(|line| &line.runs)
            .map(|run| run.text.as_str())
            .collect::<String>();
        assert_eq!(painted, "abcdefghijklmnop");
        assert!(!painted.contains('-'));
        assert!(
            emergency
                .lines
                .iter()
                .flat_map(|line| &line.runs)
                .all(|run| run.style.family == FontFamily::Monospace)
        );
    }

    #[test]
    fn inline_code_emergency_breaking_is_inherited_and_stays_inside_table_cells() {
        let mut builder = FlowBuilder::new();
        let code_style = builder
            .intern_style_input(StyleInput::semantic(SemanticStyleKey::InlineCode))
            .unwrap();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let table = builder
            .add_node(NodeKind::Table(Table {
                column_count: NonZeroU32::new(1).unwrap(),
            }))
            .unwrap();
        let body = builder
            .add_node(NodeKind::RowGroup(RowGroup::Body))
            .unwrap();
        let row = builder.add_node(NodeKind::Row).unwrap();
        let cell = append_cell(&mut builder, row, CellKind::Data, 0, 1, 1);
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let link = builder
            .add_node(NodeKind::Link(Link {
                content: LinkContent::Phrasing,
                destination: FlowDestination::External("https://example.com".into()),
                relation: LinkRelation::Ordinary,
                title: None,
            }))
            .unwrap();
        let code = builder
            .add_node(NodeKind::InlineSemantic(InlineSemantic {
                role: InlineRole::Code,
                value: None,
            }))
            .unwrap();
        builder.set_style_input(code, code_style).unwrap();
        let emphasis = builder
            .add_node(NodeKind::InlineSemantic(InlineSemantic {
                role: InlineRole::Emphasis,
                value: None,
            }))
            .unwrap();
        let text = builder.add_text("abcdefghijklmnop").unwrap();
        builder.append(emphasis, text).unwrap();
        builder.append(code, emphasis).unwrap();
        builder.append(link, code).unwrap();
        builder.append(paragraph, link).unwrap();
        builder.append(cell, paragraph).unwrap();
        builder.append(body, row).unwrap();
        builder.append(table, body).unwrap();
        builder.append(root, table).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let options = LayoutOptions {
            width: 60.0,
            body_size: 10.0,
            line_spacing: 1.0,
            block_spacing: 2.0,
            table_horizontal_padding: 4.0,
            table_vertical_padding: 2.0,
            table_min_column_width: 1.0,
            hyphenation: false,
            ..LayoutOptions::default()
        };

        let layout = layout_flow_document(&document, &styles, &FixedMeasurer, &options);
        let block = &layout.blocks[0];
        let table = block.table.as_ref().unwrap();
        let content_right = table.x + table.width - options.table_horizontal_padding;
        let runs = block
            .lines
            .iter()
            .flat_map(|line| &line.runs)
            .collect::<Vec<_>>();

        assert!(block.lines.len() >= 2);
        assert!(
            runs.iter()
                .all(|run| run.x + run.width <= content_right + 0.01)
        );
        assert!(runs.iter().all(|run| {
            run.style.family == FontFamily::Monospace
                && run.style.italic
                && run.flow_link == Some(FlowDestination::External("https://example.com".into()))
        }));
        assert_eq!(
            runs.iter().map(|run| run.text.as_str()).collect::<String>(),
            "abcdefghijklmnop"
        );
    }

    #[test]
    fn inline_atomic_objects_wrap_without_rendering_alternative_glyphs() {
        let mut builder = FlowBuilder::new();
        let resource = builder
            .add_resource(Resource {
                identity: "inline.png".into(),
                media_type: None,
            })
            .unwrap();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let before = builder.add_text("before ").unwrap();
        let image = builder
            .add_node(NodeKind::Image(Image {
                resource,
                alternative: ImageAlternative::Text(NonEmptyText::new("diagram").unwrap()),
                title: None,
                intrinsic_size: Some(IntrinsicSize {
                    width: PositiveCssPixels::new(40.0).unwrap(),
                    height: PositiveCssPixels::new(20.0).unwrap(),
                }),
            }))
            .unwrap();
        let math = builder
            .add_node(NodeKind::Math(Math {
                notation: MathNotation::Tex,
                source: "x^2".into(),
                display: MathDisplay::Inline,
            }))
            .unwrap();
        let after = builder.add_text(" after").unwrap();
        builder.append(paragraph, before).unwrap();
        builder.append(paragraph, image).unwrap();
        builder.append(paragraph, math).unwrap();
        builder.append(paragraph, after).unwrap();
        builder.append(root, paragraph).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());

        let layout = layout_flow_document(
            &document,
            &styles,
            &FixedMeasurer,
            &LayoutOptions {
                width: 120.0,
                ..LayoutOptions::default()
            },
        );

        let block = &layout.blocks[0];
        assert_eq!(block.atomic_objects.len(), 2);
        assert_eq!(
            block.atomic_objects[0].range,
            TextRange { start: 7, end: 14 }
        );
        assert_eq!(
            block.atomic_objects[1].range,
            TextRange { start: 14, end: 17 }
        );
        let painted_text = block
            .lines
            .iter()
            .flat_map(|line| &line.runs)
            .map(|run| run.text.as_str())
            .collect::<String>();
        assert!(painted_text.starts_with("before"));
        assert!(!painted_text.contains("diagram"));
        assert!(!painted_text.contains("x^2"));
        assert!(block.lines.len() >= 2);
    }

    #[test]
    fn table_overflow_policies_preserve_intrinsics_and_report_fallbacks() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let table = builder
            .add_node(NodeKind::Table(Table {
                column_count: NonZeroU32::new(1).unwrap(),
            }))
            .unwrap();
        let body = builder
            .add_node(NodeKind::RowGroup(RowGroup::Body))
            .unwrap();
        let row = builder.add_node(NodeKind::Row).unwrap();
        let cell = append_cell(&mut builder, row, CellKind::Data, 0, 1, 1);
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let text = builder.add_text("abcdefghijklmnopqrst").unwrap();
        builder.append(paragraph, text).unwrap();
        builder.append(cell, paragraph).unwrap();
        builder.append(body, row).unwrap();
        builder.append(table, body).unwrap();
        builder.append(root, table).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());

        let layout_with = |policy| {
            layout_flow_document(
                &document,
                &styles,
                &FixedMeasurer,
                &LayoutOptions {
                    width: 70.0,
                    body_size: 10.0,
                    table_horizontal_padding: 3.0,
                    hyphenation: false,
                    table_overflow: policy,
                    ..LayoutOptions::default()
                },
            )
        };
        let fitted = layout_with(super::super::TableOverflowPolicy::EmergencyFit);
        let overflowed = layout_with(super::super::TableOverflowPolicy::Overflow);
        let clipped = layout_with(super::super::TableOverflowPolicy::Clip);
        let fitted_block = fitted
            .blocks
            .iter()
            .find(|block| block.kind == LayoutBlockKind::Table)
            .unwrap();
        let fitted_table = fitted_block.table.as_ref().unwrap();
        let overflowed_table = overflowed
            .blocks
            .iter()
            .find_map(|block| block.table.as_ref())
            .unwrap();
        let clipped_table = clipped
            .blocks
            .iter()
            .find_map(|block| block.table.as_ref())
            .unwrap();

        assert_eq!(fitted_table.sizing.grid_min, 106.0);
        assert_eq!(fitted_table.sizing.grid_max, 106.0);
        assert_eq!(fitted_table.sizing.emergency_min, 11.0);
        assert_eq!(fitted_table.width, 70.0);
        assert!(fitted_table.sizing.fallback_active);
        assert!(!fitted_table.sizing.overflows_available);
        assert!(
            fitted_block
                .lines
                .iter()
                .flat_map(|line| &line.runs)
                .all(|run| run.x >= fitted_table.x && run.x + run.width <= fitted_table.x + 70.0)
        );

        assert_eq!(overflowed_table.width, 106.0);
        assert!(!overflowed_table.sizing.fallback_active);
        assert!(overflowed_table.sizing.overflows_available);
        assert_eq!(
            overflowed_table.overflow_policy,
            super::super::TableOverflowPolicy::Overflow
        );
        assert_eq!(clipped_table.width, overflowed_table.width);
        assert!(!clipped_table.sizing.fallback_active);
        assert_eq!(
            clipped_table.overflow_policy,
            super::super::TableOverflowPolicy::Clip
        );
    }

    #[test]
    fn table_intrinsic_measurement_keeps_emergency_width_separate() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let table = builder
            .add_node(NodeKind::Table(Table {
                column_count: NonZeroU32::new(1).unwrap(),
            }))
            .unwrap();
        let body = builder
            .add_node(NodeKind::RowGroup(RowGroup::Body))
            .unwrap();
        let row = builder.add_node(NodeKind::Row).unwrap();
        let cell = append_cell(&mut builder, row, CellKind::Data, 0, 1, 1);
        let paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let code = builder
            .add_node(NodeKind::InlineSemantic(InlineSemantic {
                role: InlineRole::Code,
                value: None,
            }))
            .unwrap();
        let text = builder.add_text("abcdefghijklmnop").unwrap();
        builder.append(code, text).unwrap();
        builder.append(paragraph, code).unwrap();
        builder.append(cell, paragraph).unwrap();
        builder.append(body, row).unwrap();
        builder.append(table, body).unwrap();
        builder.append(root, table).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let cell = document
            .preorder(document.root())
            .find(|id| {
                document
                    .get_node(*id)
                    .is_some_and(|node| matches!(node.kind, NodeKind::Cell(_)))
            })
            .unwrap();
        let measured = intrinsic_flow_cell_width(
            &document,
            &styles,
            cell,
            &FixedMeasurer,
            &LayoutOptions {
                body_size: 10.0,
                hyphenation: false,
                ..LayoutOptions::default()
            },
            &std::collections::HashMap::new(),
        );

        assert!((measured.max_content - 73.6).abs() < 0.01);
        assert_eq!(measured.min_content, measured.max_content);
        assert!((measured.emergency_min - 4.6).abs() < 0.01);
    }

    #[test]
    fn nested_table_fallback_intrinsics_measure_atomics_not_alt_text() {
        let mut builder = FlowBuilder::new();
        let resource = builder
            .add_resource(Resource {
                identity: "nested.png".into(),
                media_type: None,
            })
            .unwrap();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let outer_table = builder
            .add_node(NodeKind::Table(Table {
                column_count: NonZeroU32::new(1).unwrap(),
            }))
            .unwrap();
        let outer_body = builder
            .add_node(NodeKind::RowGroup(RowGroup::Body))
            .unwrap();
        let outer_row = builder.add_node(NodeKind::Row).unwrap();
        let outer_cell = append_cell(&mut builder, outer_row, CellKind::Data, 0, 1, 1);
        let nested_table = builder
            .add_node(NodeKind::Table(Table {
                column_count: NonZeroU32::new(1).unwrap(),
            }))
            .unwrap();
        let nested_body = builder
            .add_node(NodeKind::RowGroup(RowGroup::Body))
            .unwrap();
        let nested_row = builder.add_node(NodeKind::Row).unwrap();
        let nested_cell = append_cell(&mut builder, nested_row, CellKind::Data, 0, 1, 1);
        let image = builder
            .add_node(NodeKind::Image(Image {
                resource,
                alternative: ImageAlternative::Text(NonEmptyText::new("tiny").unwrap()),
                title: None,
                intrinsic_size: Some(IntrinsicSize {
                    width: PositiveCssPixels::new(300.0).unwrap(),
                    height: PositiveCssPixels::new(200.0).unwrap(),
                }),
            }))
            .unwrap();
        builder.append(nested_cell, image).unwrap();
        builder.append(nested_body, nested_row).unwrap();
        builder.append(nested_table, nested_body).unwrap();
        builder.append(outer_cell, nested_table).unwrap();
        builder.append(outer_body, outer_row).unwrap();
        builder.append(outer_table, outer_body).unwrap();
        builder.append(root, outer_table).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let outer_cell = document
            .preorder(document.root())
            .find(|id| {
                document
                    .get_node(*id)
                    .is_some_and(|node| matches!(node.kind, NodeKind::Cell(_)))
            })
            .unwrap();
        let measured = intrinsic_flow_cell_width(
            &document,
            &styles,
            outer_cell,
            &FixedMeasurer,
            &LayoutOptions::default(),
            &std::collections::HashMap::new(),
        );

        assert!((measured.max_content - 300.0).abs() < 0.01);
        assert!((measured.min_content - 300.0).abs() < 0.01);
        assert!((measured.emergency_min - 1.0).abs() < 0.01);
    }

    #[test]
    fn table_intrinsic_widths_use_descendant_styles_and_atomic_geometry() {
        let mut builder = FlowBuilder::new();
        let resource = builder
            .add_resource(Resource {
                identity: "intrinsic.png".into(),
                media_type: None,
            })
            .unwrap();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let table = builder
            .add_node(NodeKind::Table(Table {
                column_count: NonZeroU32::new(2).unwrap(),
            }))
            .unwrap();
        let body = builder
            .add_node(NodeKind::RowGroup(RowGroup::Body))
            .unwrap();
        let row = builder.add_node(NodeKind::Row).unwrap();
        let styled_cell = append_cell(&mut builder, row, CellKind::Data, 0, 1, 1);
        let heading = builder
            .add_node(NodeKind::Heading(Heading {
                level: HeadingLevel::H2,
            }))
            .unwrap();
        let heading_text = builder.add_text("abcdefghij").unwrap();
        builder.append(heading, heading_text).unwrap();
        builder.append(styled_cell, heading).unwrap();
        let atomic_cell = append_cell(&mut builder, row, CellKind::Data, 1, 1, 1);
        let image = builder
            .add_node(NodeKind::Image(Image {
                resource,
                alternative: ImageAlternative::Decorative,
                title: None,
                intrinsic_size: Some(IntrinsicSize {
                    width: PositiveCssPixels::new(60.0).unwrap(),
                    height: PositiveCssPixels::new(20.0).unwrap(),
                }),
            }))
            .unwrap();
        builder.append(atomic_cell, image).unwrap();
        builder.append(body, row).unwrap();
        builder.append(table, body).unwrap();
        builder.append(root, table).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let options = LayoutOptions {
            width: 160.0,
            body_size: 10.0,
            table_horizontal_padding: 4.0,
            table_min_column_width: 1.0,
            ..LayoutOptions::default()
        };

        let layout = layout_flow_document(&document, &styles, &FixedMeasurer, &options);
        let block = &layout.blocks[0];
        let table = block.table.as_ref().unwrap();
        let heading_run = block
            .lines
            .iter()
            .flat_map(|line| &line.runs)
            .find(|run| run.text == "abcdefghij")
            .expect("styled unbreakable content retains its intrinsic width");
        let first_right = table.x + table.column_widths[0] - options.table_horizontal_padding;
        assert!(heading_run.x + heading_run.width <= first_right + 0.01);

        let image = block.atomic_objects.first().unwrap();
        let second_left = table.x + table.column_widths[0] + options.table_horizontal_padding;
        let second_right = table.x + table.width - options.table_horizontal_padding;
        assert_eq!(image.width, 60.0);
        assert!(image.x >= second_left - 0.01);
        assert!(image.x + image.width <= second_right + 0.01);
    }

    #[test]
    fn table_bidi_alignment_and_link_hits_use_visual_cell_geometry() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let table = builder
            .add_node(NodeKind::Table(Table {
                column_count: NonZeroU32::new(1).unwrap(),
            }))
            .unwrap();
        let body = builder
            .add_node(NodeKind::RowGroup(RowGroup::Body))
            .unwrap();
        let linked_row = builder.add_node(NodeKind::Row).unwrap();
        let linked_cell = append_cell(&mut builder, linked_row, CellKind::Data, 0, 1, 1);
        let linked_paragraph = builder.add_node(NodeKind::Paragraph).unwrap();
        let destination = FlowDestination::Local(crate::model::flow::AnchorId::new("target"));
        let link = builder
            .add_node(NodeKind::Link(Link {
                content: LinkContent::Phrasing,
                destination: destination.clone(),
                relation: LinkRelation::Ordinary,
                title: None,
            }))
            .unwrap();
        let mixed = builder.add_text("abc אבג").unwrap();
        let rtl_right = builder
            .intern_style_input(
                StyleInput::table_alignment(crate::model::flow::TextAlignment::Right).with_bidi(
                    BidiState {
                        direction: BaseDirection::RightToLeft,
                        mode: BidiMode::Isolate,
                    },
                ),
            )
            .unwrap();
        builder.set_style_input(linked_cell, rtl_right).unwrap();
        builder.append(link, mixed).unwrap();
        builder.append(linked_paragraph, link).unwrap();
        builder.append(linked_cell, linked_paragraph).unwrap();
        builder.append(body, linked_row).unwrap();

        let wide_row = builder.add_node(NodeKind::Row).unwrap();
        let wide_cell = append_cell(&mut builder, wide_row, CellKind::Data, 0, 1, 1);
        append_paragraph(&mut builder, wide_cell, "abcdefghijklmnopqrst");
        builder.append(body, wide_row).unwrap();
        builder.append(table, body).unwrap();
        builder.append(root, table).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());
        let options = LayoutOptions {
            width: 200.0,
            body_size: 10.0,
            table_horizontal_padding: 3.0,
            hyphenation: false,
            ..LayoutOptions::default()
        };
        let layout = layout_flow_document(&document, &styles, &FixedMeasurer, &options);
        let block = &layout.blocks[0];
        let table = block.table.as_ref().unwrap();
        let linked_runs = block
            .lines
            .iter()
            .flat_map(|line| &line.runs)
            .filter(|run| run.flow_link.as_ref() == Some(&destination))
            .collect::<Vec<_>>();
        let visual_left = linked_runs
            .iter()
            .map(|run| run.x)
            .fold(f32::INFINITY, f32::min);
        let visual_right = linked_runs
            .iter()
            .map(|run| run.x + run.width)
            .fold(0.0_f32, f32::max);

        assert!(linked_runs.len() >= 2);
        assert!(
            linked_runs
                .iter()
                .all(|run| run.style.bidi.direction == BaseDirection::RightToLeft)
        );
        assert!(visual_left >= table.x + options.table_horizontal_padding);
        assert!(
            (visual_right - (table.x + table.width - options.table_horizontal_padding)).abs()
                < 0.01
        );
        for run in linked_runs {
            let line = block
                .lines
                .iter()
                .find(|line| {
                    line.runs
                        .iter()
                        .any(|candidate| std::ptr::eq(candidate, run))
                })
                .unwrap();
            assert_eq!(
                layout.flow_link_at(run.x + run.width / 2.0, line.y + line.height / 2.0),
                Some(&destination)
            );
        }
    }

    #[test]
    fn table_cells_retain_atomic_geometry_and_readable_fallbacks_stay_text() {
        let mut builder = FlowBuilder::new();
        let resource = builder
            .add_resource(Resource {
                identity: "cell.png".into(),
                media_type: None,
            })
            .unwrap();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let table = builder
            .add_node(NodeKind::Table(Table {
                column_count: NonZeroU32::new(1).unwrap(),
            }))
            .unwrap();
        let body = builder
            .add_node(NodeKind::RowGroup(RowGroup::Body))
            .unwrap();
        let row = builder.add_node(NodeKind::Row).unwrap();
        let cell = append_cell(&mut builder, row, CellKind::Data, 0, 1, 1);
        let image = builder
            .add_node(NodeKind::Image(Image {
                resource,
                alternative: ImageAlternative::Decorative,
                title: None,
                intrinsic_size: Some(IntrinsicSize {
                    width: PositiveCssPixels::new(300.0).unwrap(),
                    height: PositiveCssPixels::new(200.0).unwrap(),
                }),
            }))
            .unwrap();
        builder.append(cell, image).unwrap();
        builder.append(body, row).unwrap();
        builder.append(table, body).unwrap();
        builder.append(root, table).unwrap();

        let fallback = builder
            .add_node(NodeKind::UnsupportedObject(UnsupportedObject {
                placement: Placement::Flow,
                media_kind: MediaKind::Video,
                title: Some("ignored title".into()),
                size_hint: None,
                activation: None,
                reason: UnsupportedReason::NoNativePresenter,
            }))
            .unwrap();
        append_paragraph(&mut builder, fallback, "readable fallback");
        builder.append(root, fallback).unwrap();
        let placeholder = builder
            .add_node(NodeKind::UnsupportedObject(UnsupportedObject {
                placement: Placement::Flow,
                media_kind: MediaKind::Audio,
                title: None,
                size_hint: Some(IntrinsicSize {
                    width: PositiveCssPixels::new(80.0).unwrap(),
                    height: PositiveCssPixels::new(30.0).unwrap(),
                }),
                activation: None,
                reason: UnsupportedReason::NoNativePresenter,
            }))
            .unwrap();
        builder.append(root, placeholder).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());

        let layout = layout_flow_document(
            &document,
            &styles,
            &FixedMeasurer,
            &LayoutOptions::default(),
        );

        assert_eq!(layout.blocks[0].kind, LayoutBlockKind::Table);
        assert_eq!(layout.blocks[0].atomic_objects.len(), 1);
        assert!(layout.blocks[0].atomic_objects[0].y >= layout.blocks[0].y);
        let narrow = layout_flow_document(
            &document,
            &styles,
            &FixedMeasurer,
            &LayoutOptions {
                width: 100.0,
                ..LayoutOptions::default()
            },
        );
        let narrow_table = narrow.blocks[0].table.as_ref().unwrap();
        assert_eq!(narrow_table.width, 100.0);
        assert!(narrow_table.sizing.fallback_active);
        assert!(!narrow_table.sizing.overflows_available);
        assert!((narrow.blocks[0].atomic_objects[0].width - 80.0).abs() < 0.01);
        assert!(layout.blocks[1].atomic_objects.is_empty());
        assert_eq!(layout.blocks[1].lines[0].runs[0].text, "readable");
        assert_eq!(layout.blocks[2].atomic_objects.len(), 1);
        assert_eq!(
            (
                layout.blocks[2].atomic_objects[0].width,
                layout.blocks[2].atomic_objects[0].height
            ),
            (80.0, 30.0)
        );
    }

    #[test]
    fn traversal_keeps_tables_as_single_semantic_layout_units() {
        let mut builder = FlowBuilder::new();
        let root = builder.add_node(NodeKind::Document).unwrap();
        let table = builder
            .add_node(NodeKind::Table(crate::model::flow::Table {
                column_count: std::num::NonZeroU32::new(1).unwrap(),
            }))
            .unwrap();
        let body = builder
            .add_node(NodeKind::RowGroup(crate::model::flow::RowGroup::Body))
            .unwrap();
        let row = builder.add_node(NodeKind::Row).unwrap();
        let cell = builder
            .add_node(NodeKind::Cell(crate::model::flow::Cell {
                kind: crate::model::flow::CellKind::Data,
                column_start: 0,
                column_span: std::num::NonZeroU32::new(1).unwrap(),
                row_span: std::num::NonZeroU32::new(1).unwrap(),
                scope: None,
                explicit_headers: None,
            }))
            .unwrap();
        let hidden = builder.add_node(NodeKind::Paragraph).unwrap();
        let hidden_text = builder.add_text("cell").unwrap();
        builder.append(hidden, hidden_text).unwrap();
        builder.append(cell, hidden).unwrap();
        builder.append(row, cell).unwrap();
        builder.append(body, row).unwrap();
        builder.append(table, body).unwrap();
        builder.append(root, table).unwrap();
        let visible = builder.add_node(NodeKind::Paragraph).unwrap();
        let visible_text = builder.add_text("visible").unwrap();
        builder.append(visible, visible_text).unwrap();
        builder.append(root, visible).unwrap();
        let document = builder.finish().unwrap();
        let styles = document.resolve_styles(StyleContextKey::default());

        let layout = layout_flow_document(
            &document,
            &styles,
            &FixedMeasurer,
            &LayoutOptions::default(),
        );

        assert_eq!(layout.blocks.len(), 2);
        assert_eq!(layout.blocks[0].kind, LayoutBlockKind::Table);
        assert_eq!(
            layout.blocks[0].table.as_ref().unwrap().row_heights.len(),
            1
        );
        assert_eq!(layout.blocks[1].lines[0].runs[0].text, "visible");
    }
}
