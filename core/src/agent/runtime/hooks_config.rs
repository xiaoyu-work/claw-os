//! Persistent configuration for built-in agent hooks.
//!
//! `data_dir/agent/hooks.json` lists which built-in hook kinds
//! (currently only `logging`) should auto-register at the start of
//! every `cos agent ask` / `cos agent chat` invocation. This is the
//! piece that makes the hooks system useful from the CLI — without
//! it, a `cos agent hooks enable logging` command would only affect
//! the single short-lived process that ran the command.
//!
//! Schema (forward-compatible — unknown fields preserved):
//!
//! ```json
//! { "version": 1, "enabled": ["logging"] }
//! ```
//!
//! On disk, the file is written atomically (`<path>.tmp` + rename)
//! so a crash mid-write can never leave a half-written JSON blob.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::agent::runtime::hooks::{AuditHook, CheckpointHook, Hook, HookRegistry, LoggingHook};

/// Built-in hook kinds that can be persistently enabled.
///
/// Exhaustive `match` is enforced — adding a kind here forces an
/// instantiation arm in [`instantiate`] and a normalisation arm in
/// [`HookKind::canonical`], so a forgotten wire-up fails to compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookKind {
    Logging,
    Audit,
    Checkpoint,
}

impl HookKind {
    /// Parse a CLI/config string. Case-insensitive, accepts the
    /// `snake_case` JSON form and a few common aliases.
    pub fn parse(s: &str) -> Option<Self> {
        let lower = s.trim().to_ascii_lowercase();
        match lower.as_str() {
            "logging" | "log" | "tracing" => Some(Self::Logging),
            "audit" | "audit_log" | "auditlog" => Some(Self::Audit),
            "checkpoint" | "snapshot" | "rollback" => Some(Self::Checkpoint),
            _ => None,
        }
    }

    /// Canonical (lowercase, snake_case) name — also the JSON form.
    pub fn canonical(self) -> &'static str {
        match self {
            Self::Logging => "logging",
            Self::Audit => "audit",
            Self::Checkpoint => "checkpoint",
        }
    }
}

/// On-disk shape of `hooks.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HooksConfig {
    /// Schema version. Bumped only on incompatible changes.
    #[serde(default = "default_version")]
    pub version: u32,
    /// Hook kinds to auto-register on every agent invocation.
    /// Order is preserved; duplicates are normalised away on save.
    #[serde(default)]
    pub enabled: Vec<HookKind>,
}

fn default_version() -> u32 {
    1
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            version: 1,
            enabled: Vec::new(),
        }
    }
}

impl HooksConfig {
    /// Returns true if the given kind is in the enabled list.
    pub fn is_enabled(&self, kind: HookKind) -> bool {
        self.enabled.contains(&kind)
    }

    /// Add a kind to the enabled list (no-op if already present).
    /// Returns true if it was newly added.
    pub fn enable(&mut self, kind: HookKind) -> bool {
        if self.is_enabled(kind) {
            false
        } else {
            self.enabled.push(kind);
            true
        }
    }

    /// Remove a kind from the enabled list (no-op if absent).
    /// Returns true if it was actually removed.
    pub fn disable(&mut self, kind: HookKind) -> bool {
        let before = self.enabled.len();
        self.enabled.retain(|k| *k != kind);
        before != self.enabled.len()
    }
}

/// Read the persistent hooks config from disk.
///
/// Returns the default (empty) config if the file is missing — that
/// is the expected state on a fresh install. A malformed file is
/// surfaced as `Err` so the operator can fix it; we deliberately do
/// not silently overwrite a corrupted config with defaults (data
/// loss).
pub fn load(path: &Path) -> std::io::Result<HooksConfig> {
    match fs::read_to_string(path) {
        Ok(s) => serde_json::from_str::<HooksConfig>(&s).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("hooks config at {}: {e}", path.display()),
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HooksConfig::default()),
        Err(e) => Err(e),
    }
}

/// Write the hooks config to disk atomically.
///
/// Creates the parent directory if missing. On Windows, `rename`
/// fails when the destination already exists, so we delete the old
/// file first — this keeps the failure window microscopic but is
/// not crash-perfect on that platform. The Unix path is fully
/// crash-safe via standard rename-over-existing semantics.
pub fn save(path: &Path, cfg: &HooksConfig) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = tmp_path_for(path);
    let body = serde_json::to_string_pretty(cfg).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, format!("serialize: {e}"))
    })?;
    fs::write(&tmp, body)?;
    #[cfg(windows)]
    {
        let _ = fs::remove_file(path);
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

fn tmp_path_for(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

/// Materialise a hook instance for a given kind.
///
/// Each kind has exactly one canonical implementation. New kinds
/// must add an arm here — the `match` is exhaustive so the compiler
/// catches forgotten wire-ups.
pub fn instantiate_at(kind: HookKind, audit_path: &Path) -> Arc<dyn Hook> {
    match kind {
        HookKind::Logging => Arc::new(LoggingHook),
        HookKind::Audit => Arc::new(AuditHook::at(audit_path)),
        HookKind::Checkpoint => Arc::new(CheckpointHook::with_overrides(
            Arc::new(crate::agent::runtime::hooks::ProductionCheckpointCreator),
            audit_path,
            crate::agent::runtime::hooks::default_dangerous_tools(),
        )),
    }
}

pub fn instantiate(kind: HookKind) -> Arc<dyn Hook> {
    instantiate_at(kind, &crate::paths::agent_audit_log_path())
}

/// Auto-register every hook listed in `cfg` into `registry`.
///
/// Returns the names of hooks that were actually registered (so a
/// caller can hand them to a guard for unregistration on drop).
/// Already-registered names are skipped — the registry's own
/// idempotency would replace them, but we want strict "only touch
/// what we own" semantics so test isolation holds.
pub fn register_into(registry: &HookRegistry, cfg: &HooksConfig) -> Vec<String> {
    register_into_at(registry, cfg, &crate::paths::agent_audit_log_path())
}

pub fn register_into_at(
    registry: &HookRegistry,
    cfg: &HooksConfig,
    audit_path: &Path,
) -> Vec<String> {
    let existing: std::collections::HashSet<String> = registry.names().into_iter().collect();
    let mut ours = Vec::new();
    for kind in &cfg.enabled {
        let hook = instantiate_at(*kind, audit_path);
        let name = hook.name().to_string();
        if existing.contains(&name) {
            continue;
        }
        registry.register(hook);
        ours.push(name);
    }
    ours
}

/// RAII guard that unregisters hook names on drop. Used to keep
/// auto-loaded hooks scoped to a single `cos agent ask` invocation
/// so repeated invocations / tests don't accumulate registrations.
pub struct AutoHookGuard {
    registry: HookRegistry,
    names: Vec<String>,
}

impl AutoHookGuard {
    pub fn new(registry: HookRegistry, names: Vec<String>) -> Self {
        Self { registry, names }
    }

    /// Names this guard will unregister on drop.
    pub fn names(&self) -> &[String] {
        &self.names
    }
}

impl Drop for AutoHookGuard {
    fn drop(&mut self) {
        for n in &self.names {
            self.registry.unregister(n);
        }
    }
}

/// Convenience: `load(path) -> register_into(registry) -> guard`.
/// IO errors are swallowed and surfaced as a no-op guard so the
/// agent never refuses to run because the hooks file is missing or
/// malformed.
pub fn load_and_register(path: &Path, registry: HookRegistry) -> AutoHookGuard {
    let cfg = load(path).unwrap_or_default();
    let names = register_into(&registry, &cfg);
    AutoHookGuard::new(registry, names)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/runtime/hooks_config.rs"
    ));
}
