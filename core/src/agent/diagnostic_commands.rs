use super::{insights, memory};
use serde_json::{json, Value};

/// `cos agent insights [overall|recent|sessions] [n]` — aggregate
/// the JSONL run-record stream produced by every LLM call.
pub(super) fn insights_cmd(args: &[String]) -> Result<Value, String> {
    use chrono::DateTime;
    use insights::InsightsFilter;

    let sub = args.first().map(|s| s.as_str()).unwrap_or("overall");
    let path = crate::paths::ai_run_log_path();

    // Parse trailing flags shared across all three sub-verbs.
    // For "recent" the optional N positional must come first
    // (preserves the existing `cos agent insights recent 25` UX).
    let (n_for_recent, mut i) = if sub == "recent" {
        let n = args.get(1).and_then(|s| s.parse::<usize>().ok());
        (n, if n.is_some() { 2 } else { 1 })
    } else {
        (None, 1)
    };

    let mut filter = InsightsFilter::default();
    while i < args.len() {
        match args[i].as_str() {
            "--since" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--since needs <ISO timestamp>".to_string())?;
                filter.since = Some(
                    DateTime::parse_from_rfc3339(v)
                        .map(|d| d.with_timezone(&chrono::Utc))
                        .map_err(|e| format!("--since: {e}"))?,
                );
                i += 2;
            }
            "--until" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--until needs <ISO timestamp>".to_string())?;
                filter.until = Some(
                    DateTime::parse_from_rfc3339(v)
                        .map(|d| d.with_timezone(&chrono::Utc))
                        .map_err(|e| format!("--until: {e}"))?,
                );
                i += 2;
            }
            "--ok" => {
                filter.status_ok = Some(true);
                i += 1;
            }
            "--error" => {
                filter.status_ok = Some(false);
                i += 1;
            }
            "--provider" => {
                filter.provider = Some(
                    args.get(i + 1)
                        .cloned()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| "--provider needs <name>".to_string())?,
                );
                i += 2;
            }
            "--model" => {
                filter.model = Some(
                    args.get(i + 1)
                        .cloned()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| "--model needs <name>".to_string())?,
                );
                i += 2;
            }
            other => return Err(format!("unknown flag: {other}")),
        }
    }

    let filter_payload = json!({
        "since": filter.since.map(|d| d.to_rfc3339()),
        "until": filter.until.map(|d| d.to_rfc3339()),
        "status_ok": filter.status_ok,
        "provider": filter.provider.clone(),
        "model": filter.model.clone(),
    });

    match sub {
        "overall" | "" => {
            let report = insights::InsightsReport::from_path_filtered(&path, &filter);
            Ok(json!({
                "log": path.display().to_string(),
                "filter": filter_payload,
                "overall": report.overall,
                "per_provider": report.per_provider,
                "per_model": report.per_model,
            }))
        }
        "recent" => {
            let n = n_for_recent.unwrap_or(10);
            let rows = insights::InsightsReport::recent_filtered(&path, n, &filter);
            Ok(json!({
                "log": path.display().to_string(),
                "filter": filter_payload,
                "n": rows.len(),
                "records": rows,
            }))
        }
        "sessions" => {
            let by = insights::InsightsReport::by_session_filtered(&path, &filter);
            Ok(json!({
                "log": path.display().to_string(),
                "filter": filter_payload,
                "sessions": by,
            }))
        }
        other => Err(format!(
            "unknown insights subcommand: {other}. try: overall | recent [n] | sessions"
        )),
    }
}

/// `cos agent usage [overall|provider <name>|model <name>|session <id>|app <id>|verb <name>]`
/// `[--since <ISO>] [--until <ISO>] [--ok|--error] [--app <id>] [--verb <name>]`
/// — filtered aggregation over `ai.jsonl`. Mirrors `agent insights
/// overall` for the unfiltered case but adds the AND-combined filter
/// set from [`crate::agent::llm::usage::UsageQuery`].
pub(super) fn usage_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::llm::usage::{aggregate_path_filtered, default_log_path, UsageQuery};
    use chrono::DateTime;
    let mut query = UsageQuery::default();
    let scope = args.first().map(|s| s.as_str()).unwrap_or("overall");
    let mut i = match scope {
        "overall" | "" => 1,
        "provider" => {
            query.provider = Some(
                args.get(1)
                    .cloned()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "usage: cos agent usage provider <name>".to_string())?,
            );
            2
        }
        "model" => {
            query.model = Some(
                args.get(1)
                    .cloned()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "usage: cos agent usage model <name>".to_string())?,
            );
            2
        }
        "session" => {
            query.session_id = Some(
                args.get(1)
                    .cloned()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "usage: cos agent usage session <id>".to_string())?,
            );
            2
        }
        "app" => {
            query.app_id = Some(
                args.get(1)
                    .cloned()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "usage: cos agent usage app <id>".to_string())?,
            );
            2
        }
        "verb" => {
            query.verb = Some(
                args.get(1)
                    .cloned()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "usage: cos agent usage verb <name>".to_string())?,
            );
            2
        }
        other => {
            return Err(format!(
                "unknown usage scope: {other}. try: overall | provider <name> | model <name> | session <id> | app <id> | verb <name>"
            ))
        }
    };
    while i < args.len() {
        match args[i].as_str() {
            "--since" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--since needs <ISO timestamp>".to_string())?;
                query.since = Some(
                    DateTime::parse_from_rfc3339(v)
                        .map(|d| d.with_timezone(&chrono::Utc))
                        .map_err(|e| format!("--since: {e}"))?,
                );
                i += 2;
            }
            "--until" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--until needs <ISO timestamp>".to_string())?;
                query.until = Some(
                    DateTime::parse_from_rfc3339(v)
                        .map(|d| d.with_timezone(&chrono::Utc))
                        .map_err(|e| format!("--until: {e}"))?,
                );
                i += 2;
            }
            "--ok" => {
                query.status_ok = Some(true);
                i += 1;
            }
            "--error" => {
                query.status_ok = Some(false);
                i += 1;
            }
            "--app" => {
                let v = args
                    .get(i + 1)
                    .cloned()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "--app needs <id>".to_string())?;
                query.app_id = Some(v);
                i += 2;
            }
            "--verb" => {
                let v = args
                    .get(i + 1)
                    .cloned()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "--verb needs <name>".to_string())?;
                query.verb = Some(v);
                i += 2;
            }
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    let path = default_log_path();
    let summary = aggregate_path_filtered(&path, &query);
    Ok(json!({
        "log": path.display().to_string(),
        "scope": scope,
        "filter": {
            "provider": query.provider,
            "model": query.model,
            "session_id": query.session_id,
            "app_id": query.app_id,
            "verb": query.verb,
            "since": query.since.map(|d| d.to_rfc3339()),
            "until": query.until.map(|d| d.to_rfc3339()),
            "status_ok": query.status_ok,
        },
        "total": summary.total,
        "by_provider": summary.by_provider,
        "by_model": summary.by_model,
        "by_session": summary.by_session,
        "by_app": summary.by_app,
        "by_verb": summary.by_verb,
        "parse_errors": summary.parse_errors,
    }))
}

/// `cos agent display <subcommand>` — render conversation
/// history from the memory DB through [`crate::agent::display`]'s
/// pure-functional formatter, so operators can preview what a
/// terminal/gateway would show without firing up a real session.
///
/// Subcommands:
///
/// * `transcript --session <id> [--limit N] [--width W]
///   [--no-truncate] [--truncate-at N] [--indent N]` — render the
///   most-recent N messages of `<id>` (oldest first) as a
///   single-string transcript using `display::render_message`.
/// * `format-bytes <n>` — preview `display::format_bytes`.
/// * `format-duration <ms>` — preview `display::format_duration`.
pub(super) fn display_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "transcript" => display_transcript_cmd(&args[1..]),
        "format-bytes" => display_format_bytes_cmd(&args[1..]),
        "format-duration" => display_format_duration_cmd(&args[1..]),
        "" => Err(
            "usage: cos agent display transcript --session <id> [--limit N] [--width W] [--no-truncate] [--truncate-at N] [--indent N] | format-bytes <n> | format-duration <ms>"
                .to_string(),
        ),
        other => Err(format!(
            "unknown display subcommand: {other}. try: transcript | format-bytes | format-duration"
        )),
    }
}

#[derive(Debug, Default)]
struct DisplayTranscriptArgs {
    session: Option<String>,
    limit: Option<usize>,
    width: Option<usize>,
    indent: Option<usize>,
    no_truncate: bool,
    truncate_at: Option<usize>,
}

fn parse_display_transcript_args(args: &[String]) -> Result<DisplayTranscriptArgs, String> {
    let mut out = DisplayTranscriptArgs::default();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--session" => {
                out.session = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--session needs an id".to_string())?,
                );
                i += 2;
            }
            "--limit" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--limit needs a number".to_string())?;
                out.limit = Some(
                    raw.parse::<usize>()
                        .map_err(|_| format!("--limit not numeric: {raw}"))?,
                );
                i += 2;
            }
            "--width" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--width needs a number".to_string())?;
                out.width = Some(
                    raw.parse::<usize>()
                        .map_err(|_| format!("--width not numeric: {raw}"))?,
                );
                i += 2;
            }
            "--indent" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--indent needs a number".to_string())?;
                out.indent = Some(
                    raw.parse::<usize>()
                        .map_err(|_| format!("--indent not numeric: {raw}"))?,
                );
                i += 2;
            }
            "--no-truncate" => {
                out.no_truncate = true;
                i += 1;
            }
            "--truncate-at" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--truncate-at needs a number".to_string())?;
                out.truncate_at = Some(
                    raw.parse::<usize>()
                        .map_err(|_| format!("--truncate-at not numeric: {raw}"))?,
                );
                i += 2;
            }
            other => return Err(format!("unknown display transcript flag: {other}")),
        }
    }
    Ok(out)
}

fn display_config_from(args: &DisplayTranscriptArgs) -> crate::agent::display::DisplayConfig {
    let mut cfg = crate::agent::display::DisplayConfig::default();
    if let Some(w) = args.width {
        cfg.wrap_at = w;
    }
    if let Some(ind) = args.indent {
        cfg.continuation_indent = ind;
    }
    if args.no_truncate {
        cfg.truncate_at = None;
    } else if let Some(cap) = args.truncate_at {
        cfg.truncate_at = Some(cap);
    }
    cfg
}

fn role_from_str(raw: &str) -> crate::agent::display::Role {
    use crate::agent::display::Role;
    match raw {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        _ => Role::System,
    }
}

fn display_transcript_cmd(args: &[String]) -> Result<Value, String> {
    let parsed = parse_display_transcript_args(args)?;
    let session = parsed
        .session
        .clone()
        .ok_or_else(|| "--session <id> is required".to_string())?;
    let db = crate::agent::memory::sqlite_fts::MemoryDb::open_default()
        .map_err(|e| format!("open memory db: {e}"))?;
    display_transcript_with(&db, &session, &parsed)
}

fn display_transcript_with(
    db: &crate::agent::memory::sqlite_fts::MemoryDb,
    session_id: &str,
    parsed: &DisplayTranscriptArgs,
) -> Result<Value, String> {
    let cfg = display_config_from(parsed);
    let limit = parsed.limit.unwrap_or(50);
    let rows = db
        .recent(session_id, limit)
        .map_err(|e| format!("read session {session_id}: {e}"))?;
    let lines: Vec<String> = rows
        .iter()
        .map(|row| {
            let content = memory::history::sanitize_stored_content(&row.role, &row.content);
            crate::agent::display::render_message(role_from_str(&row.role), &content, &cfg)
        })
        .collect();
    let transcript = lines.join("\n");
    Ok(json!({
        "session_id": session_id,
        "message_count": rows.len(),
        "limit": limit,
        "wrap_at": cfg.wrap_at,
        "continuation_indent": cfg.continuation_indent,
        "truncate_at": cfg.truncate_at,
        "transcript": transcript,
    }))
}

fn display_format_bytes_cmd(args: &[String]) -> Result<Value, String> {
    let raw = args
        .first()
        .ok_or_else(|| "usage: cos agent display format-bytes <n>".to_string())?;
    let n: u64 = raw
        .parse()
        .map_err(|_| format!("format-bytes needs a positive integer, got: {raw}"))?;
    Ok(json!({
        "input": n,
        "formatted": crate::agent::display::format_bytes(n),
    }))
}

fn display_format_duration_cmd(args: &[String]) -> Result<Value, String> {
    let raw = args
        .first()
        .ok_or_else(|| "usage: cos agent display format-duration <ms>".to_string())?;
    let ms: u64 = raw
        .parse()
        .map_err(|_| format!("format-duration needs a positive integer (ms), got: {raw}"))?;
    Ok(json!({
        "input_ms": ms,
        "formatted": crate::agent::display::format_duration(std::time::Duration::from_millis(ms)),
    }))
}

/// `cos agent shell-hooks <init <bash|zsh|fish>|record-pre <cmd>|record-post <exit>|tail [--limit N]|clear --yes|path>`
///
/// Exposes [`crate::agent::shell_hooks`] as a CLI surface so the
/// user can install shell-init scripts that capture interactive
/// commands into a JSONL log the agent can later read for ambient
/// context. The `record-*` verbs are called by the init-script
/// hooks themselves; humans only invoke `init`, `tail`, `clear`,
/// `path`.
pub(super) fn shell_hooks_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("path");
    match sub {
        "path" | "" => Ok(json!({
            "path": crate::agent::shell_hooks::default_log_path().display().to_string(),
        })),
        "init" => {
            let raw = args
                .get(1)
                .map(|s| s.as_str())
                .ok_or_else(|| {
                    "usage: cos agent shell-hooks init <bash|zsh|fish>".to_string()
                })?;
            let shell = crate::agent::shell_hooks::Shell::parse(raw)?;
            let script = crate::agent::shell_hooks::render_init(shell);
            Ok(json!({
                "shell": shell.label(),
                "log_path": crate::agent::shell_hooks::default_log_path().display().to_string(),
                "script": script,
                "instructions": init_instructions_for(shell),
            }))
        }
        "record-pre" => {
            let cmd = args
                .get(1)
                .cloned()
                .ok_or_else(|| "usage: cos agent shell-hooks record-pre <cmd>".to_string())?;
            let path = crate::agent::shell_hooks::default_log_path();
            let ts_ms = crate::agent::shell_hooks::now_ms();
            crate::agent::shell_hooks::append_pre_at(&path, &cmd, ts_ms)
                .map_err(|e| format!("write failed: {e}"))?;
            Ok(json!({
                "kind": "pre",
                "ts_ms": ts_ms,
                "cmd": cmd,
                "path": path.display().to_string(),
            }))
        }
        "record-post" => {
            let raw = args
                .get(1)
                .ok_or_else(|| "usage: cos agent shell-hooks record-post <exit>".to_string())?;
            let exit: i32 = raw
                .parse()
                .map_err(|_| format!("record-post needs an integer exit code, got: {raw}"))?;
            let path = crate::agent::shell_hooks::default_log_path();
            let ts_ms = crate::agent::shell_hooks::now_ms();
            crate::agent::shell_hooks::append_post_at(&path, exit, ts_ms)
                .map_err(|e| format!("write failed: {e}"))?;
            Ok(json!({
                "kind": "post",
                "ts_ms": ts_ms,
                "exit": exit,
                "path": path.display().to_string(),
            }))
        }
        "tail" => {
            let mut limit: usize = 20;
            let mut i = 1usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--limit" => {
                        let v = args
                            .get(i + 1)
                            .ok_or_else(|| "--limit needs <n>".to_string())?;
                        limit = v
                            .parse()
                            .map_err(|_| format!("--limit must be a positive integer, got: {v}"))?;
                        i += 2;
                    }
                    other => return Err(format!("unknown flag for `shell-hooks tail`: {other}")),
                }
            }
            let path = crate::agent::shell_hooks::default_log_path();
            let rows = crate::agent::shell_hooks::tail_at(&path, limit)
                .map_err(|e| format!("read failed: {e}"))?;
            Ok(json!({
                "path": path.display().to_string(),
                "limit": limit,
                "n": rows.len(),
                "records": rows,
            }))
        }
        "clear" => {
            // Require explicit --yes so it can never happen by accident.
            let confirmed = args.iter().any(|a| a == "--yes");
            if !confirmed {
                return Err(
                    "usage: cos agent shell-hooks clear --yes  (truncates the JSONL log)".into(),
                );
            }
            let path = crate::agent::shell_hooks::default_log_path();
            let cleared = crate::agent::shell_hooks::clear_at(&path)
                .map_err(|e| format!("clear failed: {e}"))?;
            Ok(json!({
                "path": path.display().to_string(),
                "cleared": cleared,
            }))
        }
        other => Err(format!(
            "unknown shell-hooks subcommand: {other}. try: init <bash|zsh|fish> | record-pre <cmd> | record-post <exit> | tail [--limit N] | clear --yes | path"
        )),
    }
}

fn init_instructions_for(shell: crate::agent::shell_hooks::Shell) -> &'static str {
    use crate::agent::shell_hooks::Shell;
    match shell {
        Shell::Bash => {
            "append the script to ~/.bashrc, or eval it inline: eval \"$(cos agent shell-hooks init bash | jq -r .script)\""
        }
        Shell::Zsh => {
            "append the script to ~/.zshrc, or eval it inline: eval \"$(cos agent shell-hooks init zsh | jq -r .script)\""
        }
        Shell::Fish => {
            "append the script to ~/.config/fish/config.fish, or source it: cos agent shell-hooks init fish | jq -r .script | source"
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/diagnostic_commands.rs"
    ));
}
