use super::{MAX_GRID_COLUMNS, grid_columns_for_width, scroll_ratio};

#[test]
fn grid_columns_fill_wide_outputs() {
    assert_eq!(grid_columns_for_width(1920.0, 128.0, 16.0), MAX_GRID_COLUMNS);
}

#[test]
fn grid_columns_shrink_on_narrow_outputs() {
    assert!(grid_columns_for_width(800.0, 128.0, 16.0) < MAX_GRID_COLUMNS);
    assert_eq!(grid_columns_for_width(100.0, 128.0, 16.0), 1);
}

/// A cell is the 120 px button plus `space_xs` either side. If the two
/// drift apart the row lays out a different number of columns than the
/// keyboard navigation believes are on screen.
#[test]
fn grid_cell_matches_the_application_button() {
    assert_eq!(super::MIN_GRID_CELL_WIDTH, 120.0 + 12.0 * 2.0);
}

#[test]
fn scroll_ratio_reaches_the_end_of_the_grid() {
    // 18 apps over 3 columns is exactly 6 rows; the last row must scroll
    // all the way down instead of stopping at 5/6.
    assert_eq!(scroll_ratio(17, 18, 3), 1.0);
    assert_eq!(scroll_ratio(0, 18, 3), 0.0);
    assert_eq!(scroll_ratio(9, 18, 3), 0.6);
}

#[test]
fn scroll_ratio_handles_degenerate_grids() {
    assert_eq!(scroll_ratio(0, 0, 7), 0.0);
    assert_eq!(scroll_ratio(3, 4, 7), 0.0);
    assert_eq!(scroll_ratio(5, 6, 0), 1.0);
}
