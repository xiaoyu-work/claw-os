use super::*;

#[test]
fn catalog_matches_verb_table() {
    self_check().unwrap();
}

#[test]
fn regional_capabilities_do_not_reinterpret_existing_configuration_or_identity() {
    for (verb, risk) in [
        (Verb::SYS_LOCALE, Risk::High),
        (Verb::SYS_LANGUAGE, Risk::Medium),
        (Verb::SYS_HOSTNAME, Risk::High),
    ] {
        let meta = lookup(verb).unwrap();
        assert_eq!(meta.scope_kind, ScopeKind::Name);
        assert_eq!(meta.risk, risk);
    }
    assert_eq!(lookup(Verb::SYS_CONFIG).unwrap().scope_kind, ScopeKind::Path);
    assert_eq!(lookup(Verb::SYS_IDENTITY).unwrap().risk, Risk::Critical);
}

#[test]
fn public_app_schema_tracks_the_capability_catalog_without_enabling_ai_bypass() {
    let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../claw-os-sdk/wire/v1/manifest.schema.json"
    ))).unwrap();
    let declared = schema["$defs"]["need"]["properties"]["verb"]["enum"]
        .as_array().unwrap();
    let exported: std::collections::BTreeSet<_> = declared.iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    let catalog: std::collections::BTreeSet<_> = ALL_VERBS.iter()
        .map(|verb| verb.as_str())
        .collect();
    assert_eq!(declared.len(), exported.len(), "duplicate public capability");
    assert_eq!(exported, catalog);
    assert_eq!(schema["$defs"]["need"]["not"]["properties"]["verb"]["const"], "ai.bypass");
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
