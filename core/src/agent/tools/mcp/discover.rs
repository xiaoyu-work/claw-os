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
    /// Optional — not required for `mcp+http` servers.
    #[serde(default)]
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

    /// Remote endpoint for `transport: "mcp+http"` / `"mcp+streamable-http"`
    /// servers. Ignored for stdio.
    #[serde(default)]
    pub url: Option<String>,

    /// Env var name holding a bearer token for an authenticated remote
    /// server. The token value is never stored in the manifest.
    #[serde(default)]
    pub bearer_env: Option<String>,
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
    #[error("{path}: unsupported transport `{transport}` (supported: `mcp+stdio`, `mcp+http`)")]
    Transport { path: PathBuf, transport: String },
    #[error("{path}: missing required field `{field}`")]
    MissingField { path: PathBuf, field: &'static str },
    #[error("{path}: unsafe value in `{field}`: {detail}")]
    UnsafeValue {
        path: PathBuf,
        field: &'static str,
        detail: String,
    },
}

/// Characters that have shell-grammar meaning. Even though we
/// never run `spec.command` through `/bin/sh -c`, an unescaped
/// semicolon / backtick / pipe in a manifest is a strong signal of
/// either a typo or a hostile payload — we'd rather reject and have
/// the operator fix the manifest than silently spawn a binary
/// named `/usr/bin/python3; rm -rf /home`.
const SHELL_METACHARS: &[char] = &[';', '|', '&', '$', '`', '<', '>', '(', ')', '{', '}', '\n'];

/// Verify that a string used as a path-like value in a manifest
/// doesn't contain shell metacharacters or `..` segments. Returns
/// an `UnsafeValue` error otherwise.
fn validate_no_shell_metachars(
    path: &Path,
    field: &'static str,
    value: &str,
) -> Result<(), ManifestError> {
    if let Some(c) = value.chars().find(|c| SHELL_METACHARS.contains(c)) {
        return Err(ManifestError::UnsafeValue {
            path: path.to_path_buf(),
            field,
            detail: format!("contains shell metacharacter {c:?}"),
        });
    }
    if value.contains("..") {
        return Err(ManifestError::UnsafeValue {
            path: path.to_path_buf(),
            field,
            detail: format!("contains parent-traversal `..` segment ({value:?})"),
        });
    }
    Ok(())
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
    let is_http = matches!(m.transport.as_str(), "mcp+http" | "mcp+streamable-http");
    let is_stdio = m.transport == "mcp+stdio";
    if !is_http && !is_stdio {
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
    if is_stdio && m.command.is_empty() {
        return Err(ManifestError::MissingField {
            path: path.to_path_buf(),
            field: "command",
        });
    }
    if is_http && m.url.as_deref().unwrap_or("").trim().is_empty() {
        return Err(ManifestError::MissingField {
            path: path.to_path_buf(),
            field: "url",
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

    // Reject shell-metacharacter injection in path-shaped fields.
    // We do this *post*-substitution so a hostile `${manifest_dir}`
    // doesn't get a free pass — though `manifest_dir` is derived
    // from the file's own path, which a user with write access to
    // an XDG dir already controls; the defensive check is cheap.
    let cmd = sub(&m.command);
    validate_no_shell_metachars(path, "command", &cmd)?;
    let args: Vec<String> = m
        .args
        .iter()
        .map(|a| {
            let s = sub(a);
            validate_no_shell_metachars(path, "args", &s)?;
            Ok::<_, ManifestError>(s)
        })
        .collect::<Result<_, _>>()?;
    let env: HashMap<String, String> = m
        .env
        .into_iter()
        .map(|(k, v)| {
            let v = sub(&v);
            validate_no_shell_metachars(path, "env", &v)?;
            Ok::<_, ManifestError>((k, v))
        })
        .collect::<Result<_, _>>()?;
    let cwd = match m.cwd.as_deref() {
        Some(s) => {
            let s = sub(s);
            validate_no_shell_metachars(path, "cwd", &s)?;
            Some(s)
        }
        None => None,
    };

    Ok(Some(McpServerSpec {
        name: m.name,
        command: cmd,
        args,
        env,
        cwd,
        timeout_secs: m.timeout_secs,
        url: if is_http { m.url } else { None },
        bearer_env: m.bearer_env,
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
/// First-wins dedup on the `(id, name)` pair: a manifest in a
/// higher-priority directory shadows the same `id` *or* the same
/// `name` in lower-priority ones. Dedup-by-id alone was insufficient
/// — two manifests with different `id`s but the same `name` would
/// both register and try to claim the same `mcp_<name>_<tool>`
/// prefix, producing duplicate-tool-name errors at registry-merge
/// time. Errors on individual files are logged via `tracing::warn!`
/// and skipped.
///
/// The returned [`McpServerSpec`]s are paired with their source path
/// for diagnostics. Callers that only want the specs can `.0` over
/// them.
pub fn discover_in(dirs: &[PathBuf]) -> Vec<(McpServerSpec, PathBuf)> {
    let mut seen_ids: HashMap<String, PathBuf> = HashMap::new();
    let mut seen_names: HashMap<String, PathBuf> = HashMap::new();
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
                    "agent-api: ignoring duplicate manifest {} (id {} already loaded from {})",
                    path.display(),
                    manifest.id,
                    prev.display()
                );
                continue;
            }
            if !manifest.name.is_empty() {
                if let Some(prev) = seen_names.get(&manifest.name) {
                    tracing::info!(
                        "agent-api: ignoring manifest {} (name {} collides with {})",
                        path.display(),
                        manifest.name,
                        prev.display()
                    );
                    continue;
                }
            }
            seen_ids.insert(manifest.id.clone(), path.clone());
            if !manifest.name.is_empty() {
                seen_names.insert(manifest.name.clone(), path.clone());
            }

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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/mcp/discover.rs"
    ));
}
