use super::{compute_wrapped_slots, reordered_keys_for_drag, target_index_for_drag};
use iced::{Alignment, Padding, Point, Size};
use std::collections::HashMap;

fn size_map(keys: &[usize], width: f32, height: f32) -> HashMap<usize, Size> {
    keys.iter()
        .copied()
        .map(|key| (key, Size::new(width, height)))
        .collect()
}

fn locked_map(keys: &[usize], locked_keys: &[usize]) -> HashMap<usize, bool> {
    keys.iter()
        .copied()
        .map(|key| (key, locked_keys.contains(&key)))
        .collect()
}

#[test]
fn compute_wrapped_slots_creates_new_rows() {
    let ordered_keys = vec![0, 1, 2];
    let locked_by_key = locked_map(&ordered_keys, &[]);
    let size_by_key = size_map(&ordered_keys, 100.0, 40.0);
    let (slots, intrinsic_size) = compute_wrapped_slots(
        &ordered_keys,
        &locked_by_key,
        &size_by_key,
        220.0,
        Padding::ZERO,
        10.0,
        Alignment::Start,
    );

    assert_eq!(slots[0].bounds.x, 0.0);
    assert_eq!(slots[0].bounds.y, 0.0);
    assert_eq!(slots[1].bounds.x, 110.0);
    assert_eq!(slots[1].bounds.y, 0.0);
    assert_eq!(slots[2].bounds.x, 0.0);
    assert_eq!(slots[2].bounds.y, 50.0);
    assert_eq!(intrinsic_size.width, 210.0);
    assert_eq!(intrinsic_size.height, 90.0);
}

#[test]
fn reordered_keys_for_drag_inserts_key_at_target_index() {
    let keys = [0, 1, 2, 3];
    let locked_by_key = locked_map(&keys, &[]);
    let reordered = reordered_keys_for_drag(&keys, &locked_by_key, &0, 3);
    assert_eq!(reordered, vec![1, 2, 3, 0]);
}

#[test]
fn target_index_tracks_wrapped_drop_positions() {
    let ordered_keys = vec![0, 1, 2, 3];
    let locked_by_key = locked_map(&ordered_keys, &[]);
    let size_by_key = size_map(&ordered_keys, 100.0, 40.0);

    let (slots, _) = compute_wrapped_slots(
        &ordered_keys,
        &locked_by_key,
        &size_by_key,
        220.0,
        Padding::ZERO,
        10.0,
        Alignment::Start,
    );

    let target_index = target_index_for_drag(&slots, &0, Point::new(160.0, 70.0));

    assert_eq!(target_index, 3);
}

#[test]
fn reordered_keys_for_drag_preserves_locked_positions() {
    let keys = [10, 11, 12, 13];
    let locked_by_key = locked_map(&keys, &[10, 13]);
    let reordered = reordered_keys_for_drag(&keys, &locked_by_key, &11, 1);

    assert_eq!(reordered, vec![10, 12, 11, 13]);
}
