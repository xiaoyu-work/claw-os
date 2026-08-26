use super::*;

/// Function pointers are pointer-sized — sanity check that
/// `OrtSyms` packs as a single fn pointer at scaffold stage.
#[test]
fn syms_struct_is_compact() {
    let expected = std::mem::size_of::<usize>(); // one fn ptr
    assert_eq!(std::mem::size_of::<OrtSyms>(), expected);
}
