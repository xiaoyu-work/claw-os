use std::env;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value};

use crate::agent;
use crate::ai;
use crate::apps;
use crate::audit;
use crate::bridge;
use crate::checkpoint;
use crate::credential;
use crate::cron;
use crate::engine_pkg;
use crate::model;
use crate::caps;
use crate::perms;
use crate::service;
use crate::sysinfo;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn apps_dir() -> PathBuf {
    PathBuf::from(env::var("COS_APPS_DIR").unwrap_or_else(|_| "/usr/lib/cos/apps".into()))
}

fn data_dir() -> String {
    env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into())
}

fn audit_path() -> PathBuf {
    Path::new(&data_dir()).join("logs").join("audit.jsonl")
}

/// Main dispatch: parse CLI args and route to the appropriate handler.
pub fn dispatch(args: &[String]) -> Result<Option<String>, String> {
    if args.is_empty() {
        return show_overview();
    }

    let name = &args[0];

    // Top-level help / version flags. Match what every Unix CLI does so
    // muscle memory works: bare `cos --help` / `cos help` is the same
    // overview as bare `cos`; `cos help <topic>` drills into one
    // primitive/app; `cos --version` prints just the version envelope.
    match name.as_str() {
        "--help" | "-h" => {
            if args.len() >= 2 {
                return show_help_for(&args[1]);
            }
            return show_overview();
        }
        "help" => {
            if args.len() >= 2 {
                return show_help_for(&args[1]);
            }
            return show_overview();
        }
        "--version" | "-v" | "-V" => {
            return Ok(Some(
                json!({"name": "cos", "version": VERSION}).to_string(),
            ));
        }
        _ => {}
    }

    // Hidden bridge for bundled app runtimes. This is intentionally not a
    // user-facing CLI namespace; interactive permissions are mediated by the
    // Agent UX, while apps only need an internal capability check.
    if name == "__policy" {
        let command = args
            .get(1)
            .ok_or_else(|| "internal policy command required".to_string())?;
        let value = perms::run(command, &args[2..])?;
        return Ok(Some(value.to_string()));
    }

    // "app" namespace → route to Python apps
    if name == "app" {
        return dispatch_app(&args[1..]);
    }

    // Built-in OS primitives
    match name.as_str() {
        "sys" => dispatch_builtin(args, "sys", sysinfo::run),
        "service" => dispatch_builtin(args, "service", service::run),
        "checkpoint" => dispatch_builtin(args, "checkpoint", checkpoint::run),
        "credential" => dispatch_builtin(args, "credential", credential::run),
        "cron" => dispatch_builtin(args, "cron", cron::run),
        "ai" => dispatch_builtin(args, "ai", ai::run),
        "agent" => dispatch_agent(args),
        "model" => dispatch_builtin(args, "model", model::run),
        "engine" => dispatch_builtin(args, "engine", engine_pkg::run),
        _ => {
            // Check if user forgot "app" prefix — helpful error
            let apps_dir = apps_dir();
            let discovered = apps::discover(&apps_dir);
            if discovered.contains_key(name.as_str()) {
                Err(format!(
                    "'{name}' is an app, not an OS primitive. Use: cos app {name} <command>"
                ))
            } else {
                let builtins: Vec<&str> = builtin_apps().iter().map(|(n, _, _)| *n).collect();
                Err(format!(
                    "unknown command: {name}. OS primitives: {builtins:?}. For apps: cos app"
                ))
            }
        }
    }
}

/// Dispatch to Python apps under the "cos app" namespace.
fn dispatch_app(args: &[String]) -> Result<Option<String>, String> {
    let apps_dir = apps_dir();
    let discovered = apps::discover(&apps_dir);

    // "cos app" with no further args (or with --help/help) → list apps.
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h" | "help") {
        return show_apps(&discovered);
    }

    let app_name = &args[0];

    // Special: `cos app lint [<name>]` — refuses AI-using apps that
    // import provider SDKs directly. Run before the "unknown app"
    // check so `lint` itself doesn't collide with an app name.
    if app_name == "lint" {
        let target = args.get(1).map(String::as_str);
        return lint_apps(&discovered, target);
    }

    // Special: `cos app tool list [<name>]` — list session-exposed
    // tools (the strict-schema, agent-callable surface) declared by
    // each app's manifest. Lives next to `lint` so authors can audit
    // what their app advertises to the kernel agent.
    if app_name == "tool" {
        return tool_cmd(&args[1..], &discovered);
    }

    // Special: `cos app install <source>` — validate a manifest,
    // install the App tree under apps_dir(), and (unless --no-consent)
    // walk the operator through the AI consent prompt. Lives in the
    // `app` namespace because it is an admin operation against the
    // App layer — no AI gate involved.
    if app_name == "install" {
        return install_cmd(&args[1..]);
    }

    // Special: `cos app consent <sub> [<name>] [...]` — review / grant /
    // revoke a user's explicit approval of an App's manifest AI policy.
    // Lives in the `app` namespace because it is an inherently per-app
    // user decision.
    if app_name == "consent" {
        return consent_cmd(&args[1..], &discovered);
    }

    // Check if it's a known app
    if !discovered.contains_key(app_name.as_str()) {
        let names: Vec<&String> = discovered.keys().collect();
        return Err(format!("unknown app: {app_name}. installed: {names:?}"));
    }

    // "cos app <name>" / "cos app <name> --help|-h|help" → show app help.
    if args.len() == 1 || (args.len() == 2 && matches!(args[1].as_str(), "--help" | "-h" | "help"))
    {
        return show_app_help(app_name, &discovered[app_name.as_str()]);
    }

    // cos app <name> --schema → show all command schemas for this app
    if args.len() == 2 && args[1] == "--schema" {
        return show_app_schema(app_name, &discovered[app_name.as_str()]);
    }

    let command = &args[1];
    let cmd_args: Vec<String> = args[2..].to_vec();
    let app = &discovered[app_name.as_str()];

    // If --schema is in args, return app command schema
    if cmd_args.contains(&"--schema".to_string()) {
        return show_app_command_schema(app_name, command, app);
    }

    // Validate command exists
    if !app.manifest.operations.contains_key(command.as_str()) {
        let valid: Vec<&String> = app.manifest.operations.keys().collect();
        return Err(format!(
            "unknown command: cos app {app_name} {command}. available: {valid:?}"
        ));
    }

    run_app_command(app_name, command, &cmd_args, app)
}

/// `cos app lint [<name>]` — refuse apps that smuggle in AI SDKs.
///
/// Apps are required to route every model call through the kernel's
/// `cos ai chat --app <id>` gate (via `claw-os-sdk/python/src/claw_os_sdk/ai.py`). Importing
/// `openai`, `anthropic`, or `google.generativeai` directly would
/// bypass budget, safety, and audit — so the linter looks for those
/// imports in every `*.py` file under each app's directory and reports
/// the offenders.
fn lint_apps(
    discovered: &std::collections::BTreeMap<String, apps::App>,
    target: Option<&str>,
) -> Result<Option<String>, String> {
    let mut results = Vec::new();
    let mut any_violation = false;

    let apps_to_check: Vec<&apps::App> = match target {
        Some(name) => match discovered.get(name) {
            Some(a) => vec![a],
            None => {
                let names: Vec<&String> = discovered.keys().collect();
                return Err(format!("unknown app: {name}. installed: {names:?}"));
            }
        },
        None => discovered.values().collect(),
    };

    for app in apps_to_check {
        let mut violations = scan_app_for_ai_imports(&app.dir);
        violations.extend(scan_session_block(app));
        if !violations.is_empty() {
            any_violation = true;
        }
        results.push(json!({
            "app": app.manifest.id,
            "ok": violations.is_empty(),
            "violations": violations,
        }));
    }

    Ok(Some(
        json!({
            "results": results,
            "ok": !any_violation,
            "hint": if any_violation {
                "Lint failed. Apps must (a) route AI calls through `claw_os_sdk.ai` (not direct provider SDKs) \
                 and (b) ship every file referenced by their `session.entry` so the kernel agent can spawn \
                 the MCP server. Run `cos app tool list <app>` to inspect the declared tool surface."
            } else {
                "All apps route their AI calls through the kernel gate and ship every declared session entry."
            },
        })
        .to_string(),
    ))
}

/// On-disk lint checks for an app's `session` block. The manifest
/// parser already enforces structural validity (duplicate tool
/// names, undeclared scope args, missing English text, etc.) and
/// `apps::discover` would have skipped the app otherwise. What we
/// still need to verify here is that the artefacts referenced by the
/// manifest exist on disk — most importantly the `session.entry`
/// script, since a missing entry breaks the agent at first call
/// rather than at install time.
fn scan_session_block(app: &apps::App) -> Vec<Value> {
    let Some(session) = app.manifest.session.as_ref() else {
        return Vec::new();
    };
    let entry_rel = session
        .entry
        .clone()
        .unwrap_or_else(|| {
            app.manifest
                .runtime
                .default_session_entry()
                .to_string()
        });
    let entry_abs = app.dir.join(&entry_rel);
    let mut hits = Vec::new();
    if !entry_abs.is_file() {
        hits.push(json!({
            "kind": "session.entry-missing",
            "file": entry_abs.display().to_string(),
            "hint": format!(
                "Manifest declares a `session` block with {} tool(s) but the entry script \
                 `{}` is not present on disk. The kernel agent will fail to bring up the MCP \
                 server on the first call.",
                session.tools.len(),
                entry_rel,
            ),
        }));
    }
    hits
}

/// `cos app tool <sub>` — discovery surface for App-defined session
/// tools (the strict-schema, agent-callable surface declared in each
/// manifest's `session` block).
///
/// Currently supports:
/// * `cos app tool list` — every session tool across every installed app.
/// * `cos app tool list <app>` — the tools one app exposes.
///
/// The CLI just prints what the manifest claims; it does *not* spawn
/// the App MCP server (that happens inside the agent on first call).
fn tool_cmd(
    args: &[String],
    discovered: &std::collections::BTreeMap<String, apps::App>,
) -> Result<Option<String>, String> {
    let sub = args.first().map(String::as_str).unwrap_or("list");
    match sub {
        "list" => {
            let target = args.get(1).map(String::as_str);
            let apps_to_show: Vec<&apps::App> = match target {
                Some(name) => match discovered.get(name) {
                    Some(a) => vec![a],
                    None => {
                        let names: Vec<&String> = discovered.keys().collect();
                        return Err(format!("unknown app: {name}. installed: {names:?}"));
                    }
                },
                None => discovered.values().collect(),
            };

            let mut apps_json: Vec<Value> = Vec::new();
            for app in apps_to_show {
                let tools_json: Vec<Value> = app
                    .manifest
                    .session
                    .as_ref()
                    .map(|s| {
                        s.tools
                            .iter()
                            .map(|t| {
                                json!({
                                    "name": t.name,
                                    "summary": t.summary.en_str(),
                                    "args": t.args.iter().map(|a| json!({
                                        "name": a.name,
                                        "kind": format!("{:?}", a.kind).to_lowercase(),
                                        "required": a.required,
                                    })).collect::<Vec<_>>(),
                                    "verbs": t.needs.iter()
                                        .map(|n| n.verb.as_str())
                                        .collect::<Vec<_>>(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                apps_json.push(json!({
                    "app": app.manifest.id,
                    "has_session": app.manifest.session.is_some(),
                    "tools": tools_json,
                }));
            }
            Ok(Some(json!({"apps": apps_json}).to_string()))
        }
        "--help" | "-h" | "help" => Ok(Some(
            "cos app tool list [<app>]  list session-exposed tools".to_string(),
        )),
        other => Err(format!(
            "unknown subcommand: cos app tool {other}. try: cos app tool list [<app>]"
        )),
    }
}

/// Walk an app directory looking for `*.py` files that import one of
/// the forbidden provider SDKs. Returns a list of `{file, line, text}`
/// hits.
fn scan_app_for_ai_imports(app_dir: &Path) -> Vec<Value> {
    const FORBIDDEN: &[&str] = &[
        "openai",
        "anthropic",
        "google.generativeai",
        "vertexai",
        "cohere",
        "mistralai",
        "replicate",
        "boto3.client(\"bedrock",
        "boto3.client('bedrock",
    ];
    let mut hits = Vec::new();
    walk_py(app_dir, &mut |path, contents| {
        for (idx, line) in contents.lines().enumerate() {
            let trimmed = line.trim_start();
            if !(trimmed.starts_with("import ") || trimmed.starts_with("from ")) {
                // Allow grepping for the boto3-bedrock shape too.
                if !FORBIDDEN.iter().any(|f| trimmed.contains(f)) {
                    continue;
                }
            }
            for needle in FORBIDDEN {
                if trimmed.contains(needle)
                    && (trimmed.starts_with("import ")
                        || trimmed.starts_with("from ")
                        || trimmed.contains(".client"))
                {
                    hits.push(json!({
                        "file": path.display().to_string(),
                        "line": idx + 1,
                        "text": line.to_string(),
                        "matched": needle.to_string(),
                    }));
                    break;
                }
            }
        }
    });
    hits
}

fn walk_py(dir: &Path, f: &mut dyn FnMut(&Path, &str)) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            // Skip vendored / build / hidden directories.
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "node_modules" || name == "__pycache__" {
                continue;
            }
            walk_py(&p, f);
        } else if p.extension().and_then(|e| e.to_str()) == Some("py") {
            if let Ok(contents) = std::fs::read_to_string(&p) {
                f(&p, &contents);
            }
        }
    }
}

/// `cos app install <source-dir> [--yes] [--no-consent] [--force]`
///
/// Validates an App manifest, copies the App tree under `apps_dir()`,
/// and (unless `--no-consent`) walks the operator through the AI
/// consent prompt for the App's manifest `ai` block.
///
/// Validation:
///   * `<source>/app.json` must parse via `Manifest::from_json` — same
///     rules every existing App goes through at discover/launch time.
///   * `ai.tools[]` must be a subset of the live kernel catalog
///     (`crate::ai::tools::list_names()`); typoed entries are rejected
///     before anything is copied to disk.
///   * The manifest's `id` is the install destination dir name. If the
///     source's parent dir name differs, that's fine — the manifest is
///     authoritative.
///
/// Disk layout:
///   * Default destination is `apps_dir()/<id>/`. If the source already
///     resolves to that exact path (the in-tree dev workflow where
///     someone runs `cos app install apps/<id>` against the bundled
///     tree), the copy step is skipped and only validation +
///     consent run.
///   * If the destination already exists and `--force` was not passed,
///     the install fails with a helpful message rather than silently
///     overwriting an existing App.
///
/// Consent:
///   * Apps without an `ai` block have nothing to consent to and the
///     install completes after the copy.
///   * Apps with an `ai` block prompt interactively unless `--yes`
///     (auto-grant) or `--no-consent` (defer; operator must run
///     `cos app consent grant <id>` later).
fn install_cmd(args: &[String]) -> Result<Option<String>, String> {
    let source_arg = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .ok_or_else(|| {
            "usage: cos app install <source-dir> [--yes] [--no-consent] [--force]"
                .to_string()
        })?;
    let auto_yes = args.iter().any(|a| a == "--yes");
    let no_consent = args.iter().any(|a| a == "--no-consent");
    let force = args.iter().any(|a| a == "--force");

    let source = PathBuf::from(&source_arg);
    if !source.is_dir() {
        return Err(format!(
            "install source `{}` is not a directory",
            source.display()
        ));
    }
    let manifest_path = source.join("app.json");
    if !manifest_path.is_file() {
        return Err(format!(
            "install source `{}` has no app.json",
            source.display()
        ));
    }
    let body = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("read {}: {e}", manifest_path.display()))?;
    let manifest = apps::AppManifest::from_json(&body)
        .map_err(|e| format!("parse {}: {e}", manifest_path.display()))?;
    let catalog = crate::ai::tools::list_names();
    manifest
        .validate_tools_against_catalog(&catalog)
        .map_err(|e| format!("manifest catalog check: {e}"))?;

    let dest = apps_dir().join(&manifest.id);
    let copied: bool;
    let same_path = source
        .canonicalize()
        .ok()
        .zip(dest.canonicalize().ok())
        .map(|(a, b)| a == b)
        .unwrap_or(false);

    if same_path {
        copied = false;
    } else if dest.exists() {
        if !force {
            return Err(format!(
                "destination `{}` already exists. Re-run with --force to overwrite.",
                dest.display()
            ));
        }
        std::fs::remove_dir_all(&dest)
            .map_err(|e| format!("remove existing {}: {e}", dest.display()))?;
        copy_dir_recursive(&source, &dest)
            .map_err(|e| format!("copy {} -> {}: {e}", source.display(), dest.display()))?;
        copied = true;
    } else {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        copy_dir_recursive(&source, &dest)
            .map_err(|e| format!("copy {} -> {}: {e}", source.display(), dest.display()))?;
        copied = true;
    }

    let mut envelope = json!({
        "app": manifest.id,
        "installed": true,
        "source": source.display().to_string(),
        "dest": dest.display().to_string(),
        "copied": copied,
        "in_place": same_path,
    });

    let needs_consent = manifest.ai.is_some();
    if !needs_consent {
        envelope["consent"] = json!({
            "needed": false,
            "reason": "no_ai_block",
        });
        return Ok(Some(envelope.to_string()));
    }

    if no_consent {
        envelope["consent"] = json!({
            "needed": true,
            "granted": false,
            "deferred": true,
            "hint": format!("Run `cos app consent grant {}` to approve.", manifest.id),
        });
        return Ok(Some(envelope.to_string()));
    }

    use crate::ai::consent;
    let policy = manifest.ai.as_ref().unwrap();
    let review = consent::format_for_review(&manifest.id, policy);
    if !auto_yes {
        use std::io::{BufRead, Write};
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "{review}");
        let _ = write!(stderr, "Approve this AI policy? [y/N] ");
        let _ = stderr.flush();
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|e| format!("read stdin: {e}"))?;
        let answer = line.trim().to_ascii_lowercase();
        if answer != "y" && answer != "yes" {
            envelope["consent"] = json!({
                "needed": true,
                "granted": false,
                "reason": "user_declined",
                "hint": format!("Run `cos app consent grant {}` to approve later.", manifest.id),
            });
            return Ok(Some(envelope.to_string()));
        }
    }

    let record = consent::Consent::approve(policy.clone());
    consent::save(&manifest.id, &record)?;
    envelope["consent"] = json!({
        "needed": true,
        "granted": true,
        "approved_at": record.approved_at,
        "path": consent::consent_path(&manifest.id).display().to_string(),
    });
    Ok(Some(envelope.to_string()))
}

/// Plain recursive directory copy. **Symlinks are rejected** with an
/// error rather than followed.
///
/// `fs::copy` and `Path::is_dir` traverse symlinks, so a malicious
/// install source containing a link such as `passwd -> /etc/passwd`
/// (or `data -> /var/lib/cos/credentials`) used to either escape the
/// source tree or materialise privileged content as part of the
/// installed App. For Apps we want a verbatim copy of a developer tree:
/// rejecting symlinks is both safer and matches what every shipped
/// App actually needs (none use symlinks). Use `symlink_metadata` to
/// inspect entries without traversal, the same pattern checkpoint.rs
/// uses in `copy_dir_recursive`.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&from)?;
        let ft = metadata.file_type();
        if ft.is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "refusing to copy symlink at `{}` during app install: \
                     install sources must not contain symlinks",
                    from.display()
                ),
            ));
        } else if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// `cos app consent <sub> [...]` — review / grant / revoke the user's
/// explicit approval of an App's manifest AI policy. The gate refuses
/// every AI call from an App that lacks a fresh consent record; this
/// CLI is how the user produces, inspects, and revokes those records.
///
/// Subcommands:
///   * `list`                       — every installed AI-using app +
///                                    its consent status.
///   * `show <app>`                 — print the stored consent JSON
///                                    (or `present: false`).
///   * `path <app>`                 — print the on-disk file path.
///   * `grant <app> [--yes]`        — review the manifest's AI block
///                                    and persist the approval.
///                                    Interactive y/N by default;
///                                    `--yes` skips the prompt.
///   * `revoke <app>`               — delete the consent record.
fn consent_cmd(
    args: &[String],
    discovered: &std::collections::BTreeMap<String, apps::App>,
) -> Result<Option<String>, String> {
    use crate::ai::consent;

    let sub = args.first().map(String::as_str).unwrap_or("");
    match sub {
        "" | "--help" | "-h" | "help" => Ok(Some(
            json!({
                "app": "consent",
                "description": "Approve, inspect, or revoke an App's AI policy.",
                "subcommands": {
                    "list":    "cos app consent list",
                    "show":    "cos app consent show <app>",
                    "path":    "cos app consent path <app>",
                    "grant":   "cos app consent grant <app> [--yes]",
                    "revoke":  "cos app consent revoke <app>",
                },
                "hint": "An App with an `ai` block in its manifest cannot make AI calls until you have granted consent.",
            })
            .to_string(),
        )),

        "list" => {
            let mut rows = Vec::new();
            for (id, app) in discovered {
                let policy = match &app.manifest.ai {
                    Some(p) => p,
                    None => continue,
                };
                let stored = consent::load(id).map_err(|e| e)?;
                let (status, changed): (&str, Vec<String>) = match &stored {
                    None => ("missing", Vec::new()),
                    Some(c) => match consent::freshness(policy, c) {
                        consent::Freshness::Fresh => ("fresh", Vec::new()),
                        consent::Freshness::Stale { changed } => ("stale", changed),
                    },
                };
                let mut row = json!({
                    "app": id,
                    "status": status,
                    "path": consent::consent_path(id).display().to_string(),
                });
                if let Some(c) = &stored {
                    row["approved_at"] = json!(c.approved_at);
                }
                if !changed.is_empty() {
                    row["changed"] = json!(changed);
                }
                rows.push(row);
            }
            Ok(Some(
                json!({
                    "ai_apps": rows.len(),
                    "consents": rows,
                    "hint": "Run `cos app consent grant <app>` for any 'missing' or 'stale' entry.",
                })
                .to_string(),
            ))
        }

        "show" => {
            let app = args
                .get(1)
                .ok_or_else(|| "usage: cos app consent show <app>".to_string())?;
            let stored = consent::load(app).map_err(|e| e)?;
            Ok(Some(
                json!({
                    "app": app,
                    "path": consent::consent_path(app).display().to_string(),
                    "present": stored.is_some(),
                    "consent": stored,
                })
                .to_string(),
            ))
        }

        "path" => {
            let app = args
                .get(1)
                .ok_or_else(|| "usage: cos app consent path <app>".to_string())?;
            Ok(Some(
                json!({
                    "app": app,
                    "path": consent::consent_path(app).display().to_string(),
                })
                .to_string(),
            ))
        }

        "grant" => {
            let app_id = args
                .get(1)
                .ok_or_else(|| "usage: cos app consent grant <app> [--yes]".to_string())?;
            let auto = args.iter().skip(2).any(|a| a == "--yes");

            let installed = discovered
                .get(app_id)
                .ok_or_else(|| format!("unknown app: {app_id}"))?;
            let policy = installed.manifest.ai.as_ref().ok_or_else(|| {
                format!("app `{app_id}` has no `ai` block in its manifest — nothing to consent to")
            })?;

            let review = consent::format_for_review(app_id, policy);
            if !auto {
                use std::io::{BufRead, Write};
                let mut stderr = std::io::stderr().lock();
                let _ = writeln!(stderr, "{review}");
                let _ = write!(stderr, "Approve this AI policy? [y/N] ");
                let _ = stderr.flush();
                let mut line = String::new();
                std::io::stdin()
                    .lock()
                    .read_line(&mut line)
                    .map_err(|e| format!("read stdin: {e}"))?;
                let answer = line.trim().to_ascii_lowercase();
                if answer != "y" && answer != "yes" {
                    return Ok(Some(
                        json!({
                            "app": app_id,
                            "granted": false,
                            "reason": "user_declined",
                            "path": consent::consent_path(app_id).display().to_string(),
                        })
                        .to_string(),
                    ));
                }
            }

            let record = consent::Consent::approve(policy.clone());
            consent::save(app_id, &record)?;
            Ok(Some(
                json!({
                    "app": app_id,
                    "granted": true,
                    "approved_at": record.approved_at,
                    "path": consent::consent_path(app_id).display().to_string(),
                    "policy": record.policy,
                })
                .to_string(),
            ))
        }

        "revoke" => {
            let app = args
                .get(1)
                .ok_or_else(|| "usage: cos app consent revoke <app>".to_string())?;
            let removed = consent::delete(app)?;
            Ok(Some(
                json!({
                    "app": app,
                    "revoked": removed,
                    "path": consent::consent_path(app).display().to_string(),
                })
                .to_string(),
            ))
        }

        other => Err(format!(
            "unknown consent subcommand: {other}. try: list | show | path | grant | revoke"
        )),
    }
}

fn show_overview() -> Result<Option<String>, String> {
    let mut primitives = Vec::new();
    for (name, desc, cmds) in builtin_apps() {
        let cmd_map: serde_json::Map<String, Value> = cmds
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect();
        primitives.push(json!({
            "name": name,
            "description": desc,
            "commands": cmd_map,
        }));
    }

    // Count available apps without listing them
    let apps_dir = apps_dir();
    let discovered = apps::discover(&apps_dir);
    let app_count = discovered.len();
    let total_primitives = primitives.len();

    let output = json!({
        "name": "cos",
        "version": VERSION,
        "description": "Claw OS — agent-native operating system. All commands return structured JSON.",
        "primitives": primitives,
        "total_primitives": total_primitives,
        "apps_available": app_count,
        "hint": "Run: cos <primitive> <command> for OS operations. cos help <primitive> for one. cos app to see available apps.",
    });
    Ok(Some(output.to_string()))
}

/// `cos help <topic>` — focused help for one primitive or app. Falls
/// back to the global overview when the topic is unknown so the user
/// always sees something useful (and the available names).
fn show_help_for(topic: &str) -> Result<Option<String>, String> {
    // Built-in primitives use the same shape as `cos <primitive>`
    // (no args).
    if let Some((name, desc, cmds)) = builtin_apps().into_iter().find(|(n, _, _)| *n == topic) {
        let cmd_map: serde_json::Map<String, Value> = cmds
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect();
        return Ok(Some(
            json!({
                "app": name,
                "description": desc,
                "commands": cmd_map,
                "hint": format!("Run: cos {name} <command> [args]"),
            })
            .to_string(),
        ));
    }

    // Apps: render the same help as `cos app <name>`.
    let discovered = apps::discover(&apps_dir());
    if let Some(app) = discovered.get(topic) {
        return show_app_help(topic, app);
    }
    // `cos help app` → list all apps.
    if topic == "app" {
        return show_apps(&discovered);
    }

    // Unknown topic: degrade to the overview but include a note so the
    // caller knows their topic wasn't recognised.
    let mut overview: Value = match show_overview()? {
        Some(s) => serde_json::from_str(&s).unwrap_or_else(|_| json!({})),
        None => json!({}),
    };
    if let Some(obj) = overview.as_object_mut() {
        obj.insert(
            "note".into(),
            json!(format!("unknown help topic: {topic}")),
        );
    }
    Ok(Some(overview.to_string()))
}

fn show_apps(
    discovered: &std::collections::BTreeMap<String, apps::App>,
) -> Result<Option<String>, String> {
    let mut app_list = Vec::new();
    for (name, app) in discovered {
        let cmds: serde_json::Map<String, Value> = app
            .manifest
            .operations
            .iter()
            .map(|(k, op)| (k.clone(), json!(op.label.current())))
            .collect();
        app_list.push(json!({
            "name": name,
            "label": app.manifest.name.current(),
            "description": app.manifest.summary.current(),
            "commands": cmds,
        }));
    }

    let output = json!({
        "apps": app_list,
        "total": app_list.len(),
        "hint": "Run: cos app <name> for app details, cos app <name> <command> [args] to execute. Install an App with: cos app install <source-dir>",
    });
    Ok(Some(output.to_string()))
}

fn show_app_help(name: &str, app: &apps::App) -> Result<Option<String>, String> {
    let cmds: serde_json::Map<String, Value> = app
        .manifest
        .operations
        .iter()
        .map(|(k, op)| (k.clone(), json!(op.label.current())))
        .collect();
    let output = json!({
        "app": name,
        "label": app.manifest.name.current(),
        "version": app.manifest.version,
        "description": app.manifest.summary.current(),
        "commands": cmds,
        "hint": format!("Run: cos app {name} <command> [args]"),
    });
    Ok(Some(output.to_string()))
}

fn run_app_command(
    app_name: &str,
    command: &str,
    args: &[String],
    app: &apps::App,
) -> Result<Option<String>, String> {
    let start = Instant::now();
    let audit = audit_path();
    let data = data_dir();
    let apps = apps_dir().to_string_lossy().to_string();

    // Capability gate: callers (interactive CLI or agent) must hold
    // `agent.invoke` on the app's name to dispatch any command.
    // Schema introspection is allowed unconditionally so tooling can
    // describe apps it cannot run. Strict is the default mode — the
    // user-terminal CLI gets its caps from the session it was started
    // in; ad-hoc development can opt into `COS_PERMS_MODE=permissive`.
    if command != "__schema__" {
        if let Err(denial) = caps::require(
            caps::Verb::AGENT_INVOKE,
            caps::Scope::name(app_name),
        ) {
            return Err(denial.summary());
        }
    }

    let result = bridge::run_python_app(&app.dir, command, args, &data, &apps);

    match result {
        Ok(output) => {
            let mut status = "ok";
            let err_string;
            let mut error_msg: Option<&str> = None;

            // Check if the output contains an error key
            if let Some(ref s) = output {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
                    if let Some(e) = v["error"].as_str() {
                        status = "error";
                        err_string = e.to_string();
                        error_msg = Some(&err_string);
                    }
                }
            }

            audit::log_entry(&audit, app_name, command, args, start, status, error_msg);
            Ok(output)
        }
        Err(e) => {
            audit::log_entry(&audit, app_name, command, args, start, "error", Some(&e));
            // Enrich error with recovery hints for agents. The envelope
            // is returned as `Err` (not `Ok`) so the exit code stays
            // non-zero — main.rs already re-parses Err strings that
            // happen to be JSON objects and surfaces their fields
            // verbatim, so downstream JSON consumers still get the
            // recovery payload.
            if let Some(recovery) = recovery_hint(&e) {
                let mut err_output = json!({
                    "error": e,
                    "recovery": recovery,
                });
                if let Some(code) = error_code_from_hint(&e) {
                    err_output["code"] = json!(code);
                }
                Err(err_output.to_string())
            } else {
                Err(e)
            }
        }
    }
}

fn builtin_apps() -> Vec<(
    &'static str,
    &'static str,
    Vec<(&'static str, &'static str)>,
)> {
    vec![
        ("sys", "System information — hardware, OS, environment, resources, structured /proc", vec![
            ("info", "Get OS, architecture, hostname, and version info"),
            ("env", "List environment variables, optionally filter by pattern"),
            ("resources", "Show disk, memory, and CPU usage"),
            ("uptime", "Show system uptime"),
            ("proc", "List all processes with PID, name, state, CPU, memory (structured /proc/*/stat)"),
            ("mounts", "List all mount points with filesystem type and options (structured /proc/mounts)"),
            ("net", "Show network interfaces and TCP connections (structured /proc/net/*)"),
            ("cgroup", "Show cgroup v2 limits and usage — memory, CPU, PIDs (/sys/fs/cgroup/)"),
        ]),
        ("service", "Generic service manager — lifecycle hooks, graceful shutdown, dependency ordering", vec![
            ("start", "Start a service (pre_start hook → credential injection → spawn → health check → post_start)"),
            ("stop", "Graceful stop: checkpoint → pre_stop → drain → SIGTERM → wait → SIGKILL → post_stop"),
            ("stop-all", "Stop all services in reverse dependency order with graceful shutdown"),
            ("restart", "Restart a service (graceful stop then start)"),
            ("status", "Check service running/healthy state with log tail"),
            ("health", "Run health check, optionally auto-restart (--no-restart to skip)"),
            ("list", "List all discovered services with status"),
            ("logs", "View service log output (--tail N)"),
            ("register", "Register a new service (--name, --command, --credentials KEY1,KEY2, --pre-stop, --post-stop, --drain-timeout, --stop-timeout, --checkpoint-cmd)"),
        ]),
        ("checkpoint", "OverlayFS checkpoint system — snapshot, diff, rollback, quota, namespaces", vec![
            ("create", "Freeze current changes into a named checkpoint and start fresh"),
            ("diff", "Show created, modified, and deleted files in the current upper layer"),
            ("rollback", "Restore a checkpoint or reset to base (wipe current changes)"),
            ("list", "List all saved checkpoints with metadata"),
            ("status", "Show overlay mount state, pending changes, and disk usage"),
            ("quota-set", "Set filesystem quota for the upper layer (e.g. 2G, 512M)"),
            ("quota-status", "Show current quota usage, limit, and whether exceeded"),
            ("namespaces", "Manage isolated overlay namespaces (--create, --destroy, --status <name>)"),
        ]),
        ("credential", "Encrypted credential store — secure secret storage with tier-based access, namespaces, TTL, auto-refresh, and bundles", vec![
            ("store", "Store a credential (--tier N, --namespace NS, --ttl SECS, --refresh-cmd CMD)"),
            ("load", "Load a credential value (tier check + expiry enforced, auto-refresh if configured)"),
            ("revoke", "Delete a stored credential"),
            ("list", "List credentials, optionally filtered by --namespace"),
            ("bundle", "Create a credential bundle (--keys key1,key2,key3)"),
            ("load-bundle", "Load all credentials in a bundle as a JSON object"),
            ("oauth-refresh", "Refresh OAuth token (google or microsoft) using stored refresh token"),
        ]),
        ("cron", "Agent-native job scheduler — cron with execution context, result capture, and overlap protection", vec![
            ("add", "Register a cron job (--schedule, --command, --tier, --scope, --credentials, --overlap, --timeout)"),
            ("remove", "Remove a cron job by ID"),
            ("list", "List all cron jobs with status and next run time"),
            ("status", "Detailed status of a specific job"),
            ("enable", "Enable a disabled job"),
            ("disable", "Disable a job without removing it"),
            ("logs", "View execution history for a job (--limit N)"),
            ("run", "Manually trigger a job immediately"),
            ("tick", "Process all due jobs (called by scheduler every minute)"),
        ]),
        ("ai", "App-facing AI gate — single-shot LLM / embedding / image / audio / video calls scoped to one installed App. Distinct from `cos agent`: this is the App-developer-facing primitive, not the kernel Agent product.", vec![
            ("chat", "One-shot App-gated AI call: cos ai chat --app <id> [--prompt <text>] [--prompt-file <p>] [--origin trusted|user-input|external-content] [--max-units N] [--system <text>] [--embed] [--image-input <p>|--image-output <p>] [--audio-input <p>|--audio-output <p>] [--video-input <p>|--video-output <p>]. Modality (chat/embed/image/audio/vision/video) is auto-derived from the request shape; verbs are never passed at the CLI. Apps do not pick the model — the OS owner configures it in /etc/cos/agent.toml."),
            ("tool", "Invoke one App-facing Tool by name: cos ai tool <name> --app <id> [--args <json>|--args-file <p>]. The kernel checks the App's caps grants, runs the Tool, and writes one audit row per call. List tools with `cos ai tools`."),
            ("tools", "Print the catalog of App-facing Tools (name, summary, verb, stability, JSON-Schema for args and return). Used by App authors and LLM function-call spec generators."),
        ]),
        ("agent", "OS-native agent subsystem — clawd-backed runtime, memory, skills, LLM providers, tools, and tasks", vec![
            ("setup", "Per-modality config wizard: cos agent setup <llm|tts|stt|imagegen|embed|all> [--status|--reset|--verify-only|--no-verify]. Bare `cos agent setup` opens an interactive modality picker."),
            ("ask", "Single-shot prompt with full tool/memory loop: cos agent ask \"<prompt>\" [--stream] — without --stream waits for the full response; with --stream tokens are written live to stderr while the JSON envelope still lands on stdout."),
            ("chat", "Interactive REPL for the system agent: cos agent chat [--session <id>] [--no-stream] [--no-memory] [--show-tools] [--max-turns N] (slash commands: /quit /help /session /clear /history [N] /tools). For one-shot App-gated calls use `cos ai chat --app <id>` — `cos agent chat` is the kernel Agent's own surface and is not an App entry point."),
            ("budget", "Inspect or reset an app's monthly AI budget: cos agent budget show|reset|history <app>. The system agent reports under the pseudo-app id `system.agent`."),
            ("status", "Short live verdict: provider/model/key source, ready/not-ready, most-recent session. Use `cos agent doctor` for the full provider matrix, tool list, skills, usage."),
            ("sessions", "Inspect / manage conversation sessions in the memory DB: cos agent sessions [list [N] | title <id> | set-title <id> \"<title>\" | count [<id>] | clear <id> --yes]"),
            ("recall", "FTS5 search across recorded conversations: cos agent recall \"<query>\" [limit]"),
            ("service", "Daemon-backed task queue: cos agent service {submit \"<prompt>\" | list | status <id> | result <id> | cancel <id>}. Requires clawd."),
            ("notes", "Manage agent markdown notes (MEMORY.md / USER.md / custom): cos agent notes [list|read <n>|write <n> <content>|append <n> <line>|delete <n>]"),
            ("skills", "Inspect or install skill bundles: cos agent skills [list|info <id>|install <archive.zip>|hub <list|show|install> <owner/repo>|...]"),
            ("todo", "Manage per-session agent todo lists: cos agent todo [list <session_id>|add <session_id> <id> <title>|set-status ...|remove ...|clear ...]"),
            ("mcp", "MCP (Model Context Protocol) bridge — server exposes the cos agent tool catalogue; client probes/invokes a remote MCP subprocess"),
            ("doctor", "Aggregate diagnostic — provider config matrix, engines, memory, skills, hooks, audit/run-log + last 7d usage & insights. Add --probe-network for a live provider ping."),
            ("ls", "List active / paused / failed agent tasks (durable sessions on disk). Columns: id, purpose, status, current lease holder."),
            ("show", "Show one task in detail: cos agent show <task-id> — purpose, status, lease, turn count, mutation breakdown by kind, stop-requested flag."),
            ("stop", "Politely stop a running task: cos agent stop <task-id> — drops a stop sentinel for the live runtime to notice; if no runtime is attached, flips status to paused immediately."),
            ("undo", "Replay the inverse mutation log to roll a task's filesystem changes back: cos agent undo <task-id> [--dry-run]."),
            ("resume", "Mark a paused task as ready for re-attachment: cos agent resume <task-id>. Does not itself spawn a runtime — `cos agent chat --session <id>` (or another runtime) takes it from there."),
            ("dev", "Power-user / internal namespace — exposes building blocks (token estimator, redactor, scrubbers, classifier, diagnostics dumps). Run `cos agent dev` for the list. Not a stable surface."),
        ]),
        ("model", "Local model registry + inference daemon (ort for STT/TTS/embed/vision/imagegen, llama.cpp for LLM)", vec![
            ("list", "List registered models from /var/lib/cos/models/"),
            ("import", "Register a local ONNX/GGUF file: cos model import <path> --as <name> [--version <v>] [--task llm|stt|tts|embed|vision|imagegen] [--engine ort|llama] [--format onnx|gguf] [--device <id>] [--move] [--force]"),
            ("rm", "Remove a registered model: cos model rm <name>@<version>"),
            ("check", "Check engine compatibility for a model: cos model check <name>@<version>"),
            ("load", "Load a registered model into the runtime daemon"),
            ("unload", "Unload a model from the runtime"),
            ("infer", "Run inference (routed via IPC to model-runtime daemon)"),
            ("status", "Runtime status — loaded models, RAM, devices, linked engines"),
            ("bench", "Benchmark a model"),
        ]),
        ("engine", "Native inference engine package manager — install / activate / rollback llama.cpp, ort, ort-genai versions side-by-side", vec![
            ("list", "List installed engines and their active versions"),
            ("info", "Detailed info for one engine: cos engine info <name>"),
            ("install", "Install from a local archive: cos engine install <name>@<version> --from <path.zip> [--no-activate]"),
            ("activate", "Switch active version: cos engine activate <name>@<version>"),
            ("rollback", "Swap active <-> previous: cos engine rollback <name>"),
            ("update", "Fetch + install from GitHub Releases: cos engine update <name> [--check] [--to <tag>] [--force] [--accelerator cpu|cuda|vulkan|...] [--no-activate]"),
            ("pin", "Lock active version against auto-update: cos engine pin <name>[@<version>]"),
            ("unpin", "Remove pin: cos engine unpin <name>"),
            ("gc", "Delete old installed versions, keep last N (default 3): cos engine gc <name> [--keep N]"),
            ("uninstall", "Remove a specific installed version: cos engine uninstall <name>@<version>"),
        ]),
    ]
}

/// Suggest recovery actions for common errors.
/// Agent-native: humans debug by intuition, agents need explicit guidance.
fn recovery_hint(error: &str) -> Option<serde_json::Value> {
    let err_lower = error.to_lowercase();

    if err_lower.contains("permission denied") || err_lower.contains("eperm") {
        return Some(json!({
            "hint": "Permission denied. Check file permissions.",
            "try": ["cos app exec run 'ls -la <path>'", "cos app exec run 'chmod +rw <path>'"],
        }));
    }
    if err_lower.contains("no such file")
        || err_lower.contains("enoent")
        || err_lower.contains("not found")
    {
        return Some(json!({
            "hint": "File or command not found. Verify the path exists.",
            "try": ["cos app fs ls <parent-directory>", "cos app exec which <command>"],
        }));
    }
    if err_lower.contains("no space left") || err_lower.contains("enospc") {
        return Some(json!({
            "hint": "Disk full. Free space before retrying.",
            "try": ["cos sys resources", "cos app exec run 'du -sh $HOME/* | sort -rh | head'"],
        }));
    }
    if err_lower.contains("connection refused") || err_lower.contains("econnrefused") {
        return Some(json!({
            "hint": "Connection refused. The target service may not be running.",
            "try": ["cos service list", "cos service start <service-name>"],
        }));
    }
    if err_lower.contains("timed out") || err_lower.contains("timeout") {
        return Some(json!({
            "hint": "Operation timed out. Consider increasing timeout or checking if the service is responsive.",
            "try": ["cos proc list", "cos sys resources"],
        }));
    }
    if err_lower.contains("already running")
        || err_lower.contains("address already in use")
        || err_lower.contains("eaddrinuse")
    {
        return Some(json!({
            "hint": "Port/resource already in use. Another process may be occupying it.",
            "try": ["cos proc list", "cos app exec run 'lsof -i :<port>'"],
        }));
    }
    if err_lower.contains("out of memory")
        || err_lower.contains("enomem")
        || err_lower.contains("oom")
    {
        return Some(json!({
            "hint": "Out of memory. Reduce workload or increase memory limits.",
            "try": ["cos sys resources", "cos proc list"],
        }));
    }

    None
}

/// Map an error message to a standard error code by inspecting well-known
/// substrings.  Returns `None` when the message doesn't match any pattern.
fn error_code_from_hint(error: &str) -> Option<&'static str> {
    let err_lower = error.to_lowercase();
    if err_lower.contains("permission denied") || err_lower.contains("eperm") {
        Some(crate::errors::IO_PERMISSION_DENIED)
    } else if err_lower.contains("no such file")
        || err_lower.contains("not found")
        || err_lower.contains("enoent")
    {
        Some(crate::errors::IO_FILE_NOT_FOUND)
    } else if err_lower.contains("no space left") || err_lower.contains("enospc") {
        Some(crate::errors::IO_DISK_FULL)
    } else if err_lower.contains("connection refused") || err_lower.contains("econnrefused") {
        Some(crate::errors::IO_CONNECTION_REFUSED)
    } else if err_lower.contains("timed out") || err_lower.contains("timeout") {
        Some(crate::errors::LIMIT_TIMEOUT)
    } else if err_lower.contains("already in use") || err_lower.contains("eaddrinuse") {
        Some(crate::errors::RESOURCE_BUSY)
    } else if err_lower.contains("out of memory")
        || err_lower.contains("enomem")
        || err_lower.contains("oom")
    {
        Some(crate::errors::LIMIT_OOM)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// --schema support: structured parameter introspection for every command
// ---------------------------------------------------------------------------

struct CommandSchema {
    command: &'static str,
    description: &'static str,
    params: Vec<ParamSchema>,
    example: &'static str,
}

struct ParamSchema {
    name: &'static str,
    param_type: &'static str,
    required: bool,
    description: &'static str,
    kind: &'static str, // "positional" or "flag"
}

struct Param;
impl Param {
    fn positional(
        name: &'static str,
        param_type: &'static str,
        required: bool,
        description: &'static str,
    ) -> ParamSchema {
        ParamSchema {
            name,
            param_type,
            required,
            description,
            kind: "positional",
        }
    }
    fn flag(
        name: &'static str,
        param_type: &'static str,
        required: bool,
        description: &'static str,
    ) -> ParamSchema {
        ParamSchema {
            name,
            param_type,
            required,
            description,
            kind: "flag",
        }
    }
}

fn command_schemas() -> Vec<(&'static str, &'static str, Vec<CommandSchema>)> {
    vec![
        (
            "checkpoint",
            "OverlayFS snapshot system",
            vec![
                CommandSchema {
                    command: "create",
                    description: "Freeze current changes into a named checkpoint",
                    params: vec![Param::positional(
                        "description",
                        "string",
                        true,
                        "Checkpoint description",
                    )],
                    example: "cos checkpoint create \"before refactoring\"",
                },
                CommandSchema {
                    command: "diff",
                    description: "Show created, modified, and deleted files",
                    params: vec![],
                    example: "cos checkpoint diff",
                },
                CommandSchema {
                    command: "rollback",
                    description: "Restore a checkpoint or reset to base",
                    params: vec![Param::positional(
                        "checkpoint_id",
                        "string",
                        false,
                        "Checkpoint ID to restore (omit for base)",
                    )],
                    example: "cos checkpoint rollback 002",
                },
                CommandSchema {
                    command: "list",
                    description: "List all saved checkpoints",
                    params: vec![],
                    example: "cos checkpoint list",
                },
                CommandSchema {
                    command: "status",
                    description: "Show overlay mount state and disk usage",
                    params: vec![],
                    example: "cos checkpoint status",
                },
                CommandSchema {
                    command: "quota-set",
                    description: "Set filesystem quota for the upper layer",
                    params: vec![Param::positional(
                        "size",
                        "string",
                        true,
                        "Size limit (e.g., 2G, 512M)",
                    )],
                    example: "cos checkpoint quota-set 2G",
                },
                CommandSchema {
                    command: "quota-status",
                    description: "Show current quota usage",
                    params: vec![],
                    example: "cos checkpoint quota-status",
                },
            ],
        ),
        (
            "credential",
            "Encrypted credential store",
            vec![
                CommandSchema {
                    command: "store",
                    description: "Store an encrypted credential",
                    params: vec![
                        Param::positional("name", "string", true, "Credential name"),
                        Param::positional("value", "string", true, "Secret value"),
                        Param::flag(
                            "--tier",
                            "integer",
                            false,
                            "Min tier to read (0-3, default 0)",
                        ),
                        Param::flag(
                            "--namespace",
                            "string",
                            false,
                            "Namespace (default: default)",
                        ),
                        Param::flag("--ttl", "integer", false, "Time-to-live in seconds"),
                        Param::flag(
                            "--refresh-cmd",
                            "string",
                            false,
                            "Command to execute on expiry to refresh the value",
                        ),
                    ],
                    example: "cos credential store OPENAI_KEY sk-abc123 --tier 0 --ttl 3600",
                },
                CommandSchema {
                    command: "load",
                    description: "Load a credential (tier + expiry enforced)",
                    params: vec![
                        Param::positional("name", "string", true, "Credential name"),
                        Param::flag("--namespace", "string", false, "Namespace"),
                    ],
                    example: "cos credential load OPENAI_KEY",
                },
                CommandSchema {
                    command: "list",
                    description: "List credentials (names only, never values)",
                    params: vec![Param::flag(
                        "--namespace",
                        "string",
                        false,
                        "Filter by namespace",
                    )],
                    example: "cos credential list",
                },
                CommandSchema {
                    command: "revoke",
                    description: "Delete a credential",
                    params: vec![
                        Param::positional("name", "string", true, "Credential name"),
                        Param::flag("--namespace", "string", false, "Namespace"),
                    ],
                    example: "cos credential revoke OPENAI_KEY",
                },
                CommandSchema {
                    command: "bundle",
                    description: "Create a credential bundle (group of keys)",
                    params: vec![
                        Param::positional("bundle_name", "string", true, "Bundle name"),
                        Param::flag(
                            "--keys",
                            "string",
                            true,
                            "Comma-separated credential names",
                        ),
                        Param::flag("--namespace", "string", false, "Namespace"),
                    ],
                    example: "cos credential bundle openai-config --keys OPENAI_KEY,OPENAI_ORG",
                },
                CommandSchema {
                    command: "load-bundle",
                    description: "Load all credentials in a bundle",
                    params: vec![
                        Param::positional("bundle_name", "string", true, "Bundle name"),
                        Param::flag("--namespace", "string", false, "Namespace"),
                    ],
                    example: "cos credential load-bundle openai-config",
                },
                CommandSchema {
                    command: "oauth-refresh",
                    description: "Refresh OAuth token using stored refresh token",
                    params: vec![
                        Param::positional(
                            "provider",
                            "string",
                            true,
                            "OAuth provider (google or microsoft)",
                        ),
                        Param::flag("--namespace", "string", false, "Namespace"),
                    ],
                    example: "cos credential oauth-refresh google",
                },
            ],
        ),
        (
            "cron",
            "Agent-native job scheduler",
            vec![
                CommandSchema {
                    command: "add",
                    description: "Register a cron job",
                    params: vec![
                        Param::positional("id", "string", true, "Job ID"),
                        Param::flag("--schedule", "string", true, "Cron expression (5 fields)"),
                        Param::flag("--command", "string", true, "Command to run"),
                        Param::flag("--tier", "integer", false, "Execution tier"),
                        Param::flag("--scope", "string", false, "Path restriction"),
                        Param::flag(
                            "--credentials",
                            "string",
                            false,
                            "Comma-separated credential names",
                        ),
                        Param::flag(
                            "--overlap",
                            "enum:skip|queue|kill|allow",
                            false,
                            "Overlap policy (default: skip)",
                        ),
                        Param::flag("--timeout", "integer", false, "Kill after N seconds"),
                    ],
                    example: "cos cron add health-check --schedule \"*/5 * * * *\" --command \"cos service health my-api\" --overlap skip",
                },
                CommandSchema {
                    command: "list",
                    description: "List all cron jobs",
                    params: vec![],
                    example: "cos cron list",
                },
                CommandSchema {
                    command: "run",
                    description: "Manually trigger a job",
                    params: vec![Param::positional("id", "string", true, "Job ID")],
                    example: "cos cron run health-check",
                },
                CommandSchema {
                    command: "tick",
                    description: "Process all due jobs (called by scheduler)",
                    params: vec![],
                    example: "cos cron tick",
                },
            ],
        ),
        (
            "service",
            "Service lifecycle manager",
            vec![
                CommandSchema {
                    command: "start",
                    description: "Start a service (pre_start → credential injection → spawn → health → post_start)",
                    params: vec![Param::positional("name", "string", true, "Service name")],
                    example: "cos service start my-api",
                },
                CommandSchema {
                    command: "stop",
                    description: "Graceful stop (checkpoint → pre_stop → drain → SIGTERM → wait → SIGKILL → post_stop)",
                    params: vec![Param::positional("name", "string", true, "Service name")],
                    example: "cos service stop my-api",
                },
                CommandSchema {
                    command: "stop-all",
                    description: "Stop all services in reverse dependency order",
                    params: vec![],
                    example: "cos service stop-all",
                },
                CommandSchema {
                    command: "register",
                    description: "Register a new service",
                    params: vec![
                        Param::flag("--name", "string", true, "Service name"),
                        Param::flag("--command", "string", true, "Start command"),
                        Param::flag("--workdir", "string", false, "Working directory"),
                        Param::flag("--health-url", "string", false, "Health check URL"),
                        Param::flag(
                            "--credentials",
                            "string",
                            false,
                            "Credential names (comma-separated)",
                        ),
                        Param::flag("--pre-start", "string", false, "Pre-start hook command"),
                        Param::flag("--pre-stop", "string", false, "Pre-stop hook command"),
                        Param::flag("--post-stop", "string", false, "Post-stop hook command"),
                        Param::flag("--drain-timeout", "integer", false, "Drain wait seconds"),
                        Param::flag(
                            "--stop-timeout",
                            "integer",
                            false,
                            "SIGTERM→SIGKILL seconds",
                        ),
                        Param::flag(
                            "--checkpoint-cmd",
                            "string",
                            false,
                            "State checkpoint command",
                        ),
                    ],
                    example: "cos service register --name my-api --command \"python app.py\" --health-url http://localhost:8000/health --credentials OPENAI_KEY,DB_URL",
                },
            ],
        ),
        (
            "sys",
            "System information",
            vec![
                CommandSchema {
                    command: "info",
                    description: "OS, architecture, hostname, version",
                    params: vec![],
                    example: "cos sys info",
                },
                CommandSchema {
                    command: "resources",
                    description: "Disk, memory, CPU usage",
                    params: vec![],
                    example: "cos sys resources",
                },
                CommandSchema {
                    command: "env",
                    description: "Environment variables",
                    params: vec![Param::positional(
                        "pattern",
                        "string",
                        false,
                        "Filter pattern",
                    )],
                    example: "cos sys env COS",
                },
                CommandSchema {
                    command: "proc",
                    description: "All processes with resource usage",
                    params: vec![],
                    example: "cos sys proc",
                },
            ],
        ),
    ]
}

fn show_command_schema(app_name: &str, command: &str) -> Result<Option<String>, String> {
    let schemas = command_schemas();
    let app = schemas.iter().find(|(n, _, _)| *n == app_name);
    let app = app.ok_or_else(|| format!("no schema for: {app_name}"))?;

    let cmd = app.2.iter().find(|c| c.command == command);
    let cmd = cmd.ok_or_else(|| format!("no schema for: {app_name} {command}"))?;

    let params: Vec<Value> = cmd
        .params
        .iter()
        .map(|p| {
            json!({
                "name": p.name,
                "type": p.param_type,
                "required": p.required,
                "description": p.description,
                "kind": p.kind,
            })
        })
        .collect();

    let output = json!({
        "command": format!("cos {app_name} {}", cmd.command),
        "description": cmd.description,
        "parameters": params,
        "example": cmd.example,
    });
    Ok(Some(output.to_string()))
}

fn show_builtin_schema(app_name: &str) -> Result<Option<String>, String> {
    let schemas = command_schemas();
    let app = schemas.iter().find(|(n, _, _)| *n == app_name);
    let app = app.ok_or_else(|| format!("no schema for: {app_name}"))?;

    let commands: Vec<Value> = app
        .2
        .iter()
        .map(|cmd| {
            let params: Vec<Value> = cmd
                .params
                .iter()
                .map(|p| {
                    json!({
                        "name": p.name,
                        "type": p.param_type,
                        "required": p.required,
                        "description": p.description,
                        "kind": p.kind,
                    })
                })
                .collect();
            json!({
                "command": cmd.command,
                "description": cmd.description,
                "parameters": params,
                "example": cmd.example,
            })
        })
        .collect();

    let output = json!({
        "app": app_name,
        "description": app.1,
        "commands": commands,
    });
    Ok(Some(output.to_string()))
}

fn show_app_command_schema(
    app_name: &str,
    command: &str,
    app: &apps::App,
) -> Result<Option<String>, String> {
    // Call the Python app with __schema__ to get live schema
    let data_dir = data_dir();
    let apps = apps_dir().to_string_lossy().to_string();

    match bridge::run_python_app(&app.dir, "__schema__", &[], &data_dir, &apps) {
        Ok(Some(output)) => {
            if let Ok(schema) = serde_json::from_str::<Value>(&output) {
                if let Some(cmd_schema) = schema.get(command) {
                    let desc = app
                        .manifest
                        .operations
                        .get(command)
                        .map(|op| op.summary.current().to_string())
                        .unwrap_or_else(|| "No description".to_string());
                    let mut result = json!({
                        "command": format!("cos app {app_name} {command}"),
                        "description": desc,
                    });
                    if let Some(params) = cmd_schema.get("parameters") {
                        result["parameters"] = params.clone();
                    }
                    if let Some(example) = cmd_schema.get("example") {
                        result["example"] = example.clone();
                    }
                    return Ok(Some(result.to_string()));
                }
            }
            // Schema returned but command not found in it
            let desc = app
                .manifest
                .operations
                .get(command)
                .map(|op| op.summary.current().to_string())
                .unwrap_or_else(|| "No description".to_string());
            Ok(Some(
                json!({
                    "command": format!("cos app {app_name} {command}"),
                    "description": desc,
                })
                .to_string(),
            ))
        }
        _ => {
            // App doesn't support __schema__ — return basic info
            let desc = app
                .manifest
                .operations
                .get(command)
                .map(|op| op.summary.current().to_string())
                .unwrap_or_else(|| "No description".to_string());
            Ok(Some(
                json!({
                    "command": format!("cos app {app_name} {command}"),
                    "description": desc,
                })
                .to_string(),
            ))
        }
    }
}

fn show_app_schema(app_name: &str, app: &apps::App) -> Result<Option<String>, String> {
    let data_dir = data_dir();
    let apps = apps_dir().to_string_lossy().to_string();

    // Try to get live schema from the app
    let live_schema = match bridge::run_python_app(&app.dir, "__schema__", &[], &data_dir, &apps) {
        Ok(Some(output)) => serde_json::from_str::<Value>(&output).ok(),
        _ => None,
    };

    let mut commands = Vec::new();
    for (cmd_name, op) in &app.manifest.operations {
        let mut entry = json!({
            "command": cmd_name,
            "label": op.label.current(),
            "description": op.summary.current(),
        });
        if let Some(ref schema) = live_schema {
            if let Some(cmd_schema) = schema.get(cmd_name.as_str()) {
                if let Some(params) = cmd_schema.get("parameters") {
                    entry["parameters"] = params.clone();
                }
                if let Some(example) = cmd_schema.get("example") {
                    entry["example"] = example.clone();
                }
            }
        }
        commands.push(entry);
    }

    let output = json!({
        "app": app_name,
        "label": app.manifest.name.current(),
        "description": app.manifest.summary.current(),
        "commands": commands,
    });
    Ok(Some(output.to_string()))
}

/// Special-case dispatcher for `cos agent` that turns a bare
/// invocation (no subcommand) on an interactive TTY into either
/// `setup` (when the agent has not been configured yet) or `chat`
/// (when it has). Falls through to the standard help-table behavior
/// for non-TTY callers — scripts piping `cos agent | jq` still see
/// the machine-readable command list — and for explicit `--help`.
fn dispatch_agent(args: &[String]) -> Result<Option<String>, String> {
    // Explicit help should not be hijacked.
    let explicit_help = args.len() >= 2
        && matches!(args[1].as_str(), "--help" | "-h" | "help" | "--schema");
    if !explicit_help && args.len() == 1 {
        use std::io::IsTerminal;
        let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
        if interactive {
            let cfg = &crate::config::get().agent;
            let mut rewritten: Vec<String> = Vec::with_capacity(3);
            rewritten.push(args[0].clone());
            if agent::setup::is_ready(cfg).is_ok() {
                rewritten.push("chat".into());
            } else {
                // Land directly on the LLM wizard rather than the
                // modality picker — `cos agent` not being ready almost
                // always means the conversational LLM isn't configured.
                rewritten.push("setup".into());
                rewritten.push("llm".into());
            }
            return dispatch_builtin(&rewritten, "agent", agent::run);
        }
    }
    dispatch_builtin(args, "agent", agent::run)
}

fn dispatch_builtin(
    args: &[String],
    app_name: &str,
    handler: fn(&str, &[String]) -> Result<Value, String>,
) -> Result<Option<String>, String> {
    // `cos <primitive>` and `cos <primitive> --help|-h|help` render the
    // same machine-readable command list. Doing this here means every
    // primitive picks up help support uniformly.
    let help_only = args.len() == 1
        || (args.len() == 2 && matches!(args[1].as_str(), "--help" | "-h" | "help"));
    if help_only {
        let apps = builtin_apps();
        let app = apps.iter().find(|(n, _, _)| *n == app_name).unwrap();
        let cmds: serde_json::Map<String, Value> = app
            .2
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect();
        let output = json!({
            "app": app_name,
            "description": app.1,
            "commands": cmds,
            "hint": format!("Run: cos {app_name} <command> [args]"),
        });
        return Ok(Some(output.to_string()));
    }

    // cos <primitive> --schema → show all command schemas for this primitive
    if args.len() == 2 && args[1] == "--schema" {
        return show_builtin_schema(app_name);
    }

    let command = &args[1];
    let cmd_args: Vec<String> = args[2..].to_vec();

    // If --schema is in args, return schema instead of executing
    if cmd_args.contains(&"--schema".to_string()) {
        return show_command_schema(app_name, command);
    }

    let start = std::time::Instant::now();
    let audit_p = audit_path();

    let result = handler(command, &cmd_args);

    match &result {
        Ok(v) => {
            audit::log_entry(&audit_p, app_name, command, &cmd_args, start, "ok", None);
            Ok(Some(v.to_string()))
        }
        Err(e) => {
            audit::log_entry(
                &audit_p,
                app_name,
                command,
                &cmd_args,
                start,
                "error",
                Some(e),
            );
            // Same shape as `dispatch_app` above: failures stay failures
            // (exit code 1) even when we attach a recovery envelope.
            // main.rs parses Err strings that are JSON objects and
            // surfaces them as-is, so consumers still get the structured
            // recovery payload.
            if let Some(recovery) = recovery_hint(e) {
                let mut err_output = json!({
                    "error": e.to_string(),
                    "recovery": recovery,
                });
                if let Some(code) = error_code_from_hint(e) {
                    err_output["code"] = json!(code);
                }
                Err(err_output.to_string())
            } else {
                Err(e.clone())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_hint_permission_denied() {
        let hint = recovery_hint("Permission denied on /home/cos/file.txt").unwrap();
        assert_eq!(hint["hint"], "Permission denied. Check file permissions.");
        let try_cmds = hint["try"].as_array().unwrap();
        assert!(try_cmds
            .iter()
            .any(|v| v.as_str().unwrap().contains("chmod")));
    }

    #[test]
    fn recovery_hint_eperm_variant() {
        let hint = recovery_hint("EPERM: operation not permitted").unwrap();
        assert_eq!(hint["hint"], "Permission denied. Check file permissions.");
    }

    #[test]
    fn recovery_hint_file_not_found() {
        let hint = recovery_hint("No such file or directory: /home/cos/missing").unwrap();
        assert_eq!(
            hint["hint"],
            "File or command not found. Verify the path exists."
        );
        let try_cmds = hint["try"].as_array().unwrap();
        assert!(try_cmds
            .iter()
            .any(|v| v.as_str().unwrap().contains("cos app fs ls")));
    }

    #[test]
    fn recovery_hint_enoent_variant() {
        let hint = recovery_hint("ENOENT: cannot open /tmp/data").unwrap();
        assert!(hint["hint"].as_str().unwrap().contains("not found"));
    }

    #[test]
    fn recovery_hint_not_found_variant() {
        let hint = recovery_hint("command not found: foobar").unwrap();
        assert!(hint["hint"].as_str().unwrap().contains("not found"));
    }

    #[test]
    fn recovery_hint_disk_full() {
        let hint = recovery_hint("No space left on device").unwrap();
        assert_eq!(hint["hint"], "Disk full. Free space before retrying.");
        let try_cmds = hint["try"].as_array().unwrap();
        assert!(try_cmds
            .iter()
            .any(|v| v.as_str().unwrap().contains("cos sys resources")));
    }

    #[test]
    fn recovery_hint_enospc_variant() {
        let hint = recovery_hint("ENOSPC: write failed").unwrap();
        assert!(hint["hint"].as_str().unwrap().contains("Disk full"));
    }

    #[test]
    fn recovery_hint_connection_refused() {
        let hint = recovery_hint("Connection refused to localhost:8080").unwrap();
        assert!(hint["hint"]
            .as_str()
            .unwrap()
            .contains("Connection refused"));
        let try_cmds = hint["try"].as_array().unwrap();
        assert!(try_cmds
            .iter()
            .any(|v| v.as_str().unwrap().contains("cos service")));
    }

    #[test]
    fn recovery_hint_econnrefused_variant() {
        let hint = recovery_hint("ECONNREFUSED: connect failed").unwrap();
        assert!(hint["hint"]
            .as_str()
            .unwrap()
            .contains("Connection refused"));
    }

    #[test]
    fn recovery_hint_timeout() {
        let hint = recovery_hint("Operation timed out after 30s").unwrap();
        assert!(hint["hint"].as_str().unwrap().contains("timed out"));
    }

    #[test]
    fn recovery_hint_timeout_variant() {
        let hint = recovery_hint("request timeout").unwrap();
        assert!(hint["hint"].as_str().unwrap().contains("timed out"));
    }

    #[test]
    fn recovery_hint_address_in_use() {
        let hint = recovery_hint("address already in use: 0.0.0.0:3000").unwrap();
        assert!(hint["hint"].as_str().unwrap().contains("already in use"));
    }

    #[test]
    fn recovery_hint_eaddrinuse_variant() {
        let hint = recovery_hint("EADDRINUSE: bind failed").unwrap();
        assert!(hint["hint"].as_str().unwrap().contains("already in use"));
    }

    #[test]
    fn recovery_hint_out_of_memory() {
        let hint = recovery_hint("Out of memory: cannot allocate").unwrap();
        assert!(hint["hint"].as_str().unwrap().contains("Out of memory"));
    }

    #[test]
    fn recovery_hint_enomem_variant() {
        let hint = recovery_hint("ENOMEM: mmap failed").unwrap();
        assert!(hint["hint"].as_str().unwrap().contains("Out of memory"));
    }

    #[test]
    fn recovery_hint_oom_variant() {
        let hint = recovery_hint("process killed by OOM killer").unwrap();
        assert!(hint["hint"].as_str().unwrap().contains("Out of memory"));
    }

    #[test]
    fn recovery_hint_unknown_error_returns_none() {
        assert!(recovery_hint("something completely unexpected happened").is_none());
    }

    #[test]
    fn recovery_hint_empty_string_returns_none() {
        assert!(recovery_hint("").is_none());
    }

    #[test]
    fn recovery_hint_case_insensitive() {
        // Should match regardless of case
        assert!(recovery_hint("PERMISSION DENIED").is_some());
        assert!(recovery_hint("permission denied").is_some());
        assert!(recovery_hint("Permission Denied").is_some());
    }

    #[test]
    fn recovery_hint_returns_valid_json_structure() {
        // Every hint should have both "hint" (string) and "try" (array of strings)
        let test_errors = [
            "permission denied",
            "no such file",
            "no space left",
            "connection refused",
            "timed out",
            "address already in use",
            "out of memory",
        ];
        for error in &test_errors {
            let hint =
                recovery_hint(error).unwrap_or_else(|| panic!("Expected hint for '{}'", error));
            assert!(
                hint["hint"].is_string(),
                "Missing 'hint' string for '{}'",
                error
            );
            assert!(
                hint["try"].is_array(),
                "Missing 'try' array for '{}'",
                error
            );
            let try_arr = hint["try"].as_array().unwrap();
            assert!(!try_arr.is_empty(), "Empty 'try' array for '{}'", error);
            for cmd in try_arr {
                assert!(cmd.is_string(), "Non-string in 'try' array for '{}'", error);
                assert!(
                    cmd.as_str().unwrap().starts_with("cos "),
                    "Recovery command should start with 'cos': {}",
                    cmd
                );
            }
        }
    }

    #[test]
    fn schema_for_known_builtin() {
        let schemas = command_schemas();
        assert!(schemas.iter().any(|(n, _, _)| *n == "checkpoint"));
        assert!(schemas.iter().any(|(n, _, _)| *n == "credential"));
        assert!(schemas.iter().any(|(n, _, _)| *n == "cron"));
        assert!(schemas.iter().any(|(n, _, _)| *n == "service"));
    }

    #[test]
    fn perms_namespace_is_not_user_facing() {
        let result = dispatch(&["perms".into(), "check".into(), "ui.notify".into()]);
        assert!(result.is_err());
    }

    #[test]
    fn hidden_policy_bridge_remains_available_to_runtimes() {
        let _lock = crate::test_env::lock_env();
        let prev_sess = std::env::var_os("COS_SESSION");
        let prev_mode = std::env::var_os("COS_PERMS_MODE");
        std::env::remove_var("COS_SESSION");
        std::env::set_var("COS_PERMS_MODE", "permissive");

        let output = dispatch(&["__policy".into(), "check".into(), "ui.notify".into()])
            .expect("hidden policy bridge should dispatch")
            .expect("hidden policy bridge should return JSON");
        let v: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(v["decision"], "allow");

        match prev_sess {
            Some(value) => std::env::set_var("COS_SESSION", value),
            None => std::env::remove_var("COS_SESSION"),
        }
        match prev_mode {
            Some(value) => std::env::set_var("COS_PERMS_MODE", value),
            None => std::env::remove_var("COS_PERMS_MODE"),
        }
    }

    #[test]
    fn show_command_schema_returns_json() {
        let result = show_command_schema("checkpoint", "create");
        assert!(result.is_ok());
        let output = result.unwrap().unwrap();
        let v: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(v["command"], "cos checkpoint create");
        assert!(v["parameters"].is_array());
        assert!(v["example"].is_string());
    }

    #[test]
    fn show_builtin_schema_returns_all_commands() {
        let result = show_builtin_schema("credential");
        assert!(result.is_ok());
        let output = result.unwrap().unwrap();
        let v: Value = serde_json::from_str(&output).unwrap();
        assert!(v["commands"].is_array());
        assert!(v["commands"].as_array().unwrap().len() > 3);
    }

    #[test]
    fn show_command_schema_unknown_returns_error() {
        let result = show_command_schema("nonexistent", "cmd");
        assert!(result.is_err());
    }

    #[test]
    fn show_command_schema_unknown_command_returns_error() {
        let result = show_command_schema("checkpoint", "nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn show_command_schema_has_param_details() {
        let result = show_command_schema("checkpoint", "create");
        let output = result.unwrap().unwrap();
        let v: Value = serde_json::from_str(&output).unwrap();
        let params = v["parameters"].as_array().unwrap();
        assert!(!params.is_empty());
        // Each param should have name, type, required, description, kind
        for p in params {
            assert!(p["name"].is_string());
            assert!(p["type"].is_string());
            assert!(p["required"].is_boolean());
            assert!(p["description"].is_string());
            assert!(
                p["kind"] == "positional" || p["kind"] == "flag",
                "kind must be positional or flag, got: {}",
                p["kind"]
            );
        }
    }

    #[test]
    fn show_builtin_schema_all_primitives() {
        // Every primitive that has a schema should produce valid output
        let primitives = [
            "checkpoint",
            "credential",
            "cron",
            "service",
            "sys",
        ];
        for name in &primitives {
            let result = show_builtin_schema(name);
            assert!(result.is_ok(), "Failed for primitive: {name}");
            let output = result.unwrap().unwrap();
            let v: Value = serde_json::from_str(&output).unwrap();
            assert_eq!(v["app"], *name);
            assert!(v["description"].is_string());
            assert!(v["commands"].is_array());
            assert!(
                !v["commands"].as_array().unwrap().is_empty(),
                "No commands for: {name}"
            );
        }
    }

    #[test]
    fn error_code_from_hint_maps_correctly() {
        assert_eq!(
            error_code_from_hint("Permission denied on /etc"),
            Some(crate::errors::IO_PERMISSION_DENIED)
        );
        assert_eq!(
            error_code_from_hint("No such file: /missing"),
            Some(crate::errors::IO_FILE_NOT_FOUND)
        );
        assert_eq!(
            error_code_from_hint("connection refused"),
            Some(crate::errors::IO_CONNECTION_REFUSED)
        );
        assert_eq!(
            error_code_from_hint("No space left on device"),
            Some(crate::errors::IO_DISK_FULL)
        );
        assert_eq!(
            error_code_from_hint("Operation timed out"),
            Some(crate::errors::LIMIT_TIMEOUT)
        );
        assert_eq!(
            error_code_from_hint("address already in use"),
            Some(crate::errors::RESOURCE_BUSY)
        );
        assert_eq!(
            error_code_from_hint("out of memory"),
            Some(crate::errors::LIMIT_OOM)
        );
        assert_eq!(error_code_from_hint("something random"), None);
    }

    fn parse(out: Option<String>) -> Value {
        serde_json::from_str(&out.expect("dispatch returned None")).expect("not JSON")
    }

    #[test]
    fn dispatch_help_flag_returns_overview() {
        let v = parse(dispatch(&["--help".into()]).unwrap());
        assert_eq!(v["name"], "cos");
        assert!(v["primitives"].is_array());
    }

    #[test]
    fn dispatch_h_short_flag_returns_overview() {
        let v = parse(dispatch(&["-h".into()]).unwrap());
        assert_eq!(v["name"], "cos");
    }

    #[test]
    fn dispatch_bare_help_returns_overview() {
        let v = parse(dispatch(&["help".into()]).unwrap());
        assert!(v["primitives"].is_array());
    }

    #[test]
    fn dispatch_help_topic_returns_primitive() {
        let v = parse(dispatch(&["help".into(), "sys".into()]).unwrap());
        assert_eq!(v["app"], "sys");
        assert!(v["commands"].is_object());
    }

    #[test]
    fn dispatch_help_unknown_topic_returns_overview_with_note() {
        let v = parse(dispatch(&["help".into(), "nope".into()]).unwrap());
        assert!(v["primitives"].is_array());
        assert!(v["note"].as_str().unwrap().contains("unknown help topic"));
    }

    #[test]
    fn dispatch_builtin_recovery_envelope_propagates_failure() {
        // Regression: a builtin handler that returns Err with a string
        // matching a `recovery_hint` pattern (e.g. "Permission denied"
        // when writing config as a non-root user) used to
        // be re-wrapped in `Ok(Some(envelope))`. That zeroed out the CLI
        // exit code, so callers like cosmic-settings' agent page parsed
        // the failure as a default-valued success and silently flipped
        // the provider back to openai. The wrapper must keep failures
        // failing while still attaching the recovery hints.
        fn boom(_command: &str, _args: &[String]) -> Result<Value, String> {
            Err("write /var/lib/foo.tmp: Permission denied (os error 13)".into())
        }
        let result = dispatch_builtin(&["agent".into(), "boom".into()], "agent", boom);
        let err = result.expect_err("dispatch_builtin must propagate Err for failed primitives");
        let v: Value = serde_json::from_str(&err).expect("recovery envelope must be JSON");
        assert!(
            v["error"].as_str().unwrap().contains("Permission denied"),
            "error preserved: {v}"
        );
        assert!(v["recovery"].is_object(), "recovery attached: {v}");
        assert_eq!(
            v["code"].as_str(),
            Some(crate::errors::IO_PERMISSION_DENIED),
            "structured error code attached: {v}"
        );
    }

    #[test]
    fn dispatch_version_returns_envelope() {
        for flag in ["--version", "-v", "-V"] {
            let v = parse(dispatch(&[flag.into()]).unwrap());
            assert_eq!(v["name"], "cos");
            assert_eq!(v["version"], VERSION);
        }
    }

    #[test]
    fn dispatch_builtin_help_token_returns_overview() {
        for flag in ["--help", "-h", "help"] {
            let v = parse(dispatch(&["sys".into(), flag.into()]).unwrap());
            assert_eq!(v["app"], "sys", "flag: {flag}");
            assert!(v["commands"].is_object());
        }
    }

    #[test]
    fn dispatch_agent_help_does_not_hijack() {
        // `cos agent --help` must return the command list rather than
        // dropping into the interactive chat/setup shortcut.
        let v = parse(dispatch(&["agent".into(), "--help".into()]).unwrap());
        assert_eq!(v["app"], "agent");
        assert!(v["commands"].is_object());
    }

    #[test]
    fn browser_module_compiles() {
        // cos browser is no longer a user CLI primitive — it's exposed
        // only as the `cos_browser` agent tool. Smoke-test that the module
        // is still wired up by reaching the unknown-command path.
        let err = crate::browser::run("__nope__", &[]).unwrap_err();
        assert!(err.contains("unknown"));
    }

    // -----------------------------------------------------------------
    // `cos app consent` CLI surface — see consent_cmd() above.
    // -----------------------------------------------------------------

    fn empty_apps() -> std::collections::BTreeMap<String, apps::App> {
        std::collections::BTreeMap::new()
    }

    #[test]
    fn consent_help_lists_subcommands() {
        let v = parse(consent_cmd(&[], &empty_apps()).unwrap());
        assert_eq!(v["app"], "consent");
        let subs = v["subcommands"].as_object().unwrap();
        for k in ["list", "show", "path", "grant", "revoke"] {
            assert!(subs.contains_key(k), "missing subcommand {k}");
        }
    }

    #[test]
    fn consent_path_returns_user_config_path() {
        let v = parse(
            consent_cmd(&["path".into(), "myapp".into()], &empty_apps()).unwrap(),
        );
        assert_eq!(v["app"], "myapp");
        let p = v["path"].as_str().unwrap();
        assert!(p.contains("consents"));
        assert!(p.ends_with("myapp.json"));
    }

    #[test]
    fn consent_show_missing_file_reports_absent() {
        let tmp = std::env::temp_dir().join(format!(
            "cos-consent-router-show-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let prev = std::env::var_os("COS_USER_CONFIG_DIR");
        std::env::set_var("COS_USER_CONFIG_DIR", &tmp);
        let v = parse(
            consent_cmd(
                &["show".into(), "never-granted".into()],
                &empty_apps(),
            )
            .unwrap(),
        );
        match prev {
            Some(x) => std::env::set_var("COS_USER_CONFIG_DIR", x),
            None => std::env::remove_var("COS_USER_CONFIG_DIR"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(v["present"], false);
        assert!(v["consent"].is_null());
    }

    #[test]
    fn consent_grant_unknown_app_errors() {
        let err = consent_cmd(
            &["grant".into(), "ghost".into(), "--yes".into()],
            &empty_apps(),
        )
        .unwrap_err();
        assert!(err.contains("unknown app"));
        assert!(err.contains("ghost"));
    }

    #[test]
    fn consent_revoke_missing_file_is_noop() {
        let tmp = std::env::temp_dir().join(format!(
            "cos-consent-router-revoke-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let prev = std::env::var_os("COS_USER_CONFIG_DIR");
        std::env::set_var("COS_USER_CONFIG_DIR", &tmp);
        let v = parse(
            consent_cmd(
                &["revoke".into(), "never-granted".into()],
                &empty_apps(),
            )
            .unwrap(),
        );
        match prev {
            Some(x) => std::env::set_var("COS_USER_CONFIG_DIR", x),
            None => std::env::remove_var("COS_USER_CONFIG_DIR"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(v["revoked"], false);
    }

    #[test]
    fn consent_grant_yes_writes_record_and_show_reads_it_back() {
        use crate::caps::manifest::{
            AiBudget, AiPolicy, AiSafety, Manifest, PromptOrigin, Runtime,
        };
        use crate::i18n::LocalizedText;
        use std::collections::BTreeMap;

        let tmp = std::env::temp_dir().join(format!(
            "cos-consent-router-grant-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let prev = std::env::var_os("COS_USER_CONFIG_DIR");
        std::env::set_var("COS_USER_CONFIG_DIR", &tmp);

        let manifest = Manifest {
            id: "demo".into(),
            version: "0.0.1".into(),
            name: LocalizedText::en("Demo"),
            summary: LocalizedText::default(),
            icon: None,
            runtime: Runtime::default(),
            entry: None,
            operations: BTreeMap::new(),
            ai: Some(AiPolicy {
                budget: AiBudget { monthly_units: 1000 },
                safety: AiSafety::Standard,
                origins: vec![PromptOrigin::Trusted],
                tools: Vec::new(),
            }),
            session: None,
            dependencies: serde_json::Value::Null,
        };
        let mut discovered = std::collections::BTreeMap::new();
        discovered.insert(
            "demo".to_string(),
            apps::App {
                manifest,
                dir: tmp.join("does-not-matter"),
            },
        );

        let granted = parse(
            consent_cmd(
                &["grant".into(), "demo".into(), "--yes".into()],
                &discovered,
            )
            .unwrap(),
        );
        assert_eq!(granted["granted"], true);
        assert_eq!(granted["app"], "demo");

        let shown = parse(
            consent_cmd(&["show".into(), "demo".into()], &discovered).unwrap(),
        );
        assert_eq!(shown["present"], true);
        assert_eq!(shown["consent"]["policy"]["budget"]["monthly_units"], 1000);

        let listed = parse(consent_cmd(&["list".into()], &discovered).unwrap());
        let rows = listed["consents"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["app"], "demo");
        assert_eq!(rows[0]["status"], "fresh");

        let revoked = parse(
            consent_cmd(&["revoke".into(), "demo".into()], &discovered).unwrap(),
        );
        assert_eq!(revoked["revoked"], true);

        match prev {
            Some(x) => std::env::set_var("COS_USER_CONFIG_DIR", x),
            None => std::env::remove_var("COS_USER_CONFIG_DIR"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // -----------------------------------------------------------------
    // `cos app install` CLI surface — see install_cmd() above.
    // -----------------------------------------------------------------

    fn write_min_app(dir: &std::path::Path, id: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("app.json"), body).unwrap();
        // A tiny main.py so the copy step has something to move.
        std::fs::write(
            dir.join("main.py"),
            format!("# stub for {id}\n"),
        )
        .unwrap();
    }

    #[test]
    fn install_requires_source() {
        let err = install_cmd(&[]).unwrap_err();
        assert!(err.contains("usage:"), "got: {err}");
    }

    #[test]
    fn install_rejects_non_directory_source() {
        let err = install_cmd(&["/dev/null".into()]).unwrap_err();
        assert!(err.contains("not a directory"), "got: {err}");
    }

    #[test]
    fn install_rejects_missing_manifest() {
        let tmp = std::env::temp_dir()
            .join(format!("cos-install-no-manifest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let err = install_cmd(&[tmp.display().to_string()]).unwrap_err();
        assert!(err.contains("no app.json"), "got: {err}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn install_rejects_unknown_tool_in_manifest() {
        let pid = std::process::id();
        let src = std::env::temp_dir().join(format!("cos-install-bad-tool-src-{pid}"));
        let dst = std::env::temp_dir().join(format!("cos-install-bad-tool-dst-{pid}"));
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
        write_min_app(
            &src,
            "bad",
            r#"{
              "id": "bad",
              "version": "0.0.1",
              "name": "Bad",
              "ai": {
                "budget": {"monthly_units": 1},
                "safety": "strict",
                "origins": ["trusted"],
                "tools": ["fs.unicorn"]
              }
            }"#,
        );

        let prev_apps = std::env::var_os("COS_APPS_DIR");
        std::env::set_var("COS_APPS_DIR", &dst);
        let err = install_cmd(&[src.display().to_string()]).unwrap_err();
        match prev_apps {
            Some(x) => std::env::set_var("COS_APPS_DIR", x),
            None => std::env::remove_var("COS_APPS_DIR"),
        }
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);

        assert!(err.contains("manifest catalog check"), "got: {err}");
        assert!(err.contains("fs.unicorn"), "got: {err}");
    }

    #[test]
    fn install_copies_app_without_ai_block_and_skips_consent() {
        let pid = std::process::id();
        let src = std::env::temp_dir().join(format!("cos-install-noai-src-{pid}"));
        let dst = std::env::temp_dir().join(format!("cos-install-noai-dst-{pid}"));
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
        write_min_app(
            &src,
            "calc",
            r#"{
              "id": "calc",
              "version": "0.0.1",
              "name": "Calc"
            }"#,
        );

        let prev_apps = std::env::var_os("COS_APPS_DIR");
        std::env::set_var("COS_APPS_DIR", &dst);
        let v = parse(install_cmd(&[src.display().to_string()]).unwrap());
        match prev_apps {
            Some(x) => std::env::set_var("COS_APPS_DIR", x),
            None => std::env::remove_var("COS_APPS_DIR"),
        }

        assert_eq!(v["installed"], true);
        assert_eq!(v["app"], "calc");
        assert_eq!(v["copied"], true);
        assert_eq!(v["consent"]["needed"], false);
        assert!(dst.join("calc").join("app.json").is_file());
        assert!(dst.join("calc").join("main.py").is_file());

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn install_no_consent_defers_consent_for_ai_app() {
        let pid = std::process::id();
        let src = std::env::temp_dir().join(format!("cos-install-defer-src-{pid}"));
        let dst = std::env::temp_dir().join(format!("cos-install-defer-dst-{pid}"));
        let cfg = std::env::temp_dir().join(format!("cos-install-defer-cfg-{pid}"));
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
        let _ = std::fs::remove_dir_all(&cfg);
        write_min_app(
            &src,
            "summ",
            r#"{
              "id": "summ",
              "version": "0.0.1",
              "name": "Summ",
              "ai": {
                "budget": {"monthly_units": 100},
                "safety": "strict",
                "origins": ["trusted"],
                "tools": ["fs.read_text"]
              }
            }"#,
        );

        let prev_apps = std::env::var_os("COS_APPS_DIR");
        let prev_cfg = std::env::var_os("COS_USER_CONFIG_DIR");
        std::env::set_var("COS_APPS_DIR", &dst);
        std::env::set_var("COS_USER_CONFIG_DIR", &cfg);
        let v = parse(
            install_cmd(&[src.display().to_string(), "--no-consent".into()])
                .unwrap(),
        );
        match prev_apps {
            Some(x) => std::env::set_var("COS_APPS_DIR", x),
            None => std::env::remove_var("COS_APPS_DIR"),
        }
        match prev_cfg {
            Some(x) => std::env::set_var("COS_USER_CONFIG_DIR", x),
            None => std::env::remove_var("COS_USER_CONFIG_DIR"),
        }

        assert_eq!(v["installed"], true);
        assert_eq!(v["consent"]["needed"], true);
        assert_eq!(v["consent"]["granted"], false);
        assert_eq!(v["consent"]["deferred"], true);
        assert!(dst.join("summ").join("app.json").is_file());

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
        let _ = std::fs::remove_dir_all(&cfg);
    }

    #[test]
    fn install_yes_grants_consent_for_ai_app() {
        let pid = std::process::id();
        let src = std::env::temp_dir().join(format!("cos-install-yes-src-{pid}"));
        let dst = std::env::temp_dir().join(format!("cos-install-yes-dst-{pid}"));
        let cfg = std::env::temp_dir().join(format!("cos-install-yes-cfg-{pid}"));
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
        let _ = std::fs::remove_dir_all(&cfg);
        write_min_app(
            &src,
            "yes",
            r#"{
              "id": "yes",
              "version": "0.0.1",
              "name": "Yes",
              "ai": {
                "budget": {"monthly_units": 100},
                "safety": "strict",
                "origins": ["trusted"]
              }
            }"#,
        );

        let prev_apps = std::env::var_os("COS_APPS_DIR");
        let prev_cfg = std::env::var_os("COS_USER_CONFIG_DIR");
        std::env::set_var("COS_APPS_DIR", &dst);
        std::env::set_var("COS_USER_CONFIG_DIR", &cfg);
        let v = parse(
            install_cmd(&[src.display().to_string(), "--yes".into()]).unwrap(),
        );
        match prev_apps {
            Some(x) => std::env::set_var("COS_APPS_DIR", x),
            None => std::env::remove_var("COS_APPS_DIR"),
        }
        match prev_cfg {
            Some(x) => std::env::set_var("COS_USER_CONFIG_DIR", x),
            None => std::env::remove_var("COS_USER_CONFIG_DIR"),
        }

        assert_eq!(v["installed"], true);
        assert_eq!(v["consent"]["needed"], true);
        assert_eq!(v["consent"]["granted"], true);
        assert!(v["consent"]["approved_at"].is_string());

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
        let _ = std::fs::remove_dir_all(&cfg);
    }

    #[test]
    fn install_refuses_to_overwrite_without_force() {
        let pid = std::process::id();
        let src = std::env::temp_dir().join(format!("cos-install-overw-src-{pid}"));
        let dst = std::env::temp_dir().join(format!("cos-install-overw-dst-{pid}"));
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
        write_min_app(
            &src,
            "twice",
            r#"{
              "id": "twice",
              "version": "0.0.1",
              "name": "Twice"
            }"#,
        );
        std::fs::create_dir_all(dst.join("twice")).unwrap();
        std::fs::write(dst.join("twice").join("placeholder"), b"existing").unwrap();

        let prev_apps = std::env::var_os("COS_APPS_DIR");
        std::env::set_var("COS_APPS_DIR", &dst);
        let err = install_cmd(&[src.display().to_string()]).unwrap_err();
        match prev_apps {
            Some(x) => std::env::set_var("COS_APPS_DIR", x),
            None => std::env::remove_var("COS_APPS_DIR"),
        }
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);

        assert!(err.contains("already exists"), "got: {err}");
        assert!(err.contains("--force"), "got: {err}");
    }

    #[test]
    fn install_force_replaces_existing_install() {
        let pid = std::process::id();
        let src = std::env::temp_dir().join(format!("cos-install-force-src-{pid}"));
        let dst = std::env::temp_dir().join(format!("cos-install-force-dst-{pid}"));
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
        write_min_app(
            &src,
            "force",
            r#"{
              "id": "force",
              "version": "0.0.1",
              "name": "Force"
            }"#,
        );
        std::fs::create_dir_all(dst.join("force")).unwrap();
        std::fs::write(dst.join("force").join("stale"), b"junk").unwrap();

        let prev_apps = std::env::var_os("COS_APPS_DIR");
        std::env::set_var("COS_APPS_DIR", &dst);
        let v = parse(
            install_cmd(&[src.display().to_string(), "--force".into()]).unwrap(),
        );
        match prev_apps {
            Some(x) => std::env::set_var("COS_APPS_DIR", x),
            None => std::env::remove_var("COS_APPS_DIR"),
        }

        assert_eq!(v["installed"], true);
        assert_eq!(v["copied"], true);
        assert!(dst.join("force").join("app.json").is_file());
        assert!(
            !dst.join("force").join("stale").is_file(),
            "--force must clear the old tree before copying"
        );

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }

    /// Regression: a symlink anywhere in the install source tree must
    /// be rejected. Otherwise an attacker who can plant a link inside a
    /// "trusted developer tree" can either copy out-of-tree files (e.g.
    /// `/etc/shadow`, the system credential store) into the installed
    /// App location, or escape the source tree during recursion.
    #[cfg(unix)]
    #[test]
    fn install_rejects_symlink_in_source_tree() {
        let pid = std::process::id();
        let src = std::env::temp_dir().join(format!("cos-install-symlink-src-{pid}"));
        let dst = std::env::temp_dir().join(format!("cos-install-symlink-dst-{pid}"));
        let outside = std::env::temp_dir().join(format!("cos-install-symlink-outside-{pid}"));
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
        let _ = std::fs::remove_file(&outside);

        write_min_app(
            &src,
            "linky",
            r#"{
              "id": "linky",
              "version": "0.0.1",
              "name": "Linky"
            }"#,
        );

        // Create a target outside the source tree we wouldn't want
        // materialised inside the App.
        std::fs::write(&outside, b"secret-bytes-not-meant-for-this-app").unwrap();
        // Plant a symlink in the source tree pointing at the outside
        // target. With the old `fs::copy` traversal this would be
        // copied verbatim under `dst/linky/secret`.
        std::os::unix::fs::symlink(&outside, src.join("secret")).unwrap();

        let prev_apps = std::env::var_os("COS_APPS_DIR");
        std::env::set_var("COS_APPS_DIR", &dst);
        let err = install_cmd(&[src.display().to_string()]).unwrap_err();
        match prev_apps {
            Some(x) => std::env::set_var("COS_APPS_DIR", x),
            None => std::env::remove_var("COS_APPS_DIR"),
        }

        assert!(
            err.contains("symlink"),
            "expected symlink rejection error, got: {err}"
        );
        // The installed dest must not exist (or at minimum must not
        // contain the would-be copied symlink target).
        let leaked = dst.join("linky").join("secret");
        assert!(
            !leaked.is_file(),
            "symlink target must not have been materialised in install dest"
        );

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
        let _ = std::fs::remove_file(&outside);
    }
}
