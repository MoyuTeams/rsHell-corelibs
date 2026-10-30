use alacritty_terminal::{
    Term,
    event::EventListener,
    grid::{Dimensions, Grid},
    index::Line,
    term::{TermMode, cell::Cell},
};

use crate::alacritty_primary_rows::PrimaryRows;

pub(crate) fn apply<T: EventListener>(
    terminal: &mut Term<T>,
    history_limit: usize,
    primary_rows: &mut PrimaryRows,
    maximum_shift: usize,
    track_capacity: bool,
    apply: impl FnOnce(&mut Term<T>),
) {
    let was_primary = !terminal.mode().contains(TermMode::ALT_SCREEN);
    let old_history = terminal.grid().history_size();
    // A bounded operation may cross capacity in one call. Keep its shift bound
    // even when history growth alone cannot account for the completed scroll.
    let capacity_anchor = was_primary
        .then(|| capture(terminal, maximum_shift))
        .flatten();
    apply(terminal);
    let active_primary = !terminal.mode().contains(TermMode::ALT_SCREEN);
    if active_primary {
        let history = terminal.grid().history_size();
        if was_primary {
            let completed =
                if old_history == history_limit || (!track_capacity && history == history_limit) {
                    completed_shift(terminal, history_limit, capacity_anchor)
                } else {
                    0
                };
            let shift = history
                .saturating_sub(old_history)
                .saturating_add(completed);
            primary_rows.origin = primary_rows.origin.saturating_add(shift as i64);
        } else if history >= primary_rows.history {
            primary_rows.origin = primary_rows
                .origin
                .saturating_add(i64::try_from(history - primary_rows.history).unwrap_or(i64::MAX));
        } else {
            primary_rows.origin = primary_rows
                .origin
                .saturating_sub(i64::try_from(primary_rows.history - history).unwrap_or(i64::MAX));
        }
        primary_rows.history = history;
    } else if was_primary {
        primary_rows.history = old_history;
    }
}

pub(crate) struct CapacityAnchor {
    start: usize,
    maximum_shift: usize,
    identity: usize,
    oldest_history_identity: Option<usize>,
}

/// Maximum eviction count that leaves the chosen row retained in the grid.
pub(crate) fn window_capacity<T: EventListener>(terminal: &Term<T>) -> usize {
    let grid = terminal.grid();
    match grid.history_size() {
        0 => grid.total_lines().saturating_sub(1),
        1 => 1,
        history => history - 1,
    }
}

pub(crate) fn capture<T: EventListener>(
    terminal: &Term<T>,
    maximum_shift: usize,
) -> Option<CapacityAnchor> {
    let grid = terminal.grid();
    if grid.total_lines() == 0 {
        return None;
    }
    // Prefer the newest history row: unlike bottom screen rows, it moves even
    // when a top-anchored short region leaves most of the screen fixed. With
    // only one history row, the first screen row is the next retainable anchor.
    let start = window_capacity(terminal);
    Some(CapacityAnchor {
        start,
        maximum_shift: maximum_shift.min(start),
        identity: row_identity(grid, start),
        oldest_history_identity: (grid.history_size() != 0).then(|| row_identity(grid, 0)),
    })
}

pub(crate) fn completed_shift<T: EventListener>(
    terminal: &Term<T>,
    history_limit: usize,
    anchor: Option<CapacityAnchor>,
) -> usize {
    let Some(anchor) = anchor else {
        return 0;
    };
    let grid = terminal.grid();
    if grid.history_size() != history_limit {
        return 0;
    }
    let history_moved = anchor
        .oldest_history_identity
        .is_some_and(|identity| row_identity(grid, 0) != identity);
    let lower = anchor.start.saturating_sub(anchor.maximum_shift);
    for candidate in (lower..=anchor.start).rev() {
        // A screen fallback can return to its old position after local edits;
        // it cannot prove zero eviction if the oldest history row changed.
        if row_identity(grid, candidate) == anchor.identity
            && (candidate != anchor.start || !history_moved)
        {
            return anchor.start - candidate;
        }
    }
    if anchor.oldest_history_identity.is_some() && !history_moved {
        return 0;
    }

    // A bounded window cannot evict the anchored row and reuse its allocation.
    // Addresses are copied before feeding and compared only in this window. If no
    // retained row is observable, reserve a disjoint stable range rather than reuse IDs.
    grid.total_lines()
}

fn row_identity(grid: &Grid<Cell>, offset: usize) -> usize {
    let history = grid.history_size();
    let line = Line(offset as i32 - history as i32);
    // The ring's Row storage can move while an operation grows history to its
    // limit. Each retained row's cell allocation survives that reallocation.
    grid[line][..].as_ptr() as usize
}
