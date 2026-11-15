use crate::layout::LayoutBlockKind;
#[cfg(test)]
use crate::{layout::ContinuousLayout, model::flow::AuthorBreak};

mod fragments;
mod page_policy;
pub mod sequence;

pub use fragments::{
    AtomicFragment, BlockBoxFragment, ComposedPage, DecorationKind, FragmentRange,
    FragmentValidationError, LineFragment, PageFragment, PaginatedLayout, QuoteBarFragment,
    RepeatedTableHeaderFragment, RuleFragment, TableQuoteBarFragment, TableRuleFragment,
    TableSliceFragment, TableSliceKind, materialize_paginated_layout, validate_paginated_layout,
};
pub use page_policy::{
    PageBoundary, PageBox, PageBreakError, PageBreakInput, PageGlue, PagePenalty, break_page_items,
    build_page_items, debug_page_break_plan,
};

const MIN_FRAGMENT_LINES: usize = 2;
const MIN_BODY_HEIGHT: f32 = 1.0;

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RepeatedTableHeader {
    pub block: usize,
    pub source_y: f32,
    pub height: f32,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub start_y: f32,
    pub end_y: f32,
    pub text_anchor: usize,
    pub(crate) scaled_atomics: Vec<(usize, usize, f32)>,
    pub(crate) repeated_table_header: Option<RepeatedTableHeader>,
}

#[cfg(test)]
impl Page {
    pub fn document_y_at(&self, page_y: f32) -> Option<f32> {
        if page_y < 0.0 {
            return None;
        }
        if let Some(header) = self.repeated_table_header {
            if page_y < header.height {
                return Some(header.source_y + page_y);
            }
            let document_y = self.start_y + page_y - header.height;
            return (document_y < self.end_y).then_some(document_y);
        }
        let document_y = self.start_y + page_y;
        (document_y < self.end_y).then_some(document_y)
    }

    pub fn page_y_for_document_y(&self, document_y: f32) -> Option<f32> {
        if let Some(header) = self.repeated_table_header
            && document_y >= header.source_y
            && document_y < header.source_y + header.height
        {
            return Some(document_y - header.source_y);
        }
        if document_y < self.start_y || document_y >= self.end_y {
            return None;
        }
        Some(
            document_y - self.start_y
                + self
                    .repeated_table_header
                    .map_or(0.0, |header| header.height),
        )
    }
}

#[cfg(test)]
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageMap {
    pub pages: Vec<Page>,
    pub content_height: f32,
    pub body_height: f32,
}

#[cfg(test)]
impl PageMap {
    pub fn page_for_y(&self, y: f32) -> usize {
        self.pages
            .partition_point(|page| page.start_y <= y)
            .saturating_sub(1)
            .min(self.pages.len().saturating_sub(1))
    }

    pub fn page_for_text(&self, layout: &ContinuousLayout, offset: usize) -> usize {
        layout.y_for_text(offset).map_or(0, |y| self.page_for_y(y))
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
struct Candidate {
    end_y: f32,
    next_y: f32,
    block: usize,
    kind: CandidateKind,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
enum CandidateKind {
    Block,
    Line(usize),
    TableRow,
}

#[cfg(test)]
pub fn paginate(layout: &ContinuousLayout, requested_body_height: f32) -> PageMap {
    let _span = crate::telemetry::phase_span!(
        "flow.pagination.build",
        blocks = layout.blocks.len() as u64,
        body_height = requested_body_height as f64,
    );
    let body_height = if requested_body_height.is_finite() {
        requested_body_height.max(MIN_BODY_HEIGHT)
    } else {
        MIN_BODY_HEIGHT
    };
    if layout.blocks.is_empty() {
        crate::telemetry::phase_event!("flow.phase.complete", pages = 1_u64);
        return PageMap {
            pages: vec![Page {
                start_y: 0.0,
                end_y: 0.0,
                text_anchor: 0,
                scaled_atomics: Vec::new(),
                repeated_table_header: None,
            }],
            content_height: layout.height,
            body_height,
        };
    }

    let candidates = candidates(layout);
    let mut pages = Vec::new();
    let mut start_y = 0.0;
    let mut guard = 0;
    let emergency_steps = (layout.height.max(0.0) / MIN_BODY_HEIGHT).ceil() as usize;
    let maximum_steps = candidates
        .len()
        .saturating_add(layout.blocks.len())
        .saturating_add(emergency_steps)
        .saturating_add(1);

    while start_y < layout.height && guard <= maximum_steps {
        guard += 1;
        let repeated_table_header = repeated_table_header(layout, start_y, body_height);
        let repeated_header_height = repeated_table_header.map_or(0.0, |header| header.height);
        let limit = start_y + (body_height - repeated_header_height).max(MIN_BODY_HEIGHT);
        let first =
            candidates.partition_point(|candidate| candidate.next_y <= start_y + f32::EPSILON);
        let upper = candidates.partition_point(|candidate| candidate.end_y <= limit + 0.01);
        let first_progress = candidates.get(first).copied();
        let fitting = (first < upper)
            .then(|| {
                candidates[first..upper]
                    .iter()
                    .rev()
                    .copied()
                    .find(|candidate| legal_candidate(layout, *candidate, start_y))
            })
            .flatten();

        // If normal keep constraints cannot fit, first try the furthest physical
        // line boundary. On an empty page this deterministically relaxes the
        // two-line minima while still making progress.
        let chosen = fitting.or_else(|| (first < upper).then(|| candidates[upper - 1]));
        // A pathological row can contain one line taller than the page. When
        // no semantic or line boundary fits, slice its painted geometry across
        // pages rather than advancing past the row and clipping it forever.
        // Replaced atomic content is instead scaled and kept whole.
        let oversized_table_atomic = layout.blocks.iter().any(|block| {
            block.kind == LayoutBlockKind::Table
                && block.y < limit - f32::EPSILON
                && block.y + block.height > limit + f32::EPSILON
                && block.atomic_objects.iter().any(|object| {
                    object.y < limit - f32::EPSILON
                        && object.y + object.height > limit + f32::EPSILON
                })
        });
        let emergency_table_slice = (chosen.is_none() && !oversized_table_atomic)
            .then(|| {
                let (block, _) = layout.blocks.iter().enumerate().find(|(_, block)| {
                    block.kind == LayoutBlockKind::Table
                        && block.y < limit - f32::EPSILON
                        && block.y + block.height > limit + f32::EPSILON
                })?;
                (limit > start_y + f32::EPSILON).then_some(Candidate {
                    end_y: limit,
                    next_y: limit,
                    block,
                    kind: CandidateKind::TableRow,
                })
            })
            .flatten();
        let chosen = chosen
            .or(emergency_table_slice)
            .or(first_progress)
            .expect("nonempty layout has a boundary");
        let block = &layout.blocks[chosen.block];
        let available_content_height = (body_height - repeated_header_height).max(MIN_BODY_HEIGHT);
        let scaled_atomics = block
            .atomic_objects
            .iter()
            .enumerate()
            .filter(|(_, object)| {
                object.y < chosen.end_y + f32::EPSILON
                    && object.y + object.height > start_y + f32::EPSILON
            })
            .filter_map(|(index, object)| {
                let space =
                    (available_content_height - (object.y - start_y).max(0.0)).max(MIN_BODY_HEIGHT);
                (object.height > space).then_some((
                    chosen.block,
                    index,
                    (space / object.height).clamp(0.0, 1.0),
                ))
            })
            .collect();
        let text_anchor = layout
            .text_offset_for_y(start_y)
            .unwrap_or(block.range.start);
        pages.push(Page {
            start_y,
            end_y: chosen.end_y,
            text_anchor,
            scaled_atomics,
            repeated_table_header,
        });
        start_y = chosen.next_y;
    }

    if pages.is_empty() {
        pages.push(Page {
            start_y: 0.0,
            end_y: layout.height,
            text_anchor: layout.blocks[0].range.start,
            scaled_atomics: Vec::new(),
            repeated_table_header: None,
        });
    }

    crate::telemetry::phase_event!("flow.phase.complete", pages = pages.len() as u64);
    PageMap {
        pages,
        content_height: layout.height,
        body_height,
    }
}

fn safe_table_line_boundaries(block: &crate::layout::BlockLayout) -> Vec<f32> {
    let mut events = Vec::with_capacity(block.lines.len() * 2);
    for line in &block.lines {
        if line.height <= 0.0 {
            continue;
        }
        events.push((line.y, 1_i32));
        events.push((line.y + line.height, -1_i32));
    }
    events.sort_by(|left, right| left.0.total_cmp(&right.0).then(left.1.cmp(&right.1)));
    let mut boundaries = Vec::new();
    let mut active = 0_i32;
    let mut index = 0;
    while index < events.len() {
        let y = events[index].0;
        let mut ends = 0;
        let mut starts = 0;
        while index < events.len() && (events[index].0 - y).abs() < 0.01 {
            if events[index].1 < 0 {
                ends += 1;
            } else {
                starts += 1;
            }
            index += 1;
        }
        active -= ends;
        if ends > 0 && active == 0 {
            boundaries.push(y);
        }
        active += starts;
    }
    boundaries
}

#[cfg(test)]
fn candidates(layout: &ContinuousLayout) -> Vec<Candidate> {
    let mut result = Vec::new();
    for (block_index, block) in layout.blocks.iter().enumerate() {
        if is_splittable(block.kind) && block.lines.len() > 1 {
            for line_index in 0..block.lines.len() - 1 {
                let line = &block.lines[line_index];
                result.push(Candidate {
                    end_y: line.y + line.height,
                    next_y: block.lines[line_index + 1].y,
                    block: block_index,
                    kind: CandidateKind::Line(line_index),
                });
            }
        } else if block.kind == LayoutBlockKind::Table
            && let Some(table) = &block.table
        {
            if table.row_y > block.y + f32::EPSILON {
                result.push(Candidate {
                    end_y: table.row_y,
                    next_y: table.row_y,
                    block: block_index,
                    kind: CandidateKind::TableRow,
                });
            }
            let first_break_after = table.header_row_count.saturating_add(1).max(1);
            let safe_lines = safe_table_line_boundaries(block);
            let mut span_deltas = vec![0_i32; table.row_heights.len() + 1];
            for (start, end) in &table.row_spans {
                let start = start.saturating_add(1).min(table.row_heights.len());
                let end = (*end).min(table.row_heights.len());
                if start < end {
                    span_deltas[start] += 1;
                    span_deltas[end] -= 1;
                }
            }
            let mut active_spans = 0_i32;
            let mut row_start = table.row_y;
            for (row_index, row_height) in table.row_heights.iter().enumerate() {
                let row_end = row_start + row_height;
                if row_index >= table.header_row_count {
                    let first = safe_lines
                        .partition_point(|boundary| *boundary <= row_start + f32::EPSILON);
                    let last =
                        safe_lines.partition_point(|boundary| *boundary < row_end - f32::EPSILON);
                    for &boundary in &safe_lines[first..last] {
                        result.push(Candidate {
                            end_y: boundary,
                            next_y: boundary,
                            block: block_index,
                            kind: CandidateKind::TableRow,
                        });
                    }
                }
                let boundary_after = row_index + 1;
                active_spans += span_deltas[boundary_after];
                if boundary_after >= first_break_after
                    && boundary_after < table.row_heights.len()
                    && active_spans == 0
                {
                    result.push(Candidate {
                        end_y: row_end,
                        next_y: row_end,
                        block: block_index,
                        kind: CandidateKind::TableRow,
                    });
                }
                row_start = row_end;
            }
        }
        result.push(Candidate {
            end_y: block.y + block.height,
            next_y: layout
                .blocks
                .get(block_index + 1)
                .map_or(layout.height, |next| next.y),
            block: block_index,
            kind: CandidateKind::Block,
        });
    }
    result.sort_by(|left, right| {
        left.end_y
            .total_cmp(&right.end_y)
            .then_with(|| left.next_y.total_cmp(&right.next_y))
    });
    result
}

#[cfg(test)]
fn repeated_table_header(
    layout: &ContinuousLayout,
    start_y: f32,
    body_height: f32,
) -> Option<RepeatedTableHeader> {
    let (block_index, block) = layout.blocks.iter().enumerate().find(|(_, block)| {
        block.kind == LayoutBlockKind::Table
            && start_y > block.y + f32::EPSILON
            && start_y < block.y + block.height - f32::EPSILON
    })?;
    let table = block.table.as_ref()?;
    if table.header_row_count == 0 || start_y <= table.row_y + f32::EPSILON {
        return None;
    }
    let height = table.row_heights.iter().take(table.header_row_count).sum();
    if height >= body_height - MIN_BODY_HEIGHT {
        return None;
    }
    Some(RepeatedTableHeader {
        block: block_index,
        source_y: table.row_y,
        height,
    })
}

fn is_splittable(kind: LayoutBlockKind) -> bool {
    matches!(
        kind,
        LayoutBlockKind::Paragraph | LayoutBlockKind::Heading | LayoutBlockKind::Code
    )
}

#[cfg(test)]
fn legal_candidate(layout: &ContinuousLayout, candidate: Candidate, page_start: f32) -> bool {
    let block = &layout.blocks[candidate.block];

    // Forced breaks reject every later candidate on this page, leaving the
    // boundary immediately before or after the requesting block as the last
    // legal choice. The emergency path still guarantees progress for the
    // first block and for content taller than a page.
    if block.break_before == AuthorBreak::Page && page_start < block.y - f32::EPSILON {
        return false;
    }
    if candidate.block > 0 {
        let previous = &layout.blocks[candidate.block - 1];
        if previous.break_after == AuthorBreak::Page && page_start <= previous.y + f32::EPSILON {
            return false;
        }
    }

    if let CandidateKind::Line(line_index) = candidate.kind {
        if block.break_inside_avoid {
            return false;
        }
        let first_on_page = block
            .lines
            .partition_point(|line| line.y + line.height <= page_start + f32::EPSILON);
        let lines_here = line_index + 1 - first_on_page.min(line_index + 1);
        let lines_remaining = block.lines.len() - line_index - 1;
        if lines_here < usize::from(block.orphans) || lines_remaining < usize::from(block.widows) {
            return false;
        }
        // A heading immediately before this prose fragment must be accompanied
        // by at least two of its lines.
        if candidate.block > 0
            && layout.blocks[candidate.block - 1].kind == LayoutBlockKind::Heading
            && page_start <= layout.blocks[candidate.block - 1].y + f32::EPSILON
            && line_index + 1 < MIN_FRAGMENT_LINES
        {
            return false;
        }
        return true;
    }

    if matches!(candidate.kind, CandidateKind::Block)
        && (block.break_after == AuthorBreak::Avoid
            || layout
                .blocks
                .get(candidate.block + 1)
                .is_some_and(|next| next.break_before == AuthorBreak::Avoid))
    {
        return false;
    }

    // Do not leave a heading at the foot of a page. A later boundary in the
    // following prose block will be selected when it fits.
    if block.kind == LayoutBlockKind::Heading
        && layout
            .blocks
            .get(candidate.block + 1)
            .is_some_and(|next| next.kind == LayoutBlockKind::Paragraph)
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        layout::{
            AtomicLayout, AtomicLayoutKind, BlockBoxLayout, BlockLayout, BorderLayout,
            ComposedBlock, ComposedDocument, LineLayout, TableLayout, TableOverflowPolicy,
            TableSizingDiagnostics,
        },
        model::TextRange,
    };

    fn paragraph(y: f32, line_height: f32, lines: usize, source: usize) -> BlockLayout {
        BlockLayout {
            node: None,
            kind: LayoutBlockKind::Paragraph,
            range: TextRange {
                start: source,
                end: source + 10,
            },
            y,
            height: line_height * lines as f32,
            lines: (0..lines)
                .map(|index| LineLayout {
                    range: TextRange {
                        start: source + index,
                        end: source + index + 1,
                    },
                    y: y + index as f32 * line_height,
                    height: line_height,
                    baseline: line_height * 0.8,
                    runs: Vec::new(),
                })
                .collect(),
            atomic_objects: Vec::new(),
            table: None,
            rule: false,
            rule_x: 0.0,
            rule_width: 0.0,
            quoted: false,
            quote_bars: Vec::new(),
            box_layout: None,
            break_before: AuthorBreak::Auto,
            break_after: AuthorBreak::Auto,
            break_inside_avoid: false,
            widows: 2,
            orphans: 2,
        }
    }

    fn table(y: f32, row_heights: Vec<f32>, source: usize) -> BlockLayout {
        table_with_headers(y, row_heights, 1, source)
    }

    fn table_with_headers(
        y: f32,
        row_heights: Vec<f32>,
        header_row_count: usize,
        source: usize,
    ) -> BlockLayout {
        let height = row_heights.iter().sum();
        let mut cursor_y = y;
        let lines = row_heights
            .iter()
            .enumerate()
            .map(|(index, row_height)| {
                let line = LineLayout {
                    range: TextRange {
                        start: source + index,
                        end: source + index + 1,
                    },
                    y: cursor_y,
                    height: *row_height,
                    baseline: row_height * 0.8,
                    runs: Vec::new(),
                };
                cursor_y += row_height;
                line
            })
            .collect();
        BlockLayout {
            node: None,
            kind: LayoutBlockKind::Table,
            range: TextRange {
                start: source,
                end: source + row_heights.len(),
            },
            y,
            height,
            lines,
            atomic_objects: Vec::new(),
            table: Some(TableLayout {
                x: 0.0,
                width: 100.0,
                row_y: y,
                header_row_count,
                column_widths: vec![100.0],
                row_heights,
                row_spans: Vec::new(),
                overflow_policy: TableOverflowPolicy::EmergencyFit,
                sizing: TableSizingDiagnostics {
                    grid_min: 100.0,
                    grid_max: 100.0,
                    emergency_min: 100.0,
                    used_width: 100.0,
                    fallback_active: false,
                    overflows_available: false,
                },
                rules: Vec::new(),
                quote_bars: Vec::new(),
            }),
            rule: false,
            rule_x: 0.0,
            rule_width: 0.0,
            quoted: false,
            quote_bars: Vec::new(),
            box_layout: None,
            break_before: AuthorBreak::Auto,
            break_after: AuthorBreak::Auto,
            break_inside_avoid: false,
            widows: 2,
            orphans: 2,
        }
    }

    fn layout(blocks: Vec<BlockLayout>, height: f32) -> ContinuousLayout {
        ContinuousLayout::new(100.0, height, blocks)
    }

    fn composed_from_layout(layout: &ContinuousLayout) -> ComposedDocument {
        let mut cursor = 0.0;
        let mut blocks = Vec::with_capacity(layout.blocks.len());
        for source in &layout.blocks {
            let mut block = source.clone();
            let margin_before = source.y - cursor;
            block.translate_y(-source.y);
            cursor = source.y + source.height;
            blocks.push(ComposedBlock {
                layout: block,
                margin_before,
                margin_after: 0.0,
            });
        }
        ComposedDocument::from_test_blocks(100.0, blocks, 0.0, layout.height)
    }

    fn assert_vertical_parity(layout: &ContinuousLayout, body_height: f32) {
        let expected = paginate(layout, body_height);
        let stream = build_page_items(&composed_from_layout(layout));
        let actual = break_page_items(&stream, body_height).unwrap();
        assert_eq!(actual.segments.len(), expected.pages.len());
        for (selected, page) in actual.segments.iter().zip(&expected.pages) {
            assert!((selected.start - page.start_y).abs() < 0.01);
            assert!((selected.end - page.end_y).abs() < 0.01);
        }
    }

    fn phase4_layout(layout: &ContinuousLayout, body_height: f32) -> PaginatedLayout {
        let composed = composed_from_layout(layout);
        let input = build_page_items(&composed);
        let plan = break_page_items(&input, body_height).unwrap();
        materialize_paginated_layout(&composed, &input, &plan).unwrap()
    }

    #[test]
    fn paginated_layout_owns_every_line_once_with_page_local_geometry() {
        let document = layout(vec![paragraph(0.0, 10.0, 9, 0)], 90.0);
        let page_map = paginate(&document, 30.0);
        let paginated = phase4_layout(&document, 30.0);

        assert_eq!(paginated.pages.len(), page_map.pages.len());
        let lines = paginated
            .fragments
            .iter()
            .filter_map(|fragment| match fragment {
                PageFragment::Lines(fragment) => Some(fragment),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            lines
                .iter()
                .map(|line| line.line_end - line.line_start)
                .sum::<usize>(),
            9
        );
        assert!(lines.iter().all(|line| line.block == 0));
        assert!(
            lines
                .windows(2)
                .all(|lines| lines[0].line_end == lines[1].line_start)
        );
        assert!(lines.iter().all(|line| line.y >= 0.0));
        for line in &document.blocks[0].lines {
            assert_eq!(
                paginated.page_for_text(line.range.start),
                page_map.page_for_text(&document, line.range.start)
            );
        }
    }

    #[test]
    fn paginated_layout_materializes_atomics_boxes_and_quote_segments() {
        let mut image = paragraph(0.0, 100.0, 1, 0);
        image.kind = LayoutBlockKind::Image;
        image.lines.clear();
        image.height = 100.0;
        image.atomic_objects.push(AtomicLayout {
            node: None,
            range: image.range,
            x: 0.0,
            y: 0.0,
            width: 50.0,
            height: 100.0,
            leading_x: 0.0,
            trailing_x: 50.0,
            bidi: crate::model::flow::BidiState::default(),
            flow_link: None,
            kind: AtomicLayoutKind::Image {
                key: "flow-resource".into(),
                alt: "image".into(),
            },
        });
        let paginated = phase4_layout(&layout(vec![image], 100.0), 40.0);
        let atomic = paginated
            .fragments
            .iter()
            .find_map(|fragment| match fragment {
                PageFragment::Atomic(fragment) => Some(fragment),
                _ => None,
            })
            .unwrap();
        assert!((atomic.scale - 0.4).abs() < 0.01);
        assert!((atomic.height - 40.0).abs() < 0.01);

        let mut first = paragraph(0.0, 10.0, 2, 0);
        first.quoted = true;
        first.quote_bars = vec![12.0];
        let mut second = paragraph(25.0, 10.0, 2, 20);
        second.quoted = true;
        second.quote_bars = vec![12.0];
        second.box_layout = Some(BlockBoxLayout {
            x: 2.0,
            width: 96.0,
            borders: [BorderLayout {
                width: 1.0,
                color: None,
            }; 4],
        });
        let document = layout(vec![first, second], 45.0);
        let composed = composed_from_layout(&document);
        let input = build_page_items(&composed);
        let plan = break_page_items(&input, 25.0).unwrap();
        let paginated = materialize_paginated_layout(&composed, &input, &plan).unwrap();
        assert_eq!(
            paginated
                .fragments
                .iter()
                .filter(|fragment| matches!(fragment, PageFragment::QuoteBar(_)))
                .count(),
            2
        );
        assert!(
            paginated
                .fragments
                .iter()
                .any(|fragment| matches!(fragment, PageFragment::BlockBox(_)))
        );

        let quote_index = paginated
            .fragments
            .iter()
            .position(|fragment| matches!(fragment, PageFragment::QuoteBar(_)))
            .unwrap();
        let mut missing_quote = paginated.clone();
        missing_quote.fragments.remove(quote_index);
        for page in &mut missing_quote.pages {
            if page.fragments.start > quote_index {
                page.fragments.start -= 1;
                page.fragments.end -= 1;
            } else if quote_index < page.fragments.end {
                page.fragments.end -= 1;
            }
        }
        assert!(matches!(
            validate_paginated_layout(&composed, &input, &missing_quote),
            Err(FragmentValidationError::MissingDecoration {
                kind: DecorationKind::QuoteBar,
                ..
            })
        ));
    }

    #[test]
    fn paginated_layout_validator_rejects_duplicate_content() {
        let document = layout(vec![paragraph(0.0, 10.0, 3, 0)], 30.0);
        let composed = composed_from_layout(&document);
        let input = build_page_items(&composed);
        let plan = break_page_items(&input, 30.0).unwrap();
        let mut paginated = materialize_paginated_layout(&composed, &input, &plan).unwrap();
        let duplicate = paginated
            .fragments
            .iter()
            .find(|fragment| matches!(fragment, PageFragment::Lines(_)))
            .unwrap()
            .clone();
        paginated.fragments.push(duplicate);
        paginated.pages[0].fragments.end += 1;
        assert!(matches!(
            validate_paginated_layout(&composed, &input, &paginated),
            Err(FragmentValidationError::ContentOutOfOrder)
                | Err(FragmentValidationError::DuplicateLine { .. })
        ));
    }

    #[test]
    fn vertical_item_stream_matches_non_table_pagination_policy() {
        let mut heading = paragraph(25.0, 10.0, 1, 20);
        heading.kind = LayoutBlockKind::Heading;
        let mut prose = paragraph(40.0, 10.0, 7, 40);
        prose.widows = 3;
        prose.orphans = 2;
        let document = layout(vec![paragraph(0.0, 10.0, 2, 0), heading, prose], 110.0);
        for body_height in [5.0, 30.0, 45.0, 80.0, f32::INFINITY] {
            assert_vertical_parity(&document, body_height);
        }
    }

    #[test]
    fn vertical_breaker_exhaustively_matches_paragraph_boundaries_and_progresses() {
        for lines in 1..=16 {
            for body_height in [1.0, 5.0, 9.0, 10.0, 19.0, 20.0, 31.0, 80.0] {
                for (widows, orphans) in [(1, 1), (2, 2), (3, 2), (2, 4)] {
                    let mut block = paragraph(0.0, 10.0, lines, 0);
                    block.widows = widows;
                    block.orphans = orphans;
                    let document = layout(vec![block], lines as f32 * 10.0);
                    assert_vertical_parity(&document, body_height);
                    let plan = break_page_items(
                        &build_page_items(&composed_from_layout(&document)),
                        body_height,
                    )
                    .unwrap();
                    assert!(
                        plan.segments
                            .windows(2)
                            .all(|pair| pair[1].start > pair[0].start)
                    );
                }
            }
        }
    }

    #[test]
    fn vertical_item_stream_matches_forced_avoided_and_atomic_boundaries() {
        let mut first = paragraph(0.0, 10.0, 2, 0);
        first.break_after = AuthorBreak::Page;
        let mut kept = paragraph(25.0, 10.0, 4, 20);
        kept.break_inside_avoid = true;
        let document = layout(vec![first, kept], 65.0);
        assert_vertical_parity(&document, 50.0);
        assert_vertical_parity(&document, 5.0);

        let mut atomic = paragraph(0.0, 100.0, 1, 0);
        atomic.kind = LayoutBlockKind::Image;
        atomic.lines.clear();
        atomic.height = 100.0;
        assert_vertical_parity(&layout(vec![atomic], 100.0), 40.0);
    }

    #[test]
    fn vertical_item_dump_is_deterministic_and_text_free() {
        let document = layout(vec![paragraph(5.0, 10.0, 3, 0)], 40.0);
        let stream = build_page_items(&composed_from_layout(&document));
        let dump = stream.debug_dump();
        assert!(dump.starts_with("vertical-items extent=40.000"));
        assert!(dump.contains("box block=0 boundary=Line(0)"));
        assert!(dump.contains("penalty block=0 boundary=Block"));
        assert!(!dump.contains("source"));
        let plan = break_page_items(&stream, 20.0).unwrap();
        assert!(debug_page_break_plan(&plan).starts_with("break-plan extent=40.000"));
    }

    #[test]
    fn vertical_breaker_materializes_table_fragments() {
        let source = layout(vec![table(0.0, vec![10.0, 20.0], 0)], 30.0);
        let document = composed_from_layout(&source);
        let stream = build_page_items(&document);
        let plan = break_page_items(&stream, 20.0).unwrap();
        let paginated = materialize_paginated_layout(&document, &stream, &plan).unwrap();
        assert_eq!(
            validate_paginated_layout(&document, &stream, &paginated),
            Ok(())
        );
        assert!(
            paginated
                .fragments
                .iter()
                .any(|fragment| { matches!(fragment, PageFragment::TableSlice(_)) })
        );
    }

    #[test]
    fn table_pages_own_content_once_and_repeat_headers_explicitly() {
        let source = layout(
            vec![table_with_headers(0.0, vec![10.0, 10.0, 10.0, 10.0], 1, 0)],
            40.0,
        );
        let document = composed_from_layout(&source);
        let input = build_page_items(&document);
        let plan = break_page_items(&input, 20.0).unwrap();
        let paginated = materialize_paginated_layout(&document, &input, &plan).unwrap();
        assert_eq!(
            validate_paginated_layout(&document, &input, &paginated),
            Ok(())
        );
        assert!(paginated.pages.len() > 1);
        assert!(
            paginated.pages[1..]
                .iter()
                .all(|page| page.repeated_table_header.is_some())
        );
        assert!(
            paginated
                .fragments
                .iter()
                .any(|fragment| { matches!(fragment, PageFragment::Lines(line) if line.replica) })
        );
        assert!(
            paginated.fragments.iter().any(|fragment| {
                matches!(fragment, PageFragment::TableRule(rule) if rule.replica)
            })
        );
        assert!(paginated.fragments.iter().any(|fragment| {
            matches!(fragment, PageFragment::TableRule(rule) if rule.replica && rule.continuation)
        }));
        assert!(!paginated.fragments.iter().any(|fragment| {
            matches!(fragment, PageFragment::TableRule(rule) if !rule.replica && rule.rule.is_none())
        }));
        let ordinary_lines = paginated
            .fragments
            .iter()
            .filter_map(|fragment| match fragment {
                PageFragment::Lines(line) if !line.replica => Some(line.line_end - line.line_start),
                _ => None,
            })
            .sum::<usize>();
        assert_eq!(ordinary_lines, document.blocks[0].layout.lines.len());
    }

    #[test]
    fn oversized_table_rows_use_named_emergency_fragments() {
        let source = layout(vec![table(0.0, vec![10.0, 60.0], 0)], 70.0);
        let document = composed_from_layout(&source);
        let input = build_page_items(&document);
        let plan = break_page_items(&input, 25.0).unwrap();
        let paginated = materialize_paginated_layout(&document, &input, &plan).unwrap();
        assert!(paginated.fragments.iter().any(|fragment| {
            matches!(
                fragment,
                PageFragment::TableSlice(TableSliceFragment {
                    kind: TableSliceKind::EmergencySlice,
                    ..
                })
            )
        }));
    }

    #[test]
    fn exact_fit_is_one_page() {
        let map = paginate(&layout(vec![paragraph(0.0, 10.0, 4, 0)], 40.0), 40.0);
        assert_eq!(map.pages.len(), 1);
        assert_eq!(map.pages[0].end_y, 40.0);
    }

    #[test]
    fn honors_author_widows_and_orphans() {
        let mut block = paragraph(0.0, 10.0, 5, 0);
        block.widows = 3;
        block.orphans = 2;
        let map = paginate(&layout(vec![block], 50.0), 30.0);
        assert_eq!(map.pages[0].end_y, 20.0);
    }

    #[test]
    fn keeps_avoid_inside_block_together_when_it_fits_a_fresh_page() {
        let first = paragraph(0.0, 10.0, 2, 0);
        let mut kept = paragraph(20.0, 10.0, 4, 20);
        kept.break_inside_avoid = true;
        let map = paginate(&layout(vec![first, kept], 60.0), 50.0);
        assert_eq!(map.pages[0].end_y, 20.0);
        assert_eq!(map.pages[1].end_y, 60.0);
    }

    #[test]
    fn forces_and_avoids_boundaries_between_blocks() {
        let mut first = paragraph(0.0, 10.0, 2, 0);
        first.break_after = AuthorBreak::Page;
        let second = paragraph(20.0, 10.0, 2, 20);
        let map = paginate(&layout(vec![first, second], 40.0), 100.0);
        assert_eq!(map.pages.len(), 2);
        assert_eq!(map.pages[0].end_y, 20.0);

        let first = paragraph(0.0, 10.0, 2, 0);
        let mut second = paragraph(20.0, 10.0, 4, 20);
        second.break_before = AuthorBreak::Avoid;
        let map = paginate(&layout(vec![first, second], 60.0), 50.0);
        assert!(map.pages[0].end_y > 20.0);
    }

    #[test]
    fn enforces_two_line_widows_and_orphans() {
        let map = paginate(&layout(vec![paragraph(0.0, 10.0, 5, 0)], 50.0), 30.0);
        assert_eq!(map.pages.len(), 2);
        assert_eq!(map.pages[0].end_y, 30.0);
        assert_eq!(map.pages[1].start_y, 30.0);
    }

    #[test]
    fn short_paragraph_moves_as_a_unit_when_it_can() {
        let blocks = vec![paragraph(0.0, 10.0, 2, 0), paragraph(25.0, 10.0, 3, 20)];
        let map = paginate(&layout(blocks, 55.0), 40.0);
        assert_eq!(map.pages.len(), 2);
        assert_eq!(map.pages[0].end_y, 20.0);
        assert_eq!(map.pages[1].start_y, 25.0);
    }

    #[test]
    fn long_paragraph_makes_progress_through_many_pages() {
        let map = paginate(&layout(vec![paragraph(0.0, 10.0, 11, 0)], 110.0), 30.0);
        assert_eq!(map.pages.len(), 4);
        assert_eq!(map.pages.last().unwrap().end_y, 110.0);
    }

    #[test]
    fn tiny_pages_relax_constraints_and_still_progress() {
        let map = paginate(&layout(vec![paragraph(0.0, 10.0, 3, 0)], 30.0), 5.0);
        assert_eq!(map.pages.len(), 3);
        assert!(
            map.pages
                .windows(2)
                .all(|pages| pages[1].start_y > pages[0].start_y)
        );
    }

    #[test]
    fn heading_moves_to_keep_two_lines_of_following_prose() {
        let first = paragraph(0.0, 10.0, 2, 0);
        let mut heading = paragraph(25.0, 10.0, 1, 20);
        heading.kind = LayoutBlockKind::Heading;
        let prose = paragraph(40.0, 10.0, 4, 40);

        let map = paginate(&layout(vec![first, heading, prose], 80.0), 45.0);

        assert_eq!(map.pages[0].end_y, 20.0);
        assert_eq!(map.pages[1].start_y, 25.0);
        assert!(map.pages[1].end_y >= 60.0);
    }

    #[test]
    fn oversized_image_is_atomic_and_scaled() {
        let mut image = paragraph(0.0, 100.0, 1, 0);
        image.kind = LayoutBlockKind::Image;
        image.lines.clear();
        image.height = 100.0;
        image.atomic_objects.push(AtomicLayout {
            node: None,
            range: image.range,
            x: 0.0,
            y: 0.0,
            width: 50.0,
            height: 100.0,
            leading_x: 0.0,
            trailing_x: 50.0,
            bidi: crate::model::flow::BidiState::default(),
            flow_link: None,
            kind: AtomicLayoutKind::Image {
                key: "flow-resource".into(),
                alt: "image".into(),
            },
        });
        let map = paginate(&layout(vec![image], 100.0), 40.0);

        assert_eq!(map.pages.len(), 1);
        assert_eq!(map.pages[0].scaled_atomics, vec![(0, 0, 0.4)]);
    }

    #[test]
    fn oversized_inline_atomic_object_is_scaled_without_changing_block_kind() {
        let mut paragraph = paragraph(0.0, 100.0, 1, 0);
        paragraph.atomic_objects.push(AtomicLayout {
            node: None,
            range: paragraph.range,
            x: 20.0,
            y: 0.0,
            width: 80.0,
            height: 100.0,
            leading_x: 20.0,
            trailing_x: 100.0,
            bidi: crate::model::flow::BidiState::default(),
            flow_link: None,
            kind: AtomicLayoutKind::Placeholder {
                label: "large math".into(),
            },
        });

        let map = paginate(&layout(vec![paragraph], 100.0), 40.0);

        assert_eq!(map.pages.len(), 1);
        assert_eq!(map.pages[0].scaled_atomics, vec![(0, 0, 0.4)]);
    }

    #[test]
    fn oversized_table_caption_atomic_does_not_skip_following_rows() {
        let mut table = table(0.0, vec![10.0, 10.0], 10);
        table.height = 120.0;
        for line in &mut table.lines {
            line.y += 100.0;
        }
        table.table.as_mut().unwrap().row_y = 100.0;
        table.atomic_objects.push(AtomicLayout {
            node: None,
            range: TextRange { start: 0, end: 7 },
            x: 0.0,
            y: 0.0,
            width: 80.0,
            height: 100.0,
            leading_x: 0.0,
            trailing_x: 80.0,
            bidi: crate::model::flow::BidiState::default(),
            flow_link: None,
            kind: AtomicLayoutKind::Image {
                key: "caption-image".into(),
                alt: "caption".into(),
            },
        });

        let map = paginate(&layout(vec![table], 120.0), 40.0);

        assert_eq!(map.pages.len(), 2);
        assert_eq!((map.pages[0].start_y, map.pages[0].end_y), (0.0, 100.0));
        assert_eq!(map.pages[0].scaled_atomics, vec![(0, 0, 0.4)]);
        assert_eq!((map.pages[1].start_y, map.pages[1].end_y), (100.0, 120.0));
    }

    #[test]
    fn oversized_table_atomic_is_kept_whole_below_repeated_headers() {
        let mut table = table(0.0, vec![10.0, 10.0, 100.0], 0);
        table.atomic_objects.push(AtomicLayout {
            node: None,
            range: TextRange { start: 2, end: 3 },
            x: 0.0,
            y: 20.0,
            width: 80.0,
            height: 100.0,
            leading_x: 0.0,
            trailing_x: 80.0,
            bidi: crate::model::flow::BidiState::default(),
            flow_link: None,
            kind: AtomicLayoutKind::Image {
                key: "large-table-image".into(),
                alt: "large".into(),
            },
        });

        let map = paginate(&layout(vec![table], 120.0), 40.0);

        assert_eq!(map.pages.len(), 2);
        assert_eq!((map.pages[0].start_y, map.pages[0].end_y), (0.0, 20.0));
        assert_eq!((map.pages[1].start_y, map.pages[1].end_y), (20.0, 120.0));
        assert_eq!(map.pages[1].repeated_table_header.unwrap().height, 10.0);
        assert_eq!(map.pages[1].scaled_atomics, vec![(0, 0, 0.3)]);
    }

    #[test]
    fn splits_tables_at_row_boundaries_and_repeats_the_header() {
        let layout = layout(
            vec![table(0.0, vec![10.0, 10.0, 10.0, 10.0, 10.0, 10.0], 100)],
            60.0,
        );
        let map = paginate(&layout, 25.0);

        assert_eq!(map.pages.len(), 5);
        assert_eq!((map.pages[0].start_y, map.pages[0].end_y), (0.0, 20.0));
        assert!(map.pages[0].repeated_table_header.is_none());
        for page in &map.pages[1..] {
            let header = page
                .repeated_table_header
                .expect("continuation page repeats the table header");
            assert_eq!(header.block, 0);
            assert_eq!(header.source_y, 0.0);
            assert_eq!(header.height, 10.0);
        }
    }

    #[test]
    fn repeated_table_header_and_body_map_back_to_document_coordinates() {
        let layout = layout(vec![table(0.0, vec![10.0, 10.0, 10.0, 10.0], 100)], 40.0);
        let map = paginate(&layout, 25.0);
        let page = &map.pages[1];

        assert_eq!(page.document_y_at(2.0), Some(2.0));
        assert_eq!(page.document_y_at(12.0), Some(page.start_y + 2.0));
        assert_eq!(page.page_y_for_document_y(2.0), Some(2.0));
        assert_eq!(page.page_y_for_document_y(page.start_y + 2.0), Some(12.0));
    }

    #[test]
    fn repeats_multiple_header_rows_and_fragments_an_oversized_body_row() {
        let mut table = table_with_headers(0.0, vec![10.0, 10.0, 50.0, 10.0], 2, 100);
        table.lines = (0..7)
            .map(|index| LineLayout {
                range: TextRange {
                    start: 100 + index,
                    end: 101 + index,
                },
                y: index as f32 * 10.0,
                height: 10.0,
                baseline: 8.0,
                runs: Vec::new(),
            })
            .collect();
        let layout = layout(vec![table], 80.0);

        let map = paginate(&layout, 35.0);

        assert!(map.pages.len() >= 4);
        assert_eq!(map.pages.last().unwrap().end_y, 80.0);
        for page in &map.pages[1..] {
            let header = page
                .repeated_table_header
                .expect("continuation repeats both header rows");
            assert_eq!(header.height, 20.0);
            assert_eq!(page.document_y_at(5.0), Some(5.0));
            assert_eq!(page.page_y_for_document_y(5.0), Some(5.0));
        }
        assert!(
            map.pages
                .windows(2)
                .all(|pages| pages[1].start_y > pages[0].start_y)
        );
    }

    #[test]
    fn slices_an_unfragmentable_oversized_table_row_without_data_loss() {
        let mut table = table_with_headers(0.0, vec![100.0], 0, 100);
        table.lines[0].height = 100.0;
        let map = paginate(&layout(vec![table], 100.0), 10.0);

        assert_eq!(map.pages.len(), 10);
        assert_eq!(map.pages.last().unwrap().end_y, 100.0);
        assert!(map.pages.iter().all(|page| page.end_y > page.start_y));
        assert!(
            map.pages
                .windows(2)
                .all(|pages| (pages[0].end_y - pages[1].start_y).abs() < 0.01)
        );
    }

    #[test]
    fn row_boundaries_do_not_split_active_rowspans() {
        let mut table = table_with_headers(0.0, vec![10.0, 20.0, 20.0, 10.0], 1, 100);
        table.table.as_mut().unwrap().row_spans.push((1, 3));
        let map = paginate(&layout(vec![table], 60.0), 35.0);

        assert!(
            map.pages
                .iter()
                .all(|page| (page.end_y - 30.0).abs() > 0.01)
        );
        assert_eq!(map.pages.last().unwrap().end_y, 60.0);
    }

    #[test]
    fn table_header_is_kept_with_its_first_body_row() {
        let first = paragraph(0.0, 10.0, 1, 0);
        let table = table(15.0, vec![10.0, 10.0, 10.0], 100);
        let map = paginate(&layout(vec![first, table], 45.0), 30.0);

        assert_eq!(map.pages[0].end_y, 10.0);
        assert_eq!(map.pages[1].start_y, 15.0);
        assert!(map.pages[1].end_y >= 35.0);
    }

    #[test]
    fn logical_text_lookup_tracks_repagination() {
        let layout = layout(vec![paragraph(0.0, 10.0, 6, 100)], 60.0);
        let map = paginate(&layout, 30.0);
        assert_eq!(map.page_for_text(&layout, 104), 1);
    }
}
