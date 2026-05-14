//! Discovery of agent-API MCP servers from XDG sidecar manifests.
//!
//! ## Why
//!
//! The kernel already knows how to attach MCP servers it's been told
//! about in `config.json` (`[[agent.mcp_servers]]`). This module lets
//! externally installed open-source software show up without anyone
//! editing config: an adapter package drops a tiny JSON sidecar in
//! `/usr/share/claw/agent-api/<id>.json` (or the per-user equivalent
//! under `$XDG_DATA_HOME`) and the agent picks it up at startup.
//!
//! ## Manifest schema (`claw.agent-api/v1`)
//!
//! ```jsonc
//! {
//!   "schema": "claw.agent-api/v1",
//!   "id":     "org.tesseract",        // unique reverse-DNS id (dedup key)
//!   "name":   "tesseract",            // snake_case; becomes the
//!                                     // mcp_<name>_<tool> prefix
//!   "title":   { "en": "Tesseract OCR" },
//!   "summary": { "en": "OCR engine" },
//!   "vendor":  "claw-adapter",        // "upstream" | "claw-adapter" | "plugin"
//!   "license": "Apache-2.0",
//!   "transport": "mcp+stdio",         // only stdio is supported today
//!   "command": "python3",
//!   "args":    ["${manifest_dir}/main.py"],
//!   "env":     { "PYTHONPATH": "${manifest_dir}/../../apps" },
//!   "cwd":     null,
//!   "timeout_secs": 30,
//!   "enabled": true,
//!   "ai": {
//!     "callable_by_ai":     true,
//!     "uses_ai_internally": false,
//!     "safety":  "standard",
//!     "origins": ["external-content"]
//!   }
//! }
//! ```
//!
//! Only the fields the kernel actually needs to spawn + handshake are
//! mandatory (`id`, `name`, `command`). Everything else has a
//! sensible default.
//!
//! ## Substitution
//!
//! `${manifest_dir}` in `command`, any `args` entry, or any `env`
//! value resolves to the absolute parent directory of the manifest
//! file. This lets a single manifest work both in-repo (next to
//! `main.py`) and when installed under `/usr/lib/claw/...` without
//! changing the JSON.
//!
//! ## Search order
//!
//! 1. Each path in `cfg.agent.agent_api_paths` (when non-empty —
//!    overrides everything else for tests / dev).
//! 2. `$XDG_DATA_HOME/claw/agent-api/` (default
//!    `$HOME/.local/share/claw/agent-api/`).
//! 3. Each dir in `$XDG_DATA_DIRS` (default `/usr/local/share:/usr/share`)
//!    joined with `claw/agent-api/`.
//!
//! The first manifest seen for a given `id` wins; later ones are
//! logged as duplicates and skipped. This means a user can shadow a
//! system manifest by dropping their own copy under `$XDG_DATA_HOME`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::integration::McpServerSpec;

const SCHEMA: &str = "claw.agent-api/v1";

fn default_timeout_secs() -> u64 {
    30
}

fn default_enabled() -> bool {
    true
}

fn default_transport() -> String {
    "mcp+stdio".to_string()
}

/// On-disk shape of one agent-API sidecar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentApiManifest {
    /// Schema discriminator. Must equal [`SCHEMA`]; mismatches are
    /// rejected so we can evolve the format later without silently
    /// misreading old files.
    #[serde(default)]
    pub schema: String,

    /// Reverse-DNS unique id; used as the dedup key across all
    /// discovery paths.
    pub id: String,

    /// snake_case prefix used in tool names (`mcp_<name>_<tool>`).
    /// Must be unique within one running agent — we enforce this on
    /// the merged list, not per directory.
    pub name: String,

    /// Display title; informational only.
    #[serde(default)]
    pub title: HashMap<String, String>,

    /// One-line summary; informational only.
    #[serde(default)]
    pub summary: HashMap<String, String>,

    /// `"upstream"` (the project itself ships the manifest),
    /// `"claw-adapter"` (Claw maintains a wrapper around it),
    /// `"plugin"` (an extension to a host app), or anything else for
    /// forward compatibility.
    #[serde(default)]
    pub vendor: Option<String>,

    /// SPDX license id of the upstream tool. Documentation only.
    #[serde(default)]
    pub license: Option<String>,

    /// Wire transport. Currently only `"mcp+stdio"` is supported;
    /// other values are rejected so we don't silently spawn a server
    /// we can't talk to.
    #[serde(default = "default_transport")]
    pub transport: String,

    /// Executable to invoke. Resolved against `PATH` unless absolute.
    pub command: String,

    /// Arguments passed to the executable, in order. `${manifest_dir}`
    /// is substituted at load time.
    #[serde(default)]
    pub args: Vec<String>,

    /// Extra environment variables. `${manifest_dir}` substituted in
    /// values.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Working directory; inherits parent when `None`. `${manifest_dir}`
    /// substituted.
    #[serde(default)]
    pub cwd: Option<String>,

    /// Per-RPC timeout. `0` disables the timeout. Defaults to 30s.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,

    /// `false` keeps the manifest on disk but skips spawning. Defaults
    /// to `true`.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Free-form metadata describing how the AI may interact with this
    /// tool. The discovery loader doesn't act on this today (we attach
    /// every enabled, callable manifest); kept here so policy layers
    /// can read it later without re-parsing.
    #[serde(default)]
    pub ai: Option<AiHints>,
}

/// Optional `ai` block in a manifest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AiHints {
    /// `false` keeps the manifest registered but tells the agent
    /// runtime it's not meant to be reached from a model.
    #[serde(default = "default_enabled")]
    pub callable_by_ai: bool,

    /// `true` means the adapter itself spends LLM units (used by the
    /// `ai.budget` accountant; defer wiring until adapters that need
    /// it land).
    #[serde(default)]
    pub uses_ai_internally: bool,

    /// `"strict"` | `"standard"` | `"open"`; documentation today.
    #[serde(default)]
    pub safety: Option<String>,

    /// Tags like `"external-content"`, `"trusted"`; documentation today.
    #[serde(default)]
    pub origins: Vec<String>,
}

/// Errors a single manifest can fail with. Each variant is logged
/// + skipped; never fatal.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{path}: unsupported schema `{schema}` (expected `{SCHEMA}`)")]
    Schema { path: PathBuf, schema: String },
    #[error("{path}: unsupported transport `{transport}` (only `mcp+stdio`)")]
    Transport { path: PathBuf, transport: String },
    #[error("{path}: missing required field `{field}`")]
    MissingField { path: PathBuf, field: &'static str },
}

/// Read one manifest file → [`McpServerSpec`]. The returned spec is
/// already path-substituted and ready to hand to `attach_server`.
///
/// `None` means the manifest parsed cleanly but is disabled
/// (`enabled: false` or `ai.callable_by_ai: false`); callers should
/// treat it as "not attached" rather than an error.
pub fn load_manifest(path: &Path) -> Result<Option<McpServerSpec>, ManifestError> {
    let body = std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let m: AgentApiManifest =
        serde_json::from_str(&body).map_err(|source| ManifestError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    spec_from_manifest(path, m)
}

/// Pure helper exposed for unit tests.
fn spec_from_manifest(
    path: &Path,
    m: AgentApiManifest,
) -> Result<Option<McpServerSpec>, ManifestError> {
    if !m.schema.is_empty() && m.schema != SCHEMA {
        return Err(ManifestError::Schema {
            path: path.to_path_buf(),
            schema: m.schema,
        });
    }
    if m.transport != "mcp+stdio" {
        return Err(ManifestError::Transport {
            path: path.to_path_buf(),
            transport: m.transport,
        });
    }
    if m.id.is_empty() {
        return Err(ManifestError::MissingField {
            path: path.to_path_buf(),
            field: "id",
        });
    }
    if m.name.is_empty() {
        return Err(ManifestError::MissingField {
            path: path.to_path_buf(),
            field: "name",
        });
    }
    if m.command.is_empty() {
        return Err(ManifestError::MissingField {
            path: path.to_path_buf(),
            field: "command",
        });
    }
    if !m.enabled {
        return Ok(None);
    }
    if let Some(ai) = &m.ai {
        if !ai.callable_by_ai {
            return Ok(None);
        }
    }

    let manifest_dir = path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    let sub = |s: &str| s.replace("${manifest_dir}", &manifest_dir);

    Ok(Some(McpServerSpec {
        name: m.name,
        command: sub(&m.command),
        args: m.args.iter().map(|a| sub(a)).collect(),
        env: m
            .env
            .into_iter()
            .map(|(k, v)| (k, sub(&v)))
            .collect(),
        cwd: m.cwd.as_deref().map(sub),
        timeout_secs: m.timeout_secs,
    }))
}

/// Resolve the default discovery search dirs from the environment.
///
/// Order is high → low priority: per-user XDG_DATA_HOME, then each
/// entry in XDG_DATA_DIRS, all joined with `claw/agent-api`. Dirs
/// that don't exist are still returned — callers filter on read.
pub fn default_search_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();

    if let Some(home) = xdg_data_home() {
        out.push(home.join("claw/agent-api"));
    }
    for d in xdg_data_dirs() {
        out.push(d.join("claw/agent-api"));
    }
    out
}

fn xdg_data_home() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("XDG_DATA_HOME") {
        if !v.is_empty() {
            return Some(PathBuf::from(v));
        }
    }
    std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".local/share"))
}

fn xdg_data_dirs() -> Vec<PathBuf> {
    let raw = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    let raw = if raw.is_empty() {
        "/usr/local/share:/usr/share".to_string()
    } else {
        raw
    };
    raw.split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Scan every directory in `dirs`, parse every `*.json` it finds,
/// return the deduped list of specs.
///
/// First-wins dedup on manifest `id`: a manifest in a higher-priority
/// directory shadows the same `id` in lower-priority ones. Errors on
/// individual files are logged via `tracing::warn!` and skipped.
///
/// The returned [`McpServerSpec`]s are paired with their source path
/// for diagnostics. Callers that only want the specs can `.0` over
/// them.
pub fn discover_in(dirs: &[PathBuf]) -> Vec<(McpServerSpec, PathBuf)> {
    let mut seen_ids: HashMap<String, PathBuf> = HashMap::new();
    let mut out: Vec<(McpServerSpec, PathBuf)> = Vec::new();

    for dir in dirs {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                tracing::warn!(
                    "agent-api: read_dir({}) failed: {e}",
                    dir.display()
                );
                continue;
            }
        };

        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.extension().and_then(|x| x.to_str()) == Some("json")
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| !n.starts_with('.'))
                        .unwrap_or(false)
            })
            .collect();
        // Stable order per directory so tests + logs are deterministic.
        files.sort();

        for path in files {
            // Re-parse to grab `id` for dedup *before* converting to
            // spec, so a disabled manifest still claims the id slot
            // (preventing a lower-priority manifest from sneaking in).
            let body = match std::fs::read_to_string(&path) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!("agent-api: read {}: {e}", path.display());
                    continue;
                }
            };
            let manifest: AgentApiManifest = match serde_json::from_str(&body) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("agent-api: parse {}: {e}", path.display());
                    continue;
                }
            };
            if manifest.id.is_empty() {
                tracing::warn!(
                    "agent-api: {} missing required `id`; skipped",
                    path.display()
                );
                continue;
            }
            if let Some(prev) = seen_ids.get(&manifest.id) {
                tracing::info!(
                    "agent-api: ignoring duplicate manifest {} (already loaded from {})",
                    path.display(),
                    prev.display()
                );
                continue;
            }
            seen_ids.insert(manifest.id.clone(), path.clone());

            match spec_from_manifest(&path, manifest) {
                Ok(Some(spec)) => out.push((spec, path)),
                Ok(None) => {
                    tracing::debug!("agent-api: {} disabled; skipped", path.display());
                }
                Err(e) => {
                    tracing::warn!("agent-api: {e}");
                }
            }
        }
    }

    out
}

/// Top-level entry point used by the agent runtime. Honours an
/// explicit `paths` override (used for tests + dev) and otherwise
/// falls back to [`default_search_paths`].
pub fn discover(paths: Option<&[PathBuf]>) -> Vec<McpServerSpec> {
    let dirs: Vec<PathBuf> = match paths {
        Some(p) if !p.is_empty() => p.to_vec(),
        _ => default_search_paths(),
    };
    discover_in(&dirs).into_iter().map(|(s, _)| s).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    #[test]
    fn parses_minimal_manifest_and_substitutes_manifest_dir() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("org.tesseract.json");
        write(
            &p,
            r#"{
              "schema": "claw.agent-api/v1",
              "id": "org.tesseract",
              "name": "tesseract",
              "transport": "mcp+stdio",
              "command": "python3",
              "args": ["${manifest_dir}/main.py"],
              "env": {"PYTHONPATH": "${manifest_dir}/lib"}
            }"#,
        );
        let spec = load_manifest(&p).unwrap().expect("enabled by default");
        assert_eq!(spec.name, "tesseract");
        assert_eq!(spec.command, "python3");
        assert_eq!(
            spec.args,
            vec![format!("{}/main.py", dir.path().display())]
        );
        assert_eq!(
            spec.env.get("PYTHONPATH").unwrap(),
            &format!("{}/lib", dir.path().display())
        );
        assert_eq!(spec.timeout_secs, 30);
    }

    #[test]
    fn rejects_unknown_schema() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("bad.json");
        write(
            &p,
            r#"{"schema":"other/v9","id":"x","name":"x","transport":"mcp+stdio","command":"true"}"#,
        );
        let err = load_manifest(&p).unwrap_err();
        assert!(matches!(err, ManifestError::Schema { .. }), "got {err:?}");
    }

    #[test]
    fn rejects_unsupported_transport() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("bad.json");
        write(
            &p,
            r#"{"id":"x","name":"x","transport":"mcp+http","command":"true"}"#,
        );
        let err = load_manifest(&p).unwrap_err();
        assert!(matches!(err, ManifestError::Transport { .. }), "got {err:?}");
    }

    #[test]
    fn disabled_manifest_returns_none() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("off.json");
        write(
            &p,
            r#"{"id":"x","name":"x","transport":"mcp+stdio","command":"true","enabled":false}"#,
        );
        assert!(load_manifest(&p).unwrap().is_none());
    }

    #[test]
    fn callable_by_ai_false_returns_none() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("off.json");
        write(
            &p,
            r#"{"id":"x","name":"x","transport":"mcp+stdio","command":"true",
                "ai":{"callable_by_ai":false}}"#,
        );
        assert!(load_manifest(&p).unwrap().is_none());
    }

    #[test]
    fn discover_dedupes_by_id_first_wins() {
        let root = tempdir().unwrap();
        let high = root.path().join("high");
        let low = root.path().join("low");
        fs::create_dir_all(&high).unwrap();
        fs::create_dir_all(&low).unwrap();
        // Same `id`, different `name` so we can tell which won.
        write(
            &high.join("a.json"),
            r#"{"id":"org.thing","name":"high","transport":"mcp+stdio","command":"high-cmd"}"#,
        );
        write(
            &low.join("a.json"),
            r#"{"id":"org.thing","name":"low","transport":"mcp+stdio","command":"low-cmd"}"#,
        );
        let specs = discover_in(&[high.clone(), low.clone()]);
        assert_eq!(specs.len(), 1, "second manifest with same id ignored");
        assert_eq!(specs[0].0.name, "high");
        assert_eq!(specs[0].0.command, "high-cmd");
    }

    #[test]
    fn discover_skips_invalid_and_keeps_valid() {
        let dir = tempdir().unwrap();
        write(&dir.path().join("good.json"),
            r#"{"id":"a","name":"a","transport":"mcp+stdio","command":"x"}"#);
        write(&dir.path().join("malformed.json"), "not json at all");
        write(
            &dir.path().join("badschema.json"),
            r#"{"schema":"other","id":"b","name":"b","transport":"mcp+stdio","command":"x"}"#,
        );
        // Hidden file is ignored even when otherwise valid.
        write(&dir.path().join(".hidden.json"),
            r#"{"id":"c","name":"c","transport":"mcp+stdio","command":"x"}"#);
        let specs = discover_in(&[dir.path().to_path_buf()]);
        let names: Vec<_> = specs.iter().map(|(s, _)| s.name.as_str()).collect();
        assert_eq!(names, vec!["a"]);
    }

    #[test]
    fn discover_handles_missing_directory() {
        let nope = PathBuf::from("/tmp/claw-agent-api-does-not-exist-xyzzy");
        let out = discover_in(&[nope]);
        assert!(out.is_empty());
    }

    #[test]
    fn default_search_paths_uses_xdg() {
        let home = tempdir().unwrap();
        // Save + restore env so other tests stay deterministic.
        let prev_home = std::env::var("XDG_DATA_HOME").ok();
        let prev_dirs = std::env::var("XDG_DATA_DIRS").ok();
        std::env::set_var("XDG_DATA_HOME", home.path());
        std::env::set_var("XDG_DATA_DIRS", "/opt/share:/usr/share");

        let paths = default_search_paths();
        assert!(paths.iter().any(|p| p.starts_with(home.path())));
        assert!(paths
            .iter()
            .any(|p| p == &PathBuf::from("/opt/share/claw/agent-api")));
        assert!(paths
            .iter()
            .any(|p| p == &PathBuf::from("/usr/share/claw/agent-api")));

        match prev_home {
            Some(v) => std::env::set_var("XDG_DATA_HOME", v),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
        match prev_dirs {
            Some(v) => std::env::set_var("XDG_DATA_DIRS", v),
            None => std::env::remove_var("XDG_DATA_DIRS"),
        }
    }
}
