//! App developer and management commands for the `cos app` namespace.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::help::{show_app_command_schema, show_app_help, show_app_schema, show_apps};
use super::{apps_dir, launch_app_gui, run_app_command};
use crate::apps;

/// Directory where freedesktop `.desktop` launchers are written at
/// `cos app install`. Overridable via `COS_APPLICATIONS_DIR` (used by
/// tests and per-user installs); defaults to the system location the
/// desktop shell's applibrary/launcher already scan.
fn applications_dir() -> PathBuf {
    PathBuf::from(
        env::var("COS_APPLICATIONS_DIR").unwrap_or_else(|_| "/usr/share/applications".into()),
    )
}

/// Dispatch to Python apps under the "cos app" namespace.
pub(super) fn dispatch_app(args: &[String]) -> Result<Option<String>, String> {
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

    // Special: `cos app create <id> [--kind cli|desktop|both]` —
    // scaffold a new app directory (app.json + entry stub) so a
    // developer starts from a valid, ready-to-edit skeleton instead of
    // hand-writing the manifest. Pure file generation; no AI gate.
    if app_name == "create" {
        return create_cmd(&args[1..]);
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

    // cos app <name> <desktop.exec> [files...] → launch the GUI surface.
    // Only apps that declare a `desktop` block respond to this; the
    // generated `.desktop` invokes exactly this path so the GUI process
    // is kernel-spawned (identity/audit/consent apply).
    {
        let app = &discovered[app_name.as_str()];
        if let Some(desktop) = app.manifest.desktop.as_ref() {
            if args.len() >= 2 && args[1] == desktop.exec {
                let files: Vec<String> = args[2..].to_vec();
                return launch_app_gui(app_name, desktop.exec.as_str(), &files, app);
            }
        }
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
        let violations = app_lint_violations(app);
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

fn app_lint_violations(app: &apps::App) -> Vec<Value> {
    let mut violations = scan_app_for_ai_imports(&app.dir);
    violations.extend(scan_session_block(app));
    violations
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
        .unwrap_or_else(|| app.manifest.runtime.default_session_entry().to_string());
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
///   * Otherwise the source is copied to a hidden sibling staging
///     directory, linted and fsynced there, then renamed into place.
///   * If the destination already exists and `--force` was not passed,
///     the install fails with a helpful message rather than silently
///     overwriting an existing App.
///   * Forced replacement retains the old tree under a hidden sibling
///     backup until the staged tree has been published. A failed
///     publish restores the backup.
///
/// Consent:
///   * Apps without an `ai` block have nothing to consent to and the
///     install completes after the copy.
///   * Apps with an `ai` block prompt interactively unless `--yes`
///     (auto-grant) or `--no-consent` (defer; operator must run
///     `cos app consent grant <id>` later).
pub(super) fn install_cmd(args: &[String]) -> Result<Option<String>, String> {
    let source_arg = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .ok_or_else(|| {
            "usage: cos app install <source-dir> [--yes] [--no-consent] [--force]".to_string()
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
    let mut manifest = apps::AppManifest::from_json(&body)
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
        manifest = validate_install_tree(&source, &manifest.id, "in-place app")?;
        copied = false;
    } else {
        if path_entry_exists(&dest)
            .map_err(|e| format!("inspect destination {}: {e}", dest.display()))?
            && !force
        {
            return Err(format!(
                "destination `{}` already exists. Re-run with --force to overwrite.",
                dest.display()
            ));
        }
        manifest = stage_app_install(&source, &dest, force, &manifest.id)?;
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

    // If the app declares a `desktop` surface, emit a freedesktop
    // launcher so it appears in the applibrary/launcher. The launcher's
    // Exec routes through `cos app <id> <exec>` so the GUI process is
    // kernel-spawned and inherits the app's identity/audit/consent.
    match write_desktop_entry(&manifest) {
        Ok(Some(path)) => {
            envelope["desktop"] = json!({ "generated": true, "path": path });
        }
        Ok(None) => {}
        Err(e) => {
            // Non-fatal: the app is installed and usable headlessly even
            // if the launcher couldn't be written (e.g. unwritable
            // /usr/share/applications). Surface the reason for the operator.
            envelope["desktop"] = json!({ "generated": false, "error": e });
        }
    }

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

/// `cos app create <id> [--kind cli|desktop|both] [--dir <parent>]
/// [--force]` — scaffold a new app from a template.
///
/// Generates `<parent>/<id>/` (parent defaults to the current dir)
/// containing:
///   * `app.json` — a valid manifest for the chosen surface kind. For
///     `cli`/`both` it includes a sample `operations` entry; for
///     `desktop`/`both` it includes a `desktop` block so `cos app
///     install` will emit a launcher.
///   * `main.py` — a Python entry exposing `run(command, args)`. When a
///     desktop surface is requested the stub branches on
///     `gui.is_gui_launch()` to enter a GUI loop vs. handle an op.
///
/// The generated `app.json` is parsed back through
/// `Manifest::from_json` + `validate()` before anything is written, so
/// the scaffold can never produce a manifest the kernel would reject.
pub(super) fn create_cmd(args: &[String]) -> Result<Option<String>, String> {
    let id = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .ok_or_else(|| {
            "usage: cos app create <id> [--kind cli|desktop|both] [--dir <parent>] [--force]"
                .to_string()
        })?;
    if !is_scaffold_id(&id) {
        return Err(format!(
            "invalid app id `{id}`: must match [a-z][a-z0-9_-]*"
        ));
    }

    let kind = flag_value(args, "--kind").unwrap_or_else(|| "cli".to_string());
    let (want_ops, want_desktop) = match kind.as_str() {
        "cli" => (true, false),
        "desktop" => (false, true),
        "both" => (true, true),
        other => {
            return Err(format!(
                "unknown --kind `{other}`: expected cli, desktop, or both"
            ));
        }
    };
    let force = args.iter().any(|a| a == "--force");
    let parent = flag_value(args, "--dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let dest = parent.join(&id);

    if dest.exists() {
        if !force {
            return Err(format!(
                "destination `{}` already exists. Re-run with --force to overwrite.",
                dest.display()
            ));
        }
        std::fs::remove_dir_all(&dest)
            .map_err(|e| format!("remove existing {}: {e}", dest.display()))?;
    }

    let manifest_body = scaffold_app_json(&id, want_ops, want_desktop);
    // Fail before writing if the template wouldn't parse/validate.
    let manifest = apps::AppManifest::from_json(&manifest_body)
        .map_err(|e| format!("internal: generated manifest is invalid: {e}"))?;
    manifest
        .validate()
        .map_err(|e| format!("internal: generated manifest failed validation: {e}"))?;
    let entry_body = scaffold_main_py(&id, want_ops, want_desktop);

    std::fs::create_dir_all(&dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    let manifest_path = dest.join("app.json");
    std::fs::write(&manifest_path, &manifest_body)
        .map_err(|e| format!("write {}: {e}", manifest_path.display()))?;
    let entry_path = dest.join("main.py");
    std::fs::write(&entry_path, &entry_body)
        .map_err(|e| format!("write {}: {e}", entry_path.display()))?;

    let envelope = json!({
        "app": id,
        "created": true,
        "kind": kind,
        "dir": dest.display().to_string(),
        "files": [
            manifest_path.display().to_string(),
            entry_path.display().to_string(),
        ],
        "next": format!("Edit the stubs, then run `cos app install {}`.", dest.display()),
    });
    Ok(Some(envelope.to_string()))
}

/// Read the value following a `--flag` in an argv slice, if present.
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// Same id rule the manifest validator enforces (`[a-z][a-z0-9_-]*`),
/// applied up front so we don't scaffold a tree the kernel rejects.
fn is_scaffold_id(s: &str) -> bool {
    let mut bytes = s.bytes();
    match bytes.next() {
        Some(b) if b.is_ascii_lowercase() => {}
        _ => return false,
    }
    bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Build a valid `app.json` for the requested surfaces. Kept as a
/// literal template (rather than serializing structs) so the output is
/// human-friendly and easy for the developer to extend.
fn scaffold_app_json(id: &str, want_ops: bool, want_desktop: bool) -> String {
    let mut blocks: Vec<String> = Vec::new();
    blocks.push(format!("  \"id\": \"{id}\""));
    blocks.push("  \"version\": \"0.1.0\"".to_string());
    blocks.push(format!("  \"name\": {{ \"en\": \"{id}\" }}"));
    blocks.push(format!(
        "  \"summary\": {{ \"en\": \"{id} — a Claw OS app.\" }}"
    ));
    blocks.push("  \"runtime\": \"python\"".to_string());
    blocks.push("  \"entry\": \"main.py\"".to_string());

    if want_ops {
        blocks.push(
            r#"  "operations": {
    "greet": {
      "label": { "en": "Print a friendly greeting (--name)" },
      "args": [
        { "name": "--name", "kind": "text", "required": false }
      ],
      "needs": []
    }
  }"#
            .to_string(),
        );
    }

    if want_desktop {
        blocks.push(
            r#"  "desktop": {
    "exec": "--gui",
    "categories": ["Utility"],
    "single_instance": true
  }"#
            .to_string(),
        );
    }

    format!("{{\n{}\n}}\n", blocks.join(",\n"))
}

/// Build a `main.py` entry stub exposing `run(command, args)`. When a
/// desktop surface is requested the stub branches on the GUI launch
/// signal so the same entry serves both the headless op and the window.
fn scaffold_main_py(id: &str, want_ops: bool, want_desktop: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!("\"\"\"{id} — a Claw OS app.\n\n"));
    out.push_str("The kernel calls run(command, args) for each invocation.\n");
    out.push_str("\"\"\"\n\n");

    if want_desktop {
        out.push_str("from claw_os_sdk import gui\n\n\n");
        out.push_str("def run_gui(ctx):\n");
        out.push_str("    \"\"\"Draw your own window here (any toolkit). World A:\n");
        out.push_str("    the OS does not own the UI. Use ctx.files for any file\n");
        out.push_str("    arguments, and ctx.open_agent_overlay() to summon Claw.\n");
        out.push_str("    \"\"\"\n");
        out.push_str("    print(f\"[{ctx.app_id}] GUI launch; files={ctx.files}\")\n\n\n");
    }

    if want_ops {
        out.push_str("def greet(args):\n");
        out.push_str("    name = args.get(\"--name\", \"world\")\n");
        out.push_str("    return {\"message\": f\"Hello, {name}!\"}\n\n\n");
    }

    out.push_str("def run(command, args):\n");
    out.push_str("    \"\"\"Entry point called by cos.\"\"\"\n");
    if want_desktop {
        out.push_str("    if gui.is_gui_launch(command):\n");
        out.push_str("        return run_gui(gui.context())\n");
    }
    if want_ops {
        out.push_str("    if command == \"greet\":\n");
        out.push_str("        return greet(args)\n");
    }
    out.push_str("    return {\"error\": f\"unknown command: {command}\"}\n");
    out
}

///
/// Writes `<applications_dir>/com.clawos.<Id>.desktop` with
/// `Exec=cos app <id> <exec> [%F]`. Routing the launch through
/// `cos app <id> ...` (rather than exec-ing the app binary) is what
/// makes the GUI process kernel-spawned, so `COS_APP_ID` identity,
/// audit, and consent apply exactly as on the headless path.
///
/// Returns `Ok(None)` if the app has no `desktop` block. Best-effort
/// runs `update-desktop-database` afterwards; failure there is ignored
/// (the entry is still valid).
fn write_desktop_entry(manifest: &apps::AppManifest) -> Result<Option<String>, String> {
    let Some(desktop) = manifest.desktop.as_ref() else {
        return Ok(None);
    };

    let id = &manifest.id;
    let name = desktop
        .name
        .as_ref()
        .map(|n| n.en_str())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| manifest.name.en_str());
    let icon = desktop
        .icon
        .as_deref()
        .or(manifest.icon.as_deref())
        .unwrap_or(id);

    // Field code: only request file arguments when the app declares
    // MIME associations, otherwise launch with no arguments.
    let exec_line = if desktop.mime_types.is_empty() {
        format!("cos app {id} {} ", desktop.exec)
    } else {
        format!("cos app {id} {} %F", desktop.exec)
    };
    let exec_line = exec_line.trim_end().to_string();

    // Categories: always tag ClawOS, then the app's own (validated to
    // contain no ';' or control chars at manifest parse time).
    let mut cats = vec!["ClawOS".to_string()];
    cats.extend(desktop.categories.iter().cloned());

    let mut entry = String::new();
    entry.push_str("[Desktop Entry]\n");
    entry.push_str("Type=Application\n");
    entry.push_str("Version=1.0\n");
    entry.push_str(&format!("Name={name}\n"));
    if !manifest.summary.en_str().is_empty() {
        entry.push_str(&format!("Comment={}\n", manifest.summary.en_str()));
    }
    entry.push_str(&format!("Exec={exec_line}\n"));
    entry.push_str(&format!("Icon={icon}\n"));
    entry.push_str("Terminal=false\n");
    entry.push_str(&format!("Categories={};\n", cats.join(";")));
    if !desktop.mime_types.is_empty() {
        entry.push_str(&format!("MimeType={};\n", desktop.mime_types.join(";")));
    }
    if desktop.single_instance {
        entry.push_str("SingleMainWindow=true\n");
    }
    // Provenance marker so the launcher / audit tooling can tell a
    // Claw OS app entry from an ordinary system .desktop.
    entry.push_str(&format!("X-CLAW-App-Id={id}\n"));

    let dir = applications_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let file = dir.join(format!("com.clawos.{id}.desktop"));
    std::fs::write(&file, entry).map_err(|e| format!("write {}: {e}", file.display()))?;

    // Refresh the MIME/desktop cache so associations take effect without
    // a relogin. Best-effort: a missing tool or read-only cache is fine.
    let _ = std::process::Command::new("update-desktop-database")
        .arg(&dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    Ok(Some(file.display().to_string()))
}

fn validate_install_tree(
    dir: &Path,
    expected_id: &str,
    description: &str,
) -> Result<apps::AppManifest, String> {
    let manifest_path = dir.join("app.json");
    let body = fs::read_to_string(&manifest_path).map_err(|e| {
        format!(
            "read {description} manifest {}: {e}",
            manifest_path.display()
        )
    })?;
    let manifest = apps::AppManifest::from_json(&body).map_err(|e| {
        format!(
            "parse {description} manifest {}: {e}",
            manifest_path.display()
        )
    })?;
    if manifest.id != expected_id {
        return Err(format!(
            "{description} manifest id changed during install: expected `{expected_id}`, got `{}`",
            manifest.id
        ));
    }
    manifest
        .validate_tools_against_catalog(&crate::ai::tools::list_names())
        .map_err(|e| format!("{description} manifest catalog check: {e}"))?;

    let app = apps::App {
        manifest: manifest.clone(),
        dir: dir.to_path_buf(),
    };
    let violations = app_lint_violations(&app);
    if !violations.is_empty() {
        let details = serde_json::to_string(&violations)
            .unwrap_or_else(|_| "lint violations could not be rendered".to_string());
        return Err(format!(
            "{description} lint failed for `{expected_id}`: {details}"
        ));
    }
    Ok(manifest)
}

/// Copy, validate, and durably publish an App tree.
///
/// `staging` and any forced-install backup are siblings of `dest`, so
/// every rename stays on one filesystem. Linux uses `RENAME_EXCHANGE`
/// when the filesystem supports it; the portable fallback recovers any
/// orphaned backup before starting a later install.
fn stage_app_install(
    source: &Path,
    dest: &Path,
    force: bool,
    expected_id: &str,
) -> Result<apps::AppManifest, String> {
    stage_app_install_with_ops(
        source,
        dest,
        force,
        expected_id,
        |from, to| fs::rename(from, to),
        atomic_exchange,
    )
}

/// The rename-only path keeps rollback failures deterministic in unit
/// tests and exercises the fallback used where exchange is unavailable.
#[cfg(test)]
pub(super) fn stage_app_install_with_rename<R>(
    source: &Path,
    dest: &Path,
    force: bool,
    expected_id: &str,
    rename: R,
) -> Result<apps::AppManifest, String>
where
    R: FnMut(&Path, &Path) -> io::Result<()>,
{
    stage_app_install_with_ops(
        source,
        dest,
        force,
        expected_id,
        rename,
        |_staging, _dest| Ok(false),
    )
}

fn stage_app_install_with_ops<R, E>(
    source: &Path,
    dest: &Path,
    force: bool,
    expected_id: &str,
    mut rename: R,
    mut exchange: E,
) -> Result<apps::AppManifest, String>
where
    R: FnMut(&Path, &Path) -> io::Result<()>,
    E: FnMut(&Path, &Path) -> io::Result<bool>,
{
    let parent = dest
        .parent()
        .ok_or_else(|| format!("install destination `{}` has no parent", dest.display()))?;
    fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    recover_interrupted_app_install(parent, dest, expected_id, &mut rename)?;

    let token = uuid::Uuid::new_v4();
    let staging = parent.join(format!(".{expected_id}.install-staging-{token}"));
    fs::create_dir(&staging).map_err(|e| format!("create staging {}: {e}", staging.display()))?;
    let _staging_guard = InstallStagingGuard(staging.clone());

    copy_dir_recursive(source, &staging)
        .map_err(|e| format!("copy {} -> {}: {e}", source.display(), staging.display()))?;
    let manifest = validate_install_tree(&staging, expected_id, "staged app")?;
    sync_install_tree(&staging)
        .map_err(|e| format!("fsync staged app {}: {e}", staging.display()))?;

    let destination_exists = path_entry_exists(dest)
        .map_err(|e| format!("inspect destination {}: {e}", dest.display()))?;
    if destination_exists && !force {
        return Err(format!(
            "destination `{}` already exists. Re-run with --force to overwrite.",
            dest.display()
        ));
    }

    if destination_exists {
        match exchange(&staging, dest) {
            Ok(true) => {
                sync_directory_best_effort(parent);
                if let Err(error) = remove_path(&staging) {
                    tracing::warn!(
                        path = %staging.display(),
                        %error,
                        "app install committed but exchanged old tree cleanup failed"
                    );
                }
                sync_directory_best_effort(parent);
                return Ok(manifest);
            }
            Ok(false) => {}
            Err(error) => {
                return Err(format!(
                    "atomically exchange staged app {} with {}: {error}",
                    staging.display(),
                    dest.display()
                ));
            }
        }
    }

    let backup = if destination_exists {
        let backup = parent.join(format!(".{expected_id}.install-backup-{token}"));
        rename(dest, &backup).map_err(|e| {
            format!(
                "move existing {} -> {}: {e}",
                dest.display(),
                backup.display()
            )
        })?;
        Some(backup)
    } else {
        None
    };

    if let Err(publish_error) = rename(&staging, dest) {
        let publish_message = format!(
            "publish staged app {} -> {}: {publish_error}",
            staging.display(),
            dest.display()
        );
        if let Some(backup) = backup.as_ref() {
            match rename(backup, dest) {
                Ok(()) => {
                    sync_directory_best_effort(parent);
                    return Err(format!("{publish_message}; previous install restored"));
                }
                Err(rollback_error) => {
                    sync_directory_best_effort(parent);
                    return Err(format!(
                        "{publish_message}; rollback {} -> {} failed: {rollback_error}; \
                         previous install retained at {}",
                        backup.display(),
                        dest.display(),
                        backup.display()
                    ));
                }
            }
        }
        return Err(publish_message);
    }

    sync_directory_best_effort(parent);
    if let Some(backup) = backup {
        if let Err(error) = remove_path(&backup) {
            tracing::warn!(
                path = %backup.display(),
                %error,
                "app install committed but old backup cleanup failed"
            );
        }
        sync_directory_best_effort(parent);
    }
    Ok(manifest)
}

fn recover_interrupted_app_install<R>(
    parent: &Path,
    dest: &Path,
    expected_id: &str,
    rename: &mut R,
) -> Result<(), String>
where
    R: FnMut(&Path, &Path) -> io::Result<()>,
{
    let mut backups = install_scratch_paths(parent, expected_id, "backup")
        .map_err(|e| format!("scan interrupted App install backups: {e}"))?;
    let staging = install_scratch_paths(parent, expected_id, "staging")
        .map_err(|e| format!("scan interrupted App install staging: {e}"))?;
    let destination_exists = path_entry_exists(dest)
        .map_err(|e| format!("inspect destination {}: {e}", dest.display()))?;

    if !destination_exists && backups.len() > 1 {
        let paths = backups
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "multiple interrupted App install backups found for `{expected_id}`; \
             refusing ambiguous recovery: {paths}"
        ));
    }

    if !destination_exists {
        if let Some(backup) = backups.pop() {
            validate_recovery_candidate(&backup, expected_id)?;
            rename(&backup, dest).map_err(|e| {
                format!(
                    "recover interrupted App install {} -> {}: {e}",
                    backup.display(),
                    dest.display()
                )
            })?;
            sync_directory_best_effort(parent);
        }
    }

    for path in backups.into_iter().chain(staging) {
        remove_path(&path)
            .map_err(|e| format!("clean interrupted App install {}: {e}", path.display()))?;
    }
    sync_directory_best_effort(parent);
    Ok(())
}

fn install_scratch_paths(parent: &Path, expected_id: &str, kind: &str) -> io::Result<Vec<PathBuf>> {
    let prefix = format!(".{expected_id}.install-{kind}-");
    let mut paths = Vec::new();
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(&prefix))
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

fn validate_recovery_candidate(path: &Path, expected_id: &str) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| format!("inspect interrupted App backup {}: {e}", path.display()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "interrupted App backup `{}` is not a directory",
            path.display()
        ));
    }
    let manifest_path = path.join("app.json");
    let body = fs::read_to_string(&manifest_path).map_err(|e| {
        format!(
            "read interrupted App backup {}: {e}",
            manifest_path.display()
        )
    })?;
    let manifest = apps::AppManifest::from_json(&body).map_err(|e| {
        format!(
            "parse interrupted App backup {}: {e}",
            manifest_path.display()
        )
    })?;
    if manifest.id != expected_id {
        return Err(format!(
            "interrupted App backup `{}` declares id `{}`, expected `{expected_id}`",
            path.display(),
            manifest.id
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn atomic_exchange(staging: &Path, dest: &Path) -> io::Result<bool> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let staging = CString::new(staging.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "App install staging path contains a NUL byte",
        )
    })?;
    let dest = CString::new(dest.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "App install destination path contains a NUL byte",
        )
    })?;
    // Both pointers remain valid NUL-terminated strings for the syscall.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            staging.as_ptr(),
            libc::AT_FDCWD,
            dest.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if result == 0 {
        return Ok(true);
    }

    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ENOSYS | libc::EINVAL | libc::EOPNOTSUPP | libc::EPERM) => Ok(false),
        _ => Err(error),
    }
}

#[cfg(not(target_os = "linux"))]
fn atomic_exchange(_staging: &Path, _dest: &Path) -> io::Result<bool> {
    Ok(false)
}

struct InstallStagingGuard(PathBuf);

impl Drop for InstallStagingGuard {
    fn drop(&mut self) {
        let _ = remove_path(&self.0);
    }
}

fn path_entry_exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn remove_path(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path)
        }
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn sync_install_tree(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to fsync symlink at `{}`", path.display()),
        ));
    }
    if metadata.is_file() {
        return fs::File::open(path)?.sync_all();
    }
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unsupported app install entry at `{}`", path.display()),
        ));
    }

    for entry in fs::read_dir(path)? {
        sync_install_tree(&entry?.path())?;
    }
    sync_directory(path)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn sync_directory_best_effort(path: &Path) {
    let _ = sync_directory(path);
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
pub(super) fn consent_cmd(
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
                let stored = consent::load(id)?;
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
            let stored = consent::load(app)?;
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
