//! Focus movement over rows of tiles of different lengths. Each row remembers
//! its own column, so moving down and back up returns to the same tile (the
//! behaviour people know from TV and console launchers).

/// A move request on the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridMove {
    Up,
    Down,
    Left,
    Right,
    /// Move left or right by a page of tiles (negative is left).
    Page(isize),
    /// First tile of the row.
    RowStart,
    /// Last tile of the row.
    RowEnd,
}

/// Focused position in rows of `lens` tiles. Empty rows are skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FocusGrid {
    row: usize,
    cols: Vec<usize>,
}

impl FocusGrid {
    /// Focused `(row, column)`, or `None` when every row is empty.
    pub fn focused(&self, lens: &[usize]) -> Option<(usize, usize)> {
        let len = *lens.get(self.row)?;
        (len > 0).then(|| (self.row, self.col(self.row).min(len - 1)))
    }

    fn col(&self, row: usize) -> usize {
        self.cols.get(row).copied().unwrap_or(0)
    }

    fn set_col(&mut self, row: usize, col: usize) {
        if self.cols.len() <= row {
            self.cols.resize(row + 1, 0);
        }
        self.cols[row] = col;
    }

    /// Re-validate after the rows changed: clamp columns and move off empty
    /// rows (to the nearest non-empty row below, else above).
    pub fn clamp(&mut self, lens: &[usize]) {
        self.cols.resize(lens.len(), 0);
        for (col, len) in self.cols.iter_mut().zip(lens) {
            *col = (*col).min(len.saturating_sub(1));
        }
        if lens.get(self.row).is_some_and(|l| *l > 0) {
            return;
        }
        let below = (self.row..lens.len()).find(|r| lens[*r] > 0);
        let above = (0..self.row.min(lens.len())).rev().find(|r| lens[*r] > 0);
        self.row = below.or(above).unwrap_or(0);
    }

    /// Focus `(row, col)` directly (pointer activation, restoring a game).
    pub fn set(&mut self, row: usize, col: usize, lens: &[usize]) {
        if lens.get(row).is_some_and(|l| *l > 0) {
            self.row = row;
            self.set_col(row, col.min(lens[row] - 1));
        }
    }

    /// Apply a move. Returns whether the focus changed.
    pub fn apply(&mut self, mv: GridMove, lens: &[usize]) -> bool {
        self.clamp(lens);
        let Some((row, col)) = self.focused(lens) else {
            return false;
        };
        let len = lens[row];
        let target = match mv {
            GridMove::Up => {
                return match (0..row).rev().find(|r| lens[*r] > 0) {
                    Some(r) => {
                        self.row = r;
                        true
                    }
                    None => false,
                };
            }
            GridMove::Down => {
                return match (row + 1..lens.len()).find(|r| lens[*r] > 0) {
                    Some(r) => {
                        self.row = r;
                        true
                    }
                    None => false,
                };
            }
            GridMove::Left => col.saturating_sub(1),
            GridMove::Right => (col + 1).min(len - 1),
            GridMove::Page(delta) => col.saturating_add_signed(delta).min(len - 1),
            GridMove::RowStart => 0,
            GridMove::RowEnd => len - 1,
        };
        self.set_col(row, target);
        target != col
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_within_a_row_without_wrapping() {
        let lens = [3];
        let mut g = FocusGrid::default();
        assert_eq!(g.focused(&lens), Some((0, 0)));
        assert!(!g.apply(GridMove::Left, &lens));
        assert!(g.apply(GridMove::Right, &lens));
        assert!(g.apply(GridMove::Right, &lens));
        assert!(!g.apply(GridMove::Right, &lens));
        assert_eq!(g.focused(&lens), Some((0, 2)));
    }

    #[test]
    fn rows_remember_their_column() {
        let lens = [10, 4, 10];
        let mut g = FocusGrid::default();
        for _ in 0..6 {
            g.apply(GridMove::Right, &lens);
        }
        assert!(g.apply(GridMove::Down, &lens));
        assert_eq!(
            g.focused(&lens),
            Some((1, 0)),
            "row 1 starts at its own column"
        );
        g.apply(GridMove::Right, &lens);
        assert!(g.apply(GridMove::Up, &lens));
        assert_eq!(g.focused(&lens), Some((0, 6)));
        g.apply(GridMove::Down, &lens);
        assert_eq!(g.focused(&lens), Some((1, 1)));
    }

    #[test]
    fn skips_empty_rows_and_stops_at_edges() {
        let lens = [2, 0, 5];
        let mut g = FocusGrid::default();
        assert!(g.apply(GridMove::Down, &lens));
        assert_eq!(g.focused(&lens), Some((2, 0)));
        assert!(!g.apply(GridMove::Down, &lens));
        assert!(g.apply(GridMove::Up, &lens));
        assert_eq!(g.focused(&lens), Some((0, 0)));
        assert!(!g.apply(GridMove::Up, &lens));
    }

    #[test]
    fn pages_and_row_ends_clamp() {
        let lens = [12];
        let mut g = FocusGrid::default();
        g.apply(GridMove::Page(5), &lens);
        assert_eq!(g.focused(&lens), Some((0, 5)));
        g.apply(GridMove::Page(-8), &lens);
        assert_eq!(g.focused(&lens), Some((0, 0)));
        g.apply(GridMove::RowEnd, &lens);
        assert_eq!(g.focused(&lens), Some((0, 11)));
        g.apply(GridMove::Page(5), &lens);
        assert_eq!(g.focused(&lens), Some((0, 11)));
        g.apply(GridMove::RowStart, &lens);
        assert_eq!(g.focused(&lens), Some((0, 0)));
    }

    #[test]
    fn clamps_when_rows_shrink_or_empty() {
        let mut g = FocusGrid::default();
        g.set(1, 7, &[3, 9]);
        assert_eq!(g.focused(&[3, 9]), Some((1, 7)));
        g.clamp(&[3, 2]);
        assert_eq!(g.focused(&[3, 2]), Some((1, 1)));
        g.clamp(&[3, 0]);
        assert_eq!(g.focused(&[3, 0]), Some((0, 0)));
        g.clamp(&[0, 0]);
        assert_eq!(g.focused(&[0, 0]), None);
        assert!(!g.apply(GridMove::Right, &[0, 0]));
        assert_eq!(g.focused(&[]), None);
    }
}
