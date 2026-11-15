//! Retained selection recovery for viewer document replacement.
//! Stateless selection queries stay available without rendering.

use super::*;
use std::collections::{HashMap, HashSet};

const RECOVERY_CONTEXT_CHARS: usize = 32;
const MIN_RECOVERY_EVIDENCE_CHARS: usize = 8;

#[derive(Clone, Debug, Eq, PartialEq)]
struct StableFlowTextPosition {
    position: TextPosition,
    unit_kind: FlowSelectionUnitKind,
    relative_offset: usize,
    before: String,
    after: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StableFlowSelection {
    anchor: StableFlowTextPosition,
    active: StableFlowTextPosition,
}

pub(crate) fn capture_flow_selection(
    document: &FlowDocument,
    selection: Selection,
) -> Option<StableFlowSelection> {
    let selection = normalize_flow_selection(document, selection)?;
    Some(StableFlowSelection {
        anchor: capture_flow_position(document, selection.anchor)?,
        active: capture_flow_position(document, selection.active)?,
    })
}

pub(crate) fn recover_flow_selection(
    document: &FlowDocument,
    stable: &StableFlowSelection,
) -> Option<Selection> {
    let selection = Selection {
        anchor: recover_flow_position(document, &stable.anchor)?,
        active: recover_flow_position(document, &stable.active)?,
    };
    let selection = normalize_flow_selection(document, selection)?;
    let was_collapsed = stable.anchor.position.offset == stable.active.position.offset;
    (was_collapsed || !selection.is_collapsed()).then_some(selection)
}

fn flow_separator_units(document: &FlowDocument) -> Vec<FlowSelectionUnit> {
    let bytes = document.text().as_str().as_bytes();
    let mut units = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        if !matches!(bytes[start], b'\n' | b'\t') {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        while end < bytes.len() && matches!(bytes[end], b'\n' | b'\t') {
            end += 1;
        }
        units.push(FlowSelectionUnit {
            kind: FlowSelectionUnitKind::Separator,
            range: TextRange { start, end },
            atomic: false,
        });
        start = end;
    }
    units
}

fn flow_separator_unit_at(
    document: &FlowDocument,
    position: TextPosition,
) -> Option<FlowSelectionUnit> {
    flow_separator_units(document).into_iter().find(|unit| {
        (position.affinity == Affinity::Downstream
            && unit.range.start <= position.offset
            && position.offset < unit.range.end)
            || (position.affinity == Affinity::Upstream
                && unit.range.start < position.offset
                && position.offset <= unit.range.end)
    })
}

fn capture_flow_position(
    document: &FlowDocument,
    position: TextPosition,
) -> Option<StableFlowTextPosition> {
    let unit = flow_unit_at(document, position, false)
        .or_else(|| flow_separator_unit_at(document, position))
        .or_else(|| {
            flow_units(document)
                .filter(|unit| unit.kind != FlowSelectionUnitKind::Table)
                .filter(|unit| {
                    unit.range.start == position.offset || unit.range.end == position.offset
                })
                .min_by_key(|unit| unit.range.end.saturating_sub(unit.range.start))
        })?;
    let text = document.text().as_str();
    Some(StableFlowTextPosition {
        position,
        unit_kind: unit.kind,
        relative_offset: position.offset.saturating_sub(unit.range.start),
        before: text[..position.offset]
            .chars()
            .rev()
            .take(RECOVERY_CONTEXT_CHARS)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect(),
        after: text[position.offset..]
            .chars()
            .take(RECOVERY_CONTEXT_CHARS)
            .collect(),
    })
}

fn recover_flow_position(
    document: &FlowDocument,
    stable: &StableFlowTextPosition,
) -> Option<TextPosition> {
    let text = document.text().as_str();
    let before_len = stable.before.chars().count();
    let after_len = stable.after.chars().count();
    let required = MIN_RECOVERY_EVIDENCE_CHARS.min(before_len + after_len);
    let mut candidates_by_offset = HashMap::new();
    let units: Box<dyn Iterator<Item = FlowSelectionUnit> + '_> =
        if stable.unit_kind == FlowSelectionUnitKind::Separator {
            Box::new(flow_separator_units(document).into_iter())
        } else {
            Box::new(flow_units(document).filter(|unit| unit.kind == stable.unit_kind))
        };
    for unit in units {
        let mut offsets = HashSet::new();
        if unit.atomic {
            offsets.extend([unit.range.start, unit.range.end]);
        } else if let Some(segment) = text.get(unit.range.start..unit.range.end) {
            offsets.insert(unit.range.start);
            offsets.extend(
                segment
                    .char_indices()
                    .map(|(offset, _)| unit.range.start + offset),
            );
            offsets.insert(unit.range.end);
        }
        for offset in offsets {
            let before = matching_suffix_chars(&stable.before, &text[..offset]);
            let after = matching_prefix_chars(&stable.after, &text[offset..]);
            let candidate = FlowRecoveryCandidate {
                offset,
                before,
                after,
                before_full: before == before_len,
                after_full: after == after_len,
                relative_distance: offset
                    .saturating_sub(unit.range.start)
                    .abs_diff(stable.relative_offset),
                unit_length: unit.range.end.saturating_sub(unit.range.start),
                absolute_distance: offset.abs_diff(stable.position.offset),
            };
            if (candidate.before_full || candidate.after_full) && candidate.evidence() >= required {
                candidates_by_offset
                    .entry(offset)
                    .and_modify(|existing: &mut FlowRecoveryCandidate| {
                        if (candidate.unit_length, candidate.relative_distance)
                            < (existing.unit_length, existing.relative_distance)
                        {
                            *existing = candidate;
                        }
                    })
                    .or_insert(candidate);
            }
        }
    }
    let mut candidates: Vec<_> = candidates_by_offset.into_values().collect();
    candidates.sort_by(|left, right| {
        right
            .evidence()
            .cmp(&left.evidence())
            .then_with(|| right.full_sides().cmp(&left.full_sides()))
            .then_with(|| left.relative_distance.cmp(&right.relative_distance))
            .then_with(|| left.absolute_distance.cmp(&right.absolute_distance))
            .then_with(|| left.offset.cmp(&right.offset))
    });
    let best = *candidates.first()?;
    if let Some(second) = candidates.get(1)
        && second.evidence() == best.evidence()
        && second.full_sides() == best.full_sides()
        && second.relative_distance == best.relative_distance
        && (best.full_sides() < 2 || second.absolute_distance == best.absolute_distance)
    {
        return None;
    }
    Some(TextPosition {
        offset: best.offset,
        affinity: stable.position.affinity,
    })
}

#[derive(Clone, Copy, Debug)]
struct FlowRecoveryCandidate {
    offset: usize,
    before: usize,
    after: usize,
    before_full: bool,
    after_full: bool,
    relative_distance: usize,
    unit_length: usize,
    absolute_distance: usize,
}

impl FlowRecoveryCandidate {
    fn evidence(self) -> usize {
        self.before + self.after
    }

    fn full_sides(self) -> usize {
        usize::from(self.before_full) + usize::from(self.after_full)
    }
}

fn matching_suffix_chars(expected: &str, actual: &str) -> usize {
    expected
        .chars()
        .rev()
        .zip(actual.chars().rev())
        .take_while(|(left, right)| left == right)
        .count()
}

fn matching_prefix_chars(expected: &str, actual: &str) -> usize {
    expected
        .chars()
        .zip(actual.chars())
        .take_while(|(left, right)| left == right)
        .count()
}
