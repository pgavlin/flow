//! Per-construction lookups. Keep canonical ordering separate from visual order:
//! table-cell lines can overlap and interleave text ranges.

#[cfg(test)]
mod tests;

use super::*;
use crate::layout::{AtomicLayout, BlockLayout, LineLayout};
use std::collections::BTreeSet;

pub(crate) struct AnchorGroup<'a> {
    pub node: NodeId,
    pub position: FlowTextPosition,
    pub aliases: Vec<&'a flow::AnchorId>,
}

struct NodeAnchors<'a> {
    first_position: FlowTextPosition,
    aliases: Vec<&'a flow::AnchorId>,
}

pub(crate) struct AnchorLookup<'a> {
    groups: Vec<AnchorGroup<'a>>,
    by_position: HashMap<(NodeId, FlowTextPosition), usize>,
    by_node: HashMap<NodeId, NodeAnchors<'a>>,
    #[cfg(test)]
    inspected_anchors: usize,
}

impl<'a> AnchorLookup<'a> {
    pub fn new(document: &'a FlowDocument) -> Self {
        Self::from_anchors(document.anchors())
    }

    fn from_anchors(
        anchors: impl IntoIterator<Item = (&'a flow::AnchorId, &'a flow::Anchor)>,
    ) -> Self {
        let mut result = Self {
            groups: Vec::new(),
            by_position: HashMap::new(),
            by_node: HashMap::new(),
            #[cfg(test)]
            inspected_anchors: 0,
        };
        // Preserve exact-name order, including when aliases at different
        // positions interleave in that order.
        for (id, anchor) in anchors {
            #[cfg(test)]
            {
                result.inspected_anchors += 1;
            }
            let node = anchor.target.node;
            let position = anchor.target.position;
            result
                .by_node
                .entry(node)
                .or_insert_with(|| NodeAnchors {
                    first_position: position,
                    aliases: Vec::new(),
                })
                .aliases
                .push(id);
            let index = *result
                .by_position
                .entry((node, position))
                .or_insert_with(|| {
                    let index = result.groups.len();
                    result.groups.push(AnchorGroup {
                        node,
                        position,
                        aliases: Vec::new(),
                    });
                    index
                });
            result.groups[index].aliases.push(id);
        }
        result
    }

    pub fn position_and_anchors(
        &self,
        node: NodeId,
        default: FlowTextPosition,
    ) -> (FlowTextPosition, Vec<flow::AnchorId>) {
        let position = if self.by_position.contains_key(&(node, default)) {
            default
        } else {
            self.by_node
                .get(&node)
                .map_or(default, |anchors| anchors.first_position)
        };
        let aliases = self
            .by_position
            .get(&(node, position))
            .map_or_else(Vec::new, |index| {
                self.groups[*index]
                    .aliases
                    .iter()
                    .map(|id| (*id).clone())
                    .collect()
            });
        (position, aliases)
    }

    pub fn groups(&self) -> &[AnchorGroup<'a>] {
        &self.groups
    }

    pub fn into_node_anchors(self) -> HashMap<NodeId, Vec<flow::AnchorId>> {
        self.by_node
            .into_iter()
            .map(|(node, anchors)| (node, anchors.aliases.into_iter().cloned().collect()))
            .collect()
    }

    #[cfg(test)]
    pub fn all_anchors(&self, node: NodeId) -> Vec<flow::AnchorId> {
        self.by_node.get(&node).map_or_else(Vec::new, |anchors| {
            anchors.aliases.iter().map(|id| (*id).clone()).collect()
        })
    }
}

/// A sweep computes the first original-order interval covering each disjoint
/// coordinate span. Queries are binary searches, even with arbitrary overlaps.
/// Closed endpoints also support empty ranges and usize::MAX without overflow.
struct RangeLookup {
    spans: Vec<(usize, Option<usize>)>,
    starts: Vec<(usize, usize)>,
    #[cfg(test)]
    construction_visits: usize,
    #[cfg(test)]
    probes: std::cell::Cell<usize>,
}

impl RangeLookup {
    fn new(ranges: impl IntoIterator<Item = (usize, usize, usize)>) -> Self {
        let mut result = Self {
            spans: Vec::new(),
            starts: Vec::new(),
            #[cfg(test)]
            construction_visits: 0,
            #[cfg(test)]
            probes: std::cell::Cell::new(0),
        };
        let mut events = Vec::new();
        for (start, last, order) in ranges {
            #[cfg(test)]
            {
                result.construction_visits += 1;
            }
            result.starts.push((start, order));
            events.push((start, true, order));
            if let Some(end) = last.checked_add(1) {
                events.push((end, false, order));
            }
        }
        result.starts.sort_unstable();
        result.starts.dedup_by_key(|entry| entry.0);
        events.sort_unstable();
        let mut active = BTreeSet::new();
        let mut cursor = 0;
        while cursor < events.len() {
            let position = events[cursor].0;
            while cursor < events.len() && events[cursor].0 == position {
                #[cfg(test)]
                {
                    result.construction_visits += 1;
                }
                let (_, add, order) = events[cursor];
                if add {
                    active.insert(order);
                } else {
                    active.remove(&order);
                }
                cursor += 1;
            }
            let winner = active.first().copied();
            if result
                .spans
                .last()
                .is_none_or(|(_, previous)| *previous != winner)
            {
                result.spans.push((position, winner));
            }
        }
        result
    }

    fn containing(&self, offset: usize) -> Option<usize> {
        let index = self
            .spans
            .partition_point(|(start, _)| {
                #[cfg(test)]
                self.probes.set(self.probes.get() + 1);
                *start <= offset
            })
            .checked_sub(1)?;
        self.spans[index].1
    }

    fn following(&self, offset: usize) -> Option<usize> {
        let index = self.starts.partition_point(|(start, _)| {
            #[cfg(test)]
            self.probes.set(self.probes.get() + 1);
            *start <= offset
        });
        self.starts.get(index).map(|(_, order)| *order)
    }
}

fn last_position(range: TextRange) -> usize {
    range.end.saturating_sub(1).max(range.start)
}

pub(super) struct GeometryLookup<'a> {
    blocks: &'a [BlockLayout],
    lines: Vec<&'a LineLayout>,
    atomics: Vec<&'a AtomicLayout>,
    blocks_by_node: HashMap<NodeId, usize>,
    atomics_by_node: HashMap<NodeId, f32>,
    block_ranges: RangeLookup,
    line_ranges: RangeLookup,
    atomic_ranges: RangeLookup,
}

impl<'a> GeometryLookup<'a> {
    pub fn new(layout: &'a ContinuousLayout) -> Self {
        let mut blocks_by_node = HashMap::new();
        let mut atomics_by_node = HashMap::new();
        let mut lines = Vec::new();
        let mut atomics = Vec::new();
        for (index, block) in layout.blocks.iter().enumerate() {
            if let Some(node) = block.node {
                blocks_by_node.entry(node).or_insert(index);
            }
            for atomic in &block.atomic_objects {
                if let Some(node) = atomic.node {
                    atomics_by_node.entry(node).or_insert(atomic.y);
                }
                atomics.push(atomic);
            }
            lines.extend(&block.lines);
        }
        let block_ranges = RangeLookup::new(
            layout
                .blocks
                .iter()
                .enumerate()
                .map(|(index, block)| (block.range.start, last_position(block.range), index)),
        );
        let line_ranges = RangeLookup::new(
            lines
                .iter()
                .enumerate()
                .map(|(index, line)| (line.range.start, last_position(line.range), index)),
        );
        // ContinuousLayout::y_for_text includes an atomic's trailing endpoint.
        let atomic_ranges = RangeLookup::new(atomics.iter().enumerate().map(|(index, atomic)| {
            (
                atomic.range.start,
                atomic.range.end.max(atomic.range.start),
                index,
            )
        }));
        Self {
            blocks: &layout.blocks,
            lines,
            atomics,
            blocks_by_node,
            atomics_by_node,
            block_ranges,
            line_ranges,
            atomic_ranges,
        }
    }

    pub fn y(&self, node: NodeId, position: FlowTextPosition) -> Option<f32> {
        if let Some(y) = self.atomics_by_node.get(&node) {
            return Some(*y);
        }
        let block = self
            .blocks_by_node
            .get(&node)
            .map(|index| &self.blocks[*index]);
        if let Some(block) = block
            && (block.range.start == block.range.end
                || position.offset.as_usize() == block.range.start)
        {
            return Some(block.y);
        }
        let offset = match position.affinity {
            flow::Affinity::Downstream => position.offset.as_usize(),
            flow::Affinity::Upstream => position.offset.as_usize().saturating_sub(1),
        };
        let line = self.line_ranges.containing(offset).or_else(|| {
            (position.affinity == flow::Affinity::Downstream)
                .then(|| self.line_ranges.following(offset))
                .flatten()
        });
        line.map(|index| self.lines[index].y)
            .or_else(|| {
                self.atomic_ranges
                    .containing(offset)
                    .map(|index| self.atomics[index].y)
            })
            .or_else(|| {
                self.block_ranges
                    .containing(offset)
                    .map(|index| self.blocks[index].y)
            })
            .or_else(|| block.map(|block| block.y))
    }
}
