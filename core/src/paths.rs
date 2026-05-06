//! System-level path resolution for cos primitives.
//!
//! Follows the cos FHS convention established in `cron.rs:89`:
//!   - System data:   `/var/lib/cos`            (overridable via `COS_DATA_DIR`)
//!   - Cache:         `/var/cache/cos`          (overridable via `COS_CACHE_DIR`)
//!   - Runtime:       `/run/cos`                (overridable via `COS_RUNTIME_DIR`)
//!   - Config:        `/etc/cos`                (overridable via `COS_CONFIG_DIR`)
//!   - Logs:          `/var/log/cos`            (overridable via `COS_LOG_DIR`)
//!
//! On Windows, defaults map to `%ProgramData%\cos\` subdirectories. All
//! environment variables still apply on every platform.
//!
//! Models, agent state, audit logs, etc. should resolve their paths through
//! this module rather than hard-coding strings, so a single env var flip can
//! redirect the entire installation (useful for testing, multi-tenant hosts,
//! and per-den overlays).

use std::env;
use std::path::PathBuf;

#[cfg(windows)]
fn windows_program_data() -> PathBuf {
    env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("cos")
}

fn from_env_or_default(env_key: &str, unix_default: &str, subdir: &str) -> PathBuf {
    if let Some(v) = env::var_os(env_key) {
        return PathBuf::from(v);
    }
    #[cfg(windows)]
    {
        let _ = unix_default;
        return windows_program_data().join(subdir);
    }
    #[cfg(not(windows))]
    {
        let _ = subdir;
        PathBuf::from(unix_default)
    }
}

pub fn data_dir() -> PathBuf {
    from_env_or_default("COS_DATA_DIR", "/var/lib/cos", "data")
}

pub fn cache_dir() -> PathBuf {
    from_env_or_default("COS_CACHE_DIR", "/var/cache/cos", "cache")
}

pub fn runtime_dir() -> PathBuf {
    from_env_or_default("COS_RUNTIME_DIR", "/run/cos", "run")
}

pub fn config_dir() -> PathBuf {
    from_env_or_default("COS_CONFIG_DIR", "/etc/cos", "etc")
}

pub fn log_dir() -> PathBuf {
    from_env_or_default("COS_LOG_DIR", "/var/log/cos", "logs")
}

pub fn models_dir() -> PathBuf {
    data_dir().join("models")
}

pub fn models_cache_dir() -> PathBuf {
    cache_dir().join("models")
}

/// Root for installed inference engine versions
/// (`<data_dir>/engines/<engine>/<version>/{bin,lib,include}/`). Each
/// engine package is independently upgradable; the active version per
/// engine lives in `<engines_dir>/engines.json`.
pub fn engines_dir() -> PathBuf {
    data_dir().join("engines")
}

/// Per-engine root: `<engines_dir>/<engine>/`.
pub fn engine_dir(engine: &str) -> PathBuf {
    engines_dir().join(engine)
}

/// Specific engine version directory: `<engines_dir>/<engine>/<version>/`.
pub fn engine_version_dir(engine: &str, version: &str) -> PathBuf {
    engine_dir(engine).join(version)
}

/// Persistent registry tracking installed/active/pinned versions per
/// engine. Lives at `<engines_dir>/engines.json`.
pub fn engines_index_path() -> PathBuf {
    engines_dir().join("engines.json")
}

pub fn agent_state_dir() -> PathBuf {
    data_dir().join("agent")
}

/// Directory for agent's persistent notes (MEMORY.md, USER.md, custom notes).
/// Lives under `data_dir/agent/notes/`. Persists across reboots.
pub fn agent_notes_dir() -> PathBuf {
    agent_state_dir().join("notes")
}

/// Path to the agent's SQLite FTS5 conversation history database.
/// Lives at `data_dir/agent/memory.db`. Created on first write.
pub fn agent_memory_db_path() -> PathBuf {
    agent_state_dir().join("memory.db")
}

/// Directory for agent's per-session todo lists. Lives under
/// `data_dir/agent/todos/`. Each session writes a JSON file named
/// `<session_id>.json`.
pub fn agent_todos_dir() -> PathBuf {
    agent_state_dir().join("todos")
}

/// Directory tree for installed skills. Lives under
/// `data_dir/agent/skills/`. Each subdirectory `<skill-name>/`
/// contains a `SKILL.md` (agentskills.io frontmatter + body) plus
/// any helper scripts the skill may invoke. Phase 3 ships the
/// loader; Phase 6 ships the GitHub-based hub + sync.
pub fn agent_skills_dir() -> PathBuf {
    agent_state_dir().join("skills")
}

/// Output sink for media-tool-generated artifacts (TTS audio,
/// generated images). Lives under `data_dir/agent/media/outputs/`.
/// Tools write deterministic uuid-suffixed files here and return
/// the path to the model so it can hand the user a click-to-open
/// reference rather than inlining multi-MB binary bytes through
/// the LLM context.
pub fn agent_media_outputs_dir() -> PathBuf {
    agent_state_dir().join("media").join("outputs")
}

pub fn model_runtime_socket() -> PathBuf {
    runtime_dir().join("model-runtime.sock")
}

pub fn agent_runtime_socket() -> PathBuf {
    runtime_dir().join("agent.sock")
}

/// Append-only JSONL stream of per-LLM-call run records (Phase 2.4).
/// Each line captures provider/model/engine_name/engine_version/
/// duration/usage/finish_reason for reproducibility and debugging.
/// Distinct from `audit.rs` which logs the parent `cos <app> <cmd>`
/// invocation; one CLI call may produce many run-record lines.
pub fn llm_run_log_path() -> PathBuf {
    log_dir().join("llm.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_respects_env_override() {
        // Use a non-conflicting key path; this is best-effort and skipped
        // if the env is already set (parallel-test safety).
        if env::var_os("COS_DATA_DIR").is_some() {
            return;
        }
        // SAFETY: tests in this module are single-threaded by name uniqueness.
        unsafe {
            env::set_var("COS_DATA_DIR", "/tmp/cos-test-data");
        }
        assert_eq!(data_dir(), PathBuf::from("/tmp/cos-test-data"));
        unsafe {
            env::remove_var("COS_DATA_DIR");
        }
    }

    #[test]
    fn models_dir_lives_under_data_dir() {
        assert!(models_dir().starts_with(data_dir()));
    }

    #[test]
    fn agent_state_dir_lives_under_data_dir() {
        assert!(agent_state_dir().starts_with(data_dir()));
    }
}
