use super::*;

/// Compile-time check: opaque types are zero-sized so callers can't
/// accidentally try to construct one.
#[test]
fn opaque_types_are_zst() {
    assert_eq!(std::mem::size_of::<llama_model>(), 0);
    assert_eq!(std::mem::size_of::<llama_context>(), 0);
    assert_eq!(std::mem::size_of::<llama_sampler>(), 0);
    assert_eq!(std::mem::size_of::<llama_vocab>(), 0);
}

/// Function pointers are pointer-sized — sanity check that
/// `LlamaSyms` packs as expected.
#[test]
fn syms_struct_is_compact() {
    let expected = std::mem::size_of::<usize>() * 3; // three fn ptrs
    assert_eq!(std::mem::size_of::<LlamaSyms>(), expected);
}
