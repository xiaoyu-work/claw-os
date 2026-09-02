//! Auto-bootstrap a user-CLI session when `cos` is invoked from a
//! terminal, or as the non-interactive `cos app ...` desktop launcher,
//! without an upstream `COS_SESSION`.
//!
//! ## Why
//!
//! Strict-mode caps enforcement requires every gated call to come from
//! a known session in the proc registry. The canonical session-creation
//! path is `cos proc spawn`, which sets `COS_SESSION` for its child.
//! But when a real human runs `cos agent setup` or `cos agent chat`
//! from a shell, there is no upstream session: the user just typed the
//! command at a TTY. Without a fix every gated call would deny with
//! "Permission denied (no active session)".
//!
//! ## What this does
//!
//! At process start [`bootstrap_user_cli_session`] checks whether
//! `COS_SESSION` is unset. If so it:
//!
//! 1. Builds a [`crate::proc::SessionInfo`] with `role = Admin` and a
//!    wild-scoped [`crate::caps::CapSet`] (single-user desktop OS:
//!    anyone with shell access already has full power).
//! 2. Writes it into the proc registry so
//!    [`crate::caps::enforcement::require`] can find it.
//! 3. Sets `COS_SESSION` in the process environment so subsequent
//!    capability checks (in this process and any direct children that
//!    inherit the env) pick it up.
//! 4. Returns a [`SessionGuard`] whose `Drop` impl removes the row
//!    from the registry on clean exit. Crashes leave a ghost row;
//!    `cos proc list` GCs stale entries via `is_alive(pid)`, so this
//!    self-heals over time.
//!
//! ## Failure handling (fail-closed)
//!
//! Earlier revisions of this module silently demoted the process to
//! `COS_PERMS_MODE=permissive` if the registry write failed. That is
//! the wrong default for a security-oriented kernel: a corrupted data
//! dir would invisibly turn every gated call into a yes. We now
//! return an error from the bootstrap and let `main()` surface it to
//! the user, who can decide whether to retry, fix permissions, or
//! explicitly run with `COS_PERMS_MODE=permissive` set in the
//! environment.

use std::env;
use std::io::IsTerminal;

use crate::caps::role::Role;
use crate::caps::scope::Scope;
use crate::proc::{deregister_session, register_session, SessionInfo};
use crate::session::{SessionClient, SessionSource};

/// RAII guard that removes the bootstrapped session row from the
/// proc registry on `Drop`. Hold it for the lifetime of `main()` so
/// the row exists exactly as long as the CLI process does.
pub struct SessionGuard {
    session_id: String,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        deregister_session(&self.session_id);
        env::remove_var("COS_SESSION");
    }
}

/// Bootstrap a CLI session if `COS_SESSION` is unset. Returns a guard
/// holding the registered session id, or `None` if no work was done
/// (because `COS_SESSION` was already set by an upstream caller).
///
/// The session row is written through [`crate::proc::register_session`],
/// which resolves beneath `COS_PROC_DATA_DIR` when set, otherwise
/// beneath the normal per-process data dir. Routed clawd jobs pass
/// the same capability-state directory to App and MCP children.
///
/// On registry-write failure this function returns `None` and **does
/// not** demote to permissive mode — see the module docs.
///
/// Idempotent: if invoked twice the second call no-ops.
pub fn bootstrap_user_cli_session(args: &[String]) -> Option<SessionGuard> {
    bootstrap_user_cli_session_impl(
        args,
        std::io::stdin().is_terminal() || std::io::stderr().is_terminal(),
    )
}

fn bootstrap_user_cli_session_impl(
    args: &[String],
    interactive_terminal: bool,
) -> Option<SessionGuard> {
    if env::var_os("COS_SESSION").is_some_and(|v| !v.is_empty()) {
        return None;
    }
    let is_app_launcher = is_safe_noninteractive_app_launcher(args);
    if env::var_os("COS_APP_ID").is_some()
        || env::var_os("COS_MCP_SERVER").is_some()
        || crate::caps::enforcement::process_has_no_new_privs()
        || matches!(
            args.first().map(String::as_str),
            Some("ai" | "__policy" | "__memory")
        )
        || (!interactive_terminal && !is_app_launcher)
    {
        tracing::warn!(
            target: "cos::caps::bootstrap",
            "refusing to auto-bootstrap an untrusted CLI session"
        );
        return None;
    }

    let pid = std::process::id();
    let session_id = format!("cli-{}-{}", pid, fresh_session_suffix());

    let caps = Role::Admin.caps_with_scopes(
        Some(Scope::Wild),
        Some(Scope::Wild),
        Some(Scope::Wild),
    );

    let info = SessionInfo {
        session_id: session_id.clone(),
        pid,
        command: std::iter::once(env::args().next().unwrap_or_else(|| "cos".to_string()))
            .chain(args.iter().cloned())
            .collect(),
        started_at: now_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: None,
        parent: None,
        workdir: env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string()),
        exit_code: None,
        ended_at: None,
        tier: Some(Role::Admin.credential_tier()),
        scope: None,
        priority: None,
        caps: Some(caps),
        transient_caps: None,
        role: Some(Role::Admin.name().to_string()),
        app_id: None,
        pending_bind: false,
        start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
        client: bootstrap_client(args, interactive_terminal),
    };

    match register_session(info) {
        Ok(()) => {
            env::set_var("COS_SESSION", &session_id);
            Some(SessionGuard { session_id })
        }
        Err(e) => {
            // Fail closed. We deliberately do **not** demote to
            // permissive mode here. Surfacing the failure means the
            // user sees "Permission denied (no active session)" on
            // gated calls — the *correct* signal that something is
            // wrong with the kernel state, rather than the silent
            // open-door we used to have.
            tracing::error!(
                target: "cos::caps::bootstrap",
                error = %e,
                "failed to register CLI session in proc registry; \
                 gated calls will deny in strict mode"
            );
            None
        }
    }
}

fn bootstrap_client(args: &[String], interactive_terminal: bool) -> SessionClient {
    let source = match args {
        [first, second, ..] if first == "agent" && second == "serve" => SessionSource::LocalWeb,
        [first, second, ..] if first == "agent" && second == "mcp" => SessionSource::ExternalMcp,
        [first, ..] if first == "app" => SessionSource::App,
        _ => SessionSource::LocalCli,
    };
    SessionClient::new(
        source,
        interactive_terminal && source == SessionSource::LocalCli,
        true,
    )
}

fn is_safe_noninteractive_app_launcher(args: &[String]) -> bool {
    if args.first().map(String::as_str) != Some("app") {
        return false;
    }
    let (Some(app_id), Some(operation)) = (args.get(1), args.get(2)) else {
        return false;
    };
    let apps_dir = std::env::var_os("COS_APPS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/usr/lib/cos/apps"));
    let apps = crate::apps::discover_verified(&apps_dir);
    let Some(app) = apps.get(app_id) else {
        return false;
    };
    if app
        .manifest
        .desktop
        .as_ref()
        .is_some_and(|desktop| desktop.exec == *operation)
    {
        return true;
    }
    app.manifest
        .operations
        .get(operation)
        .is_some_and(|operation| operation.needs.is_empty())
}

/// Generate a fresh, collision-resistant suffix for the CLI session
/// id. We previously used a 32-bit `(nanos ^ pid)` value that easily
/// collided when two `cos` invocations fired in the same nanosecond
/// or when a forked child reused its parent's pid. UUIDv4 gives us
/// 122 bits of entropy from the OS RNG, which is comfortably above
/// the collision-resistance threshold even if a script kicks off
/// thousands of CLI invocations per second.
fn fresh_session_suffix() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn now_rfc3339() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/caps/bootstrap.rs"
    ));
}
