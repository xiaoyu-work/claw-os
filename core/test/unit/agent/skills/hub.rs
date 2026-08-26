use super::*;

impl SkillsHub {
    pub(crate) fn with_gh_client(cfg: HubConfig, gh: GhClient) -> Self {
        let spec = cfg.gh_spec();
        Self { cfg, spec, gh }
    }
}

#[test]
fn config_filters_empty_token() {
    let c = HubConfig::new("o", "r").with_token(Some(String::new()));
    assert!(c.token.is_none());
    let c = HubConfig::new("o", "r").with_token(Some("abc".into()));
    assert_eq!(c.token.as_deref(), Some("abc"));
}

#[test]
fn config_default_catalogue_asset_name() {
    let c = HubConfig::new("a", "b");
    assert_eq!(c.catalogue_asset, "hub.json");
    let c = c.with_catalogue_asset("registry.json");
    assert_eq!(c.catalogue_asset, "registry.json");
}

#[test]
fn catalogue_round_trips_via_serde() {
    let body = r#"{
        "schema": 1,
        "release_tag": "v0.1.0",
        "skills": [{
            "id": "pdf-extract",
            "name": "PDF Extract",
            "description": "extract pdfs",
            "version": "0.2.1",
            "asset": "pdf-extract-0.2.1.tar.gz",
            "sha256": "deadbeef",
            "homepage": "https://example.com",
            "license": "MIT",
            "tags": ["productivity", "office"]
        }]
    }"#;
    let cat: HubCatalogue = serde_json::from_str(body).unwrap();
    assert_eq!(cat.schema, 1);
    assert_eq!(cat.skills.len(), 1);
    assert_eq!(cat.skills[0].id, "pdf-extract");
    assert_eq!(cat.skills[0].tags, vec!["productivity", "office"]);
}

#[test]
fn skill_serializes_camel_round_trip() {
    let s = HubSkill {
        id: "x".into(),
        name: "X".into(),
        description: None,
        version: "1.0.0".into(),
        asset: "x-1.0.0.tar.gz".into(),
        sha256: "abc".into(),
        homepage: None,
        license: None,
        tags: vec![],
    };
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(v["id"], "x");
    // Optional empty fields must round-trip even when None /
    // empty, otherwise older catalogue files break.
    let back: HubSkill = serde_json::from_value(v).unwrap();
    assert_eq!(back, s);
}

#[test]
fn schema_version_constant_is_one() {
    assert_eq!(HUB_SCHEMA_VERSION, 1);
}

#[test]
fn hub_error_messages_are_actionable() {
    let e = HubError::CatalogueMissing {
        asset: "hub.json".into(),
        tag: "v1".into(),
    };
    assert!(e.to_string().contains("hub.json"));
    assert!(e.to_string().contains("v1"));
    let e = HubError::Schema {
        expected: 1,
        got: 7,
    };
    assert!(e.to_string().contains('1') && e.to_string().contains('7'));
}

// The full integration path (release → catalogue → asset
// download) goes through reqwest + a live HTTP server. We have
// no embedded mock server in the workspace right now; the
// GhClient/asset_select tests in engine_pkg already exercise
// the GitHub-API half against canned bodies, so we keep this
// module's tests focused on data shapes + config knobs.
