use super::*;

/// The struct holds 25 fn pointers; assert layout to catch
/// accidental reordering or duplicate-field bugs in resolve().
#[test]
fn syms_struct_size_matches_field_count() {
    const FN_COUNT: usize = 25;
    let expected = std::mem::size_of::<usize>() * FN_COUNT;
    assert_eq!(std::mem::size_of::<OrtGenaiSyms>(), expected);
}

#[test]
fn element_type_float32_is_one() {
    assert_eq!(OgaElementType::Float32 as i32, 1);
}
