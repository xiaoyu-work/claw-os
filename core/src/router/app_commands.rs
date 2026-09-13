//! App developer and management commands for the `cos app` namespace.

use std::env;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::{apps_dir, launch_app_gui, run_app_command, run_app_mcp_command};
use crate::apps::{self, installation};
use crate::cli_help::{show_app_command_schema, show_app_help, show_app_schema, show_apps};

/// Directory where freedesktop `.desktop` launchers are written at
/// `cos app install`. Overridable via `COS_APPLICATIONS_DIR` (used by
/// tests and per-user installs); defaults to the system location the
/// desktop shell's applibrary/launcher already scan.
fn applications_dir() -> PathBuf {
    PathBuf::from(
        env::var("COS_APPLICATIONS_DIR").unwrap_or_else(|_| "/usr/share/applications".into()),
    )
}

/// Dispatch to apps under the "cos app" namespace.
pub(super) fn dispatch_app(
    args: &[String],
    stdin_data: Option<Vec<u8>>,
) -> Result<Option<String>, String> {
    if args.first().map(String::as_str) == Some("stdio") {
        return stdio_schema(&args[1..]);
    }
    let apps_dir = apps_dir();
    // Listing includes quarantined installs on purpose: an operator has
    // to be able to see that an App exists and why it will not run.
    // `require_runnable` gates every path that executes App code.
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

    // Special: `cos app install <source>` — authenticate and review the
    // permission requests before publishing the App. AI consent is separate.
    // Lives in the `app` namespace because it is an admin operation against the
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
                require_runnable(app)?;
                let files: Vec<String> = args[2..].to_vec();
                return launch_app_gui(app_name, desktop.exec.as_str(), &files, app);
            }
        }
    }

    let command = &args[1];
    let cmd_args: Vec<String> = args[2..].to_vec();
    let app = &discovered[app_name.as_str()];

    // Only the option region may request schema. After `--`, the same token
    // is ordinary App data and must reach the manifest binder unchanged.
    // Schema introspection reads the manifest only and never runs App
    // code, so a quarantined App can still describe itself.
    if schema_requested(&cmd_args) {
        return show_app_command_schema(app_name, command, app);
    }

    // Select by declaration before execution. Ordinary operations retain
    // precedence; an independent MCP command never falls back to main.py.
    if apps::command_uses_mcp_cli(&app.manifest, command) {
        // Reject an ambiguous or non-matching command before launch.
        apps::mcp_tool_for_command(&app.manifest, command)?;
        require_runnable(app)?;
        return run_app_mcp_command(app_name, command, &cmd_args, app, stdin_data);
    }

    // Validate command exists
    if !app.manifest.operations.contains_key(command.as_str()) {
        let valid: Vec<&String> = app.manifest.operations.keys().collect();
        return Err(format!(
            "unknown command: cos app {app_name} {command}. available: {valid:?}"
        ));
    }

    require_runnable(app)?;
    run_app_command(app_name, command, &cmd_args, app, stdin_data)
}

pub(super) fn stdio_uses_process_streams(args: &[String]) -> bool {
    if args.first().map(String::as_str) != Some("app")
        || args.get(1).map(String::as_str) != Some("stdio")
    {
        return false;
    }
    let tail = &args[2..];
    !tail.is_empty()
        && !(tail.len() == 1 && matches!(tail[0].as_str(), "--help" | "-h" | "help" | "--schema"))
        && !(tail.len() >= 2 && schema_requested(&tail[2..]))
}

fn stdio_schema(args: &[String]) -> Result<Option<String>, String> {
    if args.is_empty()
        || (args.len() == 1 && matches!(args[0].as_str(), "--help" | "-h" | "help" | "--schema"))
    {
        return crate::cli_help::show_command_schema("app", "stdio");
    }
    if args.len() >= 2 && schema_requested(&args[2..]) {
        let app = apps::find_verified_fresh(&apps_dir(), &args[0])?;
        let operation = app
            .manifest
            .operations
            .get(&args[1])
            .ok_or_else(|| format!("App `{}` has no operation `{}`", args[0], args[1]))?;
        if !operation.stdin {
            return Err(format!(
                "App operation `{}` does not declare stdin input",
                args[1]
            ));
        }
        let mut schema = apps::operation_schema(operation);
        schema["command"] = json!(format!("cos app stdio {} {}", args[0], args[1]));
        schema["model_callable"] = json!(false);
        schema["output_format"] = json!("opaque");
        schema["entry"] = json!(app
            .manifest
            .entry
            .as_deref()
            .unwrap_or_else(|| app.manifest.runtime.default_entry()));
        return Ok(Some(schema.to_string()));
    }
    Err("opaque App stdio requires the standalone `cos app stdio` process frontend".into())
}

pub(super) fn run_stdio(args: &[String]) -> Result<(), String> {
    let (Some(app_id), Some(operation)) = (args.first(), args.get(1)) else {
        return Err("usage: cos app stdio <id> <operation> [args...]".into());
    };
    let app = apps::find_verified_fresh(&apps_dir(), app_id)?;
    require_runnable(&app)?;
    let launch = crate::bridge::AppLaunch::new(std::sync::Arc::clone(app.require_verified()?))?;
    let started = std::time::Instant::now();
    let result = crate::bridge::run_app_stdio(
        &launch,
        operation,
        &args[2..],
        &super::data_dir(),
        &apps_dir().to_string_lossy(),
    );
    crate::audit::log_entry(
        &super::audit_path(),
        app_id,
        operation,
        &args[2..],
        started,
        if result.is_ok() { "ok" } else { "error" },
        result.as_ref().err().map(String::as_str),
    );
    result
}

/// Refuse to run a quarantined App, and re-assert the verified snapshot
/// immediately before dispatch so a package replaced between discovery
/// and launch is caught.
fn require_runnable(app: &apps::App) -> Result<(), String> {
    let verified = app.require_verified()?;
    verified
        .assert_current(&crate::provenance::trust_store())
        .map_err(|e| {
            crate::errors::error(
                e.code(),
                &format!(
                    "App `{}` changed after verification and will not be launched: {e}",
                    app.manifest.id
                ),
            )
            .to_string()
        })?;
    crate::provenance::audit("provenance.app_launch", verified.audit_facts());
    Ok(())
}

pub(super) fn schema_requested(args: &[String]) -> bool {
    args.iter()
        .take_while(|arg| arg.as_str() != "--")
        .any(|arg| arg == "--schema")
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
        let violations = apps::lint::app_lint_violations(app);
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
                 and (b) ship every package-relative file referenced by their `mcp.entry` so the kernel agent can spawn \
                 the MCP server. Run `cos app tool list <app>` to inspect the declared tool surface."
            } else {
                "All apps route their AI calls through the kernel gate and ship every declared MCP entry."
            },
        })
        .to_string(),
    ))
}

/// `cos app tool <sub>` — discovery surface for App-defined MCP
/// tools (the strict-schema, agent-callable surface declared in each
/// manifest's `mcp` block).
///
/// Currently supports:
/// * `cos app tool list` — every MCP tool across every installed app.
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
                    .mcp
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
                    "has_mcp": app.manifest.mcp.is_some(),
                    "tools": tools_json,
                }));
            }
            Ok(Some(json!({"apps": apps_json}).to_string()))
        }
        "--help" | "-h" | "help" => Ok(Some(
            "cos app tool list [<app>]  list mcp-exposed tools".to_string(),
        )),
        other => Err(format!(
            "unknown subcommand: cos app tool {other}. try: cos app tool list [<app>]"
        )),
    }
}

/// `cos app install <source-dir> [--review] [--yes] [--no-consent] [--force]`
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
/// Permission disclosure happens after verification and before publication,
/// including for non-AI Apps, replacement installs and deferred AI consent.
/// `--review` returns the authenticated request without installing anything.
/// `--yes` can use existing OS confirmation, never replace first review or
/// grant AI consent. Developer trust remains a separate interactive decision.
pub(super) fn install_cmd(args: &[String]) -> Result<Option<String>, String> {
    install_cmd_with_confirmation(args, &mut |app, source, auto_yes, dev_trust| {
        if dev_trust {
            review_install_permissions(app, auto_yes).map(|review| (review, None))
        } else {
            super::system_review::confirm_install(app, source, auto_yes)
                .map(|(review, id)| (review, Some(id)))
        }
    })
}

type InstallConfirmation<'a> = dyn FnMut(
    &apps::App,
    &Path,
    bool,
    bool,
) -> Result<(apps::permission_review::PermissionReview, Option<String>), String> + 'a;

fn install_cmd_with_confirmation(
    args: &[String],
    confirm: &mut InstallConfirmation<'_>,
) -> Result<Option<String>, String> {
    let source_arg = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .ok_or_else(|| {
            "usage: cos app install <source-dir> [--review] [--yes] [--no-consent] [--force]"
                .to_string()
        })?;
    let review_only = args.iter().any(|a| a == "--review");
    let auto_yes = args.iter().any(|a| a == "--yes");
    let no_consent = args.iter().any(|a| a == "--no-consent");
    let force = args.iter().any(|a| a == "--force");
    // The only route that installs unsigned content. `record_dev_trust`
    // demands an interactive, phrase-matched confirmation on a real
    // terminal with no session active — `--yes` does not satisfy it,
    // and neither does anything the model can reach.
    let dev_trust = args.iter().any(|a| a == "--dev-trust");

    let source = PathBuf::from(&source_arg);
    let install = installation::DirectoryInstall::prepare(&source, &apps_dir())?;
    let dest = install.destination();
    let same_path = install.is_in_place();

    if review_only {
        let app = install.preview()?;
        let review = apps::permission_review::PermissionReview::from_manifest(&app.manifest)?;
        return Ok(Some(
            json!({
                "app": app.manifest.id,
                "installed": false,
                "review_only": true,
                "provenance": app.provenance_facts(),
                "permission_review": review,
            })
            .to_string(),
        ));
    }

    let mut permission_review = None;
    let mut system_review_id = None;
    let mut review_permissions = |candidate: &apps::App| {
        let (review, id) = confirm(candidate, &source, auto_yes, dev_trust)?;
        permission_review = Some(review);
        system_review_id = id;
        Ok(())
    };
    let manifest = install.publish(force, dev_trust, &mut review_permissions)?;
    let copied = !same_path;

    // Registration only happens after provenance succeeded, or after
    // the operator's explicit developer decision has been persisted.
    let provenance = if dev_trust {
        record_dev_trust(dest, &manifest.id)?
    } else {
        let verified = installation::verify_installed_app(dest, &manifest.id)?;
        crate::provenance::audit("provenance.app_installed", verified.audit_facts());
        verified.audit_facts()
    };

    let mut envelope = json!({
        "app": manifest.id,
        "installed": true,
        "source": source.display().to_string(),
        "dest": dest.display().to_string(),
        "copied": copied,
        "in_place": same_path,
        "provenance": provenance,
        "permission_review": permission_review
            .ok_or("App installation completed without a permission review")?,
        "system_review_id": system_review_id,
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

    if no_consent || auto_yes {
        envelope["consent"] = json!({
            "needed": true,
            "granted": false,
            "deferred": true,
            "reason": "installation_does_not_grant_ai_consent",
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

fn review_install_permissions(
    app: &apps::App,
    auto_yes: bool,
) -> Result<apps::permission_review::PermissionReview, String> {
    use std::io::{BufRead, IsTerminal, Write};

    let review = apps::permission_review::PermissionReview::from_manifest(&app.manifest)?;
    let mut stderr = std::io::stderr().lock();
    if let Some(reason) = app.quarantine_reason() {
        writeln!(
            stderr,
            "Unverified development source; separate developer trust is still required: {}",
            serde_json::to_string(reason)
                .map_err(|error| format!("format development trust warning: {error}"))?,
        )
        .map_err(|error| format!("display development trust warning: {error}"))?;
    } else {
        writeln!(stderr, "Verified package: {}", app.provenance_facts())
            .map_err(|error| format!("display App publisher identity: {error}"))?;
    }
    writeln!(stderr, "{}", review.format_for_review()?)
        .map_err(|error| format!("display App permission review: {error}"))?;
    stderr
        .flush()
        .map_err(|error| format!("flush App permission review: {error}"))?;
    if auto_yes {
        return Ok(review);
    }
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Err(
            "App installation requires permission review confirmation. Use --review to inspect \
             the authenticated request, or explicitly acknowledge installation with --yes. \
             Neither option grants the requested permissions."
                .to_string(),
        );
    }
    write!(
        stderr,
        "Install this App without granting its requested permissions? [y/N] "
    )
    .map_err(|error| format!("display App installation confirmation: {error}"))?;
    stderr
        .flush()
        .map_err(|error| format!("flush App installation confirmation: {error}"))?;
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|error| format!("read App installation confirmation: {error}"))?;
    if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        return Err(
            "App installation cancelled; the existing installation was not replaced".into(),
        );
    }
    Ok(review)
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
        out.push_str("    if gui.is_gui_launch():\n");
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

/// Persist an explicit developer trust decision for an unsigned App and
/// re-verify the installed tree through it.
///
/// The grant is written to the segregated per-user developer root, is
/// bound to the installed content digest, and is surfaced in the
/// install envelope so the decision stays visible.
fn record_dev_trust(dest: &Path, id: &str) -> Result<Value, String> {
    let dir = dest
        .canonicalize()
        .map_err(|e| format!("resolve {}: {e}", dest.display()))?;
    let args = vec![
        "--kind".to_string(),
        "app".to_string(),
        "--id".to_string(),
        id.to_string(),
        "--path".to_string(),
        dir.display().to_string(),
        "--note".to_string(),
        "cos app install --dev-trust".to_string(),
    ];
    // Routed through the same command an operator would run by hand, so
    // there is exactly one implementation of the consent gate.
    let grant = crate::provenance::cli::run("dev-trust", &args)?;
    let verified = installation::verify_installed_app(&dir, id)?;
    crate::provenance::audit("provenance.app_installed", verified.audit_facts());
    let mut facts = verified.audit_facts();
    if let Some(map) = facts.as_object_mut() {
        map.insert("developer_grant".to_string(), grant);
        map.insert(
            "warning".to_string(),
            json!(
                "Unsigned developer install: restricted capability ceiling, no privileged routes. \
                 Editing the installed tree invalidates the grant."
            ),
        );
    }
    Ok(facts)
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

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/router/app_commands.rs"
    ));
}
