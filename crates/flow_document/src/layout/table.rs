//! Format-neutral intrinsic table track sizing.
//!
//! This module implements the content-sizing core independently from flow
//! traversal and cell layout. Its ordering follows CSS automatic table layout:
//! single-track contributions establish the columns, then spanning cells are
//! considered in increasing span order. Equal-span cells are evaluated against
//! the same prior measurements so source order cannot affect the result.

#[derive(Clone, Copy, Debug, PartialEq)]
#[allow(dead_code)] // Populated by the staged XHTML/EPUB CSS style inputs.
pub(super) enum TrackSizing {
    Auto,
    Length(f32),
    Percentage(f32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TrackConstraint {
    pub sizing: TrackSizing,
}

impl Default for TrackConstraint {
    fn default() -> Self {
        Self {
            sizing: TrackSizing::Auto,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CellMeasure {
    pub start: usize,
    pub span: usize,
    pub min_content: f32,
    pub max_content: f32,
    pub emergency_min: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct TableSizingInput {
    pub tracks: Vec<TrackConstraint>,
    pub cells: Vec<CellMeasure>,
    pub caption_min: f32,
    pub caption_emergency_min: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ColumnMeasure {
    pub min_content: f32,
    pub max_content: f32,
    pub emergency_min: f32,
    pub sizing: TrackSizing,
    pub constrained: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct IntrinsicGrid {
    pub columns: Vec<ColumnMeasure>,
    pub grid_min: f32,
    pub grid_max: f32,
    pub emergency_min: f32,
    pub caption_min: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TableSizingError {
    EmptyGrid,
    InvalidTrackConstraint,
    InvalidCellRange,
    InvalidCellMeasure,
    InvalidCaptionMeasure,
    InvalidAvailableWidth,
}

fn valid_measure(value: f32) -> bool {
    value.is_finite() && value >= 0.0
}

fn validate(input: &TableSizingInput) -> Result<(), TableSizingError> {
    if input.tracks.is_empty() {
        return Err(TableSizingError::EmptyGrid);
    }
    if !valid_measure(input.caption_min)
        || !valid_measure(input.caption_emergency_min)
        || input.caption_emergency_min > input.caption_min
    {
        return Err(TableSizingError::InvalidCaptionMeasure);
    }
    for track in &input.tracks {
        match track.sizing {
            TrackSizing::Auto => {}
            TrackSizing::Length(value) if valid_measure(value) => {}
            TrackSizing::Percentage(value) if value.is_finite() && value >= 0.0 => {}
            TrackSizing::Length(_) | TrackSizing::Percentage(_) => {
                return Err(TableSizingError::InvalidTrackConstraint);
            }
        }
    }
    for cell in &input.cells {
        if cell.span == 0
            || cell.start >= input.tracks.len()
            || cell
                .start
                .checked_add(cell.span)
                .is_none_or(|end| end > input.tracks.len())
        {
            return Err(TableSizingError::InvalidCellRange);
        }
        if !valid_measure(cell.min_content)
            || !valid_measure(cell.max_content)
            || !valid_measure(cell.emergency_min)
            || cell.max_content < cell.min_content
            || cell.min_content < cell.emergency_min
        {
            return Err(TableSizingError::InvalidCellMeasure);
        }
    }
    Ok(())
}

pub(super) fn compute_intrinsic_grid(
    input: &TableSizingInput,
) -> Result<IntrinsicGrid, TableSizingError> {
    validate(input)?;

    let mut columns = input
        .tracks
        .iter()
        .map(|track| {
            let (minimum, constrained) = match track.sizing {
                TrackSizing::Auto | TrackSizing::Percentage(_) => (0.0, false),
                TrackSizing::Length(value) => (value, true),
            };
            ColumnMeasure {
                min_content: minimum,
                max_content: minimum,
                emergency_min: minimum,
                sizing: track.sizing,
                constrained,
            }
        })
        .collect::<Vec<_>>();

    for cell in input.cells.iter().filter(|cell| cell.span == 1) {
        let column = &mut columns[cell.start];
        column.min_content = column.min_content.max(cell.min_content);
        column.max_content = column.max_content.max(cell.max_content);
        column.emergency_min = column.emergency_min.max(cell.emergency_min);
    }

    let maximum_span = input.cells.iter().map(|cell| cell.span).max().unwrap_or(1);
    for span in 2..=maximum_span {
        let previous = columns.clone();
        let mut next = previous.clone();
        for cell in input.cells.iter().filter(|cell| cell.span == span) {
            let range = cell.start..cell.start + cell.span;
            let prior = &previous[range.clone()];
            let min_contributions =
                distribute_spanning_minimum(prior, cell.min_content, |column| column.min_content);
            let emergency_contributions = distribute_spanning_emergency_minimum(
                prior,
                cell.emergency_min,
                &min_contributions,
            );
            let max_contributions = distribute_spanning_maximum(prior, cell.max_content);
            for (offset, column) in next[range].iter_mut().enumerate() {
                column.min_content = column.min_content.max(min_contributions[offset]);
                column.max_content = column.max_content.max(max_contributions[offset]);
                column.emergency_min = column.emergency_min.max(emergency_contributions[offset]);
            }
        }
        columns = next;
    }

    for column in &mut columns {
        column.max_content = column.max_content.max(column.min_content);
        debug_assert!(column.emergency_min <= column.min_content);
    }
    let grid_min = columns.iter().map(|column| column.min_content).sum();
    let grid_max = columns.iter().map(|column| column.max_content).sum();
    let emergency_min = columns
        .iter()
        .map(|column| column.emergency_min)
        .sum::<f32>()
        .max(input.caption_emergency_min);
    Ok(IntrinsicGrid {
        columns,
        grid_min,
        grid_max,
        emergency_min,
        caption_min: input.caption_min,
    })
}

fn distribute_spanning_minimum(
    columns: &[ColumnMeasure],
    required: f32,
    minimum: impl Fn(&ColumnMeasure) -> f32,
) -> Vec<f32> {
    let mut result = columns.iter().map(&minimum).collect::<Vec<_>>();
    let baseline_min = result.iter().sum::<f32>();
    if required <= baseline_min {
        return result;
    }

    // First grow through each column's existing min-to-max flexibility.
    let flexibility = columns
        .iter()
        .zip(&result)
        .map(|(column, minimum)| (column.max_content - minimum).max(0.0))
        .collect::<Vec<_>>();
    let flexible_total = flexibility.iter().sum::<f32>();
    let flexible_growth = (required - baseline_min).min(flexible_total);
    distribute_proportionally(&mut result, flexible_growth, &flexibility);

    // Content wider than the span's prior max-content sum grows columns in
    // proportion to their prior max-content widths, or equally if all are zero.
    let remaining = required - result.iter().sum::<f32>();
    if remaining > 0.0 {
        let weights = columns
            .iter()
            .map(|column| column.max_content)
            .collect::<Vec<_>>();
        distribute_proportionally(&mut result, remaining, &weights);
    }
    result
}

fn distribute_spanning_emergency_minimum(
    columns: &[ColumnMeasure],
    required: f32,
    min_content_upper_bounds: &[f32],
) -> Vec<f32> {
    let mut result = columns
        .iter()
        .map(|column| column.emergency_min)
        .collect::<Vec<_>>();
    let extra = required - result.iter().sum::<f32>();
    if extra > 0.0 {
        let capacity = result
            .iter()
            .zip(min_content_upper_bounds)
            .map(|(emergency, minimum)| (minimum - emergency).max(0.0))
            .collect::<Vec<_>>();
        let capacity_total = capacity.iter().sum::<f32>();
        let rounding_tolerance =
            f32::EPSILON * 8.0 * capacity_total.abs().max(extra.abs()).max(1.0);
        debug_assert!(capacity_total + rounding_tolerance >= extra);
        distribute_proportionally(&mut result, extra, &capacity);
        for (emergency, minimum) in result.iter_mut().zip(min_content_upper_bounds) {
            *emergency = emergency.min(*minimum);
        }
    }
    result
}

fn distribute_spanning_maximum(columns: &[ColumnMeasure], required: f32) -> Vec<f32> {
    let mut result = columns
        .iter()
        .map(|column| column.max_content)
        .collect::<Vec<_>>();
    let extra = required - result.iter().sum::<f32>();
    if extra > 0.0 {
        let weights = result.clone();
        distribute_proportionally(&mut result, extra, &weights);
    }
    result
}

fn distribute_proportionally(values: &mut [f32], extra: f32, weights: &[f32]) {
    if values.is_empty() || extra <= 0.0 {
        return;
    }
    let weight = weights.iter().map(|weight| weight.max(0.0)).sum::<f32>();
    if weight > 0.0 {
        for (value, item_weight) in values.iter_mut().zip(weights) {
            *value += extra * item_weight.max(0.0) / weight;
        }
    } else {
        let share = extra / values.len() as f32;
        for value in values {
            *value += share;
        }
    }
}

pub(super) fn distribute_span_minima(
    count: usize,
    base: f32,
    constraints: &[(usize, usize, f32)],
) -> Result<Vec<f32>, TableSizingError> {
    if count == 0 {
        return Err(TableSizingError::EmptyGrid);
    }
    if !valid_measure(base) {
        return Err(TableSizingError::InvalidCellMeasure);
    }
    for (start, span, required) in constraints {
        if *span == 0 || *start >= count || start.checked_add(*span).is_none_or(|end| end > count) {
            return Err(TableSizingError::InvalidCellRange);
        }
        if !valid_measure(*required) {
            return Err(TableSizingError::InvalidCellMeasure);
        }
    }

    let mut values = vec![base; count];
    for (start, _, required) in constraints.iter().filter(|(_, span, _)| *span == 1) {
        values[*start] = values[*start].max(*required);
    }
    let maximum_span = constraints
        .iter()
        .map(|(_, span, _)| *span)
        .max()
        .unwrap_or(1);
    for span in 2..=maximum_span {
        let previous = values.clone();
        let mut next = previous.clone();
        for (start, _, required) in constraints
            .iter()
            .filter(|(_, constraint_span, _)| *constraint_span == span)
        {
            let range = *start..*start + span;
            let prior = &previous[range.clone()];
            let current = prior.iter().sum::<f32>();
            if *required <= current {
                continue;
            }
            let mut candidate = prior.to_vec();
            distribute_proportionally(&mut candidate, *required - current, prior);
            for (value, candidate) in next[range].iter_mut().zip(candidate) {
                *value = value.max(candidate);
            }
        }
        values = next;
    }
    Ok(values)
}

pub(super) fn distribute_emergency_width(
    grid: &IntrinsicGrid,
    assignable: f32,
) -> Result<Vec<f32>, TableSizingError> {
    if !valid_measure(assignable) {
        return Err(TableSizingError::InvalidAvailableWidth);
    }
    let target = assignable.max(grid.emergency_min);
    if target >= grid.grid_min {
        return distribute_assignable_width(grid, target);
    }

    let mut widths = grid
        .columns
        .iter()
        .map(|column| column.emergency_min)
        .collect::<Vec<_>>();
    let current = widths.iter().sum::<f32>();
    let flexibility = grid
        .columns
        .iter()
        .zip(&widths)
        .map(|(column, width)| (column.min_content - width).max(0.0))
        .collect::<Vec<_>>();
    distribute_proportionally(&mut widths, target - current, &flexibility);
    Ok(widths)
}

pub(super) fn automatic_table_width(
    grid: &IntrinsicGrid,
    available: f32,
) -> Result<f32, TableSizingError> {
    if !valid_measure(available) {
        return Err(TableSizingError::InvalidAvailableWidth);
    }
    Ok(grid
        .grid_min
        .max(grid.caption_min)
        .max(grid.grid_max.min(available)))
}

pub(super) fn distribute_assignable_width(
    grid: &IntrinsicGrid,
    assignable: f32,
) -> Result<Vec<f32>, TableSizingError> {
    if !valid_measure(assignable) {
        return Err(TableSizingError::InvalidAvailableWidth);
    }
    let min_guess = grid
        .columns
        .iter()
        .map(|column| column.min_content)
        .collect::<Vec<_>>();
    let min_percentage_guess = grid
        .columns
        .iter()
        .map(|column| match column.sizing {
            TrackSizing::Percentage(percentage) => column.min_content.max(assignable * percentage),
            TrackSizing::Auto | TrackSizing::Length(_) => column.min_content,
        })
        .collect::<Vec<_>>();
    let min_specified_guess = grid
        .columns
        .iter()
        .zip(&min_percentage_guess)
        .map(|(column, percentage)| match column.sizing {
            TrackSizing::Length(_) => column.max_content,
            TrackSizing::Auto | TrackSizing::Percentage(_) => *percentage,
        })
        .collect::<Vec<_>>();
    let max_guess = grid
        .columns
        .iter()
        .zip(&min_percentage_guess)
        .map(|(column, percentage)| match column.sizing {
            TrackSizing::Percentage(_) => *percentage,
            TrackSizing::Auto | TrackSizing::Length(_) => column.max_content,
        })
        .collect::<Vec<_>>();
    let guesses = [
        min_guess,
        min_percentage_guess,
        min_specified_guess,
        max_guess,
    ];

    if assignable <= guesses[0].iter().sum::<f32>() {
        return Ok(guesses[0].clone());
    }
    for pair in guesses.windows(2) {
        let lower_total = pair[0].iter().sum::<f32>();
        let upper_total = pair[1].iter().sum::<f32>();
        if assignable <= upper_total && upper_total > lower_total {
            let ratio = (assignable - lower_total) / (upper_total - lower_total);
            return Ok(pair[0]
                .iter()
                .zip(&pair[1])
                .map(|(lower, upper)| lower + (upper - lower) * ratio)
                .collect());
        }
    }

    let mut result = guesses[3].clone();
    let extra = assignable - result.iter().sum::<f32>();
    if extra > 0.0 {
        let auto_weights = grid
            .columns
            .iter()
            .map(|column| match column.sizing {
                TrackSizing::Auto if !column.constrained => column.max_content,
                TrackSizing::Auto | TrackSizing::Length(_) | TrackSizing::Percentage(_) => 0.0,
            })
            .collect::<Vec<_>>();
        if auto_weights.iter().any(|weight| *weight > 0.0) {
            distribute_proportionally(&mut result, extra, &auto_weights);
        } else {
            let weights = grid
                .columns
                .iter()
                .map(|column| column.max_content)
                .collect::<Vec<_>>();
            distribute_proportionally(&mut result, extra, &weights);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auto_tracks(count: usize) -> Vec<TrackConstraint> {
        vec![TrackConstraint::default(); count]
    }

    fn cell(start: usize, span: usize, min: f32, max: f32) -> CellMeasure {
        CellMeasure {
            start,
            span,
            min_content: min,
            max_content: max,
            emergency_min: min.min(4.0),
        }
    }

    fn assert_close(actual: f32, expected: f32) {
        assert!((actual - expected).abs() < 0.001, "{actual} != {expected}");
    }

    #[test]
    fn rejects_invalid_tracks_cells_and_measures() {
        let empty = TableSizingInput {
            tracks: Vec::new(),
            cells: Vec::new(),
            caption_min: 0.0,
            caption_emergency_min: 0.0,
        };
        assert_eq!(
            compute_intrinsic_grid(&empty),
            Err(TableSizingError::EmptyGrid)
        );

        let mut input = TableSizingInput {
            tracks: auto_tracks(2),
            cells: vec![cell(1, 2, 1.0, 2.0)],
            caption_min: 0.0,
            caption_emergency_min: 0.0,
        };
        assert_eq!(
            compute_intrinsic_grid(&input),
            Err(TableSizingError::InvalidCellRange)
        );
        input.cells = vec![cell(0, 1, 3.0, 2.0)];
        assert_eq!(
            compute_intrinsic_grid(&input),
            Err(TableSizingError::InvalidCellMeasure)
        );
        input.cells.clear();
        input.tracks[0].sizing = TrackSizing::Percentage(f32::NAN);
        assert_eq!(
            compute_intrinsic_grid(&input),
            Err(TableSizingError::InvalidTrackConstraint)
        );
    }

    #[test]
    fn single_span_cells_establish_intrinsic_track_measures() {
        let grid = compute_intrinsic_grid(&TableSizingInput {
            tracks: auto_tracks(2),
            cells: vec![
                CellMeasure {
                    start: 0,
                    span: 1,
                    min_content: 20.0,
                    max_content: 50.0,
                    emergency_min: 5.0,
                },
                CellMeasure {
                    start: 0,
                    span: 1,
                    min_content: 30.0,
                    max_content: 40.0,
                    emergency_min: 8.0,
                },
                CellMeasure {
                    start: 1,
                    span: 1,
                    min_content: 10.0,
                    max_content: 20.0,
                    emergency_min: 4.0,
                },
            ],
            caption_min: 12.0,
            caption_emergency_min: 3.0,
        })
        .unwrap();

        assert_eq!(
            grid.columns
                .iter()
                .map(|column| (column.min_content, column.max_content, column.emergency_min))
                .collect::<Vec<_>>(),
            vec![(30.0, 50.0, 8.0), (10.0, 20.0, 4.0)]
        );
        assert_eq!(
            (grid.grid_min, grid.grid_max, grid.emergency_min),
            (40.0, 70.0, 12.0)
        );
    }

    #[test]
    fn spanning_cells_are_processed_by_span_and_not_source_order() {
        let base = vec![
            cell(0, 1, 10.0, 20.0),
            cell(1, 1, 10.0, 30.0),
            cell(2, 1, 10.0, 10.0),
        ];
        let spans = [
            cell(0, 2, 50.0, 80.0),
            cell(1, 2, 45.0, 90.0),
            cell(0, 3, 100.0, 180.0),
        ];
        let mut forward = base.clone();
        forward.extend(spans.iter().copied());
        let mut reverse = base;
        reverse.extend(spans.iter().rev().copied());

        let first = compute_intrinsic_grid(&TableSizingInput {
            tracks: auto_tracks(3),
            cells: forward,
            caption_min: 0.0,
            caption_emergency_min: 0.0,
        })
        .unwrap();
        let second = compute_intrinsic_grid(&TableSizingInput {
            tracks: auto_tracks(3),
            cells: reverse,
            caption_min: 0.0,
            caption_emergency_min: 0.0,
        })
        .unwrap();

        assert_eq!(first, second);
        assert_close(first.grid_min, 100.0);
        assert_close(first.grid_max, 180.0);
        assert!(
            first
                .columns
                .iter()
                .all(|column| column.max_content >= column.min_content)
        );
    }

    #[test]
    fn spanning_emergency_constraints_cannot_inflate_grid_minimum() {
        let grid = compute_intrinsic_grid(&TableSizingInput {
            tracks: auto_tracks(2),
            cells: vec![
                CellMeasure {
                    start: 0,
                    span: 1,
                    min_content: 50.0,
                    max_content: 50.0,
                    emergency_min: 5.0,
                },
                CellMeasure {
                    start: 1,
                    span: 1,
                    min_content: 10.0,
                    max_content: 200.0,
                    emergency_min: 5.0,
                },
                CellMeasure {
                    start: 0,
                    span: 2,
                    min_content: 60.0,
                    max_content: 60.0,
                    emergency_min: 60.0,
                },
            ],
            caption_min: 0.0,
            caption_emergency_min: 0.0,
        })
        .unwrap();

        assert_eq!(grid.grid_min, 60.0);
        assert_eq!(grid.emergency_min, 60.0);
        assert!(
            grid.columns
                .iter()
                .all(|column| column.emergency_min <= column.min_content)
        );
    }

    #[test]
    fn row_span_minima_are_deterministic_and_processed_by_span() {
        let constraints = [(0, 1, 10.0), (1, 2, 40.0), (0, 2, 50.0), (0, 3, 90.0)];
        let forward = distribute_span_minima(3, 5.0, &constraints).unwrap();
        let reverse = distribute_span_minima(
            3,
            5.0,
            &constraints.iter().rev().copied().collect::<Vec<_>>(),
        )
        .unwrap();

        assert_eq!(forward, reverse);
        assert_close(forward.iter().sum(), 90.0);
        assert!(forward[0] >= 10.0);
        assert!(forward[1] + forward[2] >= 40.0);
        assert!(forward[0] + forward[1] >= 50.0);
    }

    #[test]
    fn auto_width_shrink_wraps_between_grid_min_and_grid_max() {
        let grid = compute_intrinsic_grid(&TableSizingInput {
            tracks: auto_tracks(2),
            cells: vec![cell(0, 1, 20.0, 60.0), cell(1, 1, 30.0, 40.0)],
            caption_min: 0.0,
            caption_emergency_min: 0.0,
        })
        .unwrap();

        assert_eq!(automatic_table_width(&grid, 40.0).unwrap(), 50.0);
        assert_eq!(automatic_table_width(&grid, 80.0).unwrap(), 80.0);
        assert_eq!(automatic_table_width(&grid, 120.0).unwrap(), 100.0);
        let widths = distribute_assignable_width(&grid, 80.0).unwrap();
        assert_close(widths[0], 44.0);
        assert_close(widths[1], 36.0);
    }

    #[test]
    fn caption_minimum_can_expand_the_table_beyond_grid_max() {
        let grid = compute_intrinsic_grid(&TableSizingInput {
            tracks: auto_tracks(2),
            cells: vec![cell(0, 1, 10.0, 20.0), cell(1, 1, 10.0, 20.0)],
            caption_min: 60.0,
            caption_emergency_min: 10.0,
        })
        .unwrap();
        let used = automatic_table_width(&grid, 100.0).unwrap();
        assert_eq!(used, 60.0);
        assert_eq!(
            distribute_assignable_width(&grid, used).unwrap(),
            vec![30.0, 30.0]
        );
    }

    #[test]
    fn fixed_and_percentage_tracks_participate_in_sizing_guesses() {
        let grid = compute_intrinsic_grid(&TableSizingInput {
            tracks: vec![
                TrackConstraint {
                    sizing: TrackSizing::Percentage(0.5),
                },
                TrackConstraint {
                    sizing: TrackSizing::Length(30.0),
                },
                TrackConstraint::default(),
            ],
            cells: vec![
                cell(0, 1, 10.0, 20.0),
                cell(1, 1, 5.0, 40.0),
                cell(2, 1, 10.0, 30.0),
            ],
            caption_min: 0.0,
            caption_emergency_min: 0.0,
        })
        .unwrap();

        let widths = distribute_assignable_width(&grid, 120.0).unwrap();
        assert_close(widths.iter().sum(), 120.0);
        assert!(widths[0] >= 60.0);
        assert!(widths[1] >= 30.0);
    }

    #[test]
    fn emergency_distribution_respects_its_own_floor() {
        let grid = compute_intrinsic_grid(&TableSizingInput {
            tracks: auto_tracks(2),
            cells: vec![
                CellMeasure {
                    start: 0,
                    span: 1,
                    min_content: 80.0,
                    max_content: 100.0,
                    emergency_min: 5.0,
                },
                CellMeasure {
                    start: 1,
                    span: 1,
                    min_content: 40.0,
                    max_content: 60.0,
                    emergency_min: 5.0,
                },
            ],
            caption_min: 90.0,
            caption_emergency_min: 20.0,
        })
        .unwrap();

        let fitted = distribute_emergency_width(&grid, 50.0).unwrap();
        assert_close(fitted.iter().sum(), 50.0);
        assert!(fitted[0] >= 5.0 && fitted[1] >= 5.0);

        let caption_limited = distribute_emergency_width(&grid, 10.0).unwrap();
        assert_close(caption_limited.iter().sum(), 20.0);
        assert!(caption_limited[0] >= 5.0 && caption_limited[1] >= 5.0);
    }

    #[test]
    fn emergency_measurements_do_not_redefine_grid_minimum() {
        let grid = compute_intrinsic_grid(&TableSizingInput {
            tracks: auto_tracks(2),
            cells: vec![
                CellMeasure {
                    start: 0,
                    span: 1,
                    min_content: 80.0,
                    max_content: 100.0,
                    emergency_min: 5.0,
                },
                CellMeasure {
                    start: 1,
                    span: 1,
                    min_content: 40.0,
                    max_content: 60.0,
                    emergency_min: 5.0,
                },
            ],
            caption_min: 0.0,
            caption_emergency_min: 0.0,
        })
        .unwrap();

        assert_eq!(grid.grid_min, 120.0);
        assert_eq!(grid.emergency_min, 10.0);
        assert_eq!(automatic_table_width(&grid, 50.0).unwrap(), 120.0);
        assert_eq!(
            distribute_assignable_width(&grid, 50.0).unwrap(),
            vec![80.0, 40.0]
        );
    }
}
