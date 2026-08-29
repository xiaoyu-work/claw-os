use super::tools;
use serde_json::{json, Value};

/// `cos agent redact <text> [--strict] [--check]`
/// `cos agent redact --file <path> [--strict] [--check]`
/// `cos agent redact --stdin [--strict] [--check]`
///
/// Standalone interface to [`crate::agent::safety::redact::Redactor`].
/// Useful for grepping a log file before posting to a bug report,
/// scrubbing pasted output before piping into a notebook, or scripting
/// "did this string contain secrets?" gates in CI without spinning up
/// a full agent loop.
///
/// `--strict` enables email redaction (off by default — most emails
/// are legitimate content).
///
/// `--check` returns `{contains_secrets: bool, pattern_count: N}`
/// instead of redacting, so callers can branch on detection.
pub(super) fn redact_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::safety::redact::Redactor;

    let mut strict = false;
    let mut check = false;
    let mut from_stdin = false;
    let mut from_file: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--strict" => {
                strict = true;
                i += 1;
            }
            "--check" => {
                check = true;
                i += 1;
            }
            "--stdin" => {
                from_stdin = true;
                i += 1;
            }
            "--file" => {
                from_file = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--file needs a path".to_string())?,
                );
                i += 2;
            }
            other => {
                positional.push(other.to_string());
                i += 1;
            }
        }
    }

    let input = if from_stdin {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("read stdin: {e}"))?;
        buf
    } else if let Some(path) = from_file {
        std::fs::read_to_string(&path).map_err(|e| format!("read {path}: {e}"))?
    } else if positional.is_empty() {
        return Err(
            "usage: cos agent redact <text> | --file <path> | --stdin [--strict] [--check]"
                .to_string(),
        );
    } else {
        positional.join(" ")
    };

    let r = if strict {
        Redactor::strict()
    } else {
        Redactor::default_set()
    };

    if check {
        Ok(json!({
            "contains_secrets": r.contains_secrets(&input),
            "pattern_count": r.pattern_count(),
            "input_chars": input.chars().count(),
            "strict": strict,
        }))
    } else {
        let redacted = r.redact(&input);
        let changed = redacted != input;
        Ok(json!({
            "redacted": redacted,
            "changed": changed,
            "input_chars": input.chars().count(),
            "output_chars": redacted.chars().count(),
            "pattern_count": r.pattern_count(),
            "strict": strict,
        }))
    }
}

/// `cos agent tools [list [--unfiltered]|show <name>|llm-list]`
/// — read-only tool registry inspection. `list` (default) returns the
/// permitted set under the runtime's guardrails (mirrors what the LLM
/// sees), with `--unfiltered` showing every registered tool including
/// those denied by config. `show <name>` returns the full schema
/// (description + JSON Schema input shape) — the same blob sent to
/// the model. `llm-list` returns the exact `Vec<llm::Tool>` the
/// model would receive (filtered).
///
/// All three subcommands construct the *same* registry+guardrails
/// pair the runtime would build, so what you see here is what the
/// model would see if you ran `cos agent ask` in the same env.
pub(super) fn tools_cmd(args: &[String]) -> Result<Value, String> {
    let config = crate::config::current_snapshot();
    let cfg = &config.agent;
    let deps = tools::registry::RegistryDeps::load_current();
    let mut registry = tools::registry::default_registry_with_deps(&deps);
    registry.set_guardrails(crate::agent::runtime::loop_::guardrails_from_cfg(cfg));
    registry.set_approval(crate::agent::runtime::loop_::approval_from_cfg(cfg));

    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" => {
            let mut unfiltered = false;
            for arg in args.iter().skip(1) {
                match arg.as_str() {
                    "--unfiltered" => unfiltered = true,
                    other => {
                        return Err(format!(
                            "unknown tools list flag: {other}. try: --unfiltered"
                        ));
                    }
                }
            }
            let names: Vec<&str> = if unfiltered {
                registry.names_unfiltered()
            } else {
                registry.names()
            };
            let entries: Vec<Value> = names
                .iter()
                .filter_map(|n| {
                    registry.get_unfiltered(n).map(|t| {
                        let permitted = registry.guardrails().permits(n);
                        json!({
                            "name": n,
                            "description": t.description(),
                            "permitted": permitted,
                        })
                    })
                })
                .collect();
            Ok(json!({
                "registered_total": registry.names_unfiltered().len(),
                "permitted_count": registry.names().len(),
                "unfiltered": unfiltered,
                "tools": entries,
            }))
        }
        "show" => {
            let name = args
                .get(1)
                .cloned()
                .ok_or_else(|| "usage: cos agent tools show <name>".to_string())?;
            let tool = registry
                .get_unfiltered(&name)
                .ok_or_else(|| format!("tool '{name}' not registered"))?;
            Ok(json!({
                "name": tool.name(),
                "description": tool.description(),
                "input_schema": tool.input_schema(),
                "permitted": registry.guardrails().permits(&name),
            }))
        }
        "llm-list" => {
            let llm_tools = if cfg.progressive_tools_enabled {
                registry.as_llm_tools_progressive()
            } else {
                tools::guardrails::filter_llm_tools(&registry, registry.guardrails())
            };
            Ok(json!({
                "count": llm_tools.len(),
                "progressive_tools_enabled": cfg.progressive_tools_enabled,
                "tools": llm_tools.iter().map(|t| json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                })).collect::<Vec<_>>(),
            }))
        }
        other => Err(format!(
            "unknown tools subcommand: {other}. try: list [--unfiltered] | show <name> | llm-list"
        )),
    }
}

/// `cos agent guardrails [show|check <tool>]`
/// — surface the allow/deny tool guardrails the runtime would build
/// from the current `AgentConfig`. `show` (default) reports the
/// active allow + deny lists. `check <tool>` runs the decision for
/// `<tool>` and returns `{permitted, decision: "allow"|"deny", reason?}`.
///
/// Useful for verifying that a `tool_allow`/`tool_deny` change in
/// `~/.config/cos/config.json` is actually parsed the way you expect
/// before running a session.
pub(super) fn guardrails_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::tools::guardrails::Decision;
    let config = crate::config::current_snapshot();
    let cfg = &config.agent;
    let g = crate::agent::runtime::loop_::guardrails_from_cfg(cfg);

    let sub = args.first().map(|s| s.as_str()).unwrap_or("show");
    match sub {
        "show" => {
            let allow_arr: Option<Vec<String>> = g.allow.as_ref().map(|set| {
                let mut v: Vec<String> = set.iter().cloned().collect();
                v.sort();
                v
            });
            let mut deny_arr: Vec<String> = g.deny.iter().cloned().collect();
            deny_arr.sort();
            Ok(json!({
                "mode": if g.allow.is_some() { "allowlist" } else { "permissive" },
                "allow": allow_arr,
                "deny": deny_arr,
                "deny_count": deny_arr.len(),
                "config_tool_allow": cfg.tool_allow.clone(),
                "config_tool_deny": cfg.tool_deny.clone(),
            }))
        }
        "check" => {
            let tool = args
                .get(1)
                .cloned()
                .ok_or_else(|| "usage: cos agent guardrails check <tool>".to_string())?;
            let decision = g.decide(&tool);
            let (verdict, reason) = match &decision {
                Decision::Allow => ("allow", None),
                Decision::Deny(r) => ("deny", Some(r.clone())),
            };
            Ok(json!({
                "tool": tool,
                "permitted": g.permits(&tool),
                "decision": verdict,
                "reason": reason,
            }))
        }
        other => Err(format!(
            "unknown guardrails subcommand: {other}. try: show | check <tool>"
        )),
    }
}

/// `cos agent approval [show|check <tool> [--input '<json>']]`
/// — surface the approval gate the runtime would build from the
/// current `AgentConfig` (auto_approve_tools / auto_deny_tools /
/// dangerous_tools). `show` lists the three sets. `check <tool>`
/// runs `ApprovalGate::evaluate` against the tool name and returns
/// the outcome (`approved` / `denied` / `deferred`).
///
/// Headless: no interactive approver is configured, so `dangerous`
/// tools without an explicit auto_approve return `deferred` — the
/// same outcome the runtime would surface back to the model as an
/// error tool_result. `--input` lets you pass a hypothetical JSON
/// payload (the gate doesn't shape-match yet but will once the
/// per-call predicate hooks land).
pub(super) fn approval_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::runtime::approval::ApprovalOutcome;
    let config = crate::config::current_snapshot();
    let cfg = &config.agent;
    let gate = crate::agent::runtime::loop_::approval_from_cfg(cfg);

    let sub = args.first().map(|s| s.as_str()).unwrap_or("show");
    match sub {
        "show" => {
            let acfg = gate.config();
            let mut auto_approve: Vec<String> = acfg.auto_approve.iter().cloned().collect();
            let mut auto_deny: Vec<String> = acfg.auto_deny.iter().cloned().collect();
            let mut dangerous: Vec<String> = acfg.dangerous.iter().cloned().collect();
            auto_approve.sort();
            auto_deny.sort();
            dangerous.sort();
            Ok(json!({
                "auto_approve": auto_approve,
                "auto_deny": auto_deny,
                "dangerous": dangerous,
                "config_auto_approve_tools": cfg.auto_approve_tools.clone(),
                "config_auto_deny_tools": cfg.auto_deny_tools.clone(),
                "config_dangerous_tools": cfg.dangerous_tools.clone(),
            }))
        }
        "check" => {
            let tool = args.get(1).cloned().ok_or_else(|| {
                "usage: cos agent approval check <tool> [--input '<json>']".to_string()
            })?;
            let mut input: Value = Value::Null;
            let mut i = 2usize;
            while i < args.len() {
                if args[i].as_str() == "--input" {
                    let raw = args
                        .get(i + 1)
                        .ok_or_else(|| "--input needs a JSON string".to_string())?;
                    input = serde_json::from_str(raw)
                        .map_err(|e| format!("--input: invalid JSON: {e}"))?;
                    i += 2;
                } else {
                    return Err(format!(
                        "unknown approval check flag: {}. try: --input <json>",
                        args[i]
                    ));
                }
            }
            // ApprovalGate::evaluate is async; spin a small runtime.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("tokio runtime: {e}"))?;
            let outcome = runtime.block_on(gate.evaluate(&tool, &input, "cli probe"));
            let (decision, note, reason, prompt) = match &outcome {
                ApprovalOutcome::Approved { note } => ("approved", note.clone(), None, None),
                ApprovalOutcome::Denied { reason } => ("denied", None, reason.clone(), None),
                ApprovalOutcome::Deferred { prompt } => ("deferred", None, None, prompt.clone()),
            };
            Ok(json!({
                "tool": tool,
                "decision": decision,
                "note": note,
                "reason": reason,
                "prompt": prompt,
                "would_short_circuit": gate.would_short_circuit(&tool),
            }))
        }
        other => Err(format!(
            "unknown approval subcommand: {other}. try: show | check <tool> [--input '<json>']"
        )),
    }
}

/// `cos agent binary-ext <list [--limit N]|check <path>|extensions>`
///
/// Surfaces [`crate::agent::safety::binary_ext`] so operators can:
///   * `check <path>` — quickly classify whether a file would be
///     treated as binary by the agent's IO helpers.
///   * `list [--limit N]` — inspect the active classifier's
///     extension set (sorted, optionally truncated).
///   * `extensions` — alias of `list` with no truncation, useful
///     when you want the raw set.
pub(super) fn binary_ext_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" | "" => {
            let mut limit: Option<usize> = Some(50);
            let mut i = 1usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--limit" => {
                        let v = args
                            .get(i + 1)
                            .ok_or_else(|| "--limit needs <n>".to_string())?;
                        limit = Some(
                            v.parse()
                                .map_err(|_| {
                                    format!("--limit must be a positive integer, got: {v}")
                                })?,
                        );
                        i += 2;
                    }
                    "--no-limit" => {
                        limit = None;
                        i += 1;
                    }
                    other => {
                        return Err(format!("unknown flag for `binary-ext list`: {other}"));
                    }
                }
            }
            let c = crate::agent::safety::binary_ext::BinaryExtensions::default();
            let total = c.len();
            let exts: Vec<&str> = match limit {
                Some(n) => c.iter().take(n).collect(),
                None => c.iter().collect(),
            };
            Ok(json!({
                "total": total,
                "limit": limit,
                "n": exts.len(),
                "extensions": exts,
            }))
        }
        "extensions" => {
            let c = crate::agent::safety::binary_ext::BinaryExtensions::default();
            let exts: Vec<&str> = c.iter().collect();
            Ok(json!({
                "total": c.len(),
                "extensions": exts,
            }))
        }
        "check" => {
            let raw = args
                .get(1)
                .ok_or_else(|| "usage: cos agent binary-ext check <path-or-extension>".to_string())?;
            let c = crate::agent::safety::binary_ext::BinaryExtensions::default();
            // Heuristic: if it looks like a bare extension (no path
            // separator, at most one leading `.`), treat it as such;
            // otherwise treat as a path.
            let looks_like_extension = !raw.contains(['/', '\\'])
                && (raw.starts_with('.') || !raw.contains('.'))
                && !raw.contains(' ');
            let (mode, is_binary, ext_resolved): (&str, bool, Option<String>) =
                if looks_like_extension {
                    let key = raw.trim().trim_start_matches('.').to_ascii_lowercase();
                    (
                        "extension",
                        c.contains_extension(raw),
                        if key.is_empty() { None } else { Some(key) },
                    )
                } else {
                    let p = std::path::Path::new(raw);
                    let ext = p
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|s| s.to_ascii_lowercase());
                    ("path", c.is_binary_path(p), ext)
                };
            Ok(json!({
                "input": raw,
                "mode": mode,
                "extension": ext_resolved,
                "is_binary": is_binary,
                "set_size": c.len(),
            }))
        }
        other => Err(format!(
            "unknown binary-ext subcommand: {other}. try: list [--limit N] [--no-limit] | extensions | check <path-or-extension>"
        )),
    }
}

/// `cos agent file-safety [check <path>|batch <path>...|categories]`
pub(super) fn file_safety_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "" => Err(
            "usage: cos agent file-safety [check <path> | batch <path>... | categories]"
                .to_string(),
        ),
        "check" => file_safety_check_cmd(&args[1..]),
        "batch" => file_safety_batch_cmd(&args[1..]),
        "categories" => Ok(json!({
            "categories": [
                "dangerous_extension",
                "credential",
                "system_directory",
                "vcs_internal",
            ],
            "verdicts": ["allow", "caution", "deny"],
        })),
        other => Err(format!(
            "unknown file-safety subcommand: {other}. try: check | batch | categories"
        )),
    }
}

fn file_safety_check_cmd(args: &[String]) -> Result<Value, String> {
    if args.is_empty() {
        return Err("usage: cos agent file-safety check <path>".to_string());
    }
    if args.len() > 1 {
        return Err(
            "file-safety check accepts a single path; use 'batch' for multiple".to_string(),
        );
    }
    let path = &args[0];
    let v = crate::agent::safety::file_safety::classify_str(path);
    Ok(file_safety_to_json(path, &v))
}

fn file_safety_batch_cmd(args: &[String]) -> Result<Value, String> {
    if args.is_empty() {
        return Err("usage: cos agent file-safety batch <path>...".to_string());
    }
    let mut results: Vec<Value> = Vec::with_capacity(args.len());
    let mut allow_count = 0u64;
    let mut caution_count = 0u64;
    let mut deny_count = 0u64;
    for path in args {
        let v = crate::agent::safety::file_safety::classify_str(path);
        match v {
            crate::agent::safety::file_safety::FileSafety::Allow => allow_count += 1,
            crate::agent::safety::file_safety::FileSafety::Caution { .. } => caution_count += 1,
            crate::agent::safety::file_safety::FileSafety::Deny { .. } => deny_count += 1,
        }
        results.push(file_safety_to_json(path, &v));
    }
    Ok(json!({
        "count": args.len(),
        "results": results,
        "summary": {
            "allow":   allow_count,
            "caution": caution_count,
            "deny":    deny_count,
        },
    }))
}

fn file_safety_to_json(path: &str, v: &crate::agent::safety::file_safety::FileSafety) -> Value {
    json!({
        "path":     path,
        "verdict":  v.label(),
        "reason":   v.reason(),
        "category": v.category().map(|c| c.as_str()),
    })
}

/// `cos agent osv [parse <file>|check <file>|query <name>@<version> --ecosystem <eco>|ecosystems]`
pub(super) fn osv_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "" => Err(
            "usage: cos agent osv [parse <file> | check <file> | query <name>@<version> --ecosystem <eco> | ecosystems]"
                .to_string(),
        ),
        "parse" => osv_parse_cmd(&args[1..]),
        "check" => osv_check_cmd(&args[1..]),
        "query" => osv_query_cmd(&args[1..]),
        "ecosystems" => Ok(json!({
            "ecosystems": [
                "crates.io",
                "npm",
                "PyPI",
                "Go",
            ],
            "lockfiles": [
                "Cargo.lock",
                "package-lock.json",
                "requirements.txt",
                "go.sum",
            ],
        })),
        other => Err(format!(
            "unknown osv subcommand: {other}. try: parse | check | query | ecosystems"
        )),
    }
}

fn osv_parse_cmd(args: &[String]) -> Result<Value, String> {
    if args.is_empty() {
        return Err("usage: cos agent osv parse <lockfile>".to_string());
    }
    if args.len() > 1 {
        return Err("osv parse accepts a single file argument".to_string());
    }
    let path = std::path::Path::new(&args[0]);
    let body =
        std::fs::read_to_string(path).map_err(|e| format!("osv: read {}: {e}", path.display()))?;
    let pkgs = crate::agent::safety::osv::parse_lockfile(path, &body)?;
    Ok(json!({
        "lockfile": path.display().to_string(),
        "count":    pkgs.len(),
        "packages": pkgs.iter().map(|p| p.to_json()).collect::<Vec<_>>(),
    }))
}

fn osv_check_cmd(args: &[String]) -> Result<Value, String> {
    if args.is_empty() {
        return Err("usage: cos agent osv check <lockfile>".to_string());
    }
    if args.len() > 1 {
        return Err("osv check accepts a single file argument".to_string());
    }
    let path = std::path::Path::new(&args[0]);
    let body =
        std::fs::read_to_string(path).map_err(|e| format!("osv: read {}: {e}", path.display()))?;
    let pkgs = crate::agent::safety::osv::parse_lockfile(path, &body)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("osv: build runtime: {e}"))?;
    // Sequential per-package querying was the dominant cost here.
    // A typical Cargo.lock contains hundreds of crates; at ~300 ms
    // round-trip per OSV.dev call that's a 30+ s `cos agent osv
    // check`. Fan out to a small worker pool — capped at 8 to stay
    // under OSV.dev's polite-client guidance and to keep memory
    // bounded — and merge results in the original order so the
    // emitted JSON remains stable.
    use futures_util::stream::{self, StreamExt};
    const CONCURRENCY: usize = 8;
    let scored: Vec<(usize, Vec<crate::agent::safety::osv::OsvVulnerability>)> =
        rt.block_on(async {
            stream::iter(pkgs.iter().enumerate())
                .map(|(idx, pkg)| async move {
                    let vulns = crate::agent::safety::osv::query(pkg)
                        .await
                        .unwrap_or_else(|e| {
                            tracing::warn!(
                                "osv: {} {} {}: {}",
                                pkg.ecosystem,
                                pkg.name,
                                pkg.version,
                                e
                            );
                            Vec::new()
                        });
                    (idx, vulns)
                })
                .buffer_unordered(CONCURRENCY)
                .collect()
                .await
        });
    let mut by_idx: Vec<Vec<crate::agent::safety::osv::OsvVulnerability>> =
        (0..pkgs.len()).map(|_| Vec::new()).collect();
    for (idx, v) in scored {
        by_idx[idx] = v;
    }
    let mut total_vulns = 0u64;
    let mut results = Vec::new();
    for (pkg, vulns) in pkgs.iter().zip(by_idx) {
        total_vulns += vulns.len() as u64;
        if !vulns.is_empty() {
            results.push(json!({
                "package": pkg.to_json(),
                "vulns":   vulns.iter().map(|v| v.to_json()).collect::<Vec<_>>(),
            }));
        }
    }
    Ok(json!({
        "lockfile":      path.display().to_string(),
        "package_count": pkgs.len(),
        "vuln_count":    total_vulns,
        "results":       results,
    }))
}

fn osv_query_cmd(args: &[String]) -> Result<Value, String> {
    let mut name_at_version: Option<String> = None;
    let mut ecosystem: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--ecosystem" => {
                i += 1;
                ecosystem = Some(
                    args.get(i)
                        .ok_or_else(|| "missing value for --ecosystem".to_string())?
                        .clone(),
                );
            }
            other if other.starts_with("--") => {
                return Err(format!("osv query: unknown flag: {other}"));
            }
            other => {
                if name_at_version.is_some() {
                    return Err("osv query: extra positional argument".to_string());
                }
                name_at_version = Some(other.to_string());
            }
        }
        i += 1;
    }
    let coord = name_at_version.ok_or_else(|| {
        "usage: cos agent osv query <name>@<version> --ecosystem <eco>".to_string()
    })?;
    let (name, version) = coord
        .rsplit_once('@')
        .ok_or_else(|| format!("osv query: '{coord}' is not in <name>@<version> format"))?;
    if name.is_empty() || version.is_empty() {
        return Err("osv query: name and version must both be non-empty".to_string());
    }
    let eco = ecosystem.ok_or_else(|| {
        "osv query: --ecosystem is required (e.g. crates.io, npm, PyPI, Go)".to_string()
    })?;
    let pkg = crate::agent::safety::osv::Package::new(eco, name, version);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("osv: build runtime: {e}"))?;
    let vulns = rt.block_on(crate::agent::safety::osv::query(&pkg))?;
    Ok(json!({
        "package":    pkg.to_json(),
        "vuln_count": vulns.len(),
        "vulns":      vulns.iter().map(|v| v.to_json()).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/safety_commands.rs"
    ));
}
