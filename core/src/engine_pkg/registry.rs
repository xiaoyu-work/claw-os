//! Persistent registry of installed engine versions
//! (`<engines_dir>/engines.json`).
//!
//! Single source of truth for:
//!   - which engines are installed (per-engine list of versions)
//!   - which version is `active` (loaded by cos at runtime)
//!   - which version was `previous` (rollback target)
//!   - whether the engine is `pinned` (refuse auto-update)
//!   - install metadata (when, from where, sha256)
//!
//! All mutations are written through `save()` which uses
//! atomic-rename-on-temp-file so a crash mid-write can never leave a
//! corrupted index.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnginesIndex {
    #[serde(default = "default_schema_version")]
    pub version: u32,
    #[serde(default)]
    pub engines: BTreeMap<String, EngineEntry>,
}

fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EngineEntry {
    #[serde(default)]
    pub active: String,
    #[serde(default)]
    pub previous: String,
    #[serde(default)]
    pub installed: Vec<InstalledVersion>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default = "default_channel")]
    pub channel: String,
    #[serde(default)]
    pub accelerator: String,
    #[serde(default)]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked: Option<DateTime<Utc>>,
}

fn default_channel() -> String {
    "release".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledVersion {
    pub version: String,
    pub installed_at: DateTime<Utc>,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub sha256: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("engine \"{0}\" not registered")]
    UnknownEngine(String),
    #[error("version \"{version}\" not installed for engine \"{engine}\"")]
    UnknownVersion { engine: String, version: String },
    #[error("version \"{version}\" already installed for engine \"{engine}\"")]
    DuplicateVersion { engine: String, version: String },
    #[error("cannot uninstall the active version \"{version}\" of \"{engine}\" (activate another version first)")]
    UninstallActive { engine: String, version: String },
    #[error("engine \"{0}\" is pinned (use `cos engine unpin {0}` first or pass --force)")]
    Pinned(String),
}

impl EnginesIndex {
    pub fn empty() -> Self {
        Self {
            version: SCHEMA_VERSION,
            engines: BTreeMap::new(),
        }
    }

    pub fn path() -> PathBuf {
        super::paths::engines_index_path()
    }

    pub fn load_or_default() -> Result<Self, RegistryError> {
        let p = Self::path();
        if !p.exists() {
            return Ok(Self::empty());
        }
        let bytes = std::fs::read(&p)?;
        let mut idx: Self = serde_json::from_slice(&bytes)?;
        if idx.version == 0 {
            idx.version = SCHEMA_VERSION;
        }
        Ok(idx)
    }

    /// Race-free load + mutate + save under a file lock.
    ///
    /// Two `cos engine` invocations running concurrently against the
    /// same `engines.json` would otherwise read the same state, each
    /// apply their mutation locally, and have the second writer's
    /// `save()` clobber the first writer's record (lost-update).
    /// Routing all writes through this helper serializes the RMW
    /// against the [`crate::filelock`] sentinel so the second writer
    /// observes the first writer's commit and applies its change on
    /// top.
    ///
    /// The closure returns whatever auxiliary value the caller wants
    /// to thread out (the previously active version, the list of
    /// pruned tags, etc.). Callers should treat any side effects
    /// inside `f` *other than* mutating `self` as best-effort:
    /// rolling back rmtree on save failure is the caller's problem.
    pub fn update_with<F, T>(f: F) -> Result<T, RegistryError>
    where
        F: FnOnce(&mut Self) -> Result<T, RegistryError>,
    {
        use std::cell::RefCell;
        let p = Self::path();
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let out: RefCell<Option<Result<T, RegistryError>>> = RefCell::new(None);
        crate::filelock::update_locked::<_, RegistryError>(&p, |current| {
            let mut idx: Self = match current {
                Some(s) if !s.trim().is_empty() => serde_json::from_str(&s)?,
                _ => Self::empty(),
            };
            if idx.version == 0 {
                idx.version = SCHEMA_VERSION;
            }
            let res = f(&mut idx);
            let was_ok = res.is_ok();
            *out.borrow_mut() = Some(res);
            if !was_ok {
                // Surface the closure error through the file-lock
                // wrapper so we don't overwrite the file on failure.
                return Err(RegistryError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "update_with closure failed",
                )));
            }
            Ok(serde_json::to_string_pretty(&idx)?)
        })
        .map_err(|e| match e {
            crate::filelock::UpdateLockError::Io(msg) => {
                RegistryError::Io(std::io::Error::other(msg))
            }
            crate::filelock::UpdateLockError::Transform(inner) => inner,
        })?;
        // SAFETY: on `Ok` path the closure always set the cell.
        out.into_inner().expect("update closure ran")
    }

    pub fn save(&self) -> Result<(), RegistryError> {
        // Take the file lock so concurrent writers can't clobber each
        // other. We do *not* re-read the file under the lock here —
        // that would silently throw away the caller's mutations.
        // Callers that need read-modify-write atomicity must use
        // [`Self::update_with`].
        let p = Self::path();
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        let body = String::from_utf8(bytes)
            .map_err(|e| RegistryError::Io(std::io::Error::other(e)))?;
        crate::filelock::update_locked::<_, RegistryError>(&p, |_existing| Ok(body.clone()))
            .map_err(|e| match e {
                crate::filelock::UpdateLockError::Io(msg) => {
                    RegistryError::Io(std::io::Error::other(msg))
                }
                crate::filelock::UpdateLockError::Transform(inner) => inner,
            })
    }

    pub fn entry(&self, engine: &str) -> Option<&EngineEntry> {
        self.engines.get(engine)
    }

    pub fn entry_mut(&mut self, engine: &str) -> &mut EngineEntry {
        let e = self.engines.entry(engine.to_string()).or_default();
        if e.channel.is_empty() {
            e.channel = default_channel();
        }
        e
    }

    pub fn record_install(
        &mut self,
        engine: &str,
        version: InstalledVersion,
    ) -> Result<(), RegistryError> {
        let entry = self.entry_mut(engine);
        if entry.installed.iter().any(|v| v.version == version.version) {
            return Err(RegistryError::DuplicateVersion {
                engine: engine.to_string(),
                version: version.version.clone(),
            });
        }
        entry.installed.push(version);
        Ok(())
    }

    pub fn activate(&mut self, engine: &str, version: &str) -> Result<String, RegistryError> {
        let entry = self
            .engines
            .get_mut(engine)
            .ok_or_else(|| RegistryError::UnknownEngine(engine.to_string()))?;
        if !entry.installed.iter().any(|v| v.version == version) {
            return Err(RegistryError::UnknownVersion {
                engine: engine.to_string(),
                version: version.to_string(),
            });
        }
        let prior = std::mem::take(&mut entry.active);
        if prior != version {
            entry.previous = prior.clone();
        }
        entry.active = version.to_string();
        Ok(prior)
    }

    pub fn rollback(&mut self, engine: &str) -> Result<(String, String), RegistryError> {
        let entry = self
            .engines
            .get_mut(engine)
            .ok_or_else(|| RegistryError::UnknownEngine(engine.to_string()))?;
        if entry.previous.is_empty() {
            return Err(RegistryError::UnknownVersion {
                engine: engine.to_string(),
                version: "<no previous version>".to_string(),
            });
        }
        if !entry.installed.iter().any(|v| v.version == entry.previous) {
            return Err(RegistryError::UnknownVersion {
                engine: engine.to_string(),
                version: entry.previous.clone(),
            });
        }
        std::mem::swap(&mut entry.active, &mut entry.previous);
        Ok((entry.active.clone(), entry.previous.clone()))
    }

    pub fn set_pinned(&mut self, engine: &str, pinned: bool) -> Result<(), RegistryError> {
        let entry = self
            .engines
            .get_mut(engine)
            .ok_or_else(|| RegistryError::UnknownEngine(engine.to_string()))?;
        entry.pinned = pinned;
        Ok(())
    }

    pub fn uninstall(&mut self, engine: &str, version: &str) -> Result<(), RegistryError> {
        let entry = self
            .engines
            .get_mut(engine)
            .ok_or_else(|| RegistryError::UnknownEngine(engine.to_string()))?;
        if entry.active == version {
            return Err(RegistryError::UninstallActive {
                engine: engine.to_string(),
                version: version.to_string(),
            });
        }
        let before = entry.installed.len();
        entry.installed.retain(|v| v.version != version);
        if entry.installed.len() == before {
            return Err(RegistryError::UnknownVersion {
                engine: engine.to_string(),
                version: version.to_string(),
            });
        }
        if entry.previous == version {
            entry.previous.clear();
        }
        // **Do not rmtree here.** We want the on-disk index to commit
        // *before* we delete bytes, so a crash mid-uninstall leaves
        // the registry in a consistent state (the version is gone
        // from the registry; the directory is now garbage and is
        // safe to remove manually or on next gc). Callers are
        // expected to invoke [`Self::cleanup_uninstalled_dir`] after
        // a successful `save()` (or `update_with()` will batch both
        // under the lock for them).
        Ok(())
    }

    /// Remove the on-disk install directory for `engine@version`.
    /// Idempotent — missing directory is not an error. Intended to
    /// run **after** the registry mutation has been persisted, so
    /// a crash never leaves the registry pointing at a half-deleted
    /// install.
    pub fn cleanup_uninstalled_dir(engine: &str, version: &str) -> Result<(), RegistryError> {
        let dir = super::paths::engine_version_dir(engine, version);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }

    pub fn gc(&mut self, engine: &str, keep: usize) -> Result<Vec<String>, RegistryError> {
        let entry = self
            .engines
            .get_mut(engine)
            .ok_or_else(|| RegistryError::UnknownEngine(engine.to_string()))?;
        let keep_set: std::collections::HashSet<String> = entry
            .installed
            .iter()
            .rev()
            .take(keep)
            .map(|v| v.version.clone())
            .chain([entry.active.clone(), entry.previous.clone()])
            .filter(|v| !v.is_empty())
            .collect();
        let mut removed = Vec::new();
        let installed = std::mem::take(&mut entry.installed);
        let mut survivors = Vec::new();
        for v in installed {
            if keep_set.contains(&v.version) {
                survivors.push(v);
            } else {
                removed.push(v.version);
            }
        }
        entry.installed = survivors;
        // Don't rmtree here either — same reasoning as `uninstall`.
        // Callers are expected to call [`Self::cleanup_uninstalled_dir`]
        // for each returned version after a successful save.
        Ok(removed)
    }

    pub fn to_list_view(&self) -> Value {
        let mut out = serde_json::Map::new();
        for engine in super::KNOWN_ENGINES.iter() {
            let entry = self.engines.get(*engine);
            let installed: Vec<&str> = entry
                .map(|e| e.installed.iter().map(|v| v.version.as_str()).collect())
                .unwrap_or_default();
            out.insert(
                (*engine).to_string(),
                json!({
                    "active": entry.map(|e| e.active.clone()).unwrap_or_default(),
                    "previous": entry.map(|e| e.previous.clone()).unwrap_or_default(),
                    "installed": installed,
                    "pinned": entry.is_some_and(|e| e.pinned),
                    "channel": entry.map(|e| e.channel.clone()).unwrap_or_else(default_channel),
                }),
            );
        }
        Value::Object(out)
    }

    pub fn info_view(&self, engine: &str) -> Value {
        let entry = self.engines.get(engine);
        let dir = super::paths::engine_dir(engine);
        json!({
            "engine": engine,
            "engine_dir": dir.display().to_string(),
            "active": entry.map(|e| e.active.clone()).unwrap_or_default(),
            "previous": entry.map(|e| e.previous.clone()).unwrap_or_default(),
            "pinned": entry.is_some_and(|e| e.pinned),
            "channel": entry.map(|e| e.channel.clone()).unwrap_or_else(default_channel),
            "accelerator": entry.map(|e| e.accelerator.clone()).unwrap_or_default(),
            "source": entry.map(|e| e.source.clone()).unwrap_or_default(),
            "last_checked": entry.and_then(|e| e.last_checked.map(|t| t.to_rfc3339())),
            "installed": entry.map(|e| {
                e.installed.iter().map(|v| {
                    json!({
                        "version": v.version,
                        "installed_at": v.installed_at.to_rfc3339(),
                        "bytes": v.bytes,
                        "source": v.source,
                        "sha256": v.sha256,
                        "path": super::paths::engine_version_dir(engine, &v.version).display().to_string(),
                    })
                }).collect::<Vec<_>>()
            }).unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnginesDirGuard {
        _td: tempfile::TempDir,
    }

    impl EnginesDirGuard {
        fn new() -> Self {
            let td = tempfile::Builder::new()
                .prefix("cos-engines-test-")
                .tempdir()
                .unwrap();
            super::super::paths::set_engines_dir_override(Some(td.path().to_path_buf()));
            Self { _td: td }
        }
    }

    impl Drop for EnginesDirGuard {
        fn drop(&mut self) {
            super::super::paths::set_engines_dir_override(None);
        }
    }

    fn fake_install(version: &str) -> InstalledVersion {
        InstalledVersion {
            version: version.into(),
            installed_at: Utc::now(),
            bytes: 1024,
            source: format!("local:fake-{version}.zip"),
            sha256: String::new(),
        }
    }

    fn lay_down_dir(engine: &str, version: &str) {
        let p = super::super::paths::engine_version_dir(engine, version).join("lib");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("placeholder"), b"x").unwrap();
    }

    #[test]
    fn load_or_default_creates_empty_when_missing() {
        let _g = EnginesDirGuard::new();
        let idx = EnginesIndex::load_or_default().unwrap();
        assert!(idx.engines.is_empty());
        assert_eq!(idx.version, SCHEMA_VERSION);
    }

    #[test]
    fn save_then_load_round_trips() {
        let _g = EnginesDirGuard::new();
        let mut idx = EnginesIndex::empty();
        idx.record_install("llama-cpp", fake_install("b4001"))
            .unwrap();
        idx.save().unwrap();
        let reloaded = EnginesIndex::load_or_default().unwrap();
        let entry = reloaded.entry("llama-cpp").unwrap();
        assert_eq!(entry.installed.len(), 1);
        assert_eq!(entry.installed[0].version, "b4001");
    }

    #[test]
    fn record_install_rejects_duplicates() {
        let _g = EnginesDirGuard::new();
        let mut idx = EnginesIndex::empty();
        idx.record_install("llama-cpp", fake_install("b4001"))
            .unwrap();
        let err = idx
            .record_install("llama-cpp", fake_install("b4001"))
            .unwrap_err();
        assert!(matches!(err, RegistryError::DuplicateVersion { .. }));
    }

    #[test]
    fn activate_sets_active_and_moves_previous() {
        let _g = EnginesDirGuard::new();
        let mut idx = EnginesIndex::empty();
        idx.record_install("llama-cpp", fake_install("b3950"))
            .unwrap();
        idx.record_install("llama-cpp", fake_install("b4001"))
            .unwrap();
        let prior = idx.activate("llama-cpp", "b3950").unwrap();
        assert_eq!(prior, "");
        let prior = idx.activate("llama-cpp", "b4001").unwrap();
        assert_eq!(prior, "b3950");
        let entry = idx.entry("llama-cpp").unwrap();
        assert_eq!(entry.active, "b4001");
        assert_eq!(entry.previous, "b3950");
    }

    #[test]
    fn activate_rejects_unknown_version() {
        let _g = EnginesDirGuard::new();
        let mut idx = EnginesIndex::empty();
        idx.record_install("llama-cpp", fake_install("b3950"))
            .unwrap();
        let err = idx.activate("llama-cpp", "b9999").unwrap_err();
        assert!(matches!(err, RegistryError::UnknownVersion { .. }));
    }

    #[test]
    fn rollback_swaps_active_and_previous() {
        let _g = EnginesDirGuard::new();
        let mut idx = EnginesIndex::empty();
        idx.record_install("llama-cpp", fake_install("b3950"))
            .unwrap();
        idx.record_install("llama-cpp", fake_install("b4001"))
            .unwrap();
        idx.activate("llama-cpp", "b3950").unwrap();
        idx.activate("llama-cpp", "b4001").unwrap();
        let (active, previous) = idx.rollback("llama-cpp").unwrap();
        assert_eq!(active, "b3950");
        assert_eq!(previous, "b4001");
        let (active, previous) = idx.rollback("llama-cpp").unwrap();
        assert_eq!(active, "b4001");
        assert_eq!(previous, "b3950");
    }

    #[test]
    fn rollback_errors_with_no_previous() {
        let _g = EnginesDirGuard::new();
        let mut idx = EnginesIndex::empty();
        idx.record_install("llama-cpp", fake_install("b4001"))
            .unwrap();
        idx.activate("llama-cpp", "b4001").unwrap();
        let err = idx.rollback("llama-cpp").unwrap_err();
        assert!(matches!(err, RegistryError::UnknownVersion { .. }));
    }

    #[test]
    fn uninstall_refuses_active_version() {
        let _g = EnginesDirGuard::new();
        let mut idx = EnginesIndex::empty();
        idx.record_install("llama-cpp", fake_install("b4001"))
            .unwrap();
        idx.activate("llama-cpp", "b4001").unwrap();
        let err = idx.uninstall("llama-cpp", "b4001").unwrap_err();
        assert!(matches!(err, RegistryError::UninstallActive { .. }));
    }

    #[test]
    fn uninstall_clears_previous_when_target_is_previous() {
        let _g = EnginesDirGuard::new();
        let mut idx = EnginesIndex::empty();
        idx.record_install("llama-cpp", fake_install("a")).unwrap();
        idx.record_install("llama-cpp", fake_install("b")).unwrap();
        lay_down_dir("llama-cpp", "a");
        lay_down_dir("llama-cpp", "b");
        idx.activate("llama-cpp", "a").unwrap();
        idx.activate("llama-cpp", "b").unwrap();
        idx.uninstall("llama-cpp", "a").unwrap();
        let entry = idx.entry("llama-cpp").unwrap();
        assert_eq!(entry.active, "b");
        assert!(entry.previous.is_empty());
        assert_eq!(entry.installed.len(), 1);
    }

    #[test]
    fn gc_keeps_active_previous_and_last_n() {
        let _g = EnginesDirGuard::new();
        let mut idx = EnginesIndex::empty();
        for v in &["v1", "v2", "v3", "v4", "v5"] {
            idx.record_install("llama-cpp", fake_install(v)).unwrap();
            lay_down_dir("llama-cpp", v);
        }
        idx.activate("llama-cpp", "v1").unwrap();
        idx.activate("llama-cpp", "v3").unwrap();
        let removed = idx.gc("llama-cpp", 2).unwrap();
        assert_eq!(removed, vec!["v2".to_string()]);
        let entry = idx.entry("llama-cpp").unwrap();
        let kept: Vec<&str> = entry.installed.iter().map(|v| v.version.as_str()).collect();
        assert_eq!(kept, vec!["v1", "v3", "v4", "v5"]);
        // gc no longer rmtree's; caller is expected to invoke
        // `cleanup_uninstalled_dir` after a successful save.
        assert!(super::super::paths::engine_version_dir("llama-cpp", "v2").exists());
        EnginesIndex::cleanup_uninstalled_dir("llama-cpp", "v2").unwrap();
        assert!(!super::super::paths::engine_version_dir("llama-cpp", "v2").exists());
        assert!(super::super::paths::engine_version_dir("llama-cpp", "v3").exists());
    }

    #[test]
    fn pin_unpin_round_trip() {
        let _g = EnginesDirGuard::new();
        let mut idx = EnginesIndex::empty();
        idx.record_install("llama-cpp", fake_install("b4001"))
            .unwrap();
        idx.set_pinned("llama-cpp", true).unwrap();
        assert!(idx.entry("llama-cpp").unwrap().pinned);
        idx.set_pinned("llama-cpp", false).unwrap();
        assert!(!idx.entry("llama-cpp").unwrap().pinned);
    }

    #[test]
    fn save_uses_atomic_rename_visible_to_load() {
        let _g = EnginesDirGuard::new();
        let mut idx = EnginesIndex::empty();
        idx.record_install("ort", fake_install("1.22.0")).unwrap();
        idx.save().unwrap();
        let p = EnginesIndex::path();
        assert!(p.exists());
        let tmp = p.with_extension("json.tmp");
        assert!(!tmp.exists());
    }

    #[test]
    fn list_view_includes_all_known_engines_even_when_empty() {
        let _g = EnginesDirGuard::new();
        let idx = EnginesIndex::empty();
        let v = idx.to_list_view();
        let obj = v.as_object().unwrap();
        for engine in super::super::KNOWN_ENGINES {
            assert!(obj.contains_key(*engine));
        }
    }

    #[test]
    fn info_view_returns_structured_metadata() {
        let _g = EnginesDirGuard::new();
        let mut idx = EnginesIndex::empty();
        idx.record_install("llama-cpp", fake_install("b4001"))
            .unwrap();
        idx.activate("llama-cpp", "b4001").unwrap();
        let v = idx.info_view("llama-cpp");
        assert_eq!(v["engine"], "llama-cpp");
        assert_eq!(v["active"], "b4001");
        assert_eq!(v["installed"].as_array().unwrap().len(), 1);
    }
}
