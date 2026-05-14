//! Shell-integration hooks: capture the user's interactive shell
//! commands into a JSONL log so the agent can reference them as
//! ambient context ("what was the user just doing in their
//! terminal?") without scraping `~/.bash_history` or fighting with
//! per-shell history config.
//!
//! Intentionally tiny: only emits init scripts (zero deps) +
//! appends/reads a JSONL log via `crate::paths::agent_state_dir()`.
//! No daemon, no IPC, no per-session correlation. The shell
//! integration calls back into `cos agent shell-hooks record-pre
//! / record-post`, so the cos binary is the single source of
//! truth for the schema.
//!
//! Threat model: the log is local-only, written under the cos data
//! dir. Operators can disable capture entirely by simply not
//! sourcing the init script. `clear --yes` truncates; the file is
//! never auto-rotated (callers can `cos agent shell-hooks tail` and
//! pipe into their own retention).

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Supported init-script shells. New shells are explicit (no
/// auto-detect) so an operator who runs `cos agent shell-hooks
/// init` always knows which dialect they're getting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

impl Shell {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "bash" => Ok(Self::Bash),
            "zsh" => Ok(Self::Zsh),
            "fish" => Ok(Self::Fish),
            other => Err(format!(
                "unsupported shell '{other}' (try bash | zsh | fish)"
            )),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
        }
    }
}

/// One JSONL row in the shell-hooks log. `kind` is one of
/// `pre` / `post` so callers can pair them by `seq`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// Wall-clock millis since epoch.
    pub ts_ms: u64,
    /// "pre" (about to run a command) or "post" (just finished one).
    pub kind: String,
    /// Command line — present on `pre`, optional on `post`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd: Option<String>,
    /// Exit code — present on `post`, absent on `pre`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
}

/// Path to the JSONL log. Lives at
/// `data_dir/agent/shell-hooks.jsonl`.
pub fn default_log_path() -> PathBuf {
    crate::paths::agent_shell_hooks_log_path()
}

/// Render the init script for `shell`. The script defines the
/// preexec / postexec hook(s) and routes them into `cos agent
/// shell-hooks record-pre / record-post`.
///
/// stderr is silenced so a missing `cos` binary doesn't pollute
/// the user's prompt; the cost of a silent miss is acceptable
/// since this is ambient telemetry, not an authoritative trace.
pub fn render_init(shell: Shell) -> String {
    match shell {
        Shell::Bash => render_bash(),
        Shell::Zsh => render_zsh(),
        Shell::Fish => render_fish(),
    }
}

fn render_bash() -> String {
    r#"# cos agent shell-hooks (bash)
__cos_pre_exec() {
    cos agent shell-hooks record-pre "$BASH_COMMAND" >/dev/null 2>&1 || true
}
__cos_post_exec() {
    local rc=$?
    cos agent shell-hooks record-post "$rc" >/dev/null 2>&1 || true
}
trap '__cos_pre_exec' DEBUG
case ":${PROMPT_COMMAND:-}:" in
    *":__cos_post_exec:"*) ;;
    *) PROMPT_COMMAND="__cos_post_exec${PROMPT_COMMAND:+; $PROMPT_COMMAND}" ;;
esac
"#
    .to_string()
}

fn render_zsh() -> String {
    r#"# cos agent shell-hooks (zsh)
function __cos_preexec() {
    cos agent shell-hooks record-pre "$1" >/dev/null 2>&1 || true
}
function __cos_precmd() {
    local rc=$?
    cos agent shell-hooks record-post "$rc" >/dev/null 2>&1 || true
}
autoload -Uz add-zsh-hook
add-zsh-hook preexec __cos_preexec
add-zsh-hook precmd __cos_precmd
"#
    .to_string()
}

fn render_fish() -> String {
    r#"# cos agent shell-hooks (fish)
function __cos_preexec --on-event fish_preexec
    cos agent shell-hooks record-pre "$argv" >/dev/null 2>&1
end
function __cos_postexec --on-event fish_postexec
    cos agent shell-hooks record-post $status >/dev/null 2>&1
end
"#
    .to_string()
}

/// Append a `pre` record to the JSONL log at `path`.
pub fn append_pre_at(path: &Path, cmd: &str, ts_ms: u64) -> std::io::Result<()> {
    write_record(
        path,
        &Record {
            ts_ms,
            kind: "pre".into(),
            cmd: Some(cmd.to_string()),
            exit: None,
        },
    )
}

/// Append a `post` record (with exit code) to the JSONL log.
pub fn append_post_at(path: &Path, exit: i32, ts_ms: u64) -> std::io::Result<()> {
    write_record(
        path,
        &Record {
            ts_ms,
            kind: "post".into(),
            cmd: None,
            exit: Some(exit),
        },
    )
}

fn write_record(path: &Path, rec: &Record) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    let line = serde_json::to_string(rec)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    writeln!(f, "{line}")?;
    Ok(())
}

/// Read the most-recent `limit` records from the JSONL log,
/// oldest first within the returned window. Missing file → empty.
/// Malformed lines are skipped (logged at trace level), so a single
/// bad record never poisons the whole tail call.
pub fn tail_at(path: &Path, limit: usize) -> std::io::Result<Vec<Record>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let f = File::open(path)?;
    let reader = BufReader::new(f);
    let mut all: Vec<Record> = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Record>(&line) {
            Ok(r) => all.push(r),
            Err(e) => {
                tracing::trace!(
                    target: "cos.agent.shell_hooks",
                    "skipping malformed shell-hook record: {e}"
                );
            }
        }
    }
    let start = all.len().saturating_sub(limit);
    Ok(all.split_off(start))
}

/// Truncate the JSONL log. Returns `Ok(false)` when the file
/// didn't exist (no-op success).
pub fn clear_at(path: &Path) -> std::io::Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    File::create(path)?;
    Ok(true)
}

/// Best-effort wall-clock millis since epoch. Falls back to 0 on
/// pre-1970 system clocks (which would be a serious environment
/// bug, not something this code can recover from).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cos-shell-hooks-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn shell_parse_accepts_three_known_dialects() {
        assert_eq!(Shell::parse("bash").unwrap(), Shell::Bash);
        assert_eq!(Shell::parse("ZSH").unwrap(), Shell::Zsh);
        assert_eq!(Shell::parse(" fish ").unwrap(), Shell::Fish);
    }

    #[test]
    fn shell_parse_errs_on_unknown() {
        let err = Shell::parse("powershell").unwrap_err();
        assert!(err.contains("powershell"));
    }

    #[test]
    fn render_bash_includes_trap_and_prompt_command() {
        let s = render_init(Shell::Bash);
        assert!(s.contains("trap '__cos_pre_exec' DEBUG"));
        assert!(s.contains("PROMPT_COMMAND"));
        assert!(s.contains("cos agent shell-hooks record-pre"));
        assert!(s.contains("cos agent shell-hooks record-post"));
    }

    #[test]
    fn render_zsh_uses_add_zsh_hook() {
        let s = render_init(Shell::Zsh);
        assert!(s.contains("add-zsh-hook preexec __cos_preexec"));
        assert!(s.contains("add-zsh-hook precmd __cos_precmd"));
    }

    #[test]
    fn render_fish_uses_event_handlers() {
        let s = render_init(Shell::Fish);
        assert!(s.contains("--on-event fish_preexec"));
        assert!(s.contains("--on-event fish_postexec"));
        assert!(s.contains("$status"));
    }

    #[test]
    fn append_then_tail_returns_records_oldest_first() {
        let dir = tempdir("rt");
        let path = dir.join("shell-hooks.jsonl");
        append_pre_at(&path, "ls -la", 1_000).unwrap();
        append_post_at(&path, 0, 1_010).unwrap();
        append_pre_at(&path, "git status", 2_000).unwrap();
        append_post_at(&path, 1, 2_050).unwrap();
        let rows = tail_at(&path, 100).unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].cmd.as_deref(), Some("ls -la"));
        assert_eq!(rows[1].exit, Some(0));
        assert_eq!(rows[2].cmd.as_deref(), Some("git status"));
        assert_eq!(rows[3].exit, Some(1));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tail_limits_to_window_size() {
        let dir = tempdir("limit");
        let path = dir.join("shell-hooks.jsonl");
        for i in 0..10 {
            append_pre_at(&path, &format!("cmd {i}"), 100 + i as u64).unwrap();
        }
        let rows = tail_at(&path, 3).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].cmd.as_deref(), Some("cmd 7"));
        assert_eq!(rows[2].cmd.as_deref(), Some("cmd 9"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tail_skips_malformed_lines() {
        let dir = tempdir("malformed");
        let path = dir.join("shell-hooks.jsonl");
        append_pre_at(&path, "ok one", 1).unwrap();
        // Append a bad line directly.
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f, "{{not valid json").unwrap();
        append_pre_at(&path, "ok two", 2).unwrap();
        let rows = tail_at(&path, 100).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].cmd.as_deref(), Some("ok one"));
        assert_eq!(rows[1].cmd.as_deref(), Some("ok two"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tail_missing_file_is_empty() {
        let dir = tempdir("missing");
        let path = dir.join("shell-hooks.jsonl");
        let rows = tail_at(&path, 100).unwrap();
        assert!(rows.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clear_truncates_existing_file_returns_true() {
        let dir = tempdir("clear");
        let path = dir.join("shell-hooks.jsonl");
        append_pre_at(&path, "before", 1).unwrap();
        let cleared = clear_at(&path).unwrap();
        assert!(cleared);
        let rows = tail_at(&path, 10).unwrap();
        assert!(rows.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clear_missing_file_returns_false() {
        let dir = tempdir("clear-missing");
        let path = dir.join("shell-hooks.jsonl");
        let cleared = clear_at(&path).unwrap();
        assert!(!cleared);
        std::fs::remove_dir_all(&dir).ok();
    }
}
