//! Per-user overrides for an installed App's AI policy.
//!
//! Lives at `$HOME/.config/cos/apps/<app_id>.json`. The Cosmic
//! Settings UI (Settings → AI → App permissions) writes this file
//! when the user adjusts the budget, safety, or origin allowlist for
//! an installed App. The kernel reads it on every AI call and uses
//! the *effective* policy — never the raw manifest.
//!
//! # Tighten only — never loosen
//!
//! User overrides can only tighten the manifest's declared policy.
//! There is no field that lets a user grant the App something the
//! manifest didn't already ask for. Merge rules:
//!
//!   * `disabled = true` — hard kill switch. Every AI call from this
//!     App is denied at the gate with `denial_reason = "app_disabled"`.
//!     Use this to silence a noisy App without uninstalling it.
//!   * `ai.budget.monthly_units` — `min(manifest, override)`. A user
//!     can lower the cap but never raise it past what the App declared.
//!   * `ai.safety` — the **stricter** of `(manifest, override)`. A
//!     user can require Strict on an App that only asked for Standard;
//!     they cannot weaken a Strict-by-default App to Minimal.
//!   * `ai.origins` — the **intersection** of the two lists. A user
//!     can shrink an App's origin allowlist (e.g. ban
//!     `external-content` on a summariser they no longer trust); they
//!     cannot add an origin the App did not declare.
//!
//! Any override field set to `null` (or absent) inherits the manifest
//! value verbatim. The file may also be entirely missing — that is
//! the default case for a freshly-installed App.
//!
//! # No CLI writer
//!
//! The kernel does **not** ship a write surface for this file. The
//! UI is the sole writer. A read-only `cos agent override show <app>`
//! exists for debugging.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::caps::manifest::{AiPolicy, AiSafety, PromptOrigin};
use crate::paths;

/// On-disk shape of `<user_config>/apps/<app_id>.json`. Every field
/// is optional; absent means "inherit from manifest".
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct UserOverride {
    /// Hard kill switch. When `true`, every AI call from this App is
    /// denied at the gate, regardless of manifest.
    #[serde(default, skip_serializing_if = "is_false")]
    pub disabled: bool,

    /// Overrides for the manifest's `ai` block. Each sub-field is
    /// independently optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai: Option<AiOverride>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct AiOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<AiBudgetOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety: Option<AiSafety>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origins: Option<Vec<PromptOrigin>>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct AiBudgetOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monthly_units: Option<u64>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Path the loader checks. Absent file is normal.
pub fn override_path(app_id: &str) -> PathBuf {
    paths::user_app_override_path(app_id)
}

/// Read the override file for an app. Returns `Ok(None)` when the
/// file does not exist (the normal case); returns `Err` only when a
/// present file fails to parse or read.
pub fn load(app_id: &str) -> Result<Option<UserOverride>, String> {
    let path = override_path(app_id);
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let parsed: UserOverride = serde_json::from_str(&raw)
        .map_err(|e| format!("parse {}: {e}", path.display()))?;
    Ok(Some(parsed))
}

/// Apply a user override on top of the manifest AI policy. The
/// returned policy is what the gate enforces. Pure function — no I/O.
pub fn apply_to_policy(manifest: &AiPolicy, ovr: Option<&UserOverride>) -> AiPolicy {
    let mut out = manifest.clone();
    let Some(ovr) = ovr else { return out };
    let Some(ai) = ovr.ai.as_ref() else { return out };

    if let Some(b) = &ai.budget {
        if let Some(units) = b.monthly_units {
            out.budget.monthly_units = out.budget.monthly_units.min(units);
        }
    }
    if let Some(s) = ai.safety {
        out.safety = stricter(out.safety, s);
    }
    if let Some(origins) = &ai.origins {
        out.origins.retain(|m| origins.contains(m));
    }
    out
}

/// Promote to the stricter of two safety profiles. Strict > Standard
/// > Minimal. Used to merge user override onto manifest.
fn stricter(a: AiSafety, b: AiSafety) -> AiSafety {
    use AiSafety::*;
    match (a, b) {
        (Strict, _) | (_, Strict) => Strict,
        (Standard, _) | (_, Standard) => Standard,
        (Minimal, Minimal) => Minimal,
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/ai/overrides.rs"
    ));
}
