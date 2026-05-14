//! Per-user aggregate AI token budget.
//!
//! Lives at `$HOME/.config/cos/ai/budget.json`. Independent of the
//! per-app budget declared in each App's manifest. The kernel reads
//! this file on every AI call and reserves the same number of units
//! against a single shared `__user__` row in the existing budget
//! ledger. If the resulting total exceeds `monthly_units`, the call
//! is hard-denied with `user_budget_exceeded`.
//!
//! # Why this exists
//!
//! An App declares "I want up to N tokens / month" in its manifest;
//! that cap is per-app and enforced separately. Without an aggregate
//! ceiling, a user who installs 50 Apps each with a 200,000-token
//! cap can rack up 10 M tokens a month even though no single App ran
//! away — the **sum** runs away. The user-level budget is the second
//! axis that catches this case.
//!
//! # Units, not money
//!
//! Like every other budget in this OS, the cap is denominated in
//! abstract billing units (≈ tokens for chat / embed; flat rates
//! for image / audio / video). There is no USD axis. The OS owner
//! who pointed the kernel at a paid provider knows their own
//! USD-per-token rate; the cap they set here is a token volume.
//!
//! # Opting out
//!
//! `monthly_units == 0` (or a missing file) means "no cap". This is
//! the default — the kernel does not impose a ceiling until the user
//! sets one. Matches `AiBudget::monthly_units == 0` semantics in the
//! manifest layer.
//!
//! # No CLI writer
//!
//! Same convention as `crate::ai::overrides`: the kernel reads, the
//! Cosmic Settings UI writes. A read-only `cos agent budget user
//! show` exists for inspection.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::paths;

/// Sentinel `app_id` used to address the user-level aggregate row in
/// the shared `ai_budget` ledger. Chosen so it cannot collide with
/// any real App id (Apps must have a non-empty alphanumeric id; the
/// double underscore is reserved for kernel-internal buckets, same
/// convention as `SYSTEM_AGENT_BUCKET` in `ai::gate`).
pub const USER_BUDGET_BUCKET: &str = "__user__";

/// On-disk shape of `$HOME/.config/cos/ai/budget.json`.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct UserBudget {
    /// Cap on total tokens used across every App per calendar month.
    /// `0` (the default) means "unlimited" — the user has opted out
    /// of an aggregate ceiling and only per-app caps apply.
    #[serde(default)]
    pub monthly_units: u64,
}

impl UserBudget {
    /// True iff the user has opted out of an aggregate ceiling.
    pub fn is_unlimited(self) -> bool {
        self.monthly_units == 0
    }
}

/// File the loader checks. Missing file is normal and means
/// "unlimited" — there is no implicit cap.
pub fn config_path() -> PathBuf {
    paths::user_budget_config_path()
}

/// Read the user budget config. Returns `UserBudget::default()`
/// (i.e. unlimited) when the file does not exist; `Err` only when a
/// present file fails to read or parse.
pub fn load() -> Result<UserBudget, String> {
    let path = config_path();
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(UserBudget::default());
        }
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(UserBudget::default());
    }
    let parsed: UserBudget = serde_json::from_str(trimmed)
        .map_err(|e| format!("parse {}: {e}", path.display()))?;
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_tmp_config_dir<R>(label: &str, f: impl FnOnce() -> R) -> R {
        let tmp = std::env::temp_dir().join(format!(
            "cos-user-budget-test-{label}-{}",
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
    fn missing_file_is_unlimited() {
        with_tmp_config_dir("missing", || {
            let b = load().unwrap();
            assert_eq!(b.monthly_units, 0);
            assert!(b.is_unlimited());
        });
    }

    #[test]
    fn empty_file_is_unlimited() {
        with_tmp_config_dir("empty", || {
            let path = config_path();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "").unwrap();
            let b = load().unwrap();
            assert!(b.is_unlimited());
        });
    }

    #[test]
    fn explicit_zero_is_unlimited() {
        with_tmp_config_dir("zero", || {
            let path = config_path();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, r#"{"monthly_units": 0}"#).unwrap();
            let b = load().unwrap();
            assert!(b.is_unlimited());
        });
    }

    #[test]
    fn nonzero_cap_loads() {
        with_tmp_config_dir("cap", || {
            let path = config_path();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, r#"{"monthly_units": 1234567}"#).unwrap();
            let b = load().unwrap();
            assert_eq!(b.monthly_units, 1234567);
            assert!(!b.is_unlimited());
        });
    }

    #[test]
    fn unknown_field_is_ignored() {
        with_tmp_config_dir("extra", || {
            let path = config_path();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(
                &path,
                r#"{"monthly_units": 42, "future_usd_axis": 99.99}"#,
            )
            .unwrap();
            let b = load().unwrap();
            assert_eq!(b.monthly_units, 42);
        });
    }

    #[test]
    fn malformed_file_errors() {
        with_tmp_config_dir("bad", || {
            let path = config_path();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "{not json").unwrap();
            assert!(load().is_err());
        });
    }

    #[test]
    fn user_budget_bucket_is_reserved_id() {
        // Sentinel must contain a character (underscore) that App ids
        // cannot. Apps validate to alphanumeric-plus-hyphen.
        assert!(USER_BUDGET_BUCKET.starts_with("__"));
        assert!(USER_BUDGET_BUCKET.ends_with("__"));
    }
}
