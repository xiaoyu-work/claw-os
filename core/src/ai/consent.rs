//! Per-app user consent for AI-using Apps.
//!
//! Lives at `$HOME/.config/cos/consents/<app_id>.json`. Records the
//! user's explicit approval of an App's declared AI policy at a
//! specific moment in time. The gate refuses to run any AI call from
//! an App that lacks a current, fresh consent record.
//!
//! # What problem this solves
//!
//! Manifest validation already rejects an App that declares an
//! `ai.*` need without an `ai` block. But validation only proves the
//! App is *internally consistent*; it doesn't ask the user whether
//! they *want* the App to spend tokens, route through which safety
//! profile, accept external content, and so on. That's what consent
//! adds: a one-time review step in which the user sees the App's full
//! AI ask in plain language and either approves or refuses.
//!
//! # Freshness and re-prompting
//!
//! A consent record snapshots the AiPolicy that was approved. On
//! every gate call, the current manifest policy is compared field-by-
//! field against the snapshot. If anything has changed — budget,
//! safety, origins — the consent becomes **stale** and the gate
//! denies with `consent_stale`, listing which fields changed. The
//! user re-runs `cos app consent grant <app>` to inspect the new ask
//! and re-approve.
//!
//! Per-user overrides (`crate::ai::overrides`) are independent: this
//! module tracks the **manifest** policy. A user who tightens their
//! own override does not need to re-consent. A developer who pushes
//! a manifest update with a looser AI ask *does* trigger re-consent.
//!
//! # Storage
//!
//! Tiny atomic JSON file. Writes go through tmp + rename. Reads
//! treat missing-file as `Ok(None)`. Malformed JSON returns `Err`
//! and the gate surfaces it as `BadConsent`.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::caps::manifest::{AiPolicy, AiSafety, PromptOrigin};
use crate::paths;

/// Current schema version. Bump if the on-disk shape changes in a
/// way that requires a re-prompt.
pub const SCHEMA_VERSION: u32 = 1;

/// On-disk shape of `<user_config>/consents/<app_id>.json`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Consent {
    /// Schema version. Mismatch ⇒ stale (the kernel knows the user
    /// approved an older shape, but doesn't know what fields it had).
    pub version: u32,
    /// RFC3339 timestamp of when the user approved the snapshot.
    pub approved_at: String,
    /// Verbatim copy of the AiPolicy that was approved. Used by
    /// [`freshness`] to detect drift in the manifest.
    pub policy: AiPolicy,
}

impl Consent {
    /// Build a fresh consent record from a policy snapshot using the
    /// current wall-clock time.
    pub fn approve(policy: AiPolicy) -> Self {
        Self {
            version: SCHEMA_VERSION,
            approved_at: now_rfc3339(),
            policy,
        }
    }
}

/// Result of comparing a current manifest policy to a stored consent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Freshness {
    /// Consent is current: stored snapshot matches the manifest.
    Fresh,
    /// Consent is out of date. `changed` names the AiPolicy fields
    /// that differ between snapshot and current manifest. The CLI
    /// uses this list to tell the user what they're being asked to
    /// re-approve.
    Stale { changed: Vec<String> },
}

/// File the loader checks. Missing-file is normal.
pub fn consent_path(app_id: &str) -> PathBuf {
    paths::user_app_consent_path(app_id)
}

/// Read the consent file for an app. Returns `Ok(None)` when the
/// file does not exist; `Err` only when a present file fails to
/// parse or read. Schema-version mismatch is *not* an error — the
/// caller treats a wrong-version record as stale.
pub fn load(app_id: &str) -> Result<Option<Consent>, String> {
    let path = consent_path(app_id);
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let parsed: Consent =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
    Ok(Some(parsed))
}

/// Atomically write the consent record to disk. Creates the parent
/// directory if needed; uses tmp + rename to avoid half-written files.
pub fn save(app_id: &str, consent: &Consent) -> Result<(), String> {
    let path = consent_path(app_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let body = serde_json::to_string_pretty(consent)
        .map_err(|e| format!("serialize consent: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &body).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, &path)
        .map_err(|e| format!("rename {} → {}: {e}", tmp.display(), path.display()))?;
    Ok(())
}

/// Delete the consent record for an app. Returns `Ok(true)` if a
/// file was removed, `Ok(false)` if there was nothing to remove.
pub fn delete(app_id: &str) -> Result<bool, String> {
    let path = consent_path(app_id);
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("remove {}: {e}", path.display())),
    }
}

/// Compare a current manifest policy to a stored consent snapshot.
/// Pure function — no I/O. A schema-version mismatch is reported as
/// stale with a synthetic `"version"` entry so the CLI can explain.
pub fn freshness(current: &AiPolicy, consent: &Consent) -> Freshness {
    let mut changed = Vec::new();

    if consent.version != SCHEMA_VERSION {
        changed.push("version".to_string());
    }
    if current.budget.monthly_units != consent.policy.budget.monthly_units {
        changed.push("budget.monthly_units".to_string());
    }
    if current.safety != consent.policy.safety {
        changed.push("safety".to_string());
    }
    if !origins_equal(&current.origins, &consent.policy.origins) {
        changed.push("origins".to_string());
    }

    if changed.is_empty() {
        Freshness::Fresh
    } else {
        Freshness::Stale { changed }
    }
}

/// Compare two origin lists as sets — order is not semantically
/// meaningful, so an author reordering the list in the manifest
/// shouldn't force a re-consent.
fn origins_equal(a: &[PromptOrigin], b: &[PromptOrigin]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().all(|x| b.contains(x))
}

/// Render an AiPolicy as a plain-text review block for the CLI
/// consent prompt. Stable, grep-friendly format suitable for tests.
pub fn format_for_review(app_id: &str, policy: &AiPolicy) -> String {
    let mut out = String::new();
    out.push_str(&format!("App: {app_id}\n"));
    out.push_str("AI policy declared by the manifest:\n");
    out.push_str(&format!(
        "  budget.monthly_units : {}\n",
        policy.budget.monthly_units
    ));
    out.push_str(&format!("  safety               : {}\n", safety_label(policy.safety)));
    let origins = policy
        .origins
        .iter()
        .map(|o| origin_label(*o))
        .collect::<Vec<_>>()
        .join(", ");
    out.push_str(&format!("  origins              : [{origins}]\n"));
    out
}

fn safety_label(s: AiSafety) -> &'static str {
    match s {
        AiSafety::Strict => "strict",
        AiSafety::Standard => "standard",
        AiSafety::Minimal => "minimal",
    }
}

fn origin_label(o: PromptOrigin) -> &'static str {
    match o {
        PromptOrigin::Trusted => "trusted",
        PromptOrigin::UserInput => "user-input",
        PromptOrigin::ExternalContent => "external-content",
    }
}

fn now_rfc3339() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_unix_seconds_rfc3339(secs)
}

/// RFC3339 (UTC, no fractional seconds) formatter used by the
/// consent record's `approved_at` field. Kept dependency-free so we
/// don't pull `chrono` into the kernel just for one timestamp.
fn format_unix_seconds_rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let time_of_day = (secs % 86_400) as u32;
    let h = time_of_day / 3600;
    let m = (time_of_day % 3600) / 60;
    let s = time_of_day % 60;
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Howard Hinnant's `civil_from_days`. Returns (year, month, day).
/// Public domain. Handles dates beyond 4000 AD without issue —
/// adequate for any approved_at timestamp this OS will ever record.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = y + if m <= 2 { 1 } else { 0 };
    (y as i32, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
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
}
