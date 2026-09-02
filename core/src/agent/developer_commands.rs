use serde_json::{json, Value};

/// `cos agent interrupt <subcmd>` — signal a running agent session
/// so its loop unwinds cleanly between turns.
///
///   list                 — registered (live) session ids
///   signal <session-id>  — request interrupt; idempotent. JSON
///                          `{"signaled": true}` if a session was
///                          found, `{"signaled": false, "reason":
///                          "not registered"}` otherwise.
///
/// Sessions auto-register the moment they enter the agent loop and
/// auto-unregister on exit, so the live list mirrors what's actively
/// running in this `cos` process. Note that this does NOT cross
/// process boundaries — to interrupt sessions running under a
/// separate `cos agent service` worker, use the IPC `service cancel`
/// surface (different mechanism, persisted job-cancellation
/// semantics).
pub(super) fn interrupt_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args
        .first()
        .map(|s| s.as_str())
        .ok_or("usage: cos agent interrupt <list|signal> ...")?;
    match sub {
        "list" => {
            let mut sessions = crate::agent::runtime::interrupt::registered_sessions();
            sessions.sort();
            Ok(json!({
                "sessions": sessions,
                "count": sessions.len(),
            }))
        }
        "signal" => {
            let sid = args
                .get(1)
                .map(|s| s.as_str())
                .ok_or("usage: cos agent interrupt signal <session-id>")?;
            let signaled = crate::agent::runtime::interrupt::signal(sid);
            if signaled {
                Ok(json!({
                    "signaled": true,
                    "session_id": sid,
                }))
            } else {
                Ok(json!({
                    "signaled": false,
                    "session_id": sid,
                    "reason": "not registered (session not running in this process)",
                }))
            }
        }
        other => Err(format!(
            "unknown interrupt subcommand: {other}. try: list | signal"
        )),
    }
}

/// `cos agent hooks <subcmd>` — manage the runtime hook registry
/// and the persistent `data_dir/agent/hooks.json` config that
/// auto-registers hooks on every agent invocation.
///
///   list                       — names currently registered in
///                                this process + persistently
///                                enabled kinds (from disk).
///   enable <kind>              — add `<kind>` to hooks.json and
///                                register it in the current
///                                process. Idempotent.
///   disable <kind>             — remove `<kind>` from hooks.json
///                                and unregister it from the
///                                current process. Idempotent.
///
/// Supported kinds: `logging`. CLI `--kind <k>` form is also
/// accepted for `enable`/`disable` to mirror common subcommand
/// conventions.
pub(super) fn hooks_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::runtime::hooks::global_registry;
    use crate::agent::runtime::hooks_config::{self, HookKind};

    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" => {
            let registry = global_registry();
            let names = registry.names();
            let cfg = hooks_config::load(&crate::paths::agent_hooks_path()).unwrap_or_default();
            let enabled_kinds: Vec<String> = cfg
                .enabled
                .iter()
                .map(|k| k.canonical().to_string())
                .collect();
            Ok(json!({
                "hooks": names.clone(),
                "count": names.len(),
                "persistent": enabled_kinds.clone(),
                "config_path": crate::paths::agent_hooks_path().display().to_string(),
            }))
        }
        "enable" => {
            let kind_str = parse_kind_arg(&args[1..])?;
            let kind = HookKind::parse(&kind_str)
                .ok_or_else(|| format!("unknown hook kind: {kind_str}. try: logging"))?;
            let path = crate::paths::agent_hooks_path();
            let mut cfg = hooks_config::load(&path).map_err(|e| e.to_string())?;
            let added = cfg.enable(kind);
            if added {
                hooks_config::save(&path, &cfg).map_err(|e| e.to_string())?;
            }
            // Also register in the current process so the change is
            // visible to anything else running in this binary
            // invocation (e.g. an immediate follow-up call).
            let registry = global_registry();
            let already = registry.names().iter().any(|n| n == kind.canonical());
            if !already {
                registry.register(hooks_config::instantiate(kind));
            }
            Ok(json!({
                "kind": kind.canonical(),
                "persisted": added,
                "registered_now": !already,
                "config_path": path.display().to_string(),
            }))
        }
        "disable" => {
            let kind_str = parse_kind_arg(&args[1..])?;
            let kind = HookKind::parse(&kind_str)
                .ok_or_else(|| format!("unknown hook kind: {kind_str}. try: logging"))?;
            let path = crate::paths::agent_hooks_path();
            let mut cfg = hooks_config::load(&path).map_err(|e| e.to_string())?;
            let removed = cfg.disable(kind);
            if removed {
                hooks_config::save(&path, &cfg).map_err(|e| e.to_string())?;
            }
            let unreg = global_registry().unregister(kind.canonical());
            Ok(json!({
                "kind": kind.canonical(),
                "persisted": removed,
                "unregistered_now": unreg,
                "config_path": path.display().to_string(),
            }))
        }
        other => Err(format!(
            "unknown hooks subcommand: {other}. try: list | enable <kind> | disable <kind>"
        )),
    }
}

/// Pull the kind out of `<kind>` or `--kind <kind>` positional/flag
/// forms. `cos agent hooks enable logging` and
/// `cos agent hooks enable --kind logging` both work.
fn parse_kind_arg(rest: &[String]) -> Result<String, String> {
    let mut iter = rest.iter();
    match iter.next().map(String::as_str) {
        Some("--kind") => iter
            .next()
            .cloned()
            .ok_or_else(|| "--kind requires a value".to_string()),
        Some(value) if !value.starts_with("--") => Ok(value.to_string()),
        Some(other) => Err(format!("unexpected flag: {other}")),
        None => Err("missing hook kind (positional or --kind <kind>)".to_string()),
    }
}

/// `cos agent context <subcommand>` — surface for the
/// [`crate::agent::context`] modules:
///
///   * `hints [--cwd <path>] [--depth N=0] [--render]` — scan for
///     project markers (Cargo.toml, package.json, .git, …) and
///     either return JSON list or a rendered summary block.
///   * `refs --text <body> [--unique]` — extract `@`-references
///     from a user message body.
///   * `markers` — dump the static marker table for inspection.
pub(super) fn context_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "hints" => context_hints_cmd(&args[1..]),
        "refs" | "references" => context_refs_cmd(&args[1..]),
        "markers" => context_markers_cmd(&args[1..]),
        "build" => context_build_cmd(&args[1..]),
        "" => Err(
            "usage: cos agent context <hints|refs|markers|build> ... \
             (e.g. hints [--cwd <p>] [--depth N] [--render] | refs --text <body> [--unique] | markers | build [--cwd <p>] [--depth N] [--text <body>] [--note <line>...] [--max-refs N] [--max-hints N])"
                .to_string(),
        ),
        other => Err(format!(
            "unknown context subcommand: {other}. try: hints | refs | markers | build"
        )),
    }
}

fn context_hints_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::context::subdir_hints::{render_summary, scan_dir, scan_dir_recursive};

    let mut cwd: Option<String> = None;
    let mut depth: usize = 0;
    let mut render = false;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--cwd" => {
                cwd = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--cwd needs a path".to_string())?,
                );
                i += 2;
            }
            "--depth" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--depth needs a number".to_string())?;
                depth = raw
                    .parse::<usize>()
                    .map_err(|e| format!("--depth parse: {e}"))?;
                i += 2;
            }
            "--render" => {
                render = true;
                i += 1;
            }
            other => return Err(format!("unknown context hints flag: {other}")),
        }
    }

    let root = match cwd {
        Some(s) => std::path::PathBuf::from(s),
        None => std::env::current_dir().map_err(|e| format!("get cwd: {e}"))?,
    };
    if !root.is_dir() {
        return Err(format!("not a directory: {}", root.display()));
    }

    let hits = if depth == 0 {
        scan_dir(&root)
    } else {
        scan_dir_recursive(&root, depth)
    };

    if render {
        return Ok(json!({
            "root": root.to_string_lossy(),
            "depth": depth,
            "count": hits.len(),
            "summary": render_summary(&hits),
        }));
    }

    let hits_json: Vec<Value> = hits
        .iter()
        .map(|h| {
            json!({
                "rel": h.rel,
                "kind": format!("{:?}", h.kind),
                "label": h.label,
                "is_dir": h.is_dir,
            })
        })
        .collect();

    Ok(json!({
        "root": root.to_string_lossy(),
        "depth": depth,
        "count": hits.len(),
        "hints": hits_json,
    }))
}

fn context_refs_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::context::references::{extract, extract_unique};

    let mut text: Option<String> = None;
    let mut unique = false;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--text" => {
                text = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--text needs a value".to_string())?,
                );
                i += 2;
            }
            "--unique" => {
                unique = true;
                i += 1;
            }
            other => return Err(format!("unknown context refs flag: {other}")),
        }
    }

    let body = text.ok_or_else(|| "context refs: --text <body> required".to_string())?;
    let refs = if unique {
        extract_unique(&body)
    } else {
        extract(&body)
    };
    let refs_json: Vec<Value> = refs
        .iter()
        .map(|r| {
            json!({
                "raw": r.raw,
                "kind": format!("{:?}", r.kind),
                "start": r.start,
                "end": r.end,
            })
        })
        .collect();

    Ok(json!({
        "unique": unique,
        "count": refs.len(),
        "references": refs_json,
    }))
}

fn context_markers_cmd(_args: &[String]) -> Result<Value, String> {
    use crate::agent::context::subdir_hints::{HintKind, MARKERS, NOISE_DIRS};
    let by_kind = |k: HintKind| -> Vec<&'static str> {
        let mut v: Vec<&'static str> = MARKERS
            .iter()
            .filter(|m| m.kind == k)
            .map(|m| m.name)
            .collect();
        v.sort();
        v
    };
    Ok(json!({
        "total": MARKERS.len(),
        "by_kind": {
            "Manifest":  by_kind(HintKind::Manifest),
            "Vcs":       by_kind(HintKind::Vcs),
            "Ci":        by_kind(HintKind::Ci),
            "Framework": by_kind(HintKind::Framework),
            "Editor":    by_kind(HintKind::Editor),
            "Env":       by_kind(HintKind::Env),
        },
        "noise_dirs": NOISE_DIRS,
    }))
}

/// `cos agent context build [--cwd <p>] [--depth N] [--text <body>] [--note <line>...] [--max-refs N] [--max-hints N] [--no-dedup]`
fn context_build_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::context::engine::{build, ContextOptions};

    let mut opts = ContextOptions::default();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--cwd" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "missing value for --cwd".to_string())?;
                let p = std::path::PathBuf::from(v);
                if !p.is_dir() {
                    return Err(format!("context build: --cwd is not a directory: {v}"));
                }
                opts.cwd = Some(p);
            }
            "--depth" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "missing value for --depth".to_string())?;
                opts.scan_depth = v
                    .parse()
                    .map_err(|_| format!("--depth: invalid integer: {v}"))?;
            }
            "--text" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "missing value for --text".to_string())?;
                opts.user_text = Some(v.clone());
            }
            "--note" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "missing value for --note".to_string())?;
                opts.notes.push(v.clone());
            }
            "--max-refs" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "missing value for --max-refs".to_string())?;
                opts.max_refs = Some(
                    v.parse()
                        .map_err(|_| format!("--max-refs: invalid integer: {v}"))?,
                );
            }
            "--max-hints" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "missing value for --max-hints".to_string())?;
                opts.max_hints = Some(
                    v.parse()
                        .map_err(|_| format!("--max-hints: invalid integer: {v}"))?,
                );
            }
            "--no-dedup" => {
                opts.dedup_refs = false;
            }
            other => return Err(format!("context build: unknown flag: {other}")),
        }
        i += 1;
    }
    Ok(build(&opts).to_json())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/developer_commands.rs"
    ));
}
