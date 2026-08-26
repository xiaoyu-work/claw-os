use super::{EdgeScrollDirection, edge_scroll_adjustment};

const BUFFER_HEIGHT: f32 = 200.0;
const CELL_HEIGHT: f32 = 20.0;
const MAX_ROW: f32 = 9.0;

#[test]
fn edge_scroll_small_top_overshoot_does_not_scroll_immediately() {
    let (delta, row, remainder, direction, overshoot) = edge_scroll_adjustment(
        -5.0,
        BUFFER_HEIGHT,
        CELL_HEIGHT,
        MAX_ROW,
        0.0,
        EdgeScrollDirection::None,
        0.0,
        0.0,
    );
    assert_eq!(delta, 0);
    assert_eq!(row, 0.0);
    assert!((remainder - 0.25).abs() < f32::EPSILON);

    let (delta_repeat, row_repeat, remainder_repeat, _, _) = edge_scroll_adjustment(
        -5.0,
        BUFFER_HEIGHT,
        CELL_HEIGHT,
        MAX_ROW,
        remainder,
        direction,
        overshoot,
        0.0,
    );
    assert_eq!(delta_repeat, 0);
    assert_eq!(row_repeat, 0.0);
    assert!((remainder_repeat - 0.25).abs() < f32::EPSILON);
}

#[test]
fn edge_scroll_top_accumulates_into_scroll() {
    let (_delta, _row, mut remainder, mut direction, mut overshoot) = edge_scroll_adjustment(
        -5.0,
        BUFFER_HEIGHT,
        CELL_HEIGHT,
        MAX_ROW,
        0.0,
        EdgeScrollDirection::None,
        0.0,
        0.0,
    );
    let (delta, row, new_remainder, new_direction, new_overshoot) = edge_scroll_adjustment(
        -45.0,
        BUFFER_HEIGHT,
        CELL_HEIGHT,
        MAX_ROW,
        remainder,
        direction,
        overshoot,
        0.0,
    );
    assert_eq!(delta, 2);
    assert_eq!(row, 0.0);
    assert!((new_remainder - 0.25).abs() < f32::EPSILON);
    remainder = new_remainder;
    direction = new_direction;
    overshoot = new_overshoot;

    // repeated event with the same overshoot should not accumulate more scroll
    let (delta, _, remainder, _, _) = edge_scroll_adjustment(
        -45.0,
        BUFFER_HEIGHT,
        CELL_HEIGHT,
        MAX_ROW,
        remainder,
        direction,
        overshoot,
        0.0,
    );
    assert_eq!(delta, 0);
    assert!((remainder - 0.25).abs() < f32::EPSILON);
}

#[test]
fn edge_scroll_inside_viewport_resets_remainder() {
    let (_delta, _row, remainder, direction, overshoot) = edge_scroll_adjustment(
        -25.0,
        BUFFER_HEIGHT,
        CELL_HEIGHT,
        MAX_ROW,
        0.0,
        EdgeScrollDirection::None,
        0.0,
        0.0,
    );
    let (delta, row, remainder, direction, overshoot) = edge_scroll_adjustment(
        60.0,
        BUFFER_HEIGHT,
        CELL_HEIGHT,
        MAX_ROW,
        remainder,
        direction,
        overshoot,
        0.0,
    );
    assert_eq!(delta, 0);
    assert!((row - 3.0).abs() < f32::EPSILON);
    assert_eq!(remainder, 0.0);
    assert_eq!(direction, EdgeScrollDirection::None);
    assert_eq!(overshoot, 0.0);
}

#[test]
fn edge_scroll_bottom_accumulates_scroll() {
    let (delta, row, remainder, direction, overshoot) = edge_scroll_adjustment(
        BUFFER_HEIGHT + 1.0,
        BUFFER_HEIGHT,
        CELL_HEIGHT,
        MAX_ROW,
        0.0,
        EdgeScrollDirection::None,
        0.0,
        0.0,
    );
    assert_eq!(delta, 0);
    assert!((row - MAX_ROW).abs() < f32::EPSILON);
    assert!((remainder - 0.05).abs() < f32::EPSILON);

    let (delta, row, remainder, direction, overshoot) = edge_scroll_adjustment(
        BUFFER_HEIGHT + 45.0,
        BUFFER_HEIGHT,
        CELL_HEIGHT,
        MAX_ROW,
        remainder,
        direction,
        overshoot,
        0.0,
    );
    assert_eq!(delta, -2);
    assert!((row - MAX_ROW).abs() < f32::EPSILON);
    assert!((remainder - 0.25).abs() < f32::EPSILON);

    let (delta, _, remainder, _, _) = edge_scroll_adjustment(
        BUFFER_HEIGHT + 45.0,
        BUFFER_HEIGHT,
        CELL_HEIGHT,
        MAX_ROW,
        remainder,
        direction,
        overshoot,
        0.0,
    );
    assert_eq!(delta, 0);
    assert!((remainder - 0.25).abs() < f32::EPSILON);
}

#[test]
fn edge_scroll_forced_increment_continues_scrolling() {
    let (delta, _, remainder, direction, overshoot) = edge_scroll_adjustment(
        -5.0,
        BUFFER_HEIGHT,
        CELL_HEIGHT,
        MAX_ROW,
        0.0,
        EdgeScrollDirection::None,
        0.0,
        0.0,
    );
    assert_eq!(delta, 0);

    let (delta, _, remainder, direction, overshoot) = edge_scroll_adjustment(
        -5.0,
        BUFFER_HEIGHT,
        CELL_HEIGHT,
        MAX_ROW,
        remainder,
        direction,
        overshoot,
        1.0,
    );
    assert_eq!(delta, 1);
    assert!((remainder - 0.25).abs() < f32::EPSILON);

    let (delta, _, remainder, _, _) = edge_scroll_adjustment(
        -5.0,
        BUFFER_HEIGHT,
        CELL_HEIGHT,
        MAX_ROW,
        remainder,
        direction,
        overshoot,
        1.0,
    );
    assert_eq!(delta, 1);
    assert!((remainder - 0.25).abs() < f32::EPSILON);
}
