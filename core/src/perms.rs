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
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::caps::{require, Scope, Verb};

/// Entry point for `cos perms <subcommand> [args…]`. Wired in
/// `router.rs`.
pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "check" => cmd_check(args),
        "undo" => cmd_undo(args),
        "trash" => cmd_trash(args),
        "gc" => cmd_gc(args),
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
