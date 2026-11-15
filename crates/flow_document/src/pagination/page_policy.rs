use crate::{
    layout::{ComposedDocument, LayoutBlockKind},
    model::flow::AuthorBreak,
};

use super::{
    MIN_BODY_HEIGHT, MIN_FRAGMENT_LINES, is_splittable, safe_table_line_boundaries,
    sequence::{
        self, BreakContext, BreakPolicy, FirstFit, Item, ItemStream, ItemStreamBuilder, PenaltyRef,
    },
};

const FORCED_BREAK_COST: i32 = -10_000;
const PROHIBITED_BREAK_COST: i32 = 10_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageBoundary {
    Line(usize),
    Block,
    Table,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageBox {
    pub block: usize,
    pub boundary: PageBoundary,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PageGlue;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PagePenalty {
    pub block: usize,
    pub boundary: PageBoundary,
}

/// flow-specific lowering and policy state around a generic vertical stream.
#[derive(Clone, Debug)]
pub struct PageBreakInput {
    pub stream: ItemStream<PageBox, PageGlue, PagePenalty, f32>,
    policy: PageBreakPolicy,
    contains_tables: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageBreakError {
    InvalidGeometry,
}

#[derive(Clone, Copy, Debug)]
struct Candidate {
    end_y: f32,
    next_y: f32,
    block: usize,
    kind: CandidateKind,
}

#[derive(Clone, Copy, Debug)]
enum CandidateKind {
    Block,
    Line(usize),
    TableRow,
}

#[derive(Clone, Debug)]
struct PageBreakPolicy {
    blocks: Vec<BlockPolicy>,
    line_ends: Vec<f32>,
    candidates: Vec<Candidate>,
    tables: Vec<TablePolicy>,
}

#[derive(Clone, Debug)]
struct TablePolicy {
    block: usize,
    start_y: f32,
    end_y: f32,
    row_y: f32,
    header_height: f32,
    atomic_ranges: Vec<(f32, f32)>,
}

#[derive(Clone, Debug)]
struct BlockPolicy {
    y: f32,
    kind: LayoutBlockKind,
    line_start: usize,
    line_end: usize,
    break_before: AuthorBreak,
    break_after: AuthorBreak,
    break_inside_avoid: bool,
    widows: u8,
    orphans: u8,
}

/// Lowers intrinsic composition to generic relative box/glue/penalty items plus
/// flow-owned page-break policy.
pub fn build_page_items(document: &ComposedDocument) -> PageBreakInput {
    let _span = crate::telemetry::phase_span!(
        "flow.pagination.vertical-items",
        blocks = document.blocks.len() as u64,
    );

    let mut y = document.leading_spacing();
    let mut placements = Vec::with_capacity(document.blocks.len());
    for composed in &document.blocks {
        y += composed.margin_before;
        placements.push(y);
        y += composed.layout.height + composed.margin_after;
    }
    let content_height = y.max(document.minimum_height());
    let line_capacity = document
        .blocks
        .iter()
        .map(|block| block.layout.lines.len())
        .sum();
    let mut candidates = Vec::with_capacity(line_capacity + document.blocks.len());
    let mut blocks = Vec::with_capacity(document.blocks.len());
    let mut line_ends = Vec::with_capacity(line_capacity);
    let mut contains_tables = false;
    let mut tables = Vec::new();

    for (block_index, composed) in document.blocks.iter().enumerate() {
        let block = &composed.layout;
        let block_y = placements[block_index];
        let dy = block_y - block.y;
        let next_y = placements
            .get(block_index + 1)
            .copied()
            .unwrap_or(content_height);
        append_block_candidates(block, dy, block_index, next_y, &mut candidates);
        contains_tables |= block.kind == LayoutBlockKind::Table;
        if block.kind == LayoutBlockKind::Table
            && let Some(table) = &block.table
        {
            tables.push(TablePolicy {
                block: block_index,
                start_y: block_y,
                end_y: block_y + block.height,
                row_y: block_y + table.row_y - block.y,
                header_height: table.row_heights.iter().take(table.header_row_count).sum(),
                atomic_ranges: block
                    .atomic_objects
                    .iter()
                    .map(|object| {
                        let y = block_y + object.y - block.y;
                        (y, y + object.height)
                    })
                    .collect(),
            });
        }
        let line_start = line_ends.len();
        line_ends.extend(block.lines.iter().map(|line| line.y + line.height + dy));
        blocks.push(BlockPolicy {
            y: block_y,
            kind: block.kind,
            line_start,
            line_end: line_ends.len(),
            break_before: block.break_before,
            break_after: block.break_after,
            break_inside_avoid: block.break_inside_avoid,
            widows: block.widows,
            orphans: block.orphans,
        });
    }
    candidates.sort_by(|left, right| {
        left.end_y
            .total_cmp(&right.end_y)
            .then_with(|| left.next_y.total_cmp(&right.next_y))
    });

    let mut items = ItemStreamBuilder::with_capacity(
        candidates.len().saturating_mul(3).saturating_add(2),
        candidates.len(),
    );
    let mut cursor = 0.0;
    if let Some(first) = placements.first().copied()
        && first.abs() > f32::EPSILON
    {
        items.push_glue(first, 0.0, 0.0, true, PageGlue);
        cursor = first;
    }

    for candidate in &candidates {
        let boundary = match candidate.kind {
            CandidateKind::Block => PageBoundary::Block,
            CandidateKind::Line(line) => PageBoundary::Line(line),
            CandidateKind::TableRow => PageBoundary::Table,
        };
        items.push_box(
            candidate.end_y - cursor,
            PageBox {
                block: candidate.block,
                boundary,
            },
        );
        let policy = &blocks[candidate.block];
        let next = blocks.get(candidate.block + 1);
        let forced = matches!(candidate.kind, CandidateKind::Block)
            && (policy.break_after == AuthorBreak::Page
                || next.is_some_and(|next| next.break_before == AuthorBreak::Page));
        let prohibited = match candidate.kind {
            CandidateKind::Line(_) => document.blocks[candidate.block].layout.break_inside_avoid,
            CandidateKind::Block => {
                policy.break_after == AuthorBreak::Avoid
                    || next.is_some_and(|next| next.break_before == AuthorBreak::Avoid)
                    || (policy.kind == LayoutBlockKind::Heading
                        && next.is_some_and(|next| next.kind == LayoutBlockKind::Paragraph))
            }
            CandidateKind::TableRow => false,
        };
        let base_cost = if forced {
            FORCED_BREAK_COST
        } else if prohibited {
            PROHIBITED_BREAK_COST
        } else {
            0
        };
        items.push_penalty(
            0.0,
            base_cost,
            matches!(candidate.kind, CandidateKind::Line(_)),
            forced,
            PagePenalty {
                block: candidate.block,
                boundary,
            },
        );
        let glue = candidate.next_y - candidate.end_y;
        if glue.abs() > f32::EPSILON {
            items.push_glue(glue, 0.0, 0.0, true, PageGlue);
        }
        cursor = candidate.next_y;
    }
    if cursor < content_height {
        items.push_glue(content_height - cursor, 0.0, 0.0, true, PageGlue);
    }

    PageBreakInput {
        stream: items.finish(),
        policy: PageBreakPolicy {
            blocks,
            line_ends,
            candidates,
            tables,
        },
        contains_tables,
    }
}

/// Selects greedy boundaries from generic vertical items using flow page policy.
pub fn break_page_items(
    input: &PageBreakInput,
    requested_body_height: f32,
) -> Result<sequence::BreakPlan, PageBreakError> {
    let _span = crate::telemetry::phase_span!(
        "flow.pagination.vertical-break",
        items = input.stream.items.len() as u64,
        body_height = requested_body_height as f64,
    );
    let plan = if input.contains_tables {
        break_table_pages(input, requested_body_height)?
    } else {
        FirstFit::new(MIN_BODY_HEIGHT, 0.01).break_sequence(
            &input.stream,
            requested_body_height,
            &input.policy,
        )
    };
    crate::telemetry::phase_event!(
        "flow.phase.complete",
        pages = plan.segments.len() as u64,
        relaxed = plan
            .segments
            .iter()
            .filter(|selected| selected.relaxed)
            .count() as u64,
    );
    Ok(plan)
}

fn break_table_pages(
    input: &PageBreakInput,
    requested_body_height: f32,
) -> Result<sequence::BreakPlan, PageBreakError> {
    let target_extent = if requested_body_height.is_finite() {
        requested_body_height.max(MIN_BODY_HEIGHT)
    } else {
        MIN_BODY_HEIGHT
    };
    if !input.stream.content_extent.is_finite() {
        return Err(PageBreakError::InvalidGeometry);
    }
    if input.policy.candidates.is_empty() {
        return Ok(sequence::BreakPlan {
            segments: vec![sequence::SelectedBreak {
                start: 0.0,
                end: 0.0,
                next: 0.0,
                item: None,
                relaxed: false,
            }],
            content_extent: input.stream.content_extent,
            target_extent,
        });
    }

    let candidates = &input.policy.candidates;
    let mut segments = Vec::new();
    let mut start = 0.0;
    let emergency_steps = (input.stream.content_extent.max(0.0) / MIN_BODY_HEIGHT).ceil() as usize;
    let maximum_steps = candidates
        .len()
        .saturating_add(input.policy.blocks.len())
        .saturating_add(emergency_steps)
        .saturating_add(1);
    let mut guard = 0;
    while start < input.stream.content_extent && guard <= maximum_steps {
        guard += 1;
        let header_height = input
            .repeated_table_header(start, target_extent)
            .map_or(0.0, |header| header.height);
        let available = (target_extent - header_height).max(MIN_BODY_HEIGHT);
        let limit = start + available;
        let first =
            candidates.partition_point(|candidate| candidate.next_y <= start + f32::EPSILON);
        let upper = candidates.partition_point(|candidate| candidate.end_y <= limit + 0.01);
        let fitting = (first < upper)
            .then(|| {
                candidates[first..upper]
                    .iter()
                    .rev()
                    .copied()
                    .find(|candidate| input.policy.is_candidate_legal(*candidate, start))
            })
            .flatten();
        let relaxed = fitting.is_none() && first < upper;
        let chosen = fitting.or_else(|| (first < upper).then(|| candidates[upper - 1]));
        let oversized_table_atomic = input.policy.tables.iter().any(|table| {
            table.start_y < limit - f32::EPSILON
                && table.end_y > limit + f32::EPSILON
                && table
                    .atomic_ranges
                    .iter()
                    .any(|&(object_start, object_end)| {
                        object_start < limit - f32::EPSILON && object_end > limit + f32::EPSILON
                    })
        });
        let emergency_table_slice = (chosen.is_none() && !oversized_table_atomic)
            .then(|| {
                let table = input.policy.tables.iter().find(|table| {
                    table.start_y < limit - f32::EPSILON && table.end_y > limit + f32::EPSILON
                })?;
                (limit > start + f32::EPSILON).then_some(Candidate {
                    end_y: limit,
                    next_y: limit,
                    block: table.block,
                    kind: CandidateKind::TableRow,
                })
            })
            .flatten();
        let Some(chosen) = chosen
            .or(emergency_table_slice)
            .or_else(|| candidates.get(first).copied())
        else {
            return Err(PageBreakError::InvalidGeometry);
        };
        if chosen.next_y <= start + f32::EPSILON {
            return Err(PageBreakError::InvalidGeometry);
        }
        segments.push(sequence::SelectedBreak {
            start,
            end: chosen.end_y,
            next: chosen.next_y,
            item: None,
            relaxed: relaxed || emergency_table_slice.is_some(),
        });
        start = chosen.next_y;
    }
    if start < input.stream.content_extent - f32::EPSILON {
        return Err(PageBreakError::InvalidGeometry);
    }
    Ok(sequence::BreakPlan {
        segments,
        content_extent: input.stream.content_extent,
        target_extent,
    })
}

impl PageBreakPolicy {
    fn is_candidate_legal(&self, candidate: Candidate, segment_start: f32) -> bool {
        let block = &self.blocks[candidate.block];
        if block.break_before == AuthorBreak::Page && segment_start < block.y - f32::EPSILON {
            return false;
        }
        if candidate.block > 0 {
            let previous = &self.blocks[candidate.block - 1];
            if previous.break_after == AuthorBreak::Page
                && segment_start <= previous.y + f32::EPSILON
            {
                return false;
            }
        }
        if let CandidateKind::Line(line_index) = candidate.kind {
            if block.kind == LayoutBlockKind::Table {
                return true;
            }
            let line_ends = &self.line_ends[block.line_start..block.line_end];
            let first_on_segment =
                line_ends.partition_point(|end| *end <= segment_start + f32::EPSILON);
            let lines_here = line_index + 1 - first_on_segment.min(line_index + 1);
            let lines_remaining = line_ends.len() - line_index - 1;
            if block.break_inside_avoid
                || lines_here < usize::from(block.orphans)
                || lines_remaining < usize::from(block.widows)
            {
                return false;
            }
            if candidate.block > 0
                && self.blocks[candidate.block - 1].kind == LayoutBlockKind::Heading
                && segment_start <= self.blocks[candidate.block - 1].y + f32::EPSILON
                && line_index + 1 < MIN_FRAGMENT_LINES
            {
                return false;
            }
            return true;
        }
        if matches!(candidate.kind, CandidateKind::Block)
            && (block.break_after == AuthorBreak::Avoid
                || self
                    .blocks
                    .get(candidate.block + 1)
                    .is_some_and(|next| next.break_before == AuthorBreak::Avoid))
        {
            return false;
        }
        if block.kind == LayoutBlockKind::Heading
            && self
                .blocks
                .get(candidate.block + 1)
                .is_some_and(|next| next.kind == LayoutBlockKind::Paragraph)
        {
            return false;
        }
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RepeatedTableHeaderPlan {
    pub block: usize,
    pub source_y: f32,
    pub height: f32,
}

/// Returns deterministic selected-boundary diagnostics without publication
/// text or paths.
pub fn debug_page_break_plan(plan: &sequence::BreakPlan) -> String {
    use std::fmt::Write;

    let mut output = String::new();
    let _ = writeln!(
        output,
        "break-plan extent={:.3} target={:.3} segments={}",
        plan.content_extent,
        plan.target_extent,
        plan.segments.len()
    );
    for (index, selected) in plan.segments.iter().enumerate() {
        let _ = writeln!(
            output,
            "{index:04} start={:.3} end={:.3} next={:.3} item={:?} relaxed={}",
            selected.start, selected.end, selected.next, selected.item, selected.relaxed
        );
    }
    output
}

impl PageBreakInput {
    pub(crate) fn block_y(&self, block: usize) -> Option<f32> {
        self.policy.blocks.get(block).map(|block| block.y)
    }

    pub(crate) fn repeated_table_header(
        &self,
        start_y: f32,
        body_height: f32,
    ) -> Option<RepeatedTableHeaderPlan> {
        let table = self.policy.tables.iter().find(|table| {
            start_y > table.row_y + f32::EPSILON
                && start_y < table.end_y - f32::EPSILON
                && table.header_height > 0.0
                && table.header_height < body_height - MIN_BODY_HEIGHT
        })?;
        Some(RepeatedTableHeaderPlan {
            block: table.block,
            source_y: table.row_y,
            height: table.header_height,
        })
    }

    /// Returns deterministic diagnostics without publication text or paths.
    pub fn debug_dump(&self) -> String {
        use std::fmt::Write;

        let mut output = String::new();
        let _ = writeln!(
            output,
            "vertical-items extent={:.3} items={} breaks={} tables={}",
            self.stream.content_extent,
            self.stream.items.len(),
            self.stream.break_count(),
            self.contains_tables
        );
        let mut position = 0.0;
        for (index, item) in self.stream.items.iter().enumerate() {
            match item {
                Item::Box { extent, data } => {
                    let start = position;
                    position += extent;
                    let _ = writeln!(
                        output,
                        "{index:04} box block={} boundary={:?} start={start:.3} end={position:.3}",
                        data.block, data.boundary
                    );
                }
                Item::Glue {
                    extent,
                    stretch,
                    shrink,
                    discardable,
                    ..
                } => {
                    let start = position;
                    position += extent;
                    let _ = writeln!(
                        output,
                        "{index:04} glue start={start:.3} end={position:.3} stretch={stretch:.3} shrink={shrink:.3} discardable={discardable}",
                    );
                }
                Item::Penalty {
                    break_extent,
                    cost,
                    flagged,
                    forced,
                    data,
                } => {
                    let _ = writeln!(
                        output,
                        "{index:04} penalty block={} boundary={:?} at={:.3} break_extent={break_extent:.3} cost={cost} flagged={flagged} forced={forced}",
                        data.block, data.boundary, position
                    );
                }
            }
        }
        output
    }
}

impl BreakPolicy<PagePenalty> for PageBreakPolicy {
    fn is_legal(&self, context: BreakContext, penalty: PenaltyRef<'_, PagePenalty>) -> bool {
        let block = &self.blocks[penalty.data.block];
        if penalty.cost >= PROHIBITED_BREAK_COST {
            return false;
        }
        if block.break_before == AuthorBreak::Page && context.segment_start < block.y - f32::EPSILON
        {
            return false;
        }
        if penalty.data.block > 0 {
            let previous = &self.blocks[penalty.data.block - 1];
            if previous.break_after == AuthorBreak::Page
                && context.segment_start <= previous.y + f32::EPSILON
            {
                return false;
            }
        }

        if let PageBoundary::Line(line_index) = penalty.data.boundary {
            let line_ends = &self.line_ends[block.line_start..block.line_end];
            let first_on_segment =
                line_ends.partition_point(|end| *end <= context.segment_start + f32::EPSILON);
            let lines_here = line_index + 1 - first_on_segment.min(line_index + 1);
            let lines_remaining = line_ends.len() - line_index - 1;
            if lines_here < usize::from(block.orphans)
                || lines_remaining < usize::from(block.widows)
            {
                return false;
            }
            if penalty.data.block > 0
                && self.blocks[penalty.data.block - 1].kind == LayoutBlockKind::Heading
                && context.segment_start <= self.blocks[penalty.data.block - 1].y + f32::EPSILON
                && line_index + 1 < MIN_FRAGMENT_LINES
            {
                return false;
            }
        }
        true
    }
}

#[inline]
fn append_block_candidates(
    block: &crate::layout::BlockLayout,
    dy: f32,
    block_index: usize,
    next_y: f32,
    result: &mut Vec<Candidate>,
) {
    if is_splittable(block.kind) && block.lines.len() > 1 {
        for line_index in 0..block.lines.len() - 1 {
            let line = &block.lines[line_index];
            result.push(Candidate {
                end_y: line.y + line.height + dy,
                next_y: block.lines[line_index + 1].y + dy,
                block: block_index,
                kind: CandidateKind::Line(line_index),
            });
        }
    } else if block.kind == LayoutBlockKind::Table
        && let Some(table) = &block.table
    {
        if table.row_y > block.y + f32::EPSILON {
            result.push(Candidate {
                end_y: table.row_y + dy,
                next_y: table.row_y + dy,
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
            let row_is_spanned = table
                .row_spans
                .iter()
                .any(|&(start, end)| start <= row_index && row_index < end && end > start + 1);
            if row_index >= table.header_row_count && active_spans == 0 && !row_is_spanned {
                let first =
                    safe_lines.partition_point(|boundary| *boundary <= row_start + f32::EPSILON);
                let last =
                    safe_lines.partition_point(|boundary| *boundary < row_end - f32::EPSILON);
                for &boundary in &safe_lines[first..last] {
                    result.push(Candidate {
                        end_y: boundary + dy,
                        next_y: boundary + dy,
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
                    end_y: row_end + dy,
                    next_y: row_end + dy,
                    block: block_index,
                    kind: CandidateKind::TableRow,
                });
            }
            row_start = row_end;
        }
    }
    result.push(Candidate {
        end_y: block.y + block.height + dy,
        next_y,
        block: block_index,
        kind: CandidateKind::Block,
    });
}
