use super::*;
use crate::caps::manifest::AiBudget;

fn policy(units: u64, safety: AiSafety, origins: Vec<PromptOrigin>) -> AiPolicy {
    AiPolicy {
        budget: AiBudget { monthly_units: units },
        safety,
        origins,
        tools: Vec::new(),
    }
}

// ---- pure freshness logic ----

#[test]
fn freshness_fresh_when_policy_unchanged() {
    let p = policy(1000, AiSafety::Standard, vec![PromptOrigin::Trusted]);
    let c = Consent::approve(p.clone());
    assert_eq!(freshness(&p, &c), Freshness::Fresh);
}

#[test]
fn freshness_stale_when_budget_changed() {
    let p = policy(1000, AiSafety::Standard, vec![PromptOrigin::Trusted]);
    let c = Consent::approve(p.clone());
    let updated = policy(2000, AiSafety::Standard, vec![PromptOrigin::Trusted]);
    assert_eq!(
        freshness(&updated, &c),
        Freshness::Stale {
            changed: vec!["budget.monthly_units".to_string()]
        }
    );
}

#[test]
fn freshness_stale_when_safety_changed() {
    let p = policy(1000, AiSafety::Standard, vec![PromptOrigin::Trusted]);
    let c = Consent::approve(p.clone());
    let updated = policy(1000, AiSafety::Strict, vec![PromptOrigin::Trusted]);
    match freshness(&updated, &c) {
        Freshness::Stale { changed } => assert_eq!(changed, vec!["safety".to_string()]),
        _ => panic!("expected stale"),
    }
}

#[test]
fn freshness_stale_when_origin_added() {
    let p = policy(1000, AiSafety::Standard, vec![PromptOrigin::Trusted]);
    let c = Consent::approve(p.clone());
    let updated = policy(
        1000,
        AiSafety::Standard,
        vec![PromptOrigin::Trusted, PromptOrigin::ExternalContent],
    );
    match freshness(&updated, &c) {
        Freshness::Stale { changed } => assert_eq!(changed, vec!["origins".to_string()]),
        _ => panic!("expected stale"),
    }
}

#[test]
fn freshness_stale_when_origin_removed() {
    let p = policy(
        1000,
        AiSafety::Standard,
        vec![PromptOrigin::Trusted, PromptOrigin::ExternalContent],
    );
    let c = Consent::approve(p.clone());
    let updated = policy(1000, AiSafety::Standard, vec![PromptOrigin::Trusted]);
    match freshness(&updated, &c) {
        Freshness::Stale { changed } => assert_eq!(changed, vec!["origins".to_string()]),
        _ => panic!("expected stale"),
    }
}

#[test]
fn freshness_fresh_when_origins_reordered() {
    let p = policy(
        1000,
        AiSafety::Standard,
        vec![PromptOrigin::Trusted, PromptOrigin::UserInput],
    );
    let c = Consent::approve(p);
    let updated = policy(
        1000,
        AiSafety::Standard,
        vec![PromptOrigin::UserInput, PromptOrigin::Trusted],
    );
    assert_eq!(freshness(&updated, &c), Freshness::Fresh);
}

#[test]
fn freshness_stale_on_schema_version_mismatch() {
    let p = policy(1000, AiSafety::Standard, vec![PromptOrigin::Trusted]);
    let mut c = Consent::approve(p.clone());
    c.version = SCHEMA_VERSION + 99;
    match freshness(&p, &c) {
        Freshness::Stale { changed } => assert!(changed.contains(&"version".to_string())),
        _ => panic!("expected stale"),
    }
}

#[test]
fn freshness_lists_all_changed_fields() {
    let p = policy(1000, AiSafety::Standard, vec![PromptOrigin::Trusted]);
    let c = Consent::approve(p);
    let updated = policy(
        2000,
        AiSafety::Strict,
        vec![PromptOrigin::Trusted, PromptOrigin::ExternalContent],
    );
    match freshness(&updated, &c) {
        Freshness::Stale { changed } => {
            assert!(changed.contains(&"budget.monthly_units".to_string()));
            assert!(changed.contains(&"safety".to_string()));
            assert!(changed.contains(&"origins".to_string()));
        }
        _ => panic!("expected stale"),
    }
}

// ---- tools-drift detection (audit fix) ----

/// A previously-stored consent record that listed `fs.read` only
/// must be treated as stale if the manifest now lists
/// `fs.read + fs.write`. The user has to re-approve before the
/// new tool grant takes effect — otherwise a silent manifest
/// update could broaden the agent's powers without consent.
#[test]
fn consent_drift_on_tools_change() {
    let mut current = policy(1000, AiSafety::Standard, vec![PromptOrigin::Trusted]);
    current.tools = vec!["fs.read".to_string()];

    // User approved the original (single-tool) policy.
    let stored = Consent::approve(current.clone());

    // Now the manifest adds fs.write to the tool list.
    let mut next = current.clone();
    next.tools = vec!["fs.read".to_string(), "fs.write".to_string()];

    match freshness(&next, &stored) {
        Freshness::Stale { changed } => assert!(
            changed.contains(&"tools".to_string()),
            "expected 'tools' in changed list, got {changed:?}"
        ),
        other => panic!("expected stale on tools change, got {other:?}"),
    }
}

#[test]
fn consent_fresh_when_tools_reordered() {
    let mut current = policy(1000, AiSafety::Standard, vec![PromptOrigin::Trusted]);
    current.tools = vec!["fs.read".to_string(), "fs.write".to_string()];

    let stored = Consent::approve(current.clone());

    let mut reordered = current;
    reordered.tools = vec!["fs.write".to_string(), "fs.read".to_string()];

    assert_eq!(freshness(&reordered, &stored), Freshness::Fresh);
}

#[test]
fn consent_stale_when_tool_removed() {
    let mut current = policy(1000, AiSafety::Standard, vec![PromptOrigin::Trusted]);
    current.tools = vec!["fs.read".to_string(), "fs.write".to_string()];

    let stored = Consent::approve(current.clone());

    let mut next = current;
    next.tools = vec!["fs.read".to_string()];

    match freshness(&next, &stored) {
        Freshness::Stale { changed } => assert!(changed.contains(&"tools".to_string())),
        _ => panic!("expected stale"),
    }
}

// ---- format_for_review ----

#[test]
fn format_for_review_shows_every_field() {
    let p = policy(
        12_345,
        AiSafety::Strict,
        vec![PromptOrigin::Trusted, PromptOrigin::ExternalContent],
    );
    let text = format_for_review("widget", &p);
    assert!(text.contains("App: widget"));
    assert!(text.contains("12345"));
    assert!(text.contains("strict"));
    assert!(text.contains("trusted"));
    assert!(text.contains("external-content"));
}

// ---- on-disk round trip (uses COS_USER_CONFIG_DIR) ----

fn with_tmp_consent_dir<R>(label: &str, f: impl FnOnce() -> R) -> R {
    let tmp = std::env::temp_dir().join(format!(
        "cos-consent-test-{label}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp);
    let prev = std::env::var_os("COS_USER_CONFIG_DIR");
    std::env::set_var("COS_USER_CONFIG_DIR", &tmp);
    let out = f();
    match prev {
        Some(v) => std::env::set_var("COS_USER_CONFIG_DIR", v),
        None => std::env::remove_var("COS_USER_CONFIG_DIR"),
    }
    let _ = fs::remove_dir_all(&tmp);
    out
}

#[test]
fn load_missing_file_returns_none() {
    with_tmp_consent_dir("missing", || {
        assert_eq!(load("never-existed").unwrap(), None);
    });
}

#[test]
fn save_then_load_roundtrip() {
    with_tmp_consent_dir("rt", || {
        let p = policy(500, AiSafety::Standard, vec![PromptOrigin::Trusted]);
        let c = Consent::approve(p.clone());
        save("widget", &c).unwrap();
        let loaded = load("widget").unwrap().unwrap();
        assert_eq!(loaded, c);
        // freshness against the same policy should be Fresh.
        assert_eq!(freshness(&p, &loaded), Freshness::Fresh);
    });
}

#[test]
fn save_creates_parent_directory() {
    with_tmp_consent_dir("mkdir", || {
        let p = policy(1, AiSafety::Minimal, vec![PromptOrigin::Trusted]);
        let c = Consent::approve(p);
        assert!(!consent_path("widget").parent().unwrap().exists());
        save("widget", &c).unwrap();
        assert!(consent_path("widget").is_file());
    });
}

#[test]
fn delete_removes_file_and_reports_outcome() {
    with_tmp_consent_dir("del", || {
        let p = policy(1, AiSafety::Standard, vec![PromptOrigin::Trusted]);
        save("widget", &Consent::approve(p)).unwrap();
        assert_eq!(delete("widget").unwrap(), true);
        assert_eq!(delete("widget").unwrap(), false);
        assert_eq!(load("widget").unwrap(), None);
    });
}

#[test]
fn load_malformed_file_errors() {
    with_tmp_consent_dir("bad", || {
        let dir = consent_path("widget").parent().unwrap().to_path_buf();
        fs::create_dir_all(&dir).unwrap();
        fs::write(consent_path("widget"), "{not json").unwrap();
        assert!(load("widget").is_err());
    });
}

// ---- timestamp formatter ----

#[test]
fn rfc3339_formatter_matches_known_epochs() {
    assert_eq!(format_unix_seconds_rfc3339(0), "1970-01-01T00:00:00Z");
    // 2020-01-01T00:00:00Z = 1_577_836_800
    assert_eq!(
        format_unix_seconds_rfc3339(1_577_836_800),
        "2020-01-01T00:00:00Z"
    );
    // 2024-02-29T12:34:56Z = 1_709_210_096
    assert_eq!(
        format_unix_seconds_rfc3339(1_709_210_096),
        "2024-02-29T12:34:56Z"
    );
}
