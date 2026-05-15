//! The `cos perms ...` subcommand suite.
//!
//! This is the user- and app-facing CLI surface for the capability
//! system. Today it exposes one verb (`check`); the design space for
//! the rest is in the master plan doc (perms list / show / revoke /
//! audit / undo).
//!
//! The Python helper at `apps/_lib/policy.py` shells out to
//! `cos perms check <verb> --<scope-kind> <value>` to gate operations
//! inside Python apps. The JSON envelope here is therefore part of a
//! stable contract — keep the shape backwards compatible.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::approvals::{self, GrantDuration};
use crate::caps::{lookup_meta, require, Risk, Scope, Verb};

/// Entry point for `cos perms <subcommand> [args…]`. Wired in
/// `router.rs`.
pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "check" => cmd_check(args),
        "undo" => cmd_undo(args),
        "trash" => cmd_trash(args),
        "gc" => cmd_gc(args),
        "ask" => cmd_ask(args),
        "pending" => cmd_pending(args),
        "approve" => cmd_approve(args),
        "deny" => cmd_deny(args),
        "recent" => cmd_recent(args),
        _ => Err(format!("unknown perms command: {command}")),
    }
}

// ---------------------------------------------------------------------------
// perms check
// ---------------------------------------------------------------------------

/// Run a single capability check from the command line.
///
/// ```text
/// cos perms check fs.read       --path /home/jay/notes.md
/// cos perms check net.dial      --host api.github.com:443
/// cos perms check secret.read   --name openai/api-key
/// cos perms check ui.notify                            # no scope = wild
/// cos perms check fs.delete     --path /tmp/x --wild   # explicit wild
/// ```
///
/// Output is a JSON document with a `decision` field of `allow` or
/// `deny`. On deny, the [`Denial`](crate::caps::Denial) is embedded
/// alongside, so callers do not need a second round-trip to learn
/// why.
fn cmd_check(args: &[String]) -> Result<Value, String> {
    if args.is_empty() {
        return Err("usage: cos perms check <verb> [--path <p> | --host <h> | --name <n> | --wild]"
            .into());
    }
    let verb_str = &args[0];
    let verb = Verb::parse(verb_str)
        .ok_or_else(|| format!("unknown verb `{verb_str}` (see `cos perms verbs`)"))?;

    let mut scope: Option<Scope> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--path" if i + 1 < args.len() => {
                scope = Some(Scope::path(&args[i + 1]));
                i += 2;
            }
            "--host" if i + 1 < args.len() => {
                scope = Some(Scope::host(&args[i + 1]));
                i += 2;
            }
            "--name" if i + 1 < args.len() => {
                scope = Some(Scope::name(&args[i + 1]));
                i += 2;
            }
            "--self" if i + 1 < args.len() => {
                scope = Some(Scope::self_ref(&args[i + 1]));
                i += 2;
            }
            "--wild" => {
                scope = Some(Scope::Wild);
                i += 1;
            }
            other => return Err(format!("unexpected arg: {other}")),
        }
    }

    let scope = scope.unwrap_or(Scope::Wild);

    match require(verb, scope.clone()) {
        Ok(()) => Ok(json!({
            "decision": "allow",
            "verb": verb.as_str(),
            "scope": scope,
        })),
        Err(d) => {
            let mut obj = d.to_json();
            // Splice the explicit `decision` discriminator at the top
            // so consumers can branch on one field.
            obj.as_object_mut()
                .map(|m| m.insert("decision".into(), Value::String("deny".into())));
            Ok(obj)
        }
    }
}

// ---------------------------------------------------------------------------
// perms trash / undo / gc  ―  reverse-replay the snapshots that the
// `apps/_lib/snapshot.py` helper wrote before every gated fs mutation.
// The on-disk layout (`$COS_DATA_DIR/trash/<sid>/<seq>/{meta.json,blob}`)
// is the contract documented in that module and in
// `docs/07-design-decisions.md` § 3.
// ---------------------------------------------------------------------------

fn data_root() -> PathBuf {
    PathBuf::from(std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()))
}

fn trash_root() -> PathBuf {
    data_root().join("trash")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TrashMeta {
    op: String,
    path: String,
    kind: String,
    #[serde(default)]
    snapshot_at: u64,
    #[serde(default)]
    session: String,
    #[serde(default)]
    seq: String,
}

#[derive(Debug)]
struct TrashEntry {
    meta: TrashMeta,
    dir: PathBuf,
}

fn load_entries(session_id: &str) -> Vec<TrashEntry> {
    let sid_dir = trash_root().join(session_id);
    let mut out = Vec::new();
    let read = match fs::read_dir(&sid_dir) {
        Ok(r) => r,
        Err(_) => return out,
    };
    let mut seqs: Vec<PathBuf> = read
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    seqs.sort();
    for dir in seqs {
        let meta_path = dir.join("meta.json");
        let Ok(data) = fs::read_to_string(&meta_path) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<TrashMeta>(&data) else {
            continue;
        };
        out.push(TrashEntry { meta, dir });
    }
    out
}

/// `cos perms trash [--session ID]` — show what snapshots exist for
/// the session (defaults to `$COS_SESSION`).
fn cmd_trash(args: &[String]) -> Result<Value, String> {
    let sid = parse_session_arg(args)?;
    let entries = load_entries(&sid);
    let rows: Vec<Value> = entries
        .iter()
        .map(|e| {
            json!({
                "seq": e.meta.seq,
                "op": e.meta.op,
                "path": e.meta.path,
                "kind": e.meta.kind,
                "snapshot_at": e.meta.snapshot_at,
            })
        })
        .collect();
    Ok(json!({
        "session": sid,
        "count": rows.len(),
        "entries": rows,
    }))
}

/// `cos perms undo [--session ID] [--dry-run]` — reverse-replay every
/// snapshot recorded for the session, restoring the on-disk state to
/// what it was before the gated operations ran. Always returns a
/// per-entry report.
///
/// Routing:
///
/// - If the sid points at a **durable session**
///   (`<COS_DATA_DIR>/sessions/<sid>/meta.json` exists), we replay
///   `mutations.jsonl` via [`crate::session::rollback`]. This is the
///   path long-lived agent runtimes go through.
/// - Otherwise we fall back to the legacy per-CLI trash directory
///   (`<COS_DATA_DIR>/trash/<sid>/`), preserving how every existing
///   `cos fs write` invocation has worked since before durable
///   sessions existed.
fn cmd_undo(args: &[String]) -> Result<Value, String> {
    let mut dry_run = false;
    let mut rest = Vec::new();
    for a in args {
        if a == "--dry-run" {
            dry_run = true;
        } else {
            rest.push(a.clone());
        }
    }
    let sid = parse_session_arg(&rest)?;

    if is_durable_session(&sid) {
        return cmd_undo_durable(&sid, dry_run);
    }

    let entries = load_entries(&sid);
    if entries.is_empty() {
        return Ok(json!({
            "session": sid,
            "undone": 0,
            "entries": [],
            "note": "no snapshots found",
        }));
    }

    let mut report = Vec::with_capacity(entries.len());
    for entry in entries.iter().rev() {
        let mut rec = json!({
            "seq": entry.meta.seq,
            "path": entry.meta.path,
            "op": entry.meta.op,
            "kind": entry.meta.kind,
        });
        if dry_run {
            rec.as_object_mut().unwrap().insert("dry_run".into(), json!(true));
            report.push(rec);
            continue;
        }
        match restore_entry(entry) {
            Ok(action) => {
                rec.as_object_mut().unwrap().insert("action".into(), json!(action));
                rec.as_object_mut().unwrap().insert("ok".into(), json!(true));
            }
            Err(e) => {
                rec.as_object_mut().unwrap().insert("ok".into(), json!(false));
                rec.as_object_mut().unwrap().insert("error".into(), json!(e));
            }
        }
        report.push(rec);
    }

    // Once everything is restored, drop the trash dir so a second
    // `cos perms undo` is a no-op rather than a double-restore.
    if !dry_run {
        let _ = fs::remove_dir_all(trash_root().join(&sid));
    }

    Ok(json!({
        "session": sid,
        "undone": report.len(),
        "dry_run": dry_run,
        "entries": report,
    }))
}

/// True if `sid_str` is the id of a durable session that lives under
/// `<COS_DATA_DIR>/sessions/`. A malformed sid (anything that fails
/// the `SessionId` parser) is reported as not durable so we fall
/// through to the legacy CLI-session trash path without complaining.
fn is_durable_session(sid_str: &str) -> bool {
    let Ok(sid) = sid_str.parse::<crate::session::SessionId>() else {
        return false;
    };
    crate::session::session_dir(&sid).join("meta.json").is_file()
}

/// Rollback path for durable sessions. Uses the typed mutation log
/// (`<session>/mutations.jsonl`) instead of the legacy trash dir.
fn cmd_undo_durable(sid_str: &str, dry_run: bool) -> Result<Value, String> {
    let sid: crate::session::SessionId = sid_str
        .parse()
        .map_err(|e: crate::session::InvalidSessionId| e.to_string())?;

    let muts = crate::session::iter_mutations(&sid)
        .map_err(|e| format!("read mutations: {e}"))?;

    if dry_run {
        let entries: Vec<Value> = muts
            .iter()
            .rev()
            .map(|rec| {
                json!({
                    "seq": rec.seq,
                    "mutation": &rec.mutation,
                    "dry_run": true,
                })
            })
            .collect();
        return Ok(json!({
            "session": sid_str,
            "kind": "durable",
            "undone": entries.len(),
            "dry_run": true,
            "entries": entries,
        }));
    }

    let outcomes = crate::session::rollback(&sid)
        .map_err(|e| format!("rollback: {e}"))?;
    let entries: Vec<Value> = outcomes
        .iter()
        .map(|o| {
            json!({
                "seq": o.seq,
                "verb": o.verb,
                "status": o.status,
                "detail": o.detail,
                "ok": matches!(
                    o.status,
                    crate::session::RollbackStatus::Restored
                        | crate::session::RollbackStatus::AlreadyDone
                ),
            })
        })
        .collect();
    Ok(json!({
        "session": sid_str,
        "kind": "durable",
        "undone": entries.len(),
        "dry_run": false,
        "entries": entries,
    }))
}

fn restore_entry(entry: &TrashEntry) -> Result<&'static str, String> {
    let target = Path::new(&entry.meta.path);
    match entry.meta.kind.as_str() {
        "absent" => {
            if target.is_dir() && !target.is_symlink() {
                fs::remove_dir_all(target).map_err(|e| e.to_string())?;
            } else if target.exists() || target.is_symlink() {
                fs::remove_file(target).map_err(|e| e.to_string())?;
            }
            Ok("removed")
        }
        "file" => {
            if target.is_dir() && !target.is_symlink() {
                fs::remove_dir_all(target).map_err(|e| e.to_string())?;
            }
            if let Some(parent) = target.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
            }
            fs::copy(entry.dir.join("blob"), target).map_err(|e| e.to_string())?;
            Ok("restored")
        }
        "dir" => {
            if target.exists() {
                if target.is_dir() && !target.is_symlink() {
                    fs::remove_dir_all(target).map_err(|e| e.to_string())?;
                } else {
                    fs::remove_file(target).map_err(|e| e.to_string())?;
                }
            }
            copy_dir_recursive(&entry.dir.join("blob"), target)?;
            Ok("restored")
        }
        other => Err(format!("unknown kind: {other}")),
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type().map_err(|e| e.to_string())?;
        if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ft.is_symlink() {
            #[cfg(unix)]
            {
                let link = fs::read_link(&from).map_err(|e| e.to_string())?;
                std::os::unix::fs::symlink(link, &to).map_err(|e| e.to_string())?;
            }
            #[cfg(not(unix))]
            {
                fs::copy(&from, &to).map_err(|e| e.to_string())?;
            }
        } else {
            fs::copy(&from, &to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// `cos perms gc [--older-than-days N]` — delete snapshot session
/// dirs whose newest entry is older than `N` days (default 30).
fn cmd_gc(args: &[String]) -> Result<Value, String> {
    let mut older_than: u64 = 30;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--older-than-days" && i + 1 < args.len() {
            older_than = args[i + 1]
                .parse::<u64>()
                .map_err(|_| "--older-than-days must be a positive integer".to_string())?;
            i += 2;
        } else {
            i += 1;
        }
    }
    let root = trash_root();
    if !root.is_dir() {
        return Ok(json!({"deleted": 0, "kept": 0}));
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let cutoff = now.saturating_sub(older_than.saturating_mul(86400));
    let mut deleted = 0usize;
    let mut kept = 0usize;
    let mut deleted_sessions = Vec::new();
    for entry in fs::read_dir(&root).map_err(|e| e.to_string())? {
        let Ok(entry) = entry else { continue };
        let sid_dir = entry.path();
        if !sid_dir.is_dir() {
            continue;
        }
        let mut newest = 0u64;
        if let Ok(seq_dirs) = fs::read_dir(&sid_dir) {
            for seq in seq_dirs.flatten() {
                let meta_path = seq.path().join("meta.json");
                if let Ok(data) = fs::read_to_string(&meta_path) {
                    if let Ok(meta) = serde_json::from_str::<TrashMeta>(&data) {
                        if meta.snapshot_at > newest {
                            newest = meta.snapshot_at;
                        }
                    }
                }
            }
        }
        if newest != 0 && newest < cutoff {
            if fs::remove_dir_all(&sid_dir).is_ok() {
                deleted += 1;
                if let Some(name) = sid_dir.file_name().and_then(|n| n.to_str()) {
                    deleted_sessions.push(name.to_string());
                }
            }
        } else {
            kept += 1;
        }
    }
    Ok(json!({
        "deleted": deleted,
        "kept": kept,
        "older_than_days": older_than,
        "deleted_sessions": deleted_sessions,
    }))
}

fn parse_session_arg(args: &[String]) -> Result<String, String> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--session" && i + 1 < args.len() {
            return Ok(args[i + 1].clone());
        }
        i += 1;
    }
    std::env::var("COS_SESSION")
        .map_err(|_| "no session: pass --session ID or set COS_SESSION".to_string())
}

// ---------------------------------------------------------------------------
// Approval queue subcommands (Phase 11)
//
// These render in two modes:
//   - JSON (default for non-TTY pipes — preserves the agent-first contract)
//   - Terminal-native cards (TTY only — follows the design system at
//     `desktop/agent/docs/design-system.md`: dark surface, brand blue
//     accent (`#005CFE`), traffic-light dots, monospace, risk-tier
//     colour tokens).
// ---------------------------------------------------------------------------

fn use_pretty() -> bool {
    if std::env::var("COS_PERMS_JSON").ok().as_deref() == Some("1") {
        return false;
    }
    std::io::stdout().is_terminal()
}

// ANSI tokens — pulled straight from the design-system doc.
const C_RESET: &str = "\x1b[0m";
const C_DIM: &str = "\x1b[2m\x1b[37m";
// Brand blue (`#005CFE`) rendered via 24-bit truecolor so the CLI accent
// matches the logo dot and app-icon highlights exactly. Modern terminals
// (iTerm2, Terminal.app, gnome-terminal, kitty, alacritty, wezterm,
// foot, …) all support truecolor; older terminals will degrade to the
// nearest 8-bit blue, which is still on-brand.
const C_BRAND: &str = "\x1b[38;2;0;92;254m";
const C_AMBER: &str = "\x1b[33m";
const C_RED: &str = "\x1b[91m";
const C_GREEN: &str = "\x1b[92m";
const C_YELLOW_DOT: &str = "\x1b[93m";
const C_NEUTRAL: &str = "\x1b[37m";

fn risk_color(r: Risk) -> &'static str {
    match r {
        Risk::Low => C_DIM,
        Risk::Medium => C_NEUTRAL,
        Risk::High => C_AMBER,
        Risk::Critical => C_RED,
    }
}

fn risk_label(r: Risk) -> &'static str {
    match r {
        Risk::Low => "low",
        Risk::Medium => "medium",
        Risk::High => "HIGH",
        Risk::Critical => "CRITICAL",
    }
}

fn relative_time(then: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let delta = now.saturating_sub(then);
    match delta {
        0..=4 => "just now".into(),
        5..=59 => format!("{delta}s ago"),
        60..=3599 => format!("{}m ago", delta / 60),
        3600..=86399 => format!("{}h ago", delta / 3600),
        _ => format!("{}d ago", delta / 86400),
    }
}

fn scope_one_line(scope: &Scope) -> String {
    match scope {
        Scope::Path(p) => format!("path:{p}"),
        Scope::Host(h) => format!("host:{h}"),
        Scope::Name(n) => format!("name:{n}"),
        Scope::SelfRef(s) => format!("self:{s}"),
        Scope::Wild => "WILD (covers anything)".into(),
    }
}

fn render_card(req: &approvals::Request) -> String {
    let verb = Verb::parse(&req.verb);
    let (risk, label, blurb) = match verb.and_then(lookup_meta) {
        Some(m) => (m.risk, m.label.en(), m.blurb.en()),
        None => (Risk::Medium, req.verb.as_str(), ""),
    };
    let rc = risk_color(risk);
    let rl = risk_label(risk);

    let header = format!(
        "{red}●{reset} {ylw}●{reset} {grn}●{reset}  {dim}approval{reset} {em}•{reset} {id}",
        red = C_RED,
        ylw = C_YELLOW_DOT,
        grn = C_GREEN,
        dim = C_DIM,
        em = C_BRAND,
        reset = C_RESET,
        id = req.id,
    );
    let mut body = String::new();
    body.push_str(&format!(
        "  {dim}what{reset}      {em}{verb}{reset}  {rc}[{rl}]{reset}\n",
        dim = C_DIM,
        em = C_BRAND,
        reset = C_RESET,
        verb = req.verb,
        rc = rc,
        rl = rl,
    ));
    if !label.is_empty() && label != req.verb {
        body.push_str(&format!(
            "             {dim}{label}{reset}\n",
            dim = C_DIM,
            reset = C_RESET,
            label = label,
        ));
    }
    if !blurb.is_empty() {
        body.push_str(&format!(
            "             {dim}{blurb}{reset}\n",
            dim = C_DIM,
            reset = C_RESET,
            blurb = blurb,
        ));
    }
    body.push_str(&format!(
        "  {dim}where{reset}     {scope}\n",
        dim = C_DIM,
        reset = C_RESET,
        scope = scope_one_line(&req.scope),
    ));
    body.push_str(&format!(
        "  {dim}why{reset}       {reason}\n",
        dim = C_DIM,
        reset = C_RESET,
        reason = req.reason,
    ));
    body.push_str(&format!(
        "  {dim}session{reset}   {sid}\n",
        dim = C_DIM,
        reset = C_RESET,
        sid = req.session,
    ));
    if let Some(r) = &req.requester {
        body.push_str(&format!(
            "  {dim}from{reset}      {requester}\n",
            dim = C_DIM,
            reset = C_RESET,
            requester = r,
        ));
    }
    body.push_str(&format!(
        "  {dim}requested{reset} {when}\n",
        dim = C_DIM,
        reset = C_RESET,
        when = relative_time(req.requested_at),
    ));

    let footer = format!(
        "  {em}${reset} cos perms approve {id}   {dim}|{reset}   {em}${reset} cos perms deny {id}",
        em = C_BRAND,
        reset = C_RESET,
        dim = C_DIM,
        id = req.id,
    );

    let bar = format!("{dim}{bar}{reset}", dim = C_DIM, reset = C_RESET, bar = "─".repeat(78));
    format!("{header}\n{bar}\n{body}{bar}\n{footer}")
}

/// `cos perms pending` — list outstanding approval requests.
fn cmd_pending(args: &[String]) -> Result<Value, String> {
    let _ = args;
    let pending = approvals::list_pending();
    if use_pretty() {
        if pending.is_empty() {
            println!(
                "{dim}no pending requests{reset}",
                dim = C_DIM,
                reset = C_RESET
            );
        } else {
            for (i, req) in pending.iter().enumerate() {
                if i > 0 {
                    println!();
                }
                println!("{}", render_card(req));
            }
        }
    }
    let rows: Vec<Value> = pending
        .iter()
        .map(|r| {
            let verb = Verb::parse(&r.verb);
            let meta = verb.and_then(lookup_meta);
            let meta_obj = meta.map(|m| {
                json!({
                    "label": m.label.en(),
                    "blurb": m.blurb.en(),
                    "icon": m.icon,
                    "risk": format!("{:?}", m.risk).to_lowercase(),
                })
            });
            json!({
                "id": r.id,
                "verb": r.verb,
                "scope": r.scope,
                "session": r.session,
                "reason": r.reason,
                "requester": r.requester,
                "requested_at": r.requested_at,
                "meta": meta_obj,
            })
        })
        .collect();
    Ok(json!({ "count": rows.len(), "pending": rows }))
}

fn parse_flag<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == flag && i + 1 < args.len() {
            return Some(args[i + 1].as_str());
        }
        i += 1;
    }
    None
}

fn positional<'a>(args: &'a [String]) -> Option<&'a str> {
    args.iter().find(|a| !a.starts_with("--")).map(|s| s.as_str())
}

fn typed_confirm_for_critical(req: &approvals::Request) -> Result<(), String> {
    // Critical risk: per docs/07 § 4, require typed confirmation —
    // the approver must re-type the session id. Skip in non-TTY runs
    // (use `--yes-i-mean-it` to override).
    let verb = match Verb::parse(&req.verb) {
        Some(v) => v,
        None => return Ok(()),
    };
    let risk = lookup_meta(verb).map(|m| m.risk).unwrap_or(Risk::Medium);
    if risk != Risk::Critical {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        return Err(format!(
            "critical cap requires typed confirmation; re-run interactively or pass --yes-i-mean-it={}",
            req.session
        ));
    }
    eprintln!(
        "{red}⚠ critical capability{reset} — type the session id ({em}{sid}{reset}) to confirm:",
        red = C_RED,
        em = C_BRAND,
        reset = C_RESET,
        sid = req.session,
    );
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    if line.trim() != req.session {
        return Err("confirmation phrase did not match; refusing to approve".into());
    }
    Ok(())
}

/// `cos perms approve <id> [--duration once|session|forever] [--note ...]`
fn cmd_approve(args: &[String]) -> Result<Value, String> {
    let id = positional(args).ok_or_else(|| "usage: cos perms approve <id>".to_string())?;
    let duration = match parse_flag(args, "--duration") {
        Some(s) => GrantDuration::parse(s)
            .ok_or_else(|| format!("invalid --duration `{s}` (once|session|forever)"))?,
        None => GrantDuration::Once,
    };
    let note = parse_flag(args, "--note").map(|s| s.to_string());
    let decided_by = std::env::var("USER").ok();

    let req = approvals::lookup_pending(id)
        .ok_or_else(|| format!("no pending request `{id}`"))?;

    let bypass = parse_flag(args, "--yes-i-mean-it");
    if bypass.is_some() && bypass.unwrap() == req.session {
        // typed-confirm bypass for scripted approvals
    } else {
        typed_confirm_for_critical(&req)?;
    }

    let resolved = approvals::approve(id, duration, decided_by, note)?;
    if use_pretty() {
        println!(
            "{em}✓ approved{reset} {id} {dim}({dur:?}){reset}",
            em = C_BRAND,
            dim = C_DIM,
            reset = C_RESET,
            id = id,
            dur = duration,
        );
    }
    Ok(json!({
        "ok": true,
        "id": id,
        "outcome": "approved",
        "duration": format!("{:?}", duration).to_lowercase(),
        "decided_at": resolved.decision.decided_at,
    }))
}

/// `cos perms deny <id> [--note ...]`
fn cmd_deny(args: &[String]) -> Result<Value, String> {
    let id = positional(args).ok_or_else(|| "usage: cos perms deny <id>".to_string())?;
    let note = parse_flag(args, "--note").map(|s| s.to_string());
    let decided_by = std::env::var("USER").ok();
    let resolved = approvals::deny(id, decided_by, note)?;
    if use_pretty() {
        println!(
            "{red}✗ denied{reset} {id}",
            red = C_RED,
            reset = C_RESET,
            id = id,
        );
    }
    Ok(json!({
        "ok": true,
        "id": id,
        "outcome": "denied",
        "decided_at": resolved.decision.decided_at,
    }))
}

/// `cos perms ask --verb V --reason "..." [--path P | --host H | --name N | --wild] [--wait SECS]`
/// Used by callers (Python apps, scripts) that want a human in the
/// loop. Submits a request and optionally blocks until decided.
fn cmd_ask(args: &[String]) -> Result<Value, String> {
    let verb_str = parse_flag(args, "--verb")
        .ok_or_else(|| "usage: cos perms ask --verb <V> --reason <text> [--path P|--host H|--name N|--wild] [--wait SECS]".to_string())?;
    let verb = Verb::parse(verb_str)
        .ok_or_else(|| format!("unknown verb `{verb_str}`"))?;
    let reason = parse_flag(args, "--reason").unwrap_or("(no reason given)").to_string();
    let scope = if args.iter().any(|a| a == "--wild") {
        Scope::Wild
    } else if let Some(p) = parse_flag(args, "--path") {
        Scope::Path(p.to_string())
    } else if let Some(h) = parse_flag(args, "--host") {
        Scope::Host(h.to_string())
    } else if let Some(n) = parse_flag(args, "--name") {
        Scope::Name(n.to_string())
    } else {
        Scope::Wild
    };
    let session = parse_session_arg(args)?;
    let requester = std::env::var("COS_APP_ID")
        .ok()
        .or_else(|| std::env::var("USER").ok());

    let id = approvals::submit(verb, scope, session, reason, requester)?;
    if use_pretty() {
        println!(
            "{em}?{reset} submitted approval request {dim}id={reset}{id}",
            em = C_BRAND,
            dim = C_DIM,
            reset = C_RESET,
            id = id,
        );
    }

    let wait_secs: Option<u64> = parse_flag(args, "--wait").and_then(|s| s.parse().ok());
    if let Some(secs) = wait_secs {
        match approvals::wait(&id, Duration::from_secs(secs)) {
            Ok(decision) => Ok(json!({
                "id": id,
                "outcome": format!("{:?}", decision.outcome).to_lowercase(),
                "decided_at": decision.decided_at,
                "note": decision.note,
            })),
            Err(e) => Err(e),
        }
    } else {
        Ok(json!({ "id": id, "pending": true }))
    }
}

/// `cos perms recent [--limit N]` — show the last decided approvals.
fn cmd_recent(args: &[String]) -> Result<Value, String> {
    let limit = parse_flag(args, "--limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(20usize);
    let rows = approvals::list_recent(limit);
    if use_pretty() {
        if rows.is_empty() {
            println!("{dim}no decided approvals yet{reset}", dim = C_DIM, reset = C_RESET);
        } else {
            for r in &rows {
                let mark = match r.decision.outcome {
                    approvals::Outcome::Approved => format!("{}✓{}", C_BRAND, C_RESET),
                    approvals::Outcome::Denied => format!("{}✗{}", C_RED, C_RESET),
                };
                println!(
                    "  {mark}  {dim}{when:>10}{reset}  {id}  {verb}  {dim}{scope}{reset}",
                    mark = mark,
                    dim = C_DIM,
                    reset = C_RESET,
                    when = relative_time(r.decision.decided_at),
                    id = r.request.id,
                    verb = r.request.verb,
                    scope = scope_one_line(&r.request.scope),
                );
            }
        }
    }
    let json_rows: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.request.id,
                "verb": r.request.verb,
                "scope": r.request.scope,
                "outcome": format!("{:?}", r.decision.outcome).to_lowercase(),
                "decided_at": r.decision.decided_at,
                "note": r.decision.note,
            })
        })
        .collect();
    Ok(json!({ "count": json_rows.len(), "recent": json_rows }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_requires_verb_arg() {
        let err = cmd_check(&[]).unwrap_err();
        assert!(err.contains("usage:"));
    }

    #[test]
    fn check_rejects_unknown_verb() {
        let err = cmd_check(&["fs.invalid".into()]).unwrap_err();
        assert!(err.contains("unknown verb"));
    }

    #[test]
    fn check_no_scope_defaults_to_wild_and_permissive_allows() {
        // With COS_PERMS_MODE=permissive (opt-in escape hatch) and no
        // COS_SESSION, every check is allowed. Save/restore env to
        // avoid polluting other tests.
        let prev_sess = std::env::var("COS_SESSION").ok();
        let prev_mode = std::env::var("COS_PERMS_MODE").ok();
        std::env::remove_var("COS_SESSION");
        std::env::set_var("COS_PERMS_MODE", "permissive");
        let v = cmd_check(&["ui.notify".into()]).unwrap();
        assert_eq!(v["decision"], "allow");
        if let Some(p) = prev_sess {
            std::env::set_var("COS_SESSION", p);
        }
        match prev_mode {
            Some(m) => std::env::set_var("COS_PERMS_MODE", m),
            None => std::env::remove_var("COS_PERMS_MODE"),
        }
    }

    #[test]
    fn check_with_path_scope_encodes_into_response() {
        let prev_sess = std::env::var("COS_SESSION").ok();
        let prev_mode = std::env::var("COS_PERMS_MODE").ok();
        std::env::remove_var("COS_SESSION");
        std::env::set_var("COS_PERMS_MODE", "permissive");
        let v = cmd_check(&["fs.read".into(), "--path".into(), "/tmp/x".into()]).unwrap();
        assert_eq!(v["decision"], "allow");
        assert_eq!(v["verb"], "fs.read");
        assert_eq!(v["scope"]["kind"], "path");
        assert_eq!(v["scope"]["value"], "/tmp/x");
        if let Some(p) = prev_sess {
            std::env::set_var("COS_SESSION", p);
        }
        match prev_mode {
            Some(m) => std::env::set_var("COS_PERMS_MODE", m),
            None => std::env::remove_var("COS_PERMS_MODE"),
        }
    }
}
