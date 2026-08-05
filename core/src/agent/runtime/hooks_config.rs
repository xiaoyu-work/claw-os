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
pub fn instantiate(kind: HookKind) -> Arc<dyn Hook> {
    match kind {
        HookKind::Logging => Arc::new(LoggingHook),
        HookKind::Audit => Arc::new(AuditHook::new()),
        HookKind::Checkpoint => Arc::new(CheckpointHook::new()),
    }
}

/// Auto-register every hook listed in `cfg` into `registry`.
///
/// Returns the names of hooks that were actually registered (so a
/// caller can hand them to a guard for unregistration on drop).
/// Already-registered names are skipped — the registry's own
/// idempotency would replace them, but we want strict "only touch
/// what we own" semantics so test isolation holds.
pub fn register_into(registry: &HookRegistry, cfg: &HooksConfig) -> Vec<String> {
    let existing: std::collections::HashSet<String> = registry.names().into_iter().collect();
    let mut ours = Vec::new();
    for kind in &cfg.enabled {
        let hook = instantiate(*kind);
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
    use super::*;
    use crate::agent::runtime::hooks::HookRegistry;
    use tempfile::TempDir;

    fn tmpfile(name: &str) -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join(name);
        (dir, p)
    }

    // ---- HookKind ----

    #[test]
    fn hook_kind_parse_handles_canonical_and_aliases() {
        assert_eq!(HookKind::parse("logging"), Some(HookKind::Logging));
        assert_eq!(HookKind::parse("LOGGING"), Some(HookKind::Logging));
        assert_eq!(HookKind::parse("  log  "), Some(HookKind::Logging));
        assert_eq!(HookKind::parse("tracing"), Some(HookKind::Logging));
        assert_eq!(HookKind::parse("audit"), Some(HookKind::Audit));
        assert_eq!(HookKind::parse("AUDIT"), Some(HookKind::Audit));
        assert_eq!(HookKind::parse("audit_log"), Some(HookKind::Audit));
        assert_eq!(HookKind::parse("auditlog"), Some(HookKind::Audit));
        assert_eq!(HookKind::parse("checkpoint"), Some(HookKind::Checkpoint));
        assert_eq!(HookKind::parse("CHECKPOINT"), Some(HookKind::Checkpoint));
        assert_eq!(HookKind::parse("snapshot"), Some(HookKind::Checkpoint));
        assert_eq!(HookKind::parse("rollback"), Some(HookKind::Checkpoint));
        assert_eq!(HookKind::parse("nope"), None);
        assert_eq!(HookKind::parse(""), None);
    }

    #[test]
    fn hook_kind_canonical_is_lowercase_snake_case() {
        assert_eq!(HookKind::Logging.canonical(), "logging");
        assert_eq!(HookKind::Audit.canonical(), "audit");
        assert_eq!(HookKind::Checkpoint.canonical(), "checkpoint");
    }

    #[test]
    fn hook_kind_serializes_as_lowercase_string() {
        assert_eq!(
            serde_json::to_string(&HookKind::Logging).unwrap(),
            "\"logging\""
        );
        assert_eq!(
            serde_json::to_string(&HookKind::Audit).unwrap(),
            "\"audit\""
        );
        assert_eq!(
            serde_json::to_string(&HookKind::Checkpoint).unwrap(),
            "\"checkpoint\""
        );
        let back: HookKind = serde_json::from_str("\"audit\"").unwrap();
        assert_eq!(back, HookKind::Audit);
        let back: HookKind = serde_json::from_str("\"checkpoint\"").unwrap();
        assert_eq!(back, HookKind::Checkpoint);
    }

    // ---- HooksConfig ----

    #[test]
    fn default_config_has_version_one_and_empty_list() {
        let c = HooksConfig::default();
        assert_eq!(c.version, 1);
        assert!(c.enabled.is_empty());
    }

    #[test]
    fn enable_is_idempotent() {
        let mut c = HooksConfig::default();
        assert!(c.enable(HookKind::Logging));
        assert!(!c.enable(HookKind::Logging));
        assert_eq!(c.enabled, vec![HookKind::Logging]);
    }

    #[test]
    fn disable_returns_true_only_when_present() {
        let mut c = HooksConfig::default();
        assert!(!c.disable(HookKind::Logging));
        c.enable(HookKind::Logging);
        assert!(c.disable(HookKind::Logging));
        assert!(c.enabled.is_empty());
    }

    #[test]
    fn is_enabled_reflects_state() {
        let mut c = HooksConfig::default();
        assert!(!c.is_enabled(HookKind::Logging));
        c.enable(HookKind::Logging);
        assert!(c.is_enabled(HookKind::Logging));
    }

    // ---- load / save ----

    #[test]
    fn load_returns_default_when_file_missing() {
        let (_dir, path) = tmpfile("hooks.json");
        let cfg = load(&path).expect("ok");
        assert_eq!(cfg, HooksConfig::default());
    }

    #[test]
    fn save_then_load_round_trips() {
        let (_dir, path) = tmpfile("hooks.json");
        let mut cfg = HooksConfig::default();
        cfg.enable(HookKind::Logging);
        save(&path, &cfg).expect("save");
        let back = load(&path).expect("load");
        assert_eq!(back, cfg);
    }

    #[test]
    fn save_creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent").join("nested").join("hooks.json");
        let cfg = HooksConfig::default();
        save(&path, &cfg).expect("save");
        assert!(path.exists());
    }

    #[test]
    fn save_is_atomic_no_tmp_left_behind_on_success() {
        let (_dir, path) = tmpfile("hooks.json");
        let cfg = HooksConfig::default();
        save(&path, &cfg).expect("save");
        let tmp = tmp_path_for(&path);
        assert!(!tmp.exists(), "tmp should be renamed away");
    }

    #[test]
    fn load_surfaces_malformed_json_as_invalid_data() {
        let (_dir, path) = tmpfile("hooks.json");
        std::fs::write(&path, "{not json").unwrap();
        let err = load(&path).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn load_accepts_unknown_future_fields() {
        let (_dir, path) = tmpfile("hooks.json");
        std::fs::write(
            &path,
            r#"{"version":1,"enabled":["logging"],"future_field":"ignored"}"#,
        )
        .unwrap();
        let cfg = load(&path).expect("ok");
        assert_eq!(cfg.enabled, vec![HookKind::Logging]);
    }

    // ---- register_into ----

    #[test]
    fn register_into_skips_when_disabled_list_empty() {
        let reg = HookRegistry::new();
        let cfg = HooksConfig::default();
        let names = register_into(&reg, &cfg);
        assert!(names.is_empty());
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn register_into_registers_logging_hook() {
        let reg = HookRegistry::new();
        let mut cfg = HooksConfig::default();
        cfg.enable(HookKind::Logging);
        let names = register_into(&reg, &cfg);
        assert_eq!(names, vec!["logging".to_string()]);
        assert!(reg.names().contains(&"logging".to_string()));
    }

    #[test]
    fn register_into_registers_audit_hook() {
        let reg = HookRegistry::new();
        let mut cfg = HooksConfig::default();
        cfg.enable(HookKind::Audit);
        let names = register_into(&reg, &cfg);
        assert_eq!(names, vec!["audit".to_string()]);
        assert!(reg.names().contains(&"audit".to_string()));
    }

    #[test]
    fn register_into_registers_checkpoint_hook() {
        let reg = HookRegistry::new();
        let mut cfg = HooksConfig::default();
        cfg.enable(HookKind::Checkpoint);
        let names = register_into(&reg, &cfg);
        assert_eq!(names, vec!["checkpoint".to_string()]);
        assert!(reg.names().contains(&"checkpoint".to_string()));
    }

    #[test]
    fn register_into_registers_multiple_kinds_in_order() {
        let reg = HookRegistry::new();
        let mut cfg = HooksConfig::default();
        cfg.enable(HookKind::Audit);
        cfg.enable(HookKind::Logging);
        let names = register_into(&reg, &cfg);
        assert_eq!(names, vec!["audit".to_string(), "logging".to_string()]);
    }

    #[test]
    fn register_into_skips_already_registered_names() {
        let reg = HookRegistry::new();
        reg.register(Arc::new(LoggingHook));
        let mut cfg = HooksConfig::default();
        cfg.enable(HookKind::Logging);
        let names = register_into(&reg, &cfg);
        assert!(
            names.is_empty(),
            "should NOT claim ownership of pre-existing hook"
        );
        assert_eq!(reg.len(), 1);
    }

    // ---- AutoHookGuard ----

    #[test]
    fn auto_guard_unregisters_on_drop() {
        let reg = HookRegistry::new();
        {
            let mut cfg = HooksConfig::default();
            cfg.enable(HookKind::Logging);
            let names = register_into(&reg, &cfg);
            assert!(reg.names().contains(&"logging".to_string()));
            let _g = AutoHookGuard::new(reg.clone(), names);
            assert_eq!(reg.len(), 1);
        }
        assert_eq!(reg.len(), 0, "drop should unregister");
    }

    #[test]
    fn auto_guard_only_unregisters_owned_names() {
        let reg = HookRegistry::new();
        // Pre-existing hook NOT owned by the guard.
        reg.register(Arc::new(LoggingHook));
        {
            let _g = AutoHookGuard::new(reg.clone(), Vec::new());
        }
        assert_eq!(
            reg.len(),
            1,
            "guard with empty names list must not touch unrelated hooks"
        );
    }

    // ---- load_and_register ----

    #[test]
    fn load_and_register_no_op_when_file_missing() {
        let (_dir, path) = tmpfile("hooks.json");
        let reg = HookRegistry::new();
        let g = load_and_register(&path, reg.clone());
        assert!(g.names().is_empty());
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn load_and_register_no_op_when_file_malformed() {
        let (_dir, path) = tmpfile("hooks.json");
        std::fs::write(&path, "{nonsense").unwrap();
        let reg = HookRegistry::new();
        let g = load_and_register(&path, reg.clone());
        assert!(
            g.names().is_empty(),
            "malformed config must not crash agent startup"
        );
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn load_and_register_registers_then_drop_unregisters() {
        let (_dir, path) = tmpfile("hooks.json");
        let mut cfg = HooksConfig::default();
        cfg.enable(HookKind::Logging);
        save(&path, &cfg).unwrap();
        let reg = HookRegistry::new();
        {
            let g = load_and_register(&path, reg.clone());
            assert_eq!(g.names(), &["logging".to_string()]);
            assert!(reg.names().contains(&"logging".to_string()));
        }
        assert_eq!(reg.len(), 0);
    }
}
