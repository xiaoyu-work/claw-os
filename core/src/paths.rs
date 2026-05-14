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
//! and per-home overlays).

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

/// Per-user config root. System config (`/etc/cos`) belongs to the
/// machine owner; this is the per-user overlay each `$HOME` writes.
/// On Linux: `$HOME/.config/cos` (XDG). On Windows: `%APPDATA%\cos`.
/// Override with `COS_USER_CONFIG_DIR` for tests / multi-tenant setups.
pub fn user_config_dir() -> PathBuf {
    if let Some(v) = std::env::var_os("COS_USER_CONFIG_DIR") {
        return PathBuf::from(v);
    }
    #[cfg(windows)]
    {
        if let Some(v) = std::env::var_os("APPDATA") {
            return PathBuf::from(v).join("cos");
        }
        return windows_program_data();
    }
    #[cfg(not(windows))]
    {
        let home = std::env::var_os("HOME").unwrap_or_else(|| "/root".into());
        PathBuf::from(home).join(".config").join("cos")
    }
}

/// Per-app user override file:
/// `$HOME/.config/cos/apps/<app_id>.json`. Missing is normal and
/// means "no overrides, inherit the manifest verbatim". See
/// [`crate::ai::overrides`] for the schema and merge semantics.
pub fn user_app_override_path(app_id: &str) -> PathBuf {
    user_config_dir().join("apps").join(format!("{app_id}.json"))
}

/// Per-app user consent file:
/// `$HOME/.config/cos/consents/<app_id>.json`. Records the user's
/// explicit approval of an App's declared AI policy, snapshotted at
/// the moment of approval. Missing means "the user has never seen the
/// app's AI block", which the gate treats as `consent_required`. See
/// [`crate::ai::consent`] for the schema and freshness semantics.
pub fn user_app_consent_path(app_id: &str) -> PathBuf {
    user_config_dir()
        .join("consents")
        .join(format!("{app_id}.json"))
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

/// Path to the agent's semantic-memory (vector) SQLite store.
/// Lives at `data_dir/agent/semantic.db`. Created on first index.
pub fn agent_semantic_db_path() -> PathBuf {
    agent_state_dir().join("semantic.db")
}

/// Path to the periodic-nudge store file. Lives at
/// `data_dir/agent/nudges.json`. Created on first add.
pub fn agent_nudges_path() -> PathBuf {
    agent_state_dir().join("nudges.json")
}

/// Path to the persistent agent-hooks config file. Lives at
/// `data_dir/agent/hooks.json`. Lists which built-in hook kinds
/// (`logging`, etc.) should auto-register at the start of every
/// `cos agent ask` / `cos agent chat` invocation.
pub fn agent_hooks_path() -> PathBuf {
    agent_state_dir().join("hooks.json")
}

/// Path to the agent-runtime audit log. JSONL — one event per
/// line. Captures pre/post-turn and pre/post-tool events when the
/// `audit` hook is enabled (see `cos agent hooks enable audit`).
/// Lives at `log_dir()/agent.jsonl`.
pub fn agent_audit_log_path() -> PathBuf {
    log_dir().join("agent.jsonl")
}

/// Path to the structured capability-decision log. JSONL — one
/// record per `caps::require` call (both allows and denials), with
/// session id, agent label, verb, scope, decision, reason, and the
/// resolved target resource. Powers `cos perms history`.
///
/// Lives at `log_dir()/caps.jsonl`. Set `COS_CAPS_AUDIT=0` to
/// suppress writing entirely (used by hot-path tests).
pub fn caps_audit_log_path() -> PathBuf {
    log_dir().join("caps.jsonl")
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

/// JSON file storing curator drafts (proposed/accepted/rejected).
/// Lives at `data_dir/agent/curator-drafts.json`. The file is
/// rewritten atomically (tmp + rename) on every mutation. See
/// [`crate::agent::curator_drafts`].
pub fn agent_curator_drafts_path() -> PathBuf {
    agent_state_dir().join("curator-drafts.json")
}

/// JSONL file storing skill usage records (one record per
/// invocation). Lives at `data_dir/agent/skills-usage.jsonl`. See
/// [`crate::agent::skills::provenance::UsageStore`].
pub fn agent_skills_usage_path() -> PathBuf {
    agent_state_dir().join("skills-usage.jsonl")
}

/// JSONL file storing shell-hook records (the user's interactive
/// shell pre/post-exec events captured by `cos agent shell-hooks
/// init`). Lives at `data_dir/agent/shell-hooks.jsonl`. See
/// [`crate::agent::shell_hooks`].
pub fn agent_shell_hooks_log_path() -> PathBuf {
    agent_state_dir().join("shell-hooks.jsonl")
}

/// Directory tree for the agent service FS-based job queue. Lives under
/// `data_dir/agent/jobs/` with three subdirectories: `pending/`,
/// `running/`, and `done/`. Each job is a JSON file named
/// `<job_id>.json`. Atomic state transitions use `fs::rename` between
/// these directories. See [`crate::agent::service`].
pub fn agent_jobs_dir() -> PathBuf {
    agent_state_dir().join("jobs")
}

pub fn model_runtime_socket() -> PathBuf {
    runtime_dir().join("model-runtime.sock")
}

pub fn agent_runtime_socket() -> PathBuf {
    runtime_dir().join("agent.sock")
}

/// Append-only JSONL stream of per-AI-call run records (Phase 2.4,
/// generalised in Phase 8 to cover every modality — chat, embed,
/// image, audio, vision). Each line captures provider/model/
/// engine_name/engine_version/duration/usage/finish_reason plus a
/// `decision` ("allowed"|"denied") + `denial_reason` so the gate's
/// rejection attempts show up alongside successful calls. Distinct
/// from `audit.rs` which logs the parent `cos <app> <cmd>`
/// invocation; one CLI call may produce many run-record lines.
pub fn ai_run_log_path() -> PathBuf {
    log_dir().join("ai.jsonl")
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
