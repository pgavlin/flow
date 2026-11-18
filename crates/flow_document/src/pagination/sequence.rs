//! Dimension-neutral box/glue/penalty sequencing and first-fit breaking.
//!
//! This module intentionally has no dependencies on flow layout, semantic, or
//! page-fragment types. Callers provide payloads and breakpoint policy.

/// One item in a one-dimensional layout sequence.
#[derive(Clone, Debug, PartialEq)]
pub enum Item<BoxData = (), GlueData = (), PenaltyData = (), N = f32> {
    Box {
        extent: N,
        data: BoxData,
    },
    Glue {
        extent: N,
        stretch: N,
        shrink: N,
        discardable: bool,
        data: GlueData,
    },
    Penalty {
        break_extent: N,
        cost: i32,
        flagged: bool,
        forced: bool,
        data: PenaltyData,
    },
}

/// A prepared one-dimensional item stream.
#[derive(Clone, Debug)]
pub struct ItemStream<BoxData = (), GlueData = (), PenaltyData = (), N = f32> {
    pub items: Vec<Item<BoxData, GlueData, PenaltyData, N>>,
    pub content_extent: N,
    breaks: Vec<BreakCandidate<N>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct BreakCandidate<N = f32> {
    item: usize,
    end: N,
    next: N,
}

impl<BoxData, GlueData, PenaltyData> ItemStream<BoxData, GlueData, PenaltyData, f32> {
    /// Prepares an item stream from caller-lowered relative extents.
    pub fn new(items: Vec<Item<BoxData, GlueData, PenaltyData, f32>>) -> Self {
        let mut builder = ItemStreamBuilder::with_capacity(items.len(), items.len() / 2);
        for item in items {
            builder.push(item);
        }
        builder.finish()
    }

    pub fn break_count(&self) -> usize {
        self.breaks.len()
    }
}

/// Incremental stream construction that prepares break positions in one pass.
pub struct ItemStreamBuilder<BoxData = (), GlueData = (), PenaltyData = ()> {
    items: Vec<Item<BoxData, GlueData, PenaltyData, f32>>,
    breaks: Vec<BreakCandidate<f32>>,
    position: f32,
    pending_break: Option<usize>,
}

impl<BoxData, GlueData, PenaltyData> ItemStreamBuilder<BoxData, GlueData, PenaltyData> {
    pub fn with_capacity(items: usize, breaks: usize) -> Self {
        Self {
            items: Vec::with_capacity(items),
            breaks: Vec::with_capacity(breaks),
            position: 0.0,
            pending_break: None,
        }
    }

    pub fn push(&mut self, item: Item<BoxData, GlueData, PenaltyData, f32>) {
        match item {
            Item::Box { extent, data } => self.push_box(extent, data),
            Item::Glue {
                extent,
                stretch,
                shrink,
                discardable,
                data,
            } => self.push_glue(extent, stretch, shrink, discardable, data),
            Item::Penalty {
                break_extent,
                cost,
                flagged,
                forced,
                data,
            } => self.push_penalty(break_extent, cost, flagged, forced, data),
        }
    }

    #[inline]
    pub fn push_box(&mut self, extent: f32, data: BoxData) {
        self.pending_break = None;
        self.position += extent;
        self.items.push(Item::Box { extent, data });
    }

    #[inline]
    pub fn push_glue(
        &mut self,
        extent: f32,
        stretch: f32,
        shrink: f32,
        discardable: bool,
        data: GlueData,
    ) {
        self.position += extent;
        if discardable {
            if let Some(candidate) = self.pending_break {
                self.breaks[candidate].next = self.position;
            }
        } else {
            self.pending_break = None;
        }
        self.items.push(Item::Glue {
            extent,
            stretch,
            shrink,
            discardable,
            data,
        });
    }

    #[inline]
    pub fn push_penalty(
        &mut self,
        break_extent: f32,
        cost: i32,
        flagged: bool,
        forced: bool,
        data: PenaltyData,
    ) {
        self.pending_break = None;
        self.breaks.push(BreakCandidate {
            item: self.items.len(),
            end: self.position + break_extent,
            next: self.position,
        });
        self.pending_break = Some(self.breaks.len() - 1);
        self.items.push(Item::Penalty {
            break_extent,
            cost,
            flagged,
            forced,
            data,
        });
    }

    pub fn finish(self) -> ItemStream<BoxData, GlueData, PenaltyData, f32> {
        debug_assert!(self.breaks.windows(2).all(|pair| {
            pair[0].end <= pair[1].end + f32::EPSILON && pair[0].next <= pair[1].next + f32::EPSILON
        }));
        ItemStream {
            items: self.items,
            content_extent: self.position,
            breaks: self.breaks,
        }
    }
}

/// Context supplied to caller-owned breakpoint policy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BreakContext {
    pub ordinal: usize,
    pub item: usize,
    pub segment_start: f32,
    pub break_end: f32,
    pub next_start: f32,
}

/// A penalty and its caller-owned payload.
#[derive(Clone, Copy, Debug)]
pub struct PenaltyRef<'a, PenaltyData, N = f32> {
    pub break_extent: N,
    pub cost: i32,
    pub flagged: bool,
    pub forced: bool,
    pub data: &'a PenaltyData,
}

/// Caller-owned semantic policy for a sequence's penalty payloads.
pub trait BreakPolicy<PenaltyData> {
    fn is_legal(&self, context: BreakContext, penalty: PenaltyRef<'_, PenaltyData>) -> bool;
}

/// One selected boundary in a broken one-dimensional sequence.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelectedBreak {
    pub start: f32,
    pub end: f32,
    pub next: f32,
    pub item: Option<usize>,
    pub relaxed: bool,
}

/// Selected boundaries for an entire sequence.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BreakPlan {
    pub segments: Vec<SelectedBreak>,
    pub content_extent: f32,
    pub target_extent: f32,
}

/// Greedy first-fit breaking for a one-dimensional sequence.
#[derive(Clone, Copy, Debug)]
pub struct FirstFit {
    minimum_extent: f32,
    fit_tolerance: f32,
}

impl Default for FirstFit {
    fn default() -> Self {
        Self {
            minimum_extent: 1.0,
            fit_tolerance: 0.01,
        }
    }
}

impl FirstFit {
    pub fn new(minimum_extent: f32, fit_tolerance: f32) -> Self {
        Self {
            minimum_extent,
            fit_tolerance,
        }
    }

    pub fn break_sequence<BoxData, GlueData, PenaltyData>(
        &self,
        stream: &ItemStream<BoxData, GlueData, PenaltyData, f32>,
        requested_extent: f32,
        policy: &impl BreakPolicy<PenaltyData>,
    ) -> BreakPlan {
        let target_extent = if requested_extent.is_finite() {
            requested_extent.max(self.minimum_extent)
        } else {
            self.minimum_extent
        };
        if stream.breaks.is_empty() {
            return BreakPlan {
                segments: vec![SelectedBreak {
                    start: 0.0,
                    end: 0.0,
                    next: 0.0,
                    item: None,
                    relaxed: false,
                }],
                content_extent: stream.content_extent,
                target_extent,
            };
        }

        let penalty = |candidate: usize| -> PenaltyRef<'_, PenaltyData> {
            match &stream.items[stream.breaks[candidate].item] {
                Item::Penalty {
                    break_extent,
                    cost,
                    flagged,
                    forced,
                    data,
                } => PenaltyRef {
                    break_extent: *break_extent,
                    cost: *cost,
                    flagged: *flagged,
                    forced: *forced,
                    data,
                },
                _ => unreachable!("break candidate must reference a penalty"),
            }
        };
        let mut segments = Vec::new();
        let mut start = 0.0;
        let mut guard = 0;
        let emergency_steps =
            (stream.content_extent.max(0.0) / self.minimum_extent).ceil() as usize;
        let maximum_steps = stream
            .breaks
            .len()
            .saturating_add(emergency_steps)
            .saturating_add(1);

        while start < stream.content_extent && guard <= maximum_steps {
            guard += 1;
            let limit = start + target_extent;
            let first = stream
                .breaks
                .partition_point(|candidate| candidate.next <= start + f32::EPSILON);
            let upper = stream
                .breaks
                .partition_point(|candidate| candidate.end <= limit + self.fit_tolerance);
            let fitting = (first < upper)
                .then(|| {
                    (first..upper).rev().find(|&ordinal| {
                        let candidate = stream.breaks[ordinal];
                        policy.is_legal(
                            BreakContext {
                                ordinal,
                                item: candidate.item,
                                segment_start: start,
                                break_end: candidate.end,
                                next_start: candidate.next,
                            },
                            penalty(ordinal),
                        )
                    })
                })
                .flatten();
            let (chosen, relaxed) = fitting
                .map(|chosen| (chosen, false))
                .or_else(|| (first < upper).then(|| (upper - 1, true)))
                .or_else(|| (first < stream.breaks.len()).then_some((first, true)))
                .expect("nonempty sequence has a break candidate");
            let selected = stream.breaks[chosen];
            segments.push(SelectedBreak {
                start,
                end: selected.end,
                next: selected.next,
                item: Some(selected.item),
                relaxed,
            });
            start = selected.next;
        }

        BreakPlan {
            segments,
            content_extent: stream.content_extent,
            target_extent,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    struct AllowAll;

    impl BreakPolicy<&'static str> for AllowAll {
        fn is_legal(&self, _: BreakContext, _: PenaltyRef<'_, &'static str>) -> bool {
            true
        }
    }

    #[test]
    fn module_has_no_flow_dependencies() {
        let source = include_str!("sequence.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        let crate_path = ["crate", "::"].concat();
        let parent_path = ["super", "::"].concat();
        assert!(!production.contains(&crate_path));
        assert!(!production.contains(&parent_path));
    }

    #[test]
    fn discardable_glue_moves_the_next_segment_start() {
        let stream = ItemStream::new(vec![
            Item::Box {
                extent: 10.0,
                data: 1_u8,
            },
            Item::Penalty {
                break_extent: 2.0,
                cost: 0,
                flagged: false,
                forced: false,
                data: "first",
            },
            Item::Glue {
                extent: 5.0,
                stretch: 0.0,
                shrink: 0.0,
                discardable: true,
                data: (),
            },
            Item::Box {
                extent: 10.0,
                data: 2_u8,
            },
            Item::Penalty {
                break_extent: 0.0,
                cost: 0,
                flagged: false,
                forced: false,
                data: "second",
            },
        ]);
        let plan = FirstFit::default().break_sequence(&stream, 12.0, &AllowAll);
        assert_eq!(plan.segments.len(), 2);
        assert_eq!(plan.segments[0].end, 12.0);
        assert_eq!(plan.segments[0].next, 15.0);
        assert_eq!(plan.segments[1].start, 15.0);
        assert_eq!(plan.segments[1].end, 25.0);
    }

    struct RejectAll;

    impl BreakPolicy<u8> for RejectAll {
        fn is_legal(&self, _: BreakContext, _: PenaltyRef<'_, u8>) -> bool {
            false
        }
    }

    #[test]
    fn impossible_policy_relaxes_and_makes_progress() {
        let stream = ItemStream::new(
            (0_u8..4)
                .flat_map(|index| {
                    [
                        Item::<(), (), u8>::Box {
                            extent: 10.0,
                            data: (),
                        },
                        Item::Penalty {
                            break_extent: 0.0,
                            cost: 10_000,
                            flagged: false,
                            forced: false,
                            data: index,
                        },
                    ]
                })
                .collect(),
        );
        let plan = FirstFit::default().break_sequence(&stream, 10.0, &RejectAll);
        assert_eq!(plan.segments.len(), 4);
        assert!(plan.segments.iter().all(|selected| selected.relaxed));
        assert!(
            plan.segments
                .windows(2)
                .all(|pair| pair[1].start > pair[0].start)
        );
    }
}
