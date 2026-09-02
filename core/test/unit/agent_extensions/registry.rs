use super::*;

#[test]
fn selected_ids_are_explicit_deduplicated_and_quarantined_on_failure() {
    let root = tempfile::tempdir().unwrap();
    let registry = ExtensionRegistry::load_selected(
        root.path(),
        &[
            "../escape".to_string(),
            "missing".to_string(),
            "missing".to_string(),
        ],
    );
    assert!(registry.registered.is_empty());
    assert_eq!(registry.quarantined.len(), 2);
    assert!(registry.quarantined[0]
        .diagnostic
        .contains("configured extension id"));
    assert!(registry.quarantined[1]
        .diagnostic
        .contains("quarantined"));
}
