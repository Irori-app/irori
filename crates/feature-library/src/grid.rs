//! Geometry of the cover grid and keyboard movement within it. Pure
//! functions over logical pixels and rem sizes so they can be unit-tested.

/// Gap between tiles, in rem.
pub const GAP_REM: f32 = 1.25;
/// Room under the cover for the title and status lines, in rem.
pub const CAPTION_REM: f32 = 2.75;
/// Space above each row so a focused tile's ring is not clipped, in rem.
pub const ROW_INSET_REM: f32 = 0.25;
/// Horizontal content inset of the scroll region, in rem.
pub const EDGE_REM: f32 = 1.5;
/// Height of one list-layout row, in rem.
pub const LIST_ROW_REM: f32 = 3.75;
/// Height of a section heading row, in rem.
pub const HEADING_REM: f32 = 2.75;
/// Cover aspect ratio (height / width).
pub const COVER_RATIO: f32 = 1.5;
/// Tile width bounds for the size slider, in rem.
pub const MIN_TILE_REM: f32 = 7.0;
pub const MAX_TILE_REM: f32 = 16.0;

/// List column widths, in rem (store, size, playtime, last played, status, actions).
pub const LIST_COLUMNS: [f32; 6] = [6.0, 5.5, 6.0, 7.5, 8.0, 10.0];
/// Width of a list row's thumbnail, in rem.
pub const LIST_THUMB_REM: f32 = 2.25;
/// Gap between list cells (`gap_4`), in rem.
const LIST_GAP_REM: f32 = 1.0;
/// Narrowest the title column gets before a data column is dropped, in rem.
const LIST_TITLE_MIN_REM: f32 = 12.0;
/// Data columns in the order narrow lists drop them: size, store, playtime,
/// last played. Status and the actions always stay.
const LIST_DROP_ORDER: [usize; 4] = [1, 0, 2, 3];

/// Which of [`LIST_COLUMNS`] fit in `available_rem` beside the thumbnail and a
/// readable title.
pub fn list_columns(available_rem: f32) -> [bool; 6] {
    let needed = |shown: &[bool; 6]| {
        let columns: f32 = LIST_COLUMNS
            .iter()
            .zip(shown)
            .filter(|(_, shown)| **shown)
            .map(|(w, _)| w + LIST_GAP_REM)
            .sum();
        LIST_THUMB_REM + LIST_GAP_REM + LIST_TITLE_MIN_REM + columns
    };
    let mut shown = [true; 6];
    for column in LIST_DROP_ORDER {
        if needed(&shown) <= available_rem {
            break;
        }
        shown[column] = false;
    }
    shown
}

/// Resolved layout of the grid for one viewport width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridMetrics {
    pub columns: usize,
    /// Tile width in rem: the preferred size stretched so tiles fill the row.
    pub tile_rem: f32,
    /// Height of one grid row in rem (inset, cover, caption and gap).
    pub row_rem: f32,
}

/// Fit tiles of at least `preferred_rem` into `available_rem` with `GAP_REM`
/// between them, then widen them to fill the row exactly. Tiles never grow
/// past one and a half times the preferred width (very narrow windows with a
/// single column).
pub fn grid_metrics(available_rem: f32, preferred_rem: f32) -> GridMetrics {
    let preferred = preferred_rem.clamp(MIN_TILE_REM, MAX_TILE_REM);
    let available = available_rem.max(preferred);
    let columns = (((available + GAP_REM) / (preferred + GAP_REM)).floor() as usize).max(1);
    let fill = (available - GAP_REM * (columns as f32 - 1.0)) / columns as f32;
    let tile_rem = fill.min(preferred * 1.5).max(preferred.min(available));
    GridMetrics {
        columns,
        tile_rem,
        row_rem: ROW_INSET_REM + tile_rem * COVER_RATIO + CAPTION_REM + GAP_REM,
    }
}

/// Rows needed for `count` tiles.
pub fn row_count(count: usize, columns: usize) -> usize {
    count.div_ceil(columns.max(1))
}

/// A keyboard movement in the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Left,
    Right,
    Up,
    Down,
    /// Start of the library.
    First,
    /// End of the library.
    Last,
    /// One page of `rows` rows up.
    PageUp(usize),
    /// One page of `rows` rows down.
    PageDown(usize),
}

/// Index focused after a movement. With nothing focused yet, any movement
/// focuses the first game. Up/Down keep the column; moving down from a partial
/// last row lands on the last game.
pub fn move_focus(current: Option<usize>, len: usize, columns: usize, step: Move) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let columns = columns.max(1);
    let last = len - 1;
    let Some(ix) = current.map(|i| i.min(last)) else {
        return Some(if step == Move::Last { last } else { 0 });
    };
    Some(match step {
        Move::Left => ix.saturating_sub(1),
        Move::Right => (ix + 1).min(last),
        Move::Up => ix.checked_sub(columns).unwrap_or(ix),
        Move::Down => {
            if ix + columns <= last {
                ix + columns
            } else if row_count(len, columns) > ix / columns + 1 {
                last
            } else {
                ix
            }
        }
        Move::First => 0,
        Move::Last => last,
        Move::PageUp(rows) => ix.saturating_sub(columns * rows.max(1)),
        Move::PageDown(rows) => (ix + columns * rows.max(1)).min(last),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_fill_the_row() {
        // 58rem fits 4 tiles of 11rem with 1.25rem gaps, not 5 (5*11 + 4*1.25 = 60).
        let m = grid_metrics(58.0, 11.0);
        assert_eq!(m.columns, 4);
        let used = m.tile_rem * 4.0 + GAP_REM * 3.0;
        assert!((used - 58.0).abs() < 0.01, "{used}");
        assert!(m.tile_rem >= 11.0);
        assert!(
            (m.row_rem - (ROW_INSET_REM + m.tile_rem * 1.5 + CAPTION_REM + GAP_REM)).abs() < 1e-4
        );

        // Narrower than one tile: one column, tile keeps the preferred width.
        let m = grid_metrics(5.0, 11.0);
        assert_eq!(m.columns, 1);
        assert_eq!(m.tile_rem, 11.0);

        // Preferred width is clamped to the slider range.
        let m = grid_metrics(100.0, 2.0);
        assert!(m.tile_rem >= MIN_TILE_REM);
    }

    #[test]
    fn rows() {
        assert_eq!(row_count(0, 4), 0);
        assert_eq!(row_count(4, 4), 1);
        assert_eq!(row_count(5, 4), 2);
        assert_eq!(row_count(5, 0), 5);
    }

    #[test]
    fn keyboard_movement() {
        // 10 games in 4 columns:
        //  0 1 2 3
        //  4 5 6 7
        //  8 9
        let mv = |c, s| move_focus(c, 10, 4, s);
        assert_eq!(mv(None, Move::Down), Some(0));
        assert_eq!(mv(None, Move::Last), Some(9));
        assert_eq!(mv(Some(0), Move::Left), Some(0));
        assert_eq!(mv(Some(3), Move::Right), Some(4));
        assert_eq!(mv(Some(9), Move::Right), Some(9));
        assert_eq!(mv(Some(5), Move::Up), Some(1));
        assert_eq!(mv(Some(1), Move::Up), Some(1));
        assert_eq!(mv(Some(5), Move::Down), Some(9));
        // From the second row's end, down lands on the last game of the partial row.
        assert_eq!(mv(Some(7), Move::Down), Some(9));
        assert_eq!(mv(Some(9), Move::Down), Some(9));
        assert_eq!(mv(Some(6), Move::First), Some(0));
        assert_eq!(mv(Some(6), Move::PageDown(5)), Some(9));
        assert_eq!(mv(Some(6), Move::PageUp(1)), Some(2));
        assert_eq!(move_focus(Some(3), 0, 4, Move::Down), None);
        // A stale index beyond the list is clamped first.
        assert_eq!(mv(Some(42), Move::Left), Some(8));
    }

    #[test]
    fn narrow_lists_drop_columns_in_order() {
        assert_eq!(list_columns(80.0), [true; 6]);
        assert_eq!(list_columns(60.0), [true, false, true, true, true, true]);
        assert_eq!(list_columns(51.0), [false, false, true, true, true, true]);
        assert_eq!(list_columns(50.0), [false, false, false, true, true, true]);
        assert_eq!(list_columns(20.0), [false, false, false, false, true, true]);
    }
}
