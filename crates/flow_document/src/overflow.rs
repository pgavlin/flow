//! Shared horizontal-overflow geometry and session-local presentation state.
//!
//! Owners are document-local node identities, never durable reading locations.
use std::collections::HashMap;

use crate::{
    layout::{BlockLayout, ComposedDocument, ContinuousLayout, LayoutBlockKind},
    model::flow::NodeId,
    pagination::{PageFragment, PaginatedLayout},
    pixels::PaintTarget,
};

pub type OverflowOwner = NodeId;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverflowRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl OverflowRect {
    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HorizontalOverflowPlacement {
    pub owner: OverflowOwner,
    pub viewport: OverflowRect,
    pub content_width: f32,
    pub clip: OverflowRect,
}

impl HorizontalOverflowPlacement {
    pub fn maximum_offset(self) -> f32 {
        (self.content_width - self.viewport.width).max(0.0)
    }
}

#[derive(Clone, Debug, Default)]
pub struct HorizontalOverflowState {
    pub(crate) offsets: HashMap<OverflowOwner, f32>,
    pub(crate) focused: Option<OverflowOwner>,
}

impl HorizontalOverflowState {
    pub fn offset(&self, placement: HorizontalOverflowPlacement) -> f32 {
        self.offsets
            .get(&placement.owner)
            .copied()
            .unwrap_or(0.0)
            .clamp(0.0, placement.maximum_offset())
    }

    pub fn focused(&self) -> Option<OverflowOwner> {
        self.focused
    }

    pub(crate) fn scroll(&mut self, placement: HorizontalOverflowPlacement, delta: f32) {
        if delta.is_finite() {
            let offset = (self.offset(placement) + delta).clamp(0.0, placement.maximum_offset());
            self.offsets.insert(placement.owner, offset);
        }
    }
}

pub(crate) fn block_placement(
    block: &BlockLayout,
    width: f32,
) -> Option<HorizontalOverflowPlacement> {
    let (left, right) = if let Some(table) = &block.table {
        let right = block
            .lines
            .iter()
            .flat_map(|line| &line.runs)
            .map(|run| run.x + run.width)
            .chain(
                block
                    .atomic_objects
                    .iter()
                    .map(|object| object.x + object.width),
            )
            .fold(table.x + table.width, f32::max);
        (table.x, right)
    } else if block.kind == LayoutBlockKind::Code {
        let mut runs = block.lines.iter().flat_map(|line| &line.runs);
        let first = runs.next()?;
        runs.fold((first.x, first.x + first.width), |(left, right), run| {
            (left.min(run.x), right.max(run.x + run.width))
        })
    } else {
        return None;
    };
    let viewport_width = (width - left).max(1.0);
    if right - left <= viewport_width + 0.01 {
        return None;
    }
    let viewport = OverflowRect {
        x: left,
        y: block.y,
        width: viewport_width,
        height: block.height,
    };
    Some(HorizontalOverflowPlacement {
        owner: block.node?,
        viewport,
        content_width: right - left,
        clip: viewport,
    })
}

impl ContinuousLayout {
    pub fn horizontal_overflows(&self) -> Vec<HorizontalOverflowPlacement> {
        self.overflow_placements.clone()
    }
}

pub(crate) fn fragment_block(fragment: &PageFragment) -> Option<usize> {
    Some(match fragment {
        PageFragment::Lines(f) => f.block,
        PageFragment::Atomic(f) => f.block,
        PageFragment::Rule(f) => f.block,
        PageFragment::BlockBox(f) => f.block,
        PageFragment::TableSlice(f) => f.block,
        PageFragment::TableRule(f) => f.block,
        PageFragment::TableQuoteBar(f) => f.block,
        PageFragment::QuoteBar(_) => return None,
    })
}

impl PaginatedLayout {
    /// Viewports use page-body coordinates. All replicas/fragments of a block
    /// reference the same owner, including repeated table headers.
    pub fn horizontal_overflows(
        &self,
        document: &ComposedDocument,
        page: usize,
    ) -> Vec<HorizontalOverflowPlacement> {
        let Some(page) = self.pages.get(page) else {
            return Vec::new();
        };
        let mut placements: Vec<HorizontalOverflowPlacement> = Vec::new();
        let mut intrinsic = HashMap::new();
        for fragment in &self.fragments[page.fragments.as_range()] {
            let Some(index) = fragment_block(fragment) else {
                continue;
            };
            let placement = intrinsic.entry(index).or_insert_with(|| {
                document
                    .blocks
                    .get(index)
                    .and_then(|block| block_placement(&block.layout, self.width))
            });
            let Some(mut placement) = *placement else {
                continue;
            };
            let (y, height) = match fragment {
                PageFragment::Lines(f) => (f.y, f.height),
                PageFragment::Atomic(f) => (f.y, f.height),
                PageFragment::BlockBox(f) => (f.y, f.height),
                PageFragment::TableSlice(f) => (f.y, f.height),
                PageFragment::TableQuoteBar(f) => (f.y, f.height),
                PageFragment::TableRule(f) => (f.y, 1.0),
                PageFragment::Rule(f) => (f.y, 1.0),
                PageFragment::QuoteBar(_) => continue,
            };
            let top = y.max(0.0);
            let bottom = (y + height).min(self.body_height);
            if bottom <= top {
                continue;
            }
            if let Some(existing) = placements.iter_mut().find(|p| p.owner == placement.owner) {
                let bottom = bottom.max(existing.viewport.y + existing.viewport.height);
                existing.viewport.y = existing.viewport.y.min(top);
                existing.viewport.height = bottom - existing.viewport.y;
                existing.clip = existing.viewport;
            } else {
                placement.viewport.y = top;
                placement.viewport.height = bottom - top;
                placement.clip = placement.viewport;
                placements.push(placement);
            }
        }
        placements.sort_by(|a, b| a.viewport.y.total_cmp(&b.viewport.y));
        placements
    }
}

/// Apply the same clip/translation on every CPU or host-provided paint target.
/// The callback's x origin is adjusted into the clipped target's coordinates.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_content<T: PaintTarget>(
    surface: &mut T,
    placement: Option<HorizontalOverflowPlacement>,
    state: Option<&HorizontalOverflowState>,
    x_offset: f32,
    y_offset: f32,
    scale: f32,
    mut draw: impl FnMut(&mut T, f32),
) {
    if let (Some(placement), Some(state)) = (placement, state) {
        let left = ((x_offset + placement.clip.x) * scale).floor() as i32;
        let right = ((x_offset + placement.clip.x + placement.clip.width) * scale).ceil() as i32;
        let surface_height = surface.height();
        let top = ((y_offset + placement.clip.y) * scale).floor().max(0.0) as i32;
        let bottom = ((y_offset + placement.clip.y + placement.clip.height) * scale)
            .ceil()
            .min(surface_height as f32) as i32;
        if bottom <= top {
            return;
        }
        let width = (right - left).max(1) as u32;
        surface.with_viewport((left, top), width, (bottom - top) as u32, |surface| {
            // Restore the caller's vertical coordinates without widening the
            // clip established above. Only horizontal content is translated.
            surface.with_viewport((0, -top), width, surface_height, |surface| {
                draw(
                    surface,
                    x_offset - left as f32 / scale - state.offset(placement),
                );
            });
        });
    } else {
        draw(surface, x_offset);
    }
}

pub(crate) fn paint_affordance<T: PaintTarget>(
    surface: &mut T,
    placement: HorizontalOverflowPlacement,
    state: &HorizontalOverflowState,
    origin: (f32, f32),
    scale: f32,
    color: [u8; 3],
) {
    let rect = placement.viewport;
    let x = (origin.0 + rect.x) * scale;
    let y = (origin.1 + rect.y) * scale;
    let width = rect.width * scale;
    let height = rect.height * scale;
    let thickness = scale.max(1.0);
    let offset = state.offset(placement);
    if offset > 0.0 {
        surface.fill_rect(x, y, thickness, height.min(12.0 * scale), color);
    }
    if offset < placement.maximum_offset() {
        surface.fill_rect(
            x + width - thickness,
            y,
            thickness,
            height.min(12.0 * scale),
            color,
        );
    }
    if state.focused == Some(placement.owner) {
        surface.fill_rect(x, y, width, thickness, color);
        surface.fill_rect(x, y + height - thickness, width, thickness, color);
        surface.fill_rect(x, y, thickness, height, color);
        surface.fill_rect(x + width - thickness, y, thickness, height, color);
    }
}
