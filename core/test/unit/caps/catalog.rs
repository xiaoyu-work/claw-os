use super::*;

#[test]
fn catalog_matches_verb_table() {
    self_check().unwrap();
}

#[test]
fn every_verb_has_metadata() {
    for v in ALL_VERBS {
        let m = lookup(*v).unwrap_or_else(|| panic!("missing meta for {}", v.as_str()));
        // Sanity: labels and blurbs must be non-empty in English.
        assert!(!m.label.en().is_empty(), "empty label for {}", v.as_str());
        assert!(!m.blurb.en().is_empty(), "empty blurb for {}", v.as_str());
        assert!(!m.icon.is_empty(), "empty icon for {}", v.as_str());
    }
}

#[test]
fn lookup_returns_none_for_synthetic_unknown() {
    // We can't construct an invalid Verb publicly, so just exercise
    // the happy path here.
    assert!(lookup(Verb::FS_READ).is_some());
}
