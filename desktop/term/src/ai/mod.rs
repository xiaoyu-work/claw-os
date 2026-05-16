//! AI / Copilot CLI integration for cosmic-term.
//!
//! Port of aterm's `aterm-ai` module (see `/Users/jay/workspace/aterm/aterm/aterm-ai/`)
//! adapted to Rust + alacritty_terminal + the `copilot` CLI (`@github/copilot`).
//!
//! ## Architecture
//!
//! 1. **Input state machine** ([`middleware`]) — intercepts keystrokes before they
//!    reach the PTY. When the user types `@` at the start of a line, the middleware
//!    enters capture mode: typed characters accumulate in a prompt buffer instead
//!    of being forwarded to the shell. The capturing prompt (`@ <text>`) is drawn
//!    into the alacritty grid via our own [`vte::ansi::Processor`].
//!
//! 2. **Context snapshot** ([`context`]) — when the user submits an AI prompt, we
//!    scrape the last N grid lines + cwd and write them to
//!    `$COS_AI_TMP/ac-<id>.json`. The shell function `__cos_ai` reads these files
//!    and prepends the context to the Copilot CLI prompt.
//!
//! 3. **Shell integration** ([`shell_integration`]) — bash / zsh / fish / pwsh
//!    snippets emitting OSC 133 markers and defining the `__cos_ai` function that
//!    `exec`s `copilot -p "<prompt>" --allow-all-tools`. Sourced via shell-specific
//!    mechanisms (`--rcfile`, `ZDOTDIR`, `--init-command`).
//!
//! 4. **Submission** — the middleware writes the captured prompt to
//!    `$COS_AI_TMP/aq-<id>.txt`, then injects ` __cos_ai <id>\r` into the PTY so
//!    the shell runs the function. The leading space keeps it out of history.
//!
//! Aterm relied on JS-land echo suppression to hide the injected command from
//! xterm.js (out of sync with ConPTY). cosmic-term + alacritty has direct PTY
//! ownership, so the shell function's own `\e[A\r\e[2K` cursor-up overwrite is
//! sufficient — no middleware-level echo suppression needed.

pub mod config;
pub mod context;
pub mod middleware;
pub mod shell_integration;

pub use config::AiConfig;
pub use context::capture_context;
pub use middleware::{AiAction, AiMiddleware};
pub use shell_integration::{IntegrationDirs, ensure_integration_dirs};

use alacritty_terminal::tty;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Build the per-process AI runtime: write shell integration scripts to a cache
/// directory and create a tmp directory for `aq-*`/`ac-*` files.
///
/// Returns `None` if `config.enabled == false` or if the integration scripts
/// couldn't be written (the AI features simply stay disabled).
pub fn build_runtime(config: &AiConfig) -> Option<crate::terminal::AiRuntime> {
    if !config.enabled {
        return None;
    }
    let cache_dir = cache_dir();
    let copilot_bin = std::env::var("COS_AI_COPILOT_BIN").unwrap_or_else(|_| "copilot".to_string());
    let dirs = match ensure_integration_dirs(
        &cache_dir,
        &copilot_bin,
        &config.extra_args,
        config.allow_all_tools,
        &config.model,
    ) {
        Ok(d) => d,
        Err(err) => {
            log::warn!("ai: failed to write integration dirs: {err}");
            return None;
        }
    };
    let tmp_dir = tmp_dir();
    if let Err(err) = std::fs::create_dir_all(&tmp_dir) {
        log::warn!("ai: failed to create tmp dir {}: {err}", tmp_dir.display());
        return None;
    }
    Some(crate::terminal::AiRuntime {
        config: config.clone(),
        tmp_dir,
        integration_dirs: Arc::new(dirs),
    })
}

fn cache_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("cosmic-term");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join(".cache").join("cosmic-term");
        }
    }
    std::env::temp_dir().join("cosmic-term-cache")
}

fn tmp_dir() -> PathBuf {
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        if !runtime.is_empty() {
            return PathBuf::from(runtime)
                .join("cosmic-term-ai")
                .join(format!("p{}", std::process::id()));
        }
    }
    std::env::temp_dir()
        .join("cosmic-term-ai")
        .join(format!("p{}", std::process::id()))
}

/// Inject AI env vars into [`tty::Options::env`] and, when the shell is
/// known and AI is enabled, prepend shell-startup arguments that auto-source
/// the integration script.
///
/// `existing_shell` is `Some` if the caller already determined a shell
/// (e.g., from `profile.command` or CLI `--shell`). When `None`, we read
/// `$SHELL` and synthesize a wrapping `tty::Shell`.
pub fn apply_options(
    options: &mut tty::Options,
    runtime: &crate::terminal::AiRuntime,
    existing_shell: Option<(String, Vec<String>)>,
) {
    options
        .env
        .insert("COS_AI_TMP".into(), runtime.tmp_dir.display().to_string());
    options.env.insert(
        "COS_AI_INTEGRATION_DIR".into(),
        runtime.integration_dirs.root.display().to_string(),
    );

    let (program, user_args) = match existing_shell {
        Some(s) => s,
        None => match std::env::var("SHELL") {
            Ok(s) if !s.is_empty() => (s, Vec::new()),
            _ => return,
        },
    };

    let shell_name = Path::new(&program)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let dirs = &runtime.integration_dirs;

    let mut wrapped_args: Vec<String> = match shell_name.as_str() {
        "bash" => vec![
            "--rcfile".into(),
            dirs.bash_init.display().to_string(),
        ],
        "zsh" => {
            if let Ok(prev) = std::env::var("ZDOTDIR") {
                if !prev.is_empty() {
                    options.env.insert("COS_TERM_PREV_ZDOTDIR".into(), prev);
                }
            }
            options
                .env
                .insert("ZDOTDIR".into(), dirs.zsh_dotdir.display().to_string());
            Vec::new()
        }
        "fish" => vec![
            "--init-command".into(),
            format!("source {:?}", dirs.fish_integration),
        ],
        "pwsh" | "powershell" => vec![
            "-NoExit".into(),
            "-Command".into(),
            format!(". '{}'", dirs.pwsh_integration.display()),
        ],
        _ => Vec::new(),
    };

    // If we modified shell args, build a wrapping Shell. Otherwise leave
    // options.shell alone (env-only injection).
    if !wrapped_args.is_empty() {
        wrapped_args.extend(user_args);
        options.shell = Some(tty::Shell::new(program, wrapped_args));
    }
}
