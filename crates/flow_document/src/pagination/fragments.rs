use std::ops::Range;

use crate::{
    layout::{ComposedDocument, LayoutBlockKind, TableRuleKind},
    model::TextRange,
};

use super::{PageBreakInput, sequence::BreakPlan};

const GEOMETRY_TOLERANCE: f32 = 0.01;
const MIN_FRAGMENT_EXTENT: f32 = 1.0;

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FragmentRange {
    pub start: usize,
    pub end: usize,
}

impl FragmentRange {
    pub fn as_range(self) -> Range<usize> {
        self.start..self.end
    }
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaginatedLayout {
    pub width: f32,
    pub content_height: f32,
    pub body_height: f32,
    pub pages: Vec<ComposedPage>,
    pub fragments: Vec<PageFragment>,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq)]
pub struct ComposedPage {
    pub start_y: f32,
    pub end_y: f32,
    pub next_y: f32,
    pub text_anchor: usize,
    pub source_range: Option<TextRange>,
    pub repeated_table_header: Option<RepeatedTableHeaderFragment>,
    pub fragments: FragmentRange,
}

impl ComposedPage {
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

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RepeatedTableHeaderFragment {
    pub block: usize,
    pub source_y: f32,
    pub height: f32,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq)]
pub enum PageFragment {
    Lines(LineFragment),
    Atomic(AtomicFragment),
    Rule(RuleFragment),
    QuoteBar(QuoteBarFragment),
    BlockBox(BlockBoxFragment),
    TableSlice(TableSliceFragment),
    TableRule(TableRuleFragment),
    TableQuoteBar(TableQuoteBarFragment),
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineFragment {
    pub block: usize,
    pub line_start: usize,
    pub line_end: usize,
    pub range: TextRange,
    pub y: f32,
    pub height: f32,
    pub has_generated_runs: bool,
    pub replica: bool,
    pub overflows_page: bool,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AtomicFragment {
    pub block: usize,
    pub atomic: usize,
    pub range: TextRange,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub scale: f32,
    pub replica: bool,
    pub overflows_page: bool,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RuleFragment {
    pub block: usize,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub overflows_page: bool,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QuoteBarFragment {
    pub level: usize,
    pub first_block: usize,
    pub last_block: usize,
    pub x: f32,
    pub y: f32,
    pub height: f32,
    pub overflows_page: bool,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TableSliceFragment {
    pub block: usize,
    pub source_start: f32,
    pub source_end: f32,
    pub y: f32,
    pub height: f32,
    pub kind: TableSliceKind,
    pub overflows_page: bool,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableSliceKind {
    CompleteRows,
    RowContinuation,
    EmergencySlice,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TableRuleFragment {
    pub block: usize,
    pub rule: Option<usize>,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub kind: TableRuleKind,
    pub replica: bool,
    pub continuation: bool,
    pub overflows_page: bool,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TableQuoteBarFragment {
    pub block: usize,
    pub quote_bar: usize,
    pub x: f32,
    pub y: f32,
    pub height: f32,
    pub replica: bool,
    pub overflows_page: bool,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockBoxFragment {
    pub block: usize,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub draw_top: bool,
    pub draw_bottom: bool,
    pub overflows_page: bool,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecorationKind {
    Rule,
    QuoteBar,
    BlockBox,
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FragmentValidationError {
    PagePlanMismatch,
    InvalidPageGeometry {
        page: usize,
    },
    InvalidPageFragmentRange {
        page: usize,
    },
    InvalidFragmentGeometry {
        fragment: usize,
    },
    InvalidFragmentReference {
        fragment: usize,
    },
    FragmentOwnedByWrongPage {
        page: usize,
        fragment: usize,
    },
    MissingLine {
        block: usize,
        line: usize,
    },
    DuplicateLine {
        block: usize,
        line: usize,
    },
    MissingAtomic {
        block: usize,
        atomic: usize,
    },
    DuplicateAtomic {
        block: usize,
        atomic: usize,
    },
    MissingDecoration {
        kind: DecorationKind,
        owner: usize,
        page: usize,
    },
    DuplicateDecoration {
        kind: DecorationKind,
        owner: usize,
        page: usize,
    },
    ContentOutOfOrder,
    IncorrectSourceRange {
        page: usize,
    },
}

#[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug)]
struct QuoteSpan {
    level: usize,
    first_block: usize,
    last_block: usize,
    x: f32,
    start_y: f32,
    end_y: f32,
}

/// Materializes page-local, exclusively-owned non-table fragments without
/// changing the production painter.
pub fn materialize_paginated_layout(
    document: &ComposedDocument,
    input: &PageBreakInput,
    plan: &BreakPlan,
) -> Result<PaginatedLayout, FragmentValidationError> {
    let _span = crate::telemetry::phase_span!(
        "flow.pagination.materialize",
        blocks = document.blocks.len() as u64,
        pages = plan.segments.len() as u64,
    );
    if (input.stream.content_extent - plan.content_extent).abs() > GEOMETRY_TOLERANCE {
        return Err(FragmentValidationError::PagePlanMismatch);
    }

    let quote_spans = quote_spans(document, input);
    let mut line_fragments = 0;
    for (block_index, composed) in document.blocks.iter().enumerate() {
        let block_y = input.block_y(block_index).unwrap_or(0.0);
        let mut previous_owner = None;
        for line in &composed.layout.lines {
            let line_y = block_y + line.y - composed.layout.y;
            let owner = owner_page(&plan.segments, line_y, line_y + line.height);
            if owner.is_some() && owner != previous_owner {
                line_fragments += 1;
            }
            previous_owner = owner;
        }
    }
    let content_fragments = line_fragments
        + document
            .blocks
            .iter()
            .map(|block| block.layout.atomic_objects.len() + usize::from(block.layout.rule))
            .sum::<usize>();
    let box_fragments: usize = document
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| block.layout.box_layout.is_some())
        .map(|(block_index, block)| {
            let block_y = input.block_y(block_index).unwrap_or(0.0);
            let block_end = block_y + block.layout.height;
            plan.segments
                .iter()
                .filter(|page| {
                    block_end > page.start + GEOMETRY_TOLERANCE
                        && block_y < page.end - GEOMETRY_TOLERANCE
                })
                .count()
        })
        .sum();
    let quote_fragments: usize = quote_spans
        .iter()
        .map(|span| {
            plan.segments
                .iter()
                .filter(|page| {
                    span.end_y.min(page.end) > span.start_y.max(page.start) + f32::EPSILON
                })
                .count()
        })
        .sum();
    let mut fragments = Vec::with_capacity(content_fragments + box_fragments + quote_fragments);
    let mut pages = Vec::with_capacity(plan.segments.len());

    for (page_index, selected) in plan.segments.iter().enumerate() {
        if !selected.start.is_finite()
            || !selected.end.is_finite()
            || !selected.next.is_finite()
            || selected.end < selected.start - GEOMETRY_TOLERANCE
            || selected.next < selected.start - GEOMETRY_TOLERANCE
        {
            return Err(FragmentValidationError::InvalidPageGeometry { page: page_index });
        }
        let fragment_start = fragments.len();
        let mut source_range: Option<TextRange> = None;
        let mut text_anchor = None;
        let repeated_table_header = input
            .repeated_table_header(selected.start, plan.target_extent)
            .map(|header| RepeatedTableHeaderFragment {
                block: header.block,
                source_y: header.source_y,
                height: header.height,
            });
        let content_offset = repeated_table_header.map_or(0.0, |header| header.height);

        for (block_index, composed) in document.blocks.iter().enumerate() {
            let block = &composed.layout;
            let block_y = input
                .block_y(block_index)
                .ok_or(FragmentValidationError::PagePlanMismatch)?;
            let block_end = block_y + block.height;
            if block_end <= selected.start + GEOMETRY_TOLERANCE
                || block_y >= selected.end - GEOMETRY_TOLERANCE
            {
                continue;
            }

            if let Some(box_layout) = &block.box_layout {
                let segment_start = block_y.max(selected.start);
                let segment_end = block_end.min(selected.end);
                if segment_end > segment_start + f32::EPSILON {
                    fragments.push(PageFragment::BlockBox(BlockBoxFragment {
                        block: block_index,
                        x: box_layout.x,
                        y: content_offset + segment_start - selected.start,
                        width: box_layout.width,
                        height: segment_end - segment_start,
                        draw_top: segment_start <= block_y + GEOMETRY_TOLERANCE,
                        draw_bottom: segment_end >= block_end - GEOMETRY_TOLERANCE,
                        overflows_page: content_offset + segment_end - selected.start
                            > plan.target_extent + GEOMETRY_TOLERANCE,
                    }));
                }
            }

            if block.kind == LayoutBlockKind::Table
                && let Some(table) = &block.table
            {
                let source_start = block_y.max(selected.start);
                let source_end = block_end.min(selected.end);
                let kind =
                    table_slice_kind(block, block_y, source_start, source_end, selected.relaxed);
                fragments.push(PageFragment::TableSlice(TableSliceFragment {
                    block: block_index,
                    source_start,
                    source_end,
                    y: content_offset + source_start - selected.start,
                    height: source_end - source_start,
                    kind,
                    overflows_page: content_offset + source_end - selected.start
                        > plan.target_extent + GEOMETRY_TOLERANCE,
                }));

                for (rule_index, rule) in table.rules.iter().enumerate() {
                    let rule_y = block_y + rule.y - block.y;
                    let at_final_edge = (rule_y - block_end).abs() <= GEOMETRY_TOLERANCE
                        && (source_end - block_end).abs() <= GEOMETRY_TOLERANCE;
                    if rule_y >= source_start - GEOMETRY_TOLERANCE
                        && (rule_y < source_end - GEOMETRY_TOLERANCE || at_final_edge)
                    {
                        let y = content_offset + rule_y - selected.start;
                        fragments.push(PageFragment::TableRule(TableRuleFragment {
                            block: block_index,
                            rule: Some(rule_index),
                            x: rule.x,
                            y,
                            width: rule.width,
                            kind: rule.kind,
                            replica: false,
                            continuation: false,
                            overflows_page: y > plan.target_extent + GEOMETRY_TOLERANCE,
                        }));
                    }
                }
                for (quote_index, quote) in table.quote_bars.iter().enumerate() {
                    let quote_start = block_y + quote.y - block.y;
                    let quote_end = quote_start + quote.height;
                    let segment_start = quote_start.max(source_start);
                    let segment_end = quote_end.min(source_end);
                    if segment_end > segment_start + f32::EPSILON {
                        let y = content_offset + segment_start - selected.start;
                        fragments.push(PageFragment::TableQuoteBar(TableQuoteBarFragment {
                            block: block_index,
                            quote_bar: quote_index,
                            x: quote.x,
                            y,
                            height: segment_end - segment_start,
                            replica: false,
                            overflows_page: y + segment_end - segment_start
                                > plan.target_extent + GEOMETRY_TOLERANCE,
                        }));
                    }
                }
            }

            let mut line_index = 0;
            while line_index < block.lines.len() {
                let line = &block.lines[line_index];
                let line_y = block_y + line.y - block.y;
                if owner_page(&plan.segments, line_y, line_y + line.height) != Some(page_index) {
                    line_index += 1;
                    continue;
                }
                let line_start = line_index;
                line_index += 1;
                while line_index < block.lines.len() {
                    let next = &block.lines[line_index];
                    let next_y = block_y + next.y - block.y;
                    if owner_page(&plan.segments, next_y, next_y + next.height) != Some(page_index)
                    {
                        break;
                    }
                    line_index += 1;
                }
                let first = &block.lines[line_start];
                let last = &block.lines[line_index - 1];
                // Table lines are ordered geometrically, not by canonical text:
                // adjacent columns can interleave ranges from different cells.
                let range = line_source_range(&block.lines[line_start..line_index]);
                let fragment_y = content_offset + block_y + first.y - block.y - selected.start;
                let height = last.y + last.height - first.y;
                include_source(&mut source_range, range);
                text_anchor.get_or_insert(range.start);
                fragments.push(PageFragment::Lines(LineFragment {
                    block: block_index,
                    line_start,
                    line_end: line_index,
                    range,
                    y: fragment_y,
                    height,
                    has_generated_runs: block.lines[line_start..line_index]
                        .iter()
                        .flat_map(|line| &line.runs)
                        .any(|run| run.range.is_empty() && !run.text.is_empty()),
                    replica: false,
                    overflows_page: fragment_y < -GEOMETRY_TOLERANCE
                        || fragment_y + height > plan.target_extent + GEOMETRY_TOLERANCE,
                }));
            }

            for (atomic_index, atomic) in block.atomic_objects.iter().enumerate() {
                let atomic_y = block_y + atomic.y - block.y;
                if owner_page(&plan.segments, atomic_y, atomic_y + atomic.height)
                    != Some(page_index)
                {
                    continue;
                }
                include_source(&mut source_range, atomic.range);
                text_anchor.get_or_insert(atomic.range.start);
                let fragment_y = content_offset + atomic_y - selected.start;
                let available = (plan.target_extent - fragment_y.max(0.0)).max(MIN_FRAGMENT_EXTENT);
                let scale = if atomic.height > available {
                    (available / atomic.height).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                fragments.push(PageFragment::Atomic(AtomicFragment {
                    block: block_index,
                    atomic: atomic_index,
                    range: atomic.range,
                    x: atomic.x,
                    y: fragment_y,
                    width: atomic.width * scale,
                    height: atomic.height * scale,
                    scale,
                    replica: false,
                    overflows_page: fragment_y < -GEOMETRY_TOLERANCE
                        || fragment_y + atomic.height * scale
                            > plan.target_extent + GEOMETRY_TOLERANCE,
                }));
            }

            if let Some(header) = repeated_table_header.filter(|header| header.block == block_index)
                && let Some(table) = &block.table
            {
                let header_end = header.source_y + header.height;
                for (line_index, line) in block.lines.iter().enumerate() {
                    let line_y = block_y + line.y - block.y;
                    if line_y < header_end - GEOMETRY_TOLERANCE
                        && line_y + line.height > header.source_y + GEOMETRY_TOLERANCE
                        && (line.runs.is_empty()
                            || line
                                .runs
                                .iter()
                                .any(|run| run.x >= table.x - GEOMETRY_TOLERANCE))
                    {
                        let y = line_y - header.source_y;
                        fragments.push(PageFragment::Lines(LineFragment {
                            block: block_index,
                            line_start: line_index,
                            line_end: line_index + 1,
                            range: line.range,
                            y,
                            height: line.height,
                            has_generated_runs: line
                                .runs
                                .iter()
                                .any(|run| run.range.is_empty() && !run.text.is_empty()),
                            replica: true,
                            overflows_page: y < -GEOMETRY_TOLERANCE
                                || y + line.height > header.height + GEOMETRY_TOLERANCE,
                        }));
                    }
                }
                for (atomic_index, atomic) in block.atomic_objects.iter().enumerate() {
                    let atomic_y = block_y + atomic.y - block.y;
                    if atomic_y < header_end - GEOMETRY_TOLERANCE
                        && atomic_y + atomic.height > header.source_y + GEOMETRY_TOLERANCE
                    {
                        let y = atomic_y - header.source_y;
                        fragments.push(PageFragment::Atomic(AtomicFragment {
                            block: block_index,
                            atomic: atomic_index,
                            range: atomic.range,
                            x: atomic.x,
                            y,
                            width: atomic.width,
                            height: atomic.height,
                            scale: 1.0,
                            replica: true,
                            overflows_page: y < -GEOMETRY_TOLERANCE
                                || y + atomic.height > header.height + GEOMETRY_TOLERANCE,
                        }));
                    }
                }
                for (rule_index, rule) in table.rules.iter().enumerate() {
                    let rule_y = block_y + rule.y - block.y;
                    if rule_y >= header.source_y - GEOMETRY_TOLERANCE
                        && rule_y <= header_end + GEOMETRY_TOLERANCE
                    {
                        fragments.push(PageFragment::TableRule(TableRuleFragment {
                            block: block_index,
                            rule: Some(rule_index),
                            x: rule.x,
                            y: rule_y - header.source_y,
                            width: rule.width,
                            kind: rule.kind,
                            replica: true,
                            continuation: false,
                            overflows_page: false,
                        }));
                    }
                }
                for (quote_index, quote) in table.quote_bars.iter().enumerate() {
                    let quote_start = block_y + quote.y - block.y;
                    let quote_end = quote_start + quote.height;
                    let segment_start = quote_start.max(header.source_y);
                    let segment_end = quote_end.min(header_end);
                    if segment_end > segment_start + f32::EPSILON {
                        fragments.push(PageFragment::TableQuoteBar(TableQuoteBarFragment {
                            block: block_index,
                            quote_bar: quote_index,
                            x: quote.x,
                            y: segment_start - header.source_y,
                            height: segment_end - segment_start,
                            replica: true,
                            overflows_page: false,
                        }));
                    }
                }
                fragments.push(PageFragment::TableRule(TableRuleFragment {
                    block: block_index,
                    rule: None,
                    x: table.x,
                    y: header.height,
                    width: table.width,
                    kind: TableRuleKind::Light,
                    replica: true,
                    continuation: true,
                    overflows_page: false,
                }));
            }

            if block.rule {
                let rule_y = block_y + block.height * 0.5;
                if selected.start - GEOMETRY_TOLERANCE <= rule_y
                    && rule_y < selected.end + GEOMETRY_TOLERANCE
                {
                    fragments.push(PageFragment::Rule(RuleFragment {
                        block: block_index,
                        x: block.rule_x,
                        y: content_offset + rule_y - selected.start,
                        width: block.rule_width,
                        overflows_page: content_offset + rule_y - selected.start
                            > plan.target_extent + GEOMETRY_TOLERANCE,
                    }));
                }
            }
        }

        for span in &quote_spans {
            let segment_start = span.start_y.max(selected.start);
            let segment_end = span.end_y.min(selected.end);
            if segment_end > segment_start + f32::EPSILON {
                fragments.push(PageFragment::QuoteBar(QuoteBarFragment {
                    level: span.level,
                    first_block: span.first_block,
                    last_block: span.last_block,
                    x: span.x,
                    y: content_offset + segment_start - selected.start,
                    height: segment_end - segment_start,
                    overflows_page: content_offset + segment_end - selected.start
                        > plan.target_extent + GEOMETRY_TOLERANCE,
                }));
            }
        }

        fragments[fragment_start..].sort_by(fragment_order);
        let source_range = source_range.filter(|range| range.start <= range.end);
        pages.push(ComposedPage {
            start_y: selected.start,
            end_y: selected.end,
            next_y: selected.next,
            text_anchor: text_anchor.unwrap_or_else(|| {
                document
                    .blocks
                    .iter()
                    .enumerate()
                    .find(|(index, block)| {
                        input.block_y(*index).is_some_and(|y| {
                            y + block.layout.height > selected.start + GEOMETRY_TOLERANCE
                        })
                    })
                    .map_or(0, |(_, block)| block.layout.range.start)
            }),
            source_range,
            repeated_table_header,
            fragments: FragmentRange {
                start: fragment_start,
                end: fragments.len(),
            },
        });
    }

    let layout = PaginatedLayout {
        width: document.width,
        content_height: plan.content_extent,
        body_height: plan.target_extent,
        pages,
        fragments,
    };
    debug_assert_eq!(validate_paginated_layout(document, input, &layout), Ok(()));
    crate::telemetry::phase_event!(
        "flow.phase.complete",
        pages = layout.pages.len() as u64,
        fragments = layout.fragments.len() as u64,
    );
    Ok(layout)
}

pub fn validate_paginated_layout(
    document: &ComposedDocument,
    input: &PageBreakInput,
    layout: &PaginatedLayout,
) -> Result<(), FragmentValidationError> {
    let _span = crate::telemetry::phase_span!(
        "flow.pagination.validate",
        blocks = document.blocks.len() as u64,
        pages = layout.pages.len() as u64,
        fragments = layout.fragments.len() as u64,
    );
    let mut expected_fragment_start = 0;
    let mut line_offsets = Vec::with_capacity(document.blocks.len() + 1);
    let mut atomic_offsets = Vec::with_capacity(document.blocks.len() + 1);
    line_offsets.push(0);
    atomic_offsets.push(0);
    for block in &document.blocks {
        line_offsets.push(line_offsets.last().copied().unwrap() + block.layout.lines.len());
        atomic_offsets
            .push(atomic_offsets.last().copied().unwrap() + block.layout.atomic_objects.len());
    }
    let mut line_owners = vec![0_u8; *line_offsets.last().unwrap()];
    let mut atomic_owners = vec![0_u8; *atomic_offsets.last().unwrap()];
    let page_count = layout.pages.len();
    let mut rule_owners = vec![0_u8; document.blocks.len() * page_count];
    let mut box_owners = vec![0_u8; document.blocks.len() * page_count];
    let expected_quote_spans = quote_spans(document, input);
    let mut quote_owners = vec![0_u8; expected_quote_spans.len() * page_count];
    let mut last_line = None;

    for (page_index, page) in layout.pages.iter().enumerate() {
        if page.fragments.start != expected_fragment_start
            || page.fragments.end < page.fragments.start
            || page.fragments.end > layout.fragments.len()
        {
            return Err(FragmentValidationError::InvalidPageFragmentRange { page: page_index });
        }
        expected_fragment_start = page.fragments.end;
        let mut actual_source: Option<TextRange> = None;
        for fragment_index in page.fragments.as_range() {
            let fragment = &layout.fragments[fragment_index];
            if !fragment_geometry_is_valid(fragment) {
                return Err(FragmentValidationError::InvalidFragmentGeometry {
                    fragment: fragment_index,
                });
            }
            if !fragment_fits_page(fragment, page, layout.body_height) {
                return Err(FragmentValidationError::FragmentOwnedByWrongPage {
                    page: page_index,
                    fragment: fragment_index,
                });
            }
            match fragment {
                PageFragment::Lines(fragment) => {
                    let Some(block) = document.blocks.get(fragment.block) else {
                        return Err(FragmentValidationError::InvalidFragmentReference {
                            fragment: fragment_index,
                        });
                    };
                    if fragment.line_start >= fragment.line_end
                        || fragment.line_end > block.layout.lines.len()
                    {
                        return Err(FragmentValidationError::InvalidFragmentReference {
                            fragment: fragment_index,
                        });
                    }
                    let first_layout_line = &block.layout.lines[fragment.line_start];
                    let last_layout_line = &block.layout.lines[fragment.line_end - 1];
                    let expected_range = line_source_range(
                        &block.layout.lines[fragment.line_start..fragment.line_end],
                    );
                    let block_y = input
                        .block_y(fragment.block)
                        .ok_or(FragmentValidationError::PagePlanMismatch)?;
                    let expected_y = if fragment.replica {
                        let header = page
                            .repeated_table_header
                            .filter(|header| header.block == fragment.block)
                            .ok_or(FragmentValidationError::InvalidFragmentReference {
                                fragment: fragment_index,
                            })?;
                        block_y + first_layout_line.y - block.layout.y - header.source_y
                    } else {
                        page.repeated_table_header
                            .map_or(0.0, |header| header.height)
                            + block_y
                            + first_layout_line.y
                            - block.layout.y
                            - page.start_y
                    };
                    let expected_height =
                        last_layout_line.y + last_layout_line.height - first_layout_line.y;
                    let expected_generated_runs = block.layout.lines
                        [fragment.line_start..fragment.line_end]
                        .iter()
                        .flat_map(|line| &line.runs)
                        .any(|run| run.range.is_empty() && !run.text.is_empty());
                    let expected_limit = if fragment.replica {
                        page.repeated_table_header
                            .map_or(layout.body_height, |header| header.height)
                    } else {
                        layout.body_height
                    };
                    let expected_overflow = expected_y < -GEOMETRY_TOLERANCE
                        || expected_y + expected_height > expected_limit + GEOMETRY_TOLERANCE;
                    if fragment.range != expected_range
                        || fragment.has_generated_runs != expected_generated_runs
                        || fragment.overflows_page != expected_overflow
                        || (fragment.y - expected_y).abs() > GEOMETRY_TOLERANCE
                        || (fragment.height - expected_height).abs() > GEOMETRY_TOLERANCE
                    {
                        return Err(FragmentValidationError::InvalidFragmentGeometry {
                            fragment: fragment_index,
                        });
                    }
                    if !fragment.replica {
                        let key = (fragment.block, fragment.line_start);
                        if block.layout.kind != LayoutBlockKind::Table {
                            if last_line.is_some_and(|last| last >= key) {
                                return Err(FragmentValidationError::ContentOutOfOrder);
                            }
                            last_line = Some(key);
                        }
                        for line in fragment.line_start..fragment.line_end {
                            let owner = &mut line_owners[line_offsets[fragment.block] + line];
                            *owner = owner.saturating_add(1);
                            include_source(&mut actual_source, block.layout.lines[line].range);
                        }
                    }
                }
                PageFragment::Atomic(fragment) => {
                    let Some(block) = document.blocks.get(fragment.block) else {
                        return Err(FragmentValidationError::InvalidFragmentReference {
                            fragment: fragment_index,
                        });
                    };
                    let Some(atomic) = block.layout.atomic_objects.get(fragment.atomic) else {
                        return Err(FragmentValidationError::InvalidFragmentReference {
                            fragment: fragment_index,
                        });
                    };
                    let block_y = input
                        .block_y(fragment.block)
                        .ok_or(FragmentValidationError::PagePlanMismatch)?;
                    let expected_y = if fragment.replica {
                        let header = page
                            .repeated_table_header
                            .filter(|header| header.block == fragment.block)
                            .ok_or(FragmentValidationError::InvalidFragmentReference {
                                fragment: fragment_index,
                            })?;
                        block_y + atomic.y - block.layout.y - header.source_y
                    } else {
                        page.repeated_table_header
                            .map_or(0.0, |header| header.height)
                            + block_y
                            + atomic.y
                            - block.layout.y
                            - page.start_y
                    };
                    let available =
                        (layout.body_height - expected_y.max(0.0)).max(MIN_FRAGMENT_EXTENT);
                    let expected_scale = if fragment.replica {
                        1.0
                    } else if atomic.height > available {
                        (available / atomic.height).clamp(0.0, 1.0)
                    } else {
                        1.0
                    };
                    let expected_overflow = expected_y < -GEOMETRY_TOLERANCE
                        || expected_y + atomic.height * expected_scale
                            > layout.body_height + GEOMETRY_TOLERANCE;
                    if fragment.range != atomic.range
                        || fragment.overflows_page != expected_overflow
                        || (fragment.x - atomic.x).abs() > GEOMETRY_TOLERANCE
                        || (fragment.y - expected_y).abs() > GEOMETRY_TOLERANCE
                        || (fragment.scale - expected_scale).abs() > GEOMETRY_TOLERANCE
                        || (fragment.width - atomic.width * expected_scale).abs()
                            > GEOMETRY_TOLERANCE
                        || (fragment.height - atomic.height * expected_scale).abs()
                            > GEOMETRY_TOLERANCE
                    {
                        return Err(FragmentValidationError::InvalidFragmentGeometry {
                            fragment: fragment_index,
                        });
                    }
                    let Some(&atomic_end) = atomic_offsets.get(fragment.block + 1) else {
                        return Err(FragmentValidationError::InvalidFragmentReference {
                            fragment: fragment_index,
                        });
                    };
                    let atomic_index = atomic_offsets[fragment.block] + fragment.atomic;
                    let Some(owner) = atomic_owners
                        .get_mut(atomic_index)
                        .filter(|_| atomic_index < atomic_end)
                    else {
                        return Err(FragmentValidationError::InvalidFragmentReference {
                            fragment: fragment_index,
                        });
                    };
                    if !fragment.replica {
                        *owner = owner.saturating_add(1);
                        include_source(&mut actual_source, fragment.range);
                    }
                }
                PageFragment::TableSlice(fragment) => {
                    let Some(block) = document.blocks.get(fragment.block) else {
                        return Err(FragmentValidationError::InvalidFragmentReference {
                            fragment: fragment_index,
                        });
                    };
                    if block.layout.kind != LayoutBlockKind::Table || block.layout.table.is_none() {
                        return Err(FragmentValidationError::InvalidFragmentReference {
                            fragment: fragment_index,
                        });
                    }
                    let block_y = input
                        .block_y(fragment.block)
                        .ok_or(FragmentValidationError::PagePlanMismatch)?;
                    let expected_start = block_y.max(page.start_y);
                    let expected_end = (block_y + block.layout.height).min(page.end_y);
                    let content_offset = page
                        .repeated_table_header
                        .map_or(0.0, |header| header.height);
                    if (fragment.source_start - expected_start).abs() > GEOMETRY_TOLERANCE
                        || (fragment.source_end - expected_end).abs() > GEOMETRY_TOLERANCE
                        || (fragment.y - (content_offset + expected_start - page.start_y)).abs()
                            > GEOMETRY_TOLERANCE
                        || (fragment.height - (expected_end - expected_start)).abs()
                            > GEOMETRY_TOLERANCE
                    {
                        return Err(FragmentValidationError::InvalidFragmentGeometry {
                            fragment: fragment_index,
                        });
                    }
                }
                PageFragment::TableRule(fragment) => {
                    let Some(block) = document.blocks.get(fragment.block) else {
                        return Err(FragmentValidationError::InvalidFragmentReference {
                            fragment: fragment_index,
                        });
                    };
                    let Some(table) = &block.layout.table else {
                        return Err(FragmentValidationError::InvalidFragmentReference {
                            fragment: fragment_index,
                        });
                    };
                    if let Some(rule) = fragment.rule {
                        let Some(source) = table.rules.get(rule) else {
                            return Err(FragmentValidationError::InvalidFragmentReference {
                                fragment: fragment_index,
                            });
                        };
                        if (fragment.x - source.x).abs() > GEOMETRY_TOLERANCE
                            || (fragment.width - source.width).abs() > GEOMETRY_TOLERANCE
                            || fragment.kind != source.kind
                        {
                            return Err(FragmentValidationError::InvalidFragmentGeometry {
                                fragment: fragment_index,
                            });
                        }
                    } else {
                        let Some(header) = page
                            .repeated_table_header
                            .filter(|header| header.block == fragment.block)
                        else {
                            return Err(FragmentValidationError::InvalidFragmentReference {
                                fragment: fragment_index,
                            });
                        };
                        if !fragment.replica || !fragment.continuation {
                            return Err(FragmentValidationError::InvalidFragmentReference {
                                fragment: fragment_index,
                            });
                        }
                        if (fragment.x - table.x).abs() > GEOMETRY_TOLERANCE
                            || (fragment.y - header.height).abs() > GEOMETRY_TOLERANCE
                            || (fragment.width - table.width).abs() > GEOMETRY_TOLERANCE
                            || fragment.kind != TableRuleKind::Light
                        {
                            return Err(FragmentValidationError::InvalidFragmentGeometry {
                                fragment: fragment_index,
                            });
                        }
                    }
                }
                PageFragment::TableQuoteBar(fragment) => {
                    let Some(block) = document.blocks.get(fragment.block) else {
                        return Err(FragmentValidationError::InvalidFragmentReference {
                            fragment: fragment_index,
                        });
                    };
                    let Some(source) = block
                        .layout
                        .table
                        .as_ref()
                        .and_then(|table| table.quote_bars.get(fragment.quote_bar))
                    else {
                        return Err(FragmentValidationError::InvalidFragmentReference {
                            fragment: fragment_index,
                        });
                    };
                    if (fragment.x - source.x).abs() > GEOMETRY_TOLERANCE {
                        return Err(FragmentValidationError::InvalidFragmentGeometry {
                            fragment: fragment_index,
                        });
                    }
                }
                PageFragment::Rule(fragment) => {
                    let Some(block) = document.blocks.get(fragment.block) else {
                        return Err(FragmentValidationError::InvalidFragmentReference {
                            fragment: fragment_index,
                        });
                    };
                    if !block.layout.rule {
                        return Err(FragmentValidationError::InvalidFragmentReference {
                            fragment: fragment_index,
                        });
                    }
                    let expected_y = input
                        .block_y(fragment.block)
                        .ok_or(FragmentValidationError::PagePlanMismatch)?
                        + block.layout.height * 0.5
                        - page.start_y
                        + page
                            .repeated_table_header
                            .map_or(0.0, |header| header.height);
                    let expected_overflow = expected_y > layout.body_height + GEOMETRY_TOLERANCE;
                    if fragment.overflows_page != expected_overflow
                        || (fragment.x - block.layout.rule_x).abs() > GEOMETRY_TOLERANCE
                        || (fragment.y - expected_y).abs() > GEOMETRY_TOLERANCE
                        || (fragment.width - block.layout.rule_width).abs() > GEOMETRY_TOLERANCE
                    {
                        return Err(FragmentValidationError::InvalidFragmentGeometry {
                            fragment: fragment_index,
                        });
                    }
                    let owner = &mut rule_owners[fragment.block * page_count + page_index];
                    *owner = owner.saturating_add(1);
                }
                PageFragment::BlockBox(fragment) => {
                    let Some(block) = document.blocks.get(fragment.block) else {
                        return Err(FragmentValidationError::InvalidFragmentReference {
                            fragment: fragment_index,
                        });
                    };
                    let Some(box_layout) = &block.layout.box_layout else {
                        return Err(FragmentValidationError::InvalidFragmentReference {
                            fragment: fragment_index,
                        });
                    };
                    let block_y = input
                        .block_y(fragment.block)
                        .ok_or(FragmentValidationError::PagePlanMismatch)?;
                    let block_end = block_y + block.layout.height;
                    let segment_start = block_y.max(page.start_y);
                    let segment_end = block_end.min(page.end_y);
                    let content_offset = page
                        .repeated_table_header
                        .map_or(0.0, |header| header.height);
                    let expected_y = content_offset + segment_start - page.start_y;
                    let expected_height = segment_end - segment_start;
                    let expected_overflow = content_offset + segment_end - page.start_y
                        > layout.body_height + GEOMETRY_TOLERANCE;
                    if fragment.draw_top != (segment_start <= block_y + GEOMETRY_TOLERANCE)
                        || fragment.draw_bottom != (segment_end >= block_end - GEOMETRY_TOLERANCE)
                        || fragment.overflows_page != expected_overflow
                        || (fragment.x - box_layout.x).abs() > GEOMETRY_TOLERANCE
                        || (fragment.y - expected_y).abs() > GEOMETRY_TOLERANCE
                        || (fragment.width - box_layout.width).abs() > GEOMETRY_TOLERANCE
                        || (fragment.height - expected_height).abs() > GEOMETRY_TOLERANCE
                    {
                        return Err(FragmentValidationError::InvalidFragmentGeometry {
                            fragment: fragment_index,
                        });
                    }
                    let owner = &mut box_owners[fragment.block * page_count + page_index];
                    *owner = owner.saturating_add(1);
                }
                PageFragment::QuoteBar(fragment) => {
                    let Some((span_index, span)) =
                        expected_quote_spans.iter().enumerate().find(|(_, span)| {
                            span.level == fragment.level
                                && span.first_block == fragment.first_block
                                && span.last_block == fragment.last_block
                        })
                    else {
                        return Err(FragmentValidationError::InvalidFragmentReference {
                            fragment: fragment_index,
                        });
                    };
                    let segment_start = span.start_y.max(page.start_y);
                    let segment_end = span.end_y.min(page.end_y);
                    let content_offset = page
                        .repeated_table_header
                        .map_or(0.0, |header| header.height);
                    let expected_y = content_offset + segment_start - page.start_y;
                    let expected_height = segment_end - segment_start;
                    let expected_overflow = content_offset + segment_end - page.start_y
                        > layout.body_height + GEOMETRY_TOLERANCE;
                    if fragment.overflows_page != expected_overflow
                        || (fragment.x - span.x).abs() > GEOMETRY_TOLERANCE
                        || (fragment.y - expected_y).abs() > GEOMETRY_TOLERANCE
                        || (fragment.height - expected_height).abs() > GEOMETRY_TOLERANCE
                    {
                        return Err(FragmentValidationError::InvalidFragmentGeometry {
                            fragment: fragment_index,
                        });
                    }
                    let owner = &mut quote_owners[span_index * page_count + page_index];
                    *owner = owner.saturating_add(1);
                }
            }
        }
        if page.source_range != actual_source {
            return Err(FragmentValidationError::IncorrectSourceRange { page: page_index });
        }
    }
    if expected_fragment_start != layout.fragments.len() {
        return Err(FragmentValidationError::InvalidPageFragmentRange {
            page: layout.pages.len().saturating_sub(1),
        });
    }

    for block in 0..document.blocks.len() {
        for line in 0..document.blocks[block].layout.lines.len() {
            match line_owners[line_offsets[block] + line] {
                0 => return Err(FragmentValidationError::MissingLine { block, line }),
                1 => {}
                _ => return Err(FragmentValidationError::DuplicateLine { block, line }),
            }
        }
        for atomic in 0..document.blocks[block].layout.atomic_objects.len() {
            match atomic_owners[atomic_offsets[block] + atomic] {
                0 => return Err(FragmentValidationError::MissingAtomic { block, atomic }),
                1 => {}
                _ => return Err(FragmentValidationError::DuplicateAtomic { block, atomic }),
            }
        }
    }
    for (block_index, composed) in document.blocks.iter().enumerate() {
        if composed.layout.rule {
            let block_y = input
                .block_y(block_index)
                .ok_or(FragmentValidationError::PagePlanMismatch)?;
            let rule_y = block_y + composed.layout.height * 0.5;
            let page = layout
                .pages
                .iter()
                .position(|page| {
                    page.start_y - GEOMETRY_TOLERANCE <= rule_y
                        && rule_y < page.end_y + GEOMETRY_TOLERANCE
                })
                .ok_or(FragmentValidationError::MissingDecoration {
                    kind: DecorationKind::Rule,
                    owner: block_index,
                    page: 0,
                })?;
            for page_index in 0..layout.pages.len() {
                let count = rule_owners[block_index * page_count + page_index];
                if page_index == page {
                    validate_decoration_count(
                        count,
                        DecorationKind::Rule,
                        block_index,
                        page_index,
                    )?;
                } else if count != 0 {
                    return Err(FragmentValidationError::DuplicateDecoration {
                        kind: DecorationKind::Rule,
                        owner: block_index,
                        page: page_index,
                    });
                }
            }
        }
        if composed.layout.box_layout.is_some() {
            let block_y = input
                .block_y(block_index)
                .ok_or(FragmentValidationError::PagePlanMismatch)?;
            let block_end = block_y + composed.layout.height;
            for (page_index, page) in layout.pages.iter().enumerate() {
                let expected = block_end > page.start_y + GEOMETRY_TOLERANCE
                    && block_y < page.end_y - GEOMETRY_TOLERANCE;
                let count = box_owners[block_index * page_count + page_index];
                if expected {
                    validate_decoration_count(
                        count,
                        DecorationKind::BlockBox,
                        block_index,
                        page_index,
                    )?;
                } else if count != 0 {
                    return Err(FragmentValidationError::DuplicateDecoration {
                        kind: DecorationKind::BlockBox,
                        owner: block_index,
                        page: page_index,
                    });
                }
            }
        }
    }
    for (span_index, span) in expected_quote_spans.iter().enumerate() {
        for (page_index, page) in layout.pages.iter().enumerate() {
            let expected =
                span.end_y.min(page.end_y) > span.start_y.max(page.start_y) + f32::EPSILON;
            let count = quote_owners[span_index * page_count + page_index];
            if expected {
                validate_decoration_count(count, DecorationKind::QuoteBar, span_index, page_index)?;
            } else if count != 0 {
                return Err(FragmentValidationError::DuplicateDecoration {
                    kind: DecorationKind::QuoteBar,
                    owner: span_index,
                    page: page_index,
                });
            }
        }
    }
    Ok(())
}

fn validate_decoration_count(
    count: u8,
    kind: DecorationKind,
    owner: usize,
    page: usize,
) -> Result<(), FragmentValidationError> {
    match count {
        0 => Err(FragmentValidationError::MissingDecoration { kind, owner, page }),
        1 => Ok(()),
        _ => Err(FragmentValidationError::DuplicateDecoration { kind, owner, page }),
    }
}

impl PaginatedLayout {
    pub fn page_for_y(&self, y: f32) -> usize {
        self.pages
            .partition_point(|page| page.start_y <= y)
            .saturating_sub(1)
            .min(self.pages.len().saturating_sub(1))
    }

    pub fn page_for_text(&self, offset: usize) -> usize {
        if let Some(page) = self.pages.iter().position(|page| {
            page.source_range.is_some_and(|range| {
                range.start <= offset
                    && (offset < range.end || range.is_empty() && offset == range.start)
            })
        }) {
            return page;
        }
        let first_not_before = self.pages.partition_point(|page| page.text_anchor < offset);
        if self
            .pages
            .get(first_not_before)
            .is_some_and(|page| page.text_anchor == offset)
        {
            first_not_before
        } else {
            first_not_before
                .saturating_sub(1)
                .min(self.pages.len().saturating_sub(1))
        }
    }
}

fn table_slice_kind(
    block: &crate::layout::BlockLayout,
    block_y: f32,
    source_start: f32,
    source_end: f32,
    relaxed: bool,
) -> TableSliceKind {
    let Some(table) = &block.table else {
        return TableSliceKind::EmergencySlice;
    };
    let row_y = block_y + table.row_y - block.y;
    let mut boundaries = Vec::with_capacity(table.row_heights.len() + 1);
    boundaries.push(row_y);
    for height in &table.row_heights {
        boundaries.push(boundaries.last().copied().unwrap_or(row_y) + height);
    }
    let on_boundary = |value: f32| {
        boundaries
            .iter()
            .any(|boundary| (value - boundary).abs() <= GEOMETRY_TOLERANCE)
    };
    if on_boundary(source_start) && on_boundary(source_end) {
        TableSliceKind::CompleteRows
    } else if relaxed && !on_boundary(source_end) {
        TableSliceKind::EmergencySlice
    } else {
        TableSliceKind::RowContinuation
    }
}

fn owner_page(segments: &[super::sequence::SelectedBreak], start: f32, end: f32) -> Option<usize> {
    if segments.is_empty() {
        return None;
    }
    let candidate = segments
        .partition_point(|page| page.start <= start + GEOMETRY_TOLERANCE)
        .saturating_sub(1)
        .min(segments.len() - 1);
    let page = &segments[candidate];
    if start >= page.start - GEOMETRY_TOLERANCE
        && (end <= page.end + GEOMETRY_TOLERANCE || start < page.end + GEOMETRY_TOLERANCE)
    {
        Some(candidate)
    } else {
        segments.get(candidate + 1).and_then(|page| {
            (start >= page.start - GEOMETRY_TOLERANCE
                && (end <= page.end + GEOMETRY_TOLERANCE || start < page.end + GEOMETRY_TOLERANCE))
                .then_some(candidate + 1)
        })
    }
}

fn line_source_range(lines: &[crate::layout::LineLayout]) -> TextRange {
    TextRange {
        start: lines.iter().map(|line| line.range.start).min().unwrap(),
        end: lines.iter().map(|line| line.range.end).max().unwrap(),
    }
}

fn include_source(range: &mut Option<TextRange>, item: TextRange) {
    match range {
        Some(range) => {
            range.start = range.start.min(item.start);
            range.end = range.end.max(item.end);
        }
        None => *range = Some(item),
    }
}

fn fragment_order(left: &PageFragment, right: &PageFragment) -> std::cmp::Ordering {
    fragment_sort_key(left).cmp(&fragment_sort_key(right))
}

fn fragment_sort_key(fragment: &PageFragment) -> (usize, u8, usize) {
    match fragment {
        PageFragment::BlockBox(fragment) => (fragment.block, 0, 0),
        PageFragment::TableSlice(fragment) => (fragment.block, 1, 0),
        PageFragment::Atomic(fragment) => (fragment.block, 2, fragment.atomic),
        PageFragment::TableQuoteBar(fragment) => (fragment.block, 3, fragment.quote_bar),
        PageFragment::TableRule(fragment) => {
            (fragment.block, 4, fragment.rule.unwrap_or(usize::MAX))
        }
        PageFragment::Rule(fragment) => (fragment.block, 5, 0),
        PageFragment::Lines(fragment) => (fragment.block, 6, fragment.line_start),
        PageFragment::QuoteBar(fragment) => (fragment.first_block, 7, fragment.level),
    }
}

fn fragment_geometry_is_valid(fragment: &PageFragment) -> bool {
    let values: &[f32] = match fragment {
        PageFragment::Lines(fragment) => &[fragment.y, fragment.height],
        PageFragment::Atomic(fragment) => &[
            fragment.x,
            fragment.y,
            fragment.width,
            fragment.height,
            fragment.scale,
        ],
        PageFragment::Rule(fragment) => &[fragment.x, fragment.y, fragment.width],
        PageFragment::QuoteBar(fragment) => &[fragment.x, fragment.y, fragment.height],
        PageFragment::BlockBox(fragment) => {
            &[fragment.x, fragment.y, fragment.width, fragment.height]
        }
        PageFragment::TableSlice(fragment) => &[
            fragment.source_start,
            fragment.source_end,
            fragment.y,
            fragment.height,
        ],
        PageFragment::TableRule(fragment) => &[fragment.x, fragment.y, fragment.width],
        PageFragment::TableQuoteBar(fragment) => &[fragment.x, fragment.y, fragment.height],
    };
    values.iter().all(|value| value.is_finite())
        && match fragment {
            PageFragment::Lines(fragment) => fragment.height >= 0.0,
            PageFragment::Atomic(fragment) => {
                fragment.width >= 0.0
                    && fragment.height >= 0.0
                    && 0.0 < fragment.scale
                    && fragment.scale <= 1.0
            }
            PageFragment::Rule(fragment) => fragment.width >= 0.0,
            PageFragment::QuoteBar(fragment) => fragment.height >= 0.0,
            PageFragment::BlockBox(fragment) => fragment.width >= 0.0 && fragment.height >= 0.0,
            PageFragment::TableSlice(fragment) => {
                fragment.source_end >= fragment.source_start && fragment.height >= 0.0
            }
            PageFragment::TableRule(fragment) => fragment.width >= 0.0,
            PageFragment::TableQuoteBar(fragment) => fragment.height >= 0.0,
        }
}

fn fragment_fits_page(fragment: &PageFragment, _page: &ComposedPage, page_height: f32) -> bool {
    match fragment {
        PageFragment::Lines(fragment) => {
            fragment.overflows_page
                || (fragment.y >= -GEOMETRY_TOLERANCE
                    && fragment.y + fragment.height <= page_height + GEOMETRY_TOLERANCE)
        }
        PageFragment::Atomic(fragment) => {
            fragment.overflows_page
                || (fragment.y >= -GEOMETRY_TOLERANCE
                    && fragment.y + fragment.height <= page_height + GEOMETRY_TOLERANCE)
        }
        PageFragment::Rule(fragment) => {
            fragment.overflows_page
                || (fragment.y >= -GEOMETRY_TOLERANCE
                    && fragment.y <= page_height + GEOMETRY_TOLERANCE)
        }
        PageFragment::QuoteBar(fragment) => {
            fragment.overflows_page
                || (fragment.y >= -GEOMETRY_TOLERANCE
                    && fragment.y + fragment.height <= page_height + GEOMETRY_TOLERANCE)
        }
        PageFragment::BlockBox(fragment) => {
            fragment.overflows_page
                || (fragment.y >= -GEOMETRY_TOLERANCE
                    && fragment.y + fragment.height <= page_height + GEOMETRY_TOLERANCE)
        }
        PageFragment::TableSlice(fragment) => {
            fragment.overflows_page
                || (fragment.y >= -GEOMETRY_TOLERANCE
                    && fragment.y + fragment.height <= page_height + GEOMETRY_TOLERANCE)
        }
        PageFragment::TableRule(fragment) => {
            fragment.overflows_page
                || (fragment.y >= -GEOMETRY_TOLERANCE
                    && fragment.y <= page_height + GEOMETRY_TOLERANCE)
        }
        PageFragment::TableQuoteBar(fragment) => {
            fragment.overflows_page
                || (fragment.y >= -GEOMETRY_TOLERANCE
                    && fragment.y + fragment.height <= page_height + GEOMETRY_TOLERANCE)
        }
    }
}

fn quote_spans(document: &ComposedDocument, input: &PageBreakInput) -> Vec<QuoteSpan> {
    let depth = document
        .blocks
        .iter()
        .map(|block| block.layout.quote_bars.len())
        .max()
        .unwrap_or(0);
    let mut spans = Vec::new();
    for level in 0..depth {
        let mut active: Option<QuoteSpan> = None;
        for (block_index, composed) in document.blocks.iter().enumerate() {
            let block = &composed.layout;
            if let Some(&x) = block.quote_bars.get(level) {
                let block_y = input.block_y(block_index).unwrap_or(0.0);
                match &mut active {
                    Some(active) => {
                        active.x = active.x.min(x);
                        active.last_block = block_index;
                        active.end_y = block_y + block.height;
                    }
                    None => {
                        active = Some(QuoteSpan {
                            level,
                            first_block: block_index,
                            last_block: block_index,
                            x,
                            start_y: block_y,
                            end_y: block_y + block.height,
                        });
                    }
                }
            } else if let Some(span) = active.take() {
                spans.push(span);
            }
        }
        if let Some(span) = active {
            spans.push(span);
        }
    }
    spans
}
