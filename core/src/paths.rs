//! System-level path resolution for cos primitives.
//!
//! Follows the cos FHS convention established in `cron.rs:89`:
//!   - State (user CLI): `$HOME/.local/share/cos` (overridable via `COS_DATA_DIR` or `COS_USER_DATA_DIR`)
//!   - State (clawd):    `/var/lib/cos`            (clawd's systemd unit pins `COS_DATA_DIR=/var/lib/cos`)
//!   - Cache:            `/var/cache/cos`          (overridable via `COS_CACHE_DIR`)
//!   - Runtime:          `/run/cos`                (overridable via `COS_RUNTIME_DIR`)
//!   - Config:           `/etc/cos`                (overridable via `COS_CONFIG_DIR`)
//!   - Logs:             `/var/log/cos`            (overridable via `COS_LOG_DIR`)
//!
//! `cos` is a user-level CLI: no subcommand should require root to
//! create or write its on-disk state. We therefore default
//! [`data_dir`] to the per-user XDG-style location and reserve the
//! system tree (`/var/lib/cos`) for the clawd daemon, which sets
//! `COS_DATA_DIR=/var/lib/cos` in its systemd unit. Anything that
//! genuinely modifies the system goes through the approval gate and
//! is executed by clawd on the caller's behalf, not by writing to
//! root-owned paths from the CLI process.
//!
//! On Windows, defaults map to `%APPDATA%\cos\data` (per-user) or
//! `%ProgramData%\cos\` (system) subdirectories. All environment
//! variables still apply on every platform.
//!
//! Models, agent state, audit logs, etc. should resolve their paths through
//! this module rather than hard-coding strings, so a single env var flip can
//! redirect the entire installation (useful for testing, multi-tenant hosts,
//! and per-home overlays).

use std::env;
use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Per-task HOME override (mirrors `config::with_override`).
//
// `cos agent ask` from a non-root user lands at the clawd socket; clawd
// runs as root with `HOME=/root`. Every per-user resolver in this file
// reads the `HOME` env var directly, so without an override clawd
// looks for the user's config / credentials / consents under `/root/...`
// rather than `/home/<user>/...`.
//
// Callers (the agent service worker) wrap each clawd-routed job in
// [`with_home_override`] with the requesting peer's resolved home
// directory. Inside that scope, [`user_config_dir`] / [`user_data_dir`]
// — and every helper transitively built on top of them
// ([`user_credentials_dir`], [`user_app_override_path`],
// [`user_app_consent_path`], [`user_budget_config_path`], …) — see
// the override instead of the daemon's own `HOME`. Outside any
// `with_home_override` scope the resolvers fall back to the `HOME`
// env var exactly as before.
//
// The override does NOT defeat the existing `COS_USER_CONFIG_DIR` /
// `COS_USER_DATA_DIR` env vars for the general per-user resolvers:
// those still take precedence so tests and multi-tenant overlays
// continue to work. Agent conversation memory is the exception:
// [`agent_memory_db_path`] must stay bound to the routed user's home
// even when clawd sets the machine-wide `COS_DATA_DIR`.
// ---------------------------------------------------------------------------

tokio::task_local! {
    static HOME_OVERRIDE: PathBuf;
    static OWNER_UID_OVERRIDE: u32;
    static ROUTED_JOB_OVERRIDE: bool;
}

/// Run `fut` with `home` installed as the per-task user-home override
/// visible to every per-user resolver polled inside `fut`. A separately
/// spawned Tokio task does not inherit this scope and must install its own
/// override. Outside the scope the resolvers fall back to `HOME`.
pub async fn with_home_override<F, R>(home: PathBuf, fut: F) -> R
where
    F: Future<Output = R>,
{
    HOME_OVERRIDE.scope(home, fut).await
}

pub async fn with_user_override<F, R>(uid: u32, home: PathBuf, fut: F) -> R
where
    F: Future<Output = R>,
{
    OWNER_UID_OVERRIDE
        .scope(uid, HOME_OVERRIDE.scope(home, fut))
        .await
}

pub async fn with_routed_job<F, R>(fut: F) -> R
where
    F: Future<Output = R>,
{
    ROUTED_JOB_OVERRIDE.scope(true, fut).await
}

/// Snapshot of the currently active home override (`None` outside any
/// `with_home_override` scope). Exposed for crates that need to mirror
/// the override into subprocess `HOME` env vars (e.g. when spawning a
/// shell on the user's behalf).
pub fn current_home_override() -> Option<PathBuf> {
    HOME_OVERRIDE.try_with(|h| h.clone()).ok()
}

pub fn current_owner_uid_override() -> Option<u32> {
    OWNER_UID_OVERRIDE.try_with(|uid| *uid).ok()
}

pub fn verified_home_for_uid(uid: u32) -> Result<PathBuf, String> {
    #[cfg(unix)]
    {
        use std::ffi::CStr;
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::MetadataExt;

        const BUFFER_SIZE: usize = 16 * 1024;
        let mut buffer = vec![0 as libc::c_char; BUFFER_SIZE];
        let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let code = unsafe {
            libc::getpwuid_r(
                uid,
                &mut passwd,
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut result,
            )
        };
        if code != 0 || result.is_null() || passwd.pw_dir.is_null() {
            return Err(format!("home directory is unavailable for uid {uid}"));
        }
        let bytes = unsafe { CStr::from_ptr(passwd.pw_dir) }.to_bytes().to_vec();
        if bytes.is_empty() {
            return Err(format!("home directory is unavailable for uid {uid}"));
        }
        let home = PathBuf::from(OsString::from_vec(bytes))
            .canonicalize()
            .map_err(|error| format!("canonicalize home for uid {uid}: {error}"))?;
        let metadata = std::fs::metadata(&home)
            .map_err(|error| format!("inspect home for uid {uid}: {error}"))?;
        if !metadata.is_dir() || metadata.uid() != uid {
            return Err(format!("configured home for uid {uid} is not owned by that uid"));
        }
        Ok(home)
    }
    #[cfg(not(unix))]
    {
        let _ = uid;
        Err("account home resolution requires Unix".to_string())
    }
}

pub fn is_routed_job() -> bool {
    ROUTED_JOB_OVERRIDE.try_with(|value| *value).unwrap_or(false)
}

/// Resolve the effective user home directory:
///   1. The current task's [`with_home_override`] scope, if any.
///   2. The `HOME` env var.
///   3. `/root` as a last-resort default.
///
/// Used internally by [`user_config_dir`] and [`user_data_dir`] so
/// every per-user resolver picks up the override automatically.
fn effective_home() -> OsString {
    if let Some(home) = current_home_override() {
        return home.into_os_string();
    }
    env::var_os("HOME").unwrap_or_else(|| "/root".into())
}

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

/// Resolve the on-disk state directory for this process.
///
/// * If `COS_DATA_DIR` is set, honour it verbatim. This is the
///   escape hatch tests use to redirect state into a tempdir, and
///   the mechanism clawd uses to pin its state under `/var/lib/cos`
///   (see the `Environment=` line in its systemd unit).
/// * Otherwise default to the per-user data dir
///   ([`user_data_dir`]): `$HOME/.local/share/cos` on Linux,
///   `%APPDATA%\cos\data` on Windows. The `cos` CLI is a user-level
///   command; it must never need root to write its own state.
///
/// Callers should treat this as opaque — anything that genuinely
/// requires system-wide visibility (audit logs, machine-shared model
/// registries, …) is clawd's job, and clawd already runs with
/// `COS_DATA_DIR` pointing at `/var/lib/cos`.
pub fn data_dir() -> PathBuf {
    if let Some(v) = env::var_os("COS_DATA_DIR") {
        return PathBuf::from(v);
    }
    user_data_dir()
}

pub fn caps_data_dir() -> PathBuf {
    std::env::var_os("COS_CAPS_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(data_dir)
}

pub fn proc_data_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("COS_PROC_DATA_DIR") {
        return PathBuf::from(path);
    }
    if let Some(uid) = current_owner_uid_override() {
        return PathBuf::from("/run/cos/caps").join(uid.to_string());
    }
    data_dir()
}

pub fn cache_dir() -> PathBuf {
    from_env_or_default("COS_CACHE_DIR", "/var/cache/cos", "cache")
}

pub fn runtime_dir() -> PathBuf {
    from_env_or_default("COS_RUNTIME_DIR", "/run/cos", "run")
}

pub fn clawd_socket_path() -> PathBuf {
    runtime_dir().join("clawd.sock")
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
        PathBuf::from(effective_home()).join(".config").join("cos")
    }
}

/// Path to the per-user agent config:
/// `$HOME/.config/cos/config.json`. This is where `cos agent setup`
/// writes the `[agent]`/`[tts]`/`[stt]`/`[imagegen]`/`[embed]` blocks
/// when the user picks providers, models, and keys from the wizard
/// or the cosmic-settings agent page. System-wide defaults live in
/// `CosConfig::default()`; there is no read-only `/etc/cos/config.json`
/// overlay any more.
///
/// Override the directory with `COS_USER_CONFIG_DIR`, or the file
/// directly with `COS_CONFIG_PATH` (used by tests).
pub fn user_config_path() -> PathBuf {
    user_config_dir().join("config.json")
}

/// Path to a specific user's agent config given their `$HOME` directory.
/// Used by clawd (running as root) to read the requesting peer's
/// `~/.config/cos/config.json` rather than its own — without this,
/// every `cos agent ask` from a non-root user would silently fall back
/// to clawd's default (empty) provider config and fail with
/// "no LLM provider configured".
///
/// On Linux this is `<home>/.config/cos/config.json`. On Windows the
/// concept doesn't apply (no peer-credential socket); callers there
/// should keep using [`user_config_path`].
pub fn user_config_path_for(home: &Path) -> PathBuf {
    home.join(".config").join("cos").join("config.json")
}

/// Per-user data root. Follows XDG_DATA_HOME on Linux
/// (`$HOME/.local/share/cos`) and `%APPDATA%\cos\data` on Windows.
/// Holds per-user secrets and large blobs that don't belong in the
/// config tree: encrypted credentials, future per-user model caches,
/// etc.
///
/// Override with `COS_USER_DATA_DIR` for tests / multi-tenant setups.
///
/// When `COS_DATA_DIR` is unset, [`data_dir`] resolves to the same
/// path. Code that wants the user dir specifically — independent of
/// any `COS_DATA_DIR` override that might point at `/var/lib/cos`
/// for clawd — should call `user_data_dir` directly.
pub fn user_data_dir() -> PathBuf {
    if let Some(v) = std::env::var_os("COS_USER_DATA_DIR") {
        return PathBuf::from(v);
    }
    #[cfg(windows)]
    {
        if let Some(v) = std::env::var_os("APPDATA") {
            return PathBuf::from(v).join("cos").join("data");
        }
        return windows_program_data().join("data");
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(effective_home())
            .join(".local")
            .join("share")
            .join("cos")
    }
}

/// Per-user encrypted credential store root:
/// `$HOME/.local/share/cos/credentials`. Each `cos agent setup` API
/// key, plus any other secret a user stores via `cos credential
/// store`, lands under here as `<namespace>/<name>.json` (AES-256-GCM
/// encrypted). Per-user so non-root users can save API keys without
/// touching `/var/lib/cos`. Override with `COS_CREDENTIALS_DIR`.
pub fn user_credentials_dir() -> PathBuf {
    if let Some(v) = std::env::var_os("COS_CREDENTIALS_DIR") {
        return PathBuf::from(v);
    }
    user_data_dir().join("credentials")
}

/// Per-app user override file:
/// `$HOME/.config/cos/apps/<app_id>.json`. Missing is normal and
/// means "no overrides, inherit the manifest verbatim". See
/// [`crate::ai::overrides`] for the schema and merge semantics.
pub fn user_app_override_path(app_id: &str) -> PathBuf {
    user_config_dir()
        .join("apps")
        .join(format!("{app_id}.json"))
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

/// Per-user aggregate AI budget config:
/// `$HOME/.config/cos/ai/budget.json`. Holds a single
/// `monthly_units` field which caps the **total** token spend across
/// every installed App for the current calendar month. A missing
/// file (or `monthly_units == 0`) means "no cap" — the user has
/// opted out of an aggregate ceiling. See [`crate::ai::user_budget`]
/// for the schema and gate semantics.
pub fn user_budget_config_path() -> PathBuf {
    user_config_dir().join("ai").join("budget.json")
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

fn routed_agent_state_dir() -> PathBuf {
    current_owner_uid_override()
        .map(clawd_user_agent_state_dir)
        .unwrap_or_else(agent_state_dir)
}

/// Directory for agent's persistent notes (MEMORY.md, USER.md, custom notes).
/// Lives under `data_dir/agent/notes/`. Persists across reboots.
pub fn agent_notes_dir() -> PathBuf {
    routed_agent_state_dir().join("notes")
}

/// Path to the agent's SQLite FTS5 conversation history database.
/// A clawd-routed user job uses a root-owned, UID-partitioned database under
/// `data_dir/users/<uid>/agent/`; a non-daemon home-only override uses the
/// user's XDG data directory. Otherwise it remains under `data_dir/agent/`.
/// Created on first write.
pub fn agent_memory_db_path() -> PathBuf {
    if let Some(uid) = current_owner_uid_override() {
        return clawd_user_memory_db_path(uid);
    }
    if let Some(home) = current_home_override() {
        return agent_memory_db_path_for_home(&home);
    }
    agent_state_dir().join("memory.db")
}

pub fn agent_memory_db_path_for_home(home: &Path) -> PathBuf {
    home.join(".local")
        .join("share")
        .join("cos")
        .join("agent")
        .join("memory.db")
}

pub fn clawd_user_memory_db_path(uid: u32) -> PathBuf {
    clawd_user_agent_state_dir(uid).join("memory.db")
}

pub fn clawd_user_agent_state_dir(uid: u32) -> PathBuf {
    data_dir()
        .join("users")
        .join(uid.to_string())
        .join("agent")
}

/// Path to the agent's semantic-memory (vector) SQLite store.
/// Lives at `data_dir/agent/semantic.db`. Created on first index.
pub fn agent_semantic_db_path() -> PathBuf {
    routed_agent_state_dir().join("semantic.db")
}

/// Path to the periodic-nudge store file. Lives at
/// `data_dir/agent/nudges.json`. Created on first add.
pub fn agent_nudges_path() -> PathBuf {
    routed_agent_state_dir().join("nudges.json")
}

/// Path to the persistent agent-hooks config file. Lives at
/// `data_dir/agent/hooks.json`. Lists which built-in hook kinds
/// (`logging`, etc.) should auto-register at the start of every
/// `cos agent ask` / `cos agent chat` invocation.
pub fn agent_hooks_path() -> PathBuf {
    routed_agent_state_dir().join("hooks.json")
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
/// resolved target resource. Powers Agent-facing permission history.
///
/// Lives at `log_dir()/caps.jsonl`. Set `COS_CAPS_AUDIT=0` to
/// suppress writing entirely (used by hot-path tests).
pub fn caps_audit_log_path() -> PathBuf {
    log_dir().join("caps.jsonl")
}

/// Central system-operation journal owned by `clawd`.
///
/// This is the persistent machine-level timeline AI and system UIs read
/// when they need to understand what happened recently. It receives
/// normalized events from the daemon API, capability enforcement, and
/// other system integration points as they are wired in.
pub fn system_operations_log_path() -> PathBuf {
    data_dir().join("clawd").join("system-operations.jsonl")
}

/// Append-only context-event journal owned by `clawd`.
///
/// Providers (apps, desktop integrations, shell/WSL collectors, browser
/// bridges, etc.) write source-scoped time-series events here. The daemon
/// exposes query APIs for source/app/time-range/order lookups and derives
/// recent activity snapshots from this same journal.
pub fn context_events_log_path() -> PathBuf {
    data_dir().join("clawd").join("context-events.jsonl")
}

/// Directory for agent's per-session todo lists. Lives under
/// `data_dir/agent/todos/`. Each session writes a JSON file named
/// `<session_id>.json`.
pub fn agent_todos_dir() -> PathBuf {
    routed_agent_state_dir().join("todos")
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
    routed_agent_state_dir().join("media").join("outputs")
}

/// JSON file storing curator drafts (proposed/accepted/rejected).
/// Lives at `data_dir/agent/curator-drafts.json`. The file is
/// rewritten atomically (tmp + rename) on every mutation. See
/// [`crate::agent::curator_drafts`].
pub fn agent_curator_drafts_path() -> PathBuf {
    routed_agent_state_dir().join("curator-drafts.json")
}

/// JSONL file storing skill usage records (one record per
/// invocation). Lives at `data_dir/agent/skills-usage.jsonl`. See
/// [`crate::agent::skills::provenance::UsageStore`].
pub fn agent_skills_usage_path() -> PathBuf {
    routed_agent_state_dir().join("skills-usage.jsonl")
}

/// JSONL file storing shell-hook records (the user's interactive
/// shell pre/post-exec events captured by `cos agent shell-hooks
/// init`). Lives at `data_dir/agent/shell-hooks.jsonl`. See
/// [`crate::agent::shell_hooks`].
pub fn agent_shell_hooks_log_path() -> PathBuf {
    routed_agent_state_dir().join("shell-hooks.jsonl")
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

    // ----- HOME_OVERRIDE task_local --------------------------------------

    #[test]
    fn no_override_falls_back_to_home_env() {
        assert!(current_home_override().is_none());
    }

    #[cfg(not(windows))]
    #[test]
    fn home_override_redirects_user_config_dir() {
        // Snapshot any existing COS_USER_CONFIG_DIR — we must clear it for
        // the override path to be observable.
        let prev_cfg = env::var_os("COS_USER_CONFIG_DIR");
        // SAFETY: single-threaded by test name uniqueness.
        unsafe {
            env::remove_var("COS_USER_CONFIG_DIR");
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime");

        let got = rt.block_on(async {
            with_home_override(PathBuf::from("/tmp/cos-test-home"), async {
                user_config_dir()
            })
            .await
        });
        assert_eq!(got, PathBuf::from("/tmp/cos-test-home/.config/cos"));

        // SAFETY: see above.
        unsafe {
            if let Some(v) = prev_cfg {
                env::set_var("COS_USER_CONFIG_DIR", v);
            }
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn home_override_redirects_user_data_dir_and_credentials() {
        let prev_data = env::var_os("COS_USER_DATA_DIR");
        let prev_creds = env::var_os("COS_CREDENTIALS_DIR");
        // SAFETY: single-threaded by test name uniqueness.
        unsafe {
            env::remove_var("COS_USER_DATA_DIR");
            env::remove_var("COS_CREDENTIALS_DIR");
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime");

        let (data, creds, snapshot) = rt.block_on(async {
            with_home_override(PathBuf::from("/tmp/cos-test-creds-home"), async {
                (
                    user_data_dir(),
                    user_credentials_dir(),
                    current_home_override(),
                )
            })
            .await
        });
        assert_eq!(
            data,
            PathBuf::from("/tmp/cos-test-creds-home/.local/share/cos")
        );
        assert_eq!(
            creds,
            PathBuf::from("/tmp/cos-test-creds-home/.local/share/cos/credentials")
        );
        assert_eq!(snapshot.as_deref(), Some(Path::new("/tmp/cos-test-creds-home")));

        // SAFETY: see above.
        unsafe {
            if let Some(v) = prev_data {
                env::set_var("COS_USER_DATA_DIR", v);
            }
            if let Some(v) = prev_creds {
                env::set_var("COS_CREDENTIALS_DIR", v);
            }
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn env_override_still_wins_over_home_override() {
        // Explicit env-var overrides (used by tests and multi-tenant
        // overlays) must keep winning even when a HOME override is
        // installed — otherwise we'd break existing test isolation.
        let prev = env::var_os("COS_USER_CONFIG_DIR");
        // SAFETY: see above.
        unsafe {
            env::set_var("COS_USER_CONFIG_DIR", "/tmp/cos-test-env-wins");
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime");

        let got = rt.block_on(async {
            with_home_override(PathBuf::from("/tmp/cos-test-home-loses"), async {
                user_config_dir()
            })
            .await
        });
        assert_eq!(got, PathBuf::from("/tmp/cos-test-env-wins"));

        // SAFETY: see above.
        unsafe {
            match prev {
                Some(v) => env::set_var("COS_USER_CONFIG_DIR", v),
                None => env::remove_var("COS_USER_CONFIG_DIR"),
            }
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn home_override_scopes_to_task_only() {
        let prev_cfg = env::var_os("COS_USER_CONFIG_DIR");
        // SAFETY: see above.
        unsafe {
            env::remove_var("COS_USER_CONFIG_DIR");
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime");

        // Outside the scope: no override.
        assert!(current_home_override().is_none());

        // Inside: present. After the scope exits: gone again.
        rt.block_on(async {
            with_home_override(PathBuf::from("/tmp/cos-scope"), async {
                assert_eq!(
                    current_home_override().as_deref(),
                    Some(Path::new("/tmp/cos-scope"))
                );
            })
            .await;
        });
        assert!(current_home_override().is_none());

        // SAFETY: see above.
        unsafe {
            if let Some(v) = prev_cfg {
                env::set_var("COS_USER_CONFIG_DIR", v);
            }
        }
    }
}
