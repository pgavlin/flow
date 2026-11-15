use super::builder::{BuildError, DraftNode};
use super::canonical_text::{BLOCK_SEPARATOR, OBJECT_REPLACEMENT, ROW_SEPARATOR};
use super::{
    Affinity, ImageAlternative, LogicalTextRule, NodeId, NodeKind, Placement, TextOffset, TextRange,
};

#[derive(Clone, Copy, Debug)]
struct EmptyAssignment {
    offset: usize,
    affinity: Affinity,
}

pub(super) struct FinalizedText {
    pub(super) text: Box<str>,
    pub(super) ranges: Vec<TextRange>,
    pub(super) empty_affinities: Vec<Affinity>,
}

enum EmitEvent {
    Enter(NodeId, EmptyAssignment),
    Suppress(NodeId, EmptyAssignment),
    Exit(NodeId, usize),
    Static(&'static str),
    Leaf(NodeId),
}

pub(super) fn finalize_text(
    nodes: &[DraftNode],
    root: NodeId,
) -> Result<FinalizedText, BuildError> {
    let order = preorder(nodes, root);
    let mut lengths = vec![0; nodes.len()];
    let mut readable = vec![false; nodes.len()];
    for &id in order.iter().rev() {
        readable[id.index()] = node_is_readable(nodes, id, &readable);
        lengths[id.index()] = measure_node(nodes, id, &lengths, &readable)?;
    }

    let mut output = String::with_capacity(lengths[root.index()]);
    let mut ranges = vec![TextRange::default(); nodes.len()];
    let mut empty_affinities = vec![Affinity::Downstream; nodes.len()];
    let root_assignment = EmptyAssignment {
        offset: 0,
        affinity: Affinity::Downstream,
    };
    let mut stack = vec![EmitEvent::Enter(root, root_assignment)];

    while let Some(event) = stack.pop() {
        match event {
            EmitEvent::Enter(id, assignment) => {
                if lengths[id.index()] == 0 {
                    assign_empty_subtree(nodes, id, assignment, &mut ranges, &mut empty_affinities);
                    continue;
                }

                let start = output.len();
                stack.push(EmitEvent::Exit(id, start));
                let plan = plan_node(nodes, id, &lengths, &readable, start, assignment)?;
                stack.extend(plan.into_iter().rev());
            }
            EmitEvent::Suppress(id, assignment) => {
                assign_empty_subtree(nodes, id, assignment, &mut ranges, &mut empty_affinities);
            }
            EmitEvent::Exit(id, start) => {
                ranges[id.index()] = range(start, output.len());
            }
            EmitEvent::Static(text) => output.push_str(text),
            EmitEvent::Leaf(id) => emit_leaf(nodes, id, &mut output),
        }
    }

    debug_assert_eq!(output.len(), lengths[root.index()]);
    Ok(FinalizedText {
        text: output.into_boxed_str(),
        ranges,
        empty_affinities,
    })
}

fn preorder(nodes: &[DraftNode], root: NodeId) -> Vec<NodeId> {
    let mut order = Vec::with_capacity(nodes.len());
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        order.push(id);
        stack.extend(nodes[id.index()].children.iter().rev().copied());
    }
    order
}

fn node_is_readable(nodes: &[DraftNode], id: NodeId, readable: &[bool]) -> bool {
    let node = &nodes[id.index()];
    match &node.kind {
        NodeKind::Text => node.literal.as_deref().is_some_and(|text| !text.is_empty()),
        NodeKind::Image(image) => {
            matches!(&image.alternative, ImageAlternative::Text(text) if !text.as_str().is_empty())
        }
        NodeKind::Math(math) => !math.source.is_empty(),
        NodeKind::UnsupportedObject(object) => {
            node.children.iter().any(|child| readable[child.index()])
                || object
                    .title
                    .as_deref()
                    .is_some_and(|title| !title.is_empty())
        }
        _ => node.children.iter().any(|child| readable[child.index()]),
    }
}

fn measure_node(
    nodes: &[DraftNode],
    id: NodeId,
    lengths: &[usize],
    readable: &[bool],
) -> Result<usize, BuildError> {
    let node = &nodes[id.index()];
    let child_lengths = || node.children.iter().map(|child| lengths[child.index()]);
    match node.kind.logical_text_rule() {
        LogicalTextRule::InlineChildren => checked_sum(child_lengths()),
        LogicalTextRule::FlowChildren => measure_flow(child_lengths()),
        LogicalTextRule::TableRows | LogicalTextRule::TableCells => {
            measure_structural(child_lengths())
        }
        LogicalTextRule::TableRoot => measure_table(nodes, node, lengths),
        LogicalTextRule::TextLiteral => Ok(node.literal.as_deref().unwrap_or_default().len()),
        LogicalTextRule::HardBreak => Ok(1),
        LogicalTextRule::BreakOpportunity | LogicalTextRule::Empty => Ok(0),
        LogicalTextRule::ObjectReplacement => Ok(OBJECT_REPLACEMENT.len_utf8()),
        LogicalTextRule::AtomicAlternative => match &node.kind {
            NodeKind::Image(image) => match &image.alternative {
                ImageAlternative::Text(text) => Ok(text.as_str().len()),
                ImageAlternative::Decorative => Ok(0),
                ImageAlternative::Missing => Ok(OBJECT_REPLACEMENT.len_utf8()),
            },
            NodeKind::Math(math) => Ok(math.source.len()),
            NodeKind::UnsupportedObject(object) => {
                let has_readable_fallback =
                    node.children.iter().any(|child| readable[child.index()]);
                if has_readable_fallback {
                    match object.placement {
                        Placement::Flow => measure_flow(child_lengths()),
                        Placement::Inline => checked_sum(child_lengths()),
                    }
                } else if let Some(title) =
                    object.title.as_deref().filter(|title| !title.is_empty())
                {
                    Ok(title.len())
                } else {
                    Ok(OBJECT_REPLACEMENT.len_utf8())
                }
            }
            _ => unreachable!("atomic-alternative kinds are exhaustive"),
        },
    }
}

fn checked_sum(mut lengths: impl Iterator<Item = usize>) -> Result<usize, BuildError> {
    lengths.try_fold(0usize, |sum, length| {
        sum.checked_add(length)
            .ok_or(BuildError::CanonicalTextTooLarge)
    })
}

fn measure_flow(lengths: impl Iterator<Item = usize>) -> Result<usize, BuildError> {
    let mut total = 0usize;
    let mut contributing = false;
    for length in lengths {
        if length == 0 {
            continue;
        }
        if contributing {
            total = total
                .checked_add(BLOCK_SEPARATOR.len())
                .ok_or(BuildError::CanonicalTextTooLarge)?;
        }
        total = total
            .checked_add(length)
            .ok_or(BuildError::CanonicalTextTooLarge)?;
        contributing = true;
    }
    Ok(total)
}

fn measure_structural(lengths: impl ExactSizeIterator<Item = usize>) -> Result<usize, BuildError> {
    let child_count = lengths.len();
    let content = checked_sum(lengths)?;
    content
        .checked_add(child_count.saturating_sub(1))
        .ok_or(BuildError::CanonicalTextTooLarge)
}

fn measure_table(
    nodes: &[DraftNode],
    table: &DraftNode,
    lengths: &[usize],
) -> Result<usize, BuildError> {
    let caption_length = table
        .children
        .iter()
        .find(|child| matches!(nodes[child.index()].kind, NodeKind::TableCaption))
        .map_or(0, |child| lengths[child.index()]);
    let mut group_count = 0usize;
    let mut grid_length = 0usize;
    for child in table
        .children
        .iter()
        .filter(|child| matches!(nodes[child.index()].kind, NodeKind::RowGroup(_)))
    {
        grid_length = grid_length
            .checked_add(lengths[child.index()])
            .ok_or(BuildError::CanonicalTextTooLarge)?;
        group_count += 1;
    }
    grid_length = grid_length
        .checked_add(group_count.saturating_sub(1))
        .ok_or(BuildError::CanonicalTextTooLarge)?;
    let separator = usize::from(caption_length > 0 && grid_length > 0) * BLOCK_SEPARATOR.len();
    caption_length
        .checked_add(separator)
        .and_then(|length| length.checked_add(grid_length))
        .ok_or(BuildError::CanonicalTextTooLarge)
}

fn plan_node(
    nodes: &[DraftNode],
    id: NodeId,
    lengths: &[usize],
    readable: &[bool],
    start: usize,
    inherited: EmptyAssignment,
) -> Result<Vec<EmitEvent>, BuildError> {
    let node = &nodes[id.index()];
    Ok(match node.kind.logical_text_rule() {
        LogicalTextRule::InlineChildren => plan_inline(&node.children, lengths, start, inherited)?,
        LogicalTextRule::FlowChildren => plan_flow(&node.children, lengths, start, inherited)?,
        LogicalTextRule::TableRows => {
            plan_structural(&node.children, lengths, start, inherited, "\n")?
        }
        LogicalTextRule::TableCells => {
            plan_structural(&node.children, lengths, start, inherited, "\t")?
        }
        LogicalTextRule::TableRoot => plan_table(nodes, node, lengths, start, inherited)?,
        LogicalTextRule::AtomicAlternative => match &node.kind {
            NodeKind::UnsupportedObject(object) => {
                let has_readable_fallback =
                    node.children.iter().any(|child| readable[child.index()]);
                if has_readable_fallback {
                    match object.placement {
                        Placement::Flow => plan_flow(&node.children, lengths, start, inherited)?,
                        Placement::Inline => {
                            plan_inline(&node.children, lengths, start, inherited)?
                        }
                    }
                } else {
                    let assignment = EmptyAssignment {
                        offset: start,
                        affinity: Affinity::Downstream,
                    };
                    let mut events = node
                        .children
                        .iter()
                        .copied()
                        .map(|child| EmitEvent::Suppress(child, assignment))
                        .collect::<Vec<_>>();
                    events.push(EmitEvent::Leaf(id));
                    events
                }
            }
            _ => vec![EmitEvent::Leaf(id)],
        },
        LogicalTextRule::TextLiteral
        | LogicalTextRule::HardBreak
        | LogicalTextRule::ObjectReplacement => vec![EmitEvent::Leaf(id)],
        LogicalTextRule::BreakOpportunity | LogicalTextRule::Empty => {
            unreachable!("zero-length nodes are assigned before planning")
        }
    })
}

fn plan_inline(
    children: &[NodeId],
    lengths: &[usize],
    start: usize,
    inherited: EmptyAssignment,
) -> Result<Vec<EmitEvent>, BuildError> {
    let mut starts = Vec::with_capacity(children.len());
    let mut cursor = start;
    for child in children {
        starts.push(cursor);
        cursor = cursor
            .checked_add(lengths[child.index()])
            .ok_or(BuildError::CanonicalTextTooLarge)?;
    }
    let assignments = ordinary_assignments(children, lengths, &starts, inherited, false);
    Ok(children
        .iter()
        .copied()
        .zip(assignments)
        .map(|(child, assignment)| EmitEvent::Enter(child, assignment))
        .collect())
}

fn plan_flow(
    children: &[NodeId],
    lengths: &[usize],
    start: usize,
    inherited: EmptyAssignment,
) -> Result<Vec<EmitEvent>, BuildError> {
    let mut starts = vec![start; children.len()];
    let mut cursor = start;
    let mut contributing = false;
    for (index, child) in children.iter().copied().enumerate() {
        let length = lengths[child.index()];
        if length == 0 {
            continue;
        }
        if contributing {
            cursor = cursor
                .checked_add(BLOCK_SEPARATOR.len())
                .ok_or(BuildError::CanonicalTextTooLarge)?;
        }
        starts[index] = cursor;
        cursor = cursor
            .checked_add(length)
            .ok_or(BuildError::CanonicalTextTooLarge)?;
        contributing = true;
    }
    let assignments = ordinary_assignments(children, lengths, &starts, inherited, false);
    let mut events = Vec::with_capacity(children.len() * 2);
    let mut emitted = false;
    for ((child, assignment), length) in children
        .iter()
        .copied()
        .zip(assignments)
        .zip(children.iter().map(|child| lengths[child.index()]))
    {
        if length > 0 && emitted {
            events.push(EmitEvent::Static(BLOCK_SEPARATOR));
        }
        events.push(EmitEvent::Enter(child, assignment));
        emitted |= length > 0;
    }
    Ok(events)
}

fn plan_structural(
    children: &[NodeId],
    lengths: &[usize],
    start: usize,
    inherited: EmptyAssignment,
    separator: &'static str,
) -> Result<Vec<EmitEvent>, BuildError> {
    let mut starts = Vec::with_capacity(children.len());
    let mut cursor = start;
    for (index, child) in children.iter().copied().enumerate() {
        if index > 0 {
            cursor = cursor
                .checked_add(separator.len())
                .ok_or(BuildError::CanonicalTextTooLarge)?;
        }
        starts.push(cursor);
        cursor = cursor
            .checked_add(lengths[child.index()])
            .ok_or(BuildError::CanonicalTextTooLarge)?;
    }
    let assignments = ordinary_assignments(children, lengths, &starts, inherited, true);
    let mut events = Vec::with_capacity(children.len() * 2);
    for (index, (child, assignment)) in children.iter().copied().zip(assignments).enumerate() {
        if index > 0 {
            events.push(EmitEvent::Static(separator));
        }
        events.push(EmitEvent::Enter(child, assignment));
    }
    Ok(events)
}

fn plan_table(
    nodes: &[DraftNode],
    table: &DraftNode,
    lengths: &[usize],
    start: usize,
    inherited: EmptyAssignment,
) -> Result<Vec<EmitEvent>, BuildError> {
    let caption = table
        .children
        .iter()
        .copied()
        .find(|child| matches!(nodes[child.index()].kind, NodeKind::TableCaption));
    let caption_length = caption.map_or(0, |child| lengths[child.index()]);
    let groups = table
        .children
        .iter()
        .copied()
        .filter(|child| matches!(nodes[child.index()].kind, NodeKind::RowGroup(_)))
        .collect::<Vec<_>>();
    let grid_length = measure_structural(groups.iter().map(|child| lengths[child.index()]))?;
    let group_start = start
        .checked_add(caption_length)
        .and_then(|offset| {
            offset.checked_add(
                usize::from(caption_length > 0 && grid_length > 0) * BLOCK_SEPARATOR.len(),
            )
        })
        .ok_or(BuildError::CanonicalTextTooLarge)?;

    let mut group_starts = Vec::with_capacity(groups.len());
    let mut cursor = group_start;
    for (index, group) in groups.iter().copied().enumerate() {
        if index > 0 {
            cursor = cursor
                .checked_add(ROW_SEPARATOR.len_utf8())
                .ok_or(BuildError::CanonicalTextTooLarge)?;
        }
        group_starts.push(cursor);
        cursor = cursor
            .checked_add(lengths[group.index()])
            .ok_or(BuildError::CanonicalTextTooLarge)?;
    }
    let group_inherited = if caption_length > 0 {
        EmptyAssignment {
            offset: group_start,
            affinity: Affinity::Upstream,
        }
    } else {
        inherited
    };
    let group_assignments =
        ordinary_assignments(&groups, lengths, &group_starts, group_inherited, true);
    let column_assignment = if grid_length > 0 {
        EmptyAssignment {
            offset: group_start,
            affinity: Affinity::Downstream,
        }
    } else if caption_length > 0 {
        EmptyAssignment {
            offset: group_start,
            affinity: Affinity::Upstream,
        }
    } else {
        inherited
    };
    let caption_assignment = if caption_length == 0 && grid_length > 0 {
        EmptyAssignment {
            offset: group_start,
            affinity: Affinity::Downstream,
        }
    } else {
        inherited
    };

    let mut events = Vec::with_capacity(table.children.len() * 2);
    let mut group_index = 0;
    for child in &table.children {
        match &nodes[child.index()].kind {
            NodeKind::TableCaption => {
                events.push(EmitEvent::Enter(*child, caption_assignment));
            }
            NodeKind::ColumnGroup(_) => {
                events.push(EmitEvent::Enter(*child, column_assignment));
            }
            NodeKind::RowGroup(_) => {
                if group_index == 0 && caption_length > 0 && grid_length > 0 {
                    events.push(EmitEvent::Static(BLOCK_SEPARATOR));
                } else if group_index > 0 {
                    events.push(EmitEvent::Static("\n"));
                }
                events.push(EmitEvent::Enter(*child, group_assignments[group_index]));
                group_index += 1;
            }
            _ => unreachable!("validated table children are exhaustive"),
        }
    }
    Ok(events)
}

fn ordinary_assignments(
    children: &[NodeId],
    lengths: &[usize],
    starts: &[usize],
    inherited: EmptyAssignment,
    structural_slots: bool,
) -> Vec<EmptyAssignment> {
    let mut next_contribution = None;
    let mut next_offsets = vec![None; children.len()];
    for (index, child) in children.iter().copied().enumerate().rev() {
        next_offsets[index] = next_contribution;
        if lengths[child.index()] > 0 {
            next_contribution = Some(starts[index]);
        }
    }

    let mut previous_end = None;
    let mut assignments = Vec::with_capacity(children.len());
    for (index, child) in children.iter().copied().enumerate() {
        let length = lengths[child.index()];
        let assignment = if length > 0 {
            EmptyAssignment {
                offset: starts[index],
                affinity: Affinity::Downstream,
            }
        } else if structural_slots {
            let affinity = if next_offsets[index].is_some() {
                Affinity::Downstream
            } else if previous_end.is_some() {
                Affinity::Upstream
            } else {
                inherited.affinity
            };
            EmptyAssignment {
                offset: starts[index],
                affinity,
            }
        } else if let Some(next) = next_offsets[index] {
            EmptyAssignment {
                offset: next,
                affinity: Affinity::Downstream,
            }
        } else if let Some(previous) = previous_end {
            EmptyAssignment {
                offset: previous,
                affinity: Affinity::Upstream,
            }
        } else {
            inherited
        };
        assignments.push(assignment);
        if length > 0 {
            previous_end = Some(starts[index] + length);
        }
    }
    assignments
}

fn assign_empty_subtree(
    nodes: &[DraftNode],
    root: NodeId,
    assignment: EmptyAssignment,
    ranges: &mut [TextRange],
    affinities: &mut [Affinity],
) {
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        ranges[id.index()] = range(assignment.offset, assignment.offset);
        affinities[id.index()] = assignment.affinity;
        stack.extend(nodes[id.index()].children.iter().rev().copied());
    }
}

fn emit_leaf(nodes: &[DraftNode], id: NodeId, output: &mut String) {
    let node = &nodes[id.index()];
    match &node.kind {
        NodeKind::Text => output.push_str(node.literal.as_deref().unwrap_or_default()),
        NodeKind::HardBreak => output.push('\n'),
        NodeKind::Image(image) => match &image.alternative {
            ImageAlternative::Text(text) => output.push_str(text.as_str()),
            ImageAlternative::Decorative => {}
            ImageAlternative::Missing => output.push(OBJECT_REPLACEMENT),
        },
        NodeKind::Math(math) => output.push_str(&math.source),
        NodeKind::UnsupportedObject(object) => {
            if let Some(title) = object.title.as_deref().filter(|title| !title.is_empty()) {
                output.push_str(title);
            } else {
                output.push(OBJECT_REPLACEMENT);
            }
        }
        NodeKind::ThematicBreak => output.push(OBJECT_REPLACEMENT),
        _ => unreachable!("only leaf-emitting kinds are scheduled"),
    }
}

fn range(start: usize, end: usize) -> TextRange {
    TextRange::new(TextOffset::new(start), TextOffset::new(end))
}
