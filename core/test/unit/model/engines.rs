use super::*;

/// Pin the alphabetical ordering. `cos agent status` and the
/// provider-registry consumers rely on this; changing it is a
/// surface-visible behavior change.
#[test]
fn engines_linked_returns_alphabetical_order() {
    // We can't easily induce all three engines to report installed
    // in this test (each reads from `engine_pkg::active_library_path`
    // which depends on the real or overridden `<engines_dir>`).
    // Instead, verify the *order* in which the function pushes by
    // checking the position rule: any entry that appears must
    // appear in alphabetical order relative to others.
    let list = engines_linked();
    let mut sorted = list.clone();
    sorted.sort();
    assert_eq!(list, sorted, "engines_linked() must be alphabetical");
}
