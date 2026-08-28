mod app_commands;

use std::env;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value};

use crate::agent;
use crate::ai;
use crate::apps;
use crate::audit;
use crate::bridge;
use crate::caps;
use crate::checkpoint;
use crate::cli_help::{self, builtin_apps, show_help_for, show_overview};
use crate::clawd::routes::Command;
use crate::credential;
use crate::cron;
use crate::engine_pkg;
use crate::mem_bridge;
use crate::model;
use crate::perms;
use crate::service;
use crate::sysinfo;
use crate::triggers;
use app_commands::dispatch_app;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn apps_dir() -> PathBuf {
    PathBuf::from(env::var("COS_APPS_DIR").unwrap_or_else(|_| "/usr/lib/cos/apps".into()))
}

/// Return whether argv resolves to an installed manifest operation that opts
/// into caller stdin. Management, help, schema, and desktop routes never read
/// stdin eagerly.
pub fn app_operation_accepts_stdin(args: &[String]) -> bool {
    app_operation_accepts_stdin_in(args, &apps_dir())
}

fn app_operation_accepts_stdin_in(args: &[String], root: &Path) -> bool {
    if args.first().map(String::as_str) != Some("app") || args.len() < 3 {
        return false;
    }
    let app_id = &args[1];
    if matches!(
        app_id.as_str(),
        "lint" | "tool" | "install" | "create" | "consent"
    ) {
        return false;
    }
    if app_commands::schema_requested(&args[3..]) {
        return false;
    }
    apps::find(root, app_id)
        .and_then(|app| {
            app.manifest
                .operations
                .get(&args[2])
                .map(|operation| operation.stdin)
        })
        .unwrap_or(false)
}

fn data_dir() -> String {
    crate::paths::data_dir().to_string_lossy().into_owned()
}

fn run_cron(command: &str, args: &[String]) -> Result<Value, String> {
    run_scheduler_command("cron", command, args, cron::run)
}

fn run_triggers(command: &str, args: &[String]) -> Result<Value, String> {
    run_scheduler_command("triggers", command, args, triggers::run)
}

fn run_scheduler_command(
    subsystem: &str,
    command: &str,
    args: &[String],
    local: fn(&str, &[String]) -> Result<Value, String>,
) -> Result<Value, String> {
    if !should_proxy_scheduler_command() {
        return local(command, args);
    }
    let params = json!({
        "subsystem": subsystem,
        "command": command,
        "args": args,
    });
    match scheduler_request(&params) {
        Ok(result) => Ok(result),
        Err(error) => {
            let ids = scheduler_approval_requests(&error);
            if ids.is_empty() {
                return Err(error.message);
            }
            // Waiting in this process is what keeps the retry
            // authentic: clawd re-derives the same uid/pid/start-time
            // identity on the follow-up call, so no decision, token or
            // session string has to travel between processes.
            wait_for_scheduler_approvals(&ids)?;
            scheduler_request(&params).map_err(|error| error.message)
        }
    }
}

/// A failed `scheduler.run` plus whatever structured payload the daemon
/// attached for this caller only.
struct SchedulerCallError {
    message: String,
    data: Option<Value>,
}

fn scheduler_request(params: &Value) -> Result<Value, SchedulerCallError> {
    let request = crate::clawd::protocol::Request::build(Command::SchedulerRun, params.clone());
    let response =
        crate::clawd::client::request_blocking(crate::paths::clawd_socket_path(), request)
            .map_err(|message| SchedulerCallError {
                message,
                data: None,
            })?;
    if response.ok {
        response.result.ok_or_else(|| SchedulerCallError {
            message: "clawd scheduler response had no result".to_string(),
            data: None,
        })
    } else {
        let (message, data) = match response.error {
            Some(error) => (error.message, error.data),
            None => ("clawd scheduler request failed".to_string(), None),
        };
        Err(SchedulerCallError { message, data })
    }
}

/// Approval request ids a denied scheduler command is waiting on, if
/// the daemon reported any. Ids are not authority — they only say which
/// decisions this caller needs.
fn scheduler_approval_requests(error: &SchedulerCallError) -> Vec<String> {
    error
        .data
        .as_ref()
        .filter(|data| data.get("status").and_then(Value::as_str) == Some("approval_required"))
        .and_then(|data| data.get("approval_requests"))
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Longest a scheduler command holds its place while the user decides.
const SCHEDULER_APPROVAL_WAIT: std::time::Duration = std::time::Duration::from_secs(120);
const SCHEDULER_APPROVAL_POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// Block until every listed request is decided. The wait is bounded,
/// ends immediately on a rejection, and reports a terminal error for
/// anything that is not a clean approval.
fn wait_for_scheduler_approvals(ids: &[String]) -> Result<(), String> {
    let deadline = Instant::now() + SCHEDULER_APPROVAL_WAIT;
    loop {
        let result = request_clawd(Command::PermissionStatus, json!({ "ids": ids }))?;
        let statuses = result
            .get("statuses")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut pending = false;
        for entry in &statuses {
            let id = entry.get("id").and_then(Value::as_str).unwrap_or("");
            match entry.get("status").and_then(Value::as_str) {
                Some("approved") => {}
                Some("pending" | "resolving") => pending = true,
                Some("denied") => return Err(format!("scheduler approval {id} was denied")),
                other => {
                    return Err(format!(
                        "scheduler approval {id} is no longer available ({})",
                        other.unwrap_or("unknown")
                    ))
                }
            }
        }
        if !pending {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out after {}s waiting for scheduler approval",
                SCHEDULER_APPROVAL_WAIT.as_secs()
            ));
        }
        std::thread::sleep(SCHEDULER_APPROVAL_POLL);
    }
}

fn request_clawd(command: Command, params: Value) -> Result<Value, String> {
    let request = crate::clawd::protocol::Request::build(command, params);
    let response =
        crate::clawd::client::request_blocking(crate::paths::clawd_socket_path(), request)?;
    if response.ok {
        response
            .result
            .ok_or_else(|| format!("clawd {command} response had no result"))
    } else {
        Err(response
            .error
            .map(|error| error.message)
            .unwrap_or_else(|| format!("clawd {command} request failed")))
    }
}

fn should_proxy_scheduler_command() -> bool {
    #[cfg(unix)]
    {
        crate::paths::current_owner_uid_override().is_none()
            && unsafe { libc::geteuid() } != 0
            && !crate::caps::enforcement::process_has_no_new_privs()
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn audit_path() -> PathBuf {
    Path::new(&data_dir()).join("logs").join("audit.jsonl")
}

/// Main dispatch: parse CLI args and route to the appropriate handler.
pub fn dispatch(args: &[String]) -> Result<Option<String>, String> {
    dispatch_with_stdin(args, None)
}

/// Dispatch a top-level CLI invocation with explicitly supplied stdin bytes.
///
/// Internal callers use [`dispatch`] and therefore cannot accidentally pass a
/// control pipe or service stdin through to an App.
pub fn dispatch_with_stdin(
    args: &[String],
    stdin_data: Option<Vec<u8>>,
) -> Result<Option<String>, String> {
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
            return Ok(Some(json!({"name": "cos", "version": VERSION}).to_string()));
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

    // Hidden bridge for apps that want to push searchable summaries
    // into the agent's memory. The user-facing inspect/forget surface
    // lives at `cos agent memory`. Kept off the public namespace so
    // we can evolve the schema without an SDK rev.
    if name == "__memory" {
        let command = args
            .get(1)
            .ok_or_else(|| "internal memory command required".to_string())?;
        let value = mem_bridge::run(command, &args[2..])?;
        return Ok(Some(value.to_string()));
    }

    if name == "__package" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal package command required".to_string())?;
        let package = args.get(2).cloned();
        let version = args.get(3).cloned();
        if let Some(package) = package.as_deref() {
            crate::clawd::packages::validate_package_name(package)?;
        }
        if let Some(version) = version.as_deref() {
            crate::clawd::packages::validate_version(version)?;
        }
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal package install requires COS_SESSION".to_string())?;
        let value = request_clawd(
            Command::SystemPackageControl,
            json!({
                "session": session,
                "action": action,
                "package": package,
                "version": version,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__systemd" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal systemd command required".to_string())?;
        let unit = args
            .get(2)
            .ok_or_else(|| "internal systemd command requires a unit".to_string())?;
        crate::clawd::systemd::validate_unit_name(unit)?;
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal systemd command requires COS_SESSION".to_string())?;
        let value = request_clawd(
            Command::SystemServiceControl,
            json!({
                "session": session,
                "action": action,
                "unit": unit,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__snapshot" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal snapshot command required".to_string())?;
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal snapshot command requires COS_SESSION".to_string())?;
        let value = request_clawd(
            Command::SystemSnapshotControl,
            json!({
                "session": session,
                "action": action,
                "id": args.get(2),
                "description": args.get(2),
                "confirm": args.get(3).is_some_and(|value| value == "--confirm"),
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__network" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal network command required".to_string())?;
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal network command requires COS_SESSION".to_string())?;
        let mut target = None;
        let mut state = None;
        let mut credential = None;
        let mut index = 2;
        while index < args.len() {
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("{} requires a value", args[index]))?
                .clone();
            match args[index].as_str() {
                "--target" => target = Some(value),
                "--state" => state = Some(value),
                "--credential" => credential = Some(value),
                other => return Err(format!("unknown internal network flag: {other}")),
            }
            index += 2;
        }
        let value = request_clawd(
            Command::SystemNetworkControl,
            json!({
                "session": session,
                "action": action,
                "target": target,
                "state": state,
                "credential": credential,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__crash" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal crash command required".to_string())?;
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal crash command requires COS_SESSION".to_string())?;
        let mut since_minutes = None;
        let mut limit = None;
        let mut id = None;
        let mut index = 2;
        while index < args.len() {
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("{} requires a value", args[index]))?
                .clone();
            match args[index].as_str() {
                "--since-minutes" => since_minutes = Some(value),
                "--limit" => limit = Some(value),
                "--id" => id = Some(value),
                other => return Err(format!("unknown internal crash flag: {other}")),
            }
            index += 2;
        }
        let value = request_clawd(
            Command::SystemCrashInspect,
            json!({
                "session": session,
                "action": action,
                "since_minutes": since_minutes,
                "limit": limit,
                "id": id,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__storage" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal storage command required".to_string())?;
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal storage command requires COS_SESSION".to_string())?;
        let mut device = None;
        let mut index = 2;
        while index < args.len() {
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("{} requires a value", args[index]))?
                .clone();
            match args[index].as_str() {
                "--device" => device = Some(value),
                other => return Err(format!("unknown internal storage flag: {other}")),
            }
            index += 2;
        }
        let value = request_clawd(
            Command::SystemStorageControl,
            json!({
                "session": session,
                "action": action,
                "device": device,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__audio" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal audio command required".to_string())?;
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal audio command requires COS_SESSION".to_string())?;
        let mut target = None;
        let mut value_arg = None;
        let mut index = 2;
        while index < args.len() {
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("{} requires a value", args[index]))?
                .clone();
            match args[index].as_str() {
                "--target" => target = Some(value),
                "--value" => value_arg = Some(value),
                other => return Err(format!("unknown internal audio flag: {other}")),
            }
            index += 2;
        }
        let value = request_clawd(
            Command::SystemAudioControl,
            json!({
                "session": session,
                "action": action,
                "target": target,
                "value": value_arg,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__desktop" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal desktop command required".to_string())?;
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal desktop command requires COS_SESSION".to_string())?;
        let mut identifier = None;
        let mut app_id = None;
        let mut index = 2;
        while index < args.len() {
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("{} requires a value", args[index]))?
                .clone();
            match args[index].as_str() {
                "--identifier" => identifier = Some(value),
                "--app-id" => app_id = Some(value),
                other => return Err(format!("unknown internal desktop flag: {other}")),
            }
            index += 2;
        }
        let value = request_clawd(
            Command::SystemDesktopControl,
            json!({
                "session": session,
                "action": action,
                "identifier": identifier,
                "app_id": app_id,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__bluetooth" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal Bluetooth command required".to_string())?;
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal Bluetooth command requires COS_SESSION".to_string())?;
        let mut adapter = None;
        let mut device = None;
        let mut state = None;
        let mut seconds = None;
        let mut pairing_id = None;
        let mut response = None;
        let mut index = 2;
        while index < args.len() {
            if args[index] == "--response-stdin" {
                let mut input = String::new();
                let stdin = std::io::stdin();
                let mut input_reader = stdin.lock().take(65);
                input_reader
                    .read_to_string(&mut input)
                    .map_err(|error| format!("read Bluetooth response from stdin: {error}"))?;
                if input.len() > 64 {
                    return Err("Bluetooth response exceeds 64 bytes".to_string());
                }
                let input = input.trim_end_matches(['\r', '\n']);
                if input.is_empty() {
                    return Err("Bluetooth response from stdin is empty".to_string());
                }
                response = Some(input.to_string());
                index += 1;
                continue;
            }
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("{} requires a value", args[index]))?
                .clone();
            match args[index].as_str() {
                "--adapter" => adapter = Some(value),
                "--device" => device = Some(value),
                "--state" => state = Some(value),
                "--seconds" => seconds = Some(value),
                "--pairing-id" => pairing_id = Some(value),
                other => return Err(format!("unknown internal Bluetooth flag: {other}")),
            }
            index += 2;
        }
        let value = request_clawd(
            Command::SystemBluetoothControl,
            json!({
                "session": session,
                "action": action,
                "adapter": adapter,
                "device": device,
                "state": state,
                "seconds": seconds,
                "pairing_id": pairing_id,
                "response": response,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__power" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal power command required".to_string())?;
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal power command requires COS_SESSION".to_string())?;
        let mut confirm = false;
        for flag in &args[2..] {
            match flag.as_str() {
                "--confirm" if !confirm => confirm = true,
                other => return Err(format!("unknown internal power flag: {other}")),
            }
        }
        let value = request_clawd(
            Command::SystemPowerControl,
            json!({
                "session": session,
                "action": action,
                "confirm": confirm,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__hardware" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal hardware command required".to_string())?;
        if args.len() != 2 {
            return Err("internal hardware commands do not accept arguments".to_string());
        }
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal hardware command requires COS_SESSION".to_string())?;
        let value = request_clawd(
            Command::SystemHardwareInspect,
            json!({
                "session": session,
                "action": action,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__security" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal security command required".to_string())?;
        if args.len() != 2 {
            return Err("internal security commands do not accept arguments".to_string());
        }
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal security command requires COS_SESSION".to_string())?;
        let value = request_clawd(
            Command::SystemSecurityInspect,
            json!({
                "session": session,
                "action": action,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__container" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal container command required".to_string())?;
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal container command requires COS_SESSION".to_string())?;
        let mut runtime = None;
        let mut target = None;
        let mut namespace = None;
        let mut signal = None;
        let mut lines = None;
        let mut confirm = false;
        let mut index = 2;
        while index < args.len() {
            if args[index] == "--confirm" {
                if confirm {
                    return Err("duplicate internal container --confirm".to_string());
                }
                confirm = true;
                index += 1;
                continue;
            }
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("{} requires a value", args[index]))?
                .clone();
            match args[index].as_str() {
                "--runtime" => runtime = Some(value),
                "--target" => target = Some(value),
                "--namespace" => namespace = Some(value),
                "--signal" => signal = Some(value),
                "--lines" => lines = Some(value),
                other => return Err(format!("unknown internal container flag: {other}")),
            }
            index += 2;
        }
        let value = request_clawd(
            Command::SystemContainerControl,
            json!({
                "session": session,
                "action": action,
                "runtime": runtime,
                "target": target,
                "namespace": namespace,
                "signal": signal,
                "lines": lines,
                "confirm": confirm,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__config" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal config command required".to_string())?;
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal config command requires COS_SESSION".to_string())?;
        let mut target = None;
        let mut source = None;
        let mut token = None;
        let mut confirm = false;
        let mut index = 2;
        while index < args.len() {
            if args[index] == "--confirm" {
                if confirm {
                    return Err("duplicate internal config --confirm".to_string());
                }
                confirm = true;
                index += 1;
                continue;
            }
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("{} requires a value", args[index]))?
                .clone();
            match args[index].as_str() {
                "--target" => target = Some(value),
                "--source" => source = Some(value),
                "--token" => token = Some(value),
                other => return Err(format!("unknown internal config flag: {other}")),
            }
            index += 2;
        }
        let value = request_clawd(
            Command::SystemConfigControl,
            json!({
                "session": session,
                "action": action,
                "target": target,
                "source": source,
                "token": token,
                "confirm": confirm,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__events" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal event command required".to_string())?;
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal event command requires COS_SESSION".to_string())?;
        let mut source = None;
        let mut limit = None;
        let mut pid = None;
        let mut index = 2;
        while index < args.len() {
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("{} requires a value", args[index]))?
                .clone();
            match args[index].as_str() {
                "--source" => source = Some(value),
                "--limit" => limit = Some(value),
                "--pid" => pid = Some(value),
                other => return Err(format!("unknown internal event flag: {other}")),
            }
            index += 2;
        }
        let value = request_clawd(
            Command::SystemEventsControl,
            json!({
                "session": session,
                "action": action,
                "source": source,
                "limit": limit,
                "pid": pid,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__backup" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal backup command required".to_string())?;
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal backup command requires COS_SESSION".to_string())?;
        let mut repo = None;
        let mut source = None;
        let mut destination = None;
        let mut snapshot = None;
        let mut credential = None;
        let mut tag = None;
        let mut keep_daily = None;
        let mut keep_weekly = None;
        let mut keep_monthly = None;
        let mut confirm = false;
        let mut index = 2;
        while index < args.len() {
            if args[index] == "--confirm" {
                if confirm {
                    return Err("duplicate internal backup --confirm".to_string());
                }
                confirm = true;
                index += 1;
                continue;
            }
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("{} requires a value", args[index]))?
                .clone();
            match args[index].as_str() {
                "--repo" => repo = Some(value),
                "--source" => source = Some(value),
                "--destination" => destination = Some(value),
                "--snapshot" => snapshot = Some(value),
                "--credential" => credential = Some(value),
                "--tag" => tag = Some(value),
                "--keep-daily" => keep_daily = Some(value),
                "--keep-weekly" => keep_weekly = Some(value),
                "--keep-monthly" => keep_monthly = Some(value),
                other => return Err(format!("unknown internal backup flag: {other}")),
            }
            index += 2;
        }
        let value = request_clawd(
            Command::SystemBackupControl,
            json!({
                "session": session,
                "action": action,
                "repo": repo,
                "source": source,
                "destination": destination,
                "snapshot": snapshot,
                "credential": credential,
                "tag": tag,
                "keep_daily": keep_daily,
                "keep_weekly": keep_weekly,
                "keep_monthly": keep_monthly,
                "confirm": confirm,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__firewall" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal firewall command required".to_string())?;
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal firewall command requires COS_SESSION".to_string())?;
        let mut rule_action = None;
        let mut direction = None;
        let mut protocol = None;
        let mut port = None;
        let mut remote = None;
        let mut interface = None;
        let mut rule_id = None;
        let mut token = None;
        let mut confirm = false;
        let mut index = 2;
        while index < args.len() {
            if args[index] == "--confirm" {
                if confirm {
                    return Err("duplicate internal firewall --confirm".to_string());
                }
                confirm = true;
                index += 1;
                continue;
            }
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("{} requires a value", args[index]))?
                .clone();
            match args[index].as_str() {
                "--rule-action" => rule_action = Some(value),
                "--direction" => direction = Some(value),
                "--protocol" => protocol = Some(value),
                "--port" => port = Some(value),
                "--remote" => remote = Some(value),
                "--interface" => interface = Some(value),
                "--rule-id" => rule_id = Some(value),
                "--token" => token = Some(value),
                other => return Err(format!("unknown internal firewall flag: {other}")),
            }
            index += 2;
        }
        let value = request_clawd(
            Command::SystemFirewallControl,
            json!({
                "session": session,
                "action": action,
                "rule_action": rule_action,
                "direction": direction,
                "protocol": protocol,
                "port": port,
                "remote": remote,
                "interface": interface,
                "rule_id": rule_id,
                "token": token,
                "confirm": confirm,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__users" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal user command required".to_string())?;
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal user command requires COS_SESSION".to_string())?;
        let mut user = None;
        let mut group = None;
        let mut full_name = None;
        let mut shell = None;
        let mut groups = None;
        let mut credential = None;
        let mut token = None;
        let mut confirm = false;
        let mut index = 2;
        while index < args.len() {
            if args[index] == "--confirm" {
                if confirm {
                    return Err("duplicate internal user --confirm".to_string());
                }
                confirm = true;
                index += 1;
                continue;
            }
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("{} requires a value", args[index]))?
                .clone();
            match args[index].as_str() {
                "--user" => user = Some(value),
                "--group" => group = Some(value),
                "--full-name" => full_name = Some(value),
                "--shell" => shell = Some(value),
                "--groups" => groups = Some(value),
                "--credential" => credential = Some(value),
                "--token" => token = Some(value),
                other => return Err(format!("unknown internal user flag: {other}")),
            }
            index += 2;
        }
        let value = request_clawd(
            Command::SystemUsersControl,
            json!({
                "session": session,
                "action": action,
                "user": user,
                "group": group,
                "full_name": full_name,
                "shell": shell,
                "groups": groups,
                "credential": credential,
                "token": token,
                "confirm": confirm,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__printer" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal printer command required".to_string())?;
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal printer command requires COS_SESSION".to_string())?;
        let mut printer = None;
        let mut source = None;
        let mut job_id = None;
        let mut title = None;
        let mut media = None;
        let mut sides = None;
        let mut copies = None;
        let mut confirm = false;
        let mut index = 2;
        while index < args.len() {
            if args[index] == "--confirm" {
                if confirm {
                    return Err("duplicate internal printer --confirm".to_string());
                }
                confirm = true;
                index += 1;
                continue;
            }
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("{} requires a value", args[index]))?
                .clone();
            match args[index].as_str() {
                "--printer" => printer = Some(value),
                "--source" => source = Some(value),
                "--job-id" => job_id = Some(value),
                "--title" => title = Some(value),
                "--media" => media = Some(value),
                "--sides" => sides = Some(value),
                "--copies" => copies = Some(value),
                other => return Err(format!("unknown internal printer flag: {other}")),
            }
            index += 2;
        }
        let value = request_clawd(
            Command::SystemPrinterControl,
            json!({
                "session": session,
                "action": action,
                "printer": printer,
                "source": source,
                "job_id": job_id,
                "title": title,
                "media": media,
                "sides": sides,
                "copies": copies,
                "confirm": confirm,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__clipboard" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal clipboard command required".to_string())?;
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal clipboard command requires COS_SESSION".to_string())?;
        let mut mime = None;
        let mut source = None;
        let mut primary = false;
        let mut confirm = false;
        let mut index = 2;
        while index < args.len() {
            match args[index].as_str() {
                "--primary" if !primary => {
                    primary = true;
                    index += 1;
                }
                "--confirm" if !confirm => {
                    confirm = true;
                    index += 1;
                }
                "--mime" | "--source" => {
                    let value = args
                        .get(index + 1)
                        .ok_or_else(|| format!("{} requires a value", args[index]))?
                        .clone();
                    if args[index] == "--mime" {
                        mime = Some(value);
                    } else {
                        source = Some(value);
                    }
                    index += 2;
                }
                other => return Err(format!("unknown internal clipboard flag: {other}")),
            }
        }
        let value = request_clawd(
            Command::SystemClipboardControl,
            json!({
                "session": session,
                "action": action,
                "mime": mime,
                "source": source,
                "primary": primary,
                "confirm": confirm,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__camera" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal camera command required".to_string())?;
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal camera command requires COS_SESSION".to_string())?;
        let mut node_id = None;
        let mut expected_serial = None;
        let mut destination = None;
        let mut format = None;
        let mut width = None;
        let mut height = None;
        let mut index = 2;
        while index < args.len() {
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("{} requires a value", args[index]))?
                .clone();
            match args[index].as_str() {
                "--node-id" => node_id = Some(value),
                "--expected-serial" => expected_serial = Some(value),
                "--destination" => destination = Some(value),
                "--format" => format = Some(value),
                "--width" => width = Some(value),
                "--height" => height = Some(value),
                other => return Err(format!("unknown internal camera flag: {other}")),
            }
            index += 2;
        }
        let value = request_clawd(
            Command::SystemCameraControl,
            json!({
                "session": session,
                "action": action,
                "node_id": node_id,
                "expected_serial": expected_serial,
                "destination": destination,
                "format": format,
                "width": width,
                "height": height,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__accessibility" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal accessibility command required".to_string())?;
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal accessibility command requires COS_SESSION".to_string())?;
        let mut value_arg = None;
        let mut index = 2;
        while index < args.len() {
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("{} requires a value", args[index]))?
                .clone();
            match args[index].as_str() {
                "--value" => value_arg = Some(value),
                other => return Err(format!("unknown internal accessibility flag: {other}")),
            }
            index += 2;
        }
        let value = request_clawd(
            Command::SystemAccessibilityControl,
            json!({
                "session": session,
                "action": action,
                "value": value_arg,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__display" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal display command required".to_string())?;
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal display command requires COS_SESSION".to_string())?;
        let mut output = None;
        let mut from = None;
        let mut width = None;
        let mut height = None;
        let mut refresh = None;
        let mut scale = None;
        let mut x = None;
        let mut y = None;
        let mut transform = None;
        let mut adaptive_sync = None;
        let mut source = None;
        let mut backlight = None;
        let mut percent = None;
        let mut token = None;
        let mut confirm = false;
        let mut index = 2;
        while index < args.len() {
            if args[index] == "--confirm" {
                if confirm {
                    return Err("duplicate internal display --confirm".to_string());
                }
                confirm = true;
                index += 1;
                continue;
            }
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("{} requires a value", args[index]))?
                .clone();
            match args[index].as_str() {
                "--output" => output = Some(value),
                "--from" => from = Some(value),
                "--width" => width = Some(value),
                "--height" => height = Some(value),
                "--refresh" => refresh = Some(value),
                "--scale" => scale = Some(value),
                "--x" => x = Some(value),
                "--y" => y = Some(value),
                "--transform" => transform = Some(value),
                "--adaptive-sync" => adaptive_sync = Some(value),
                "--source" => source = Some(value),
                "--backlight" => backlight = Some(value),
                "--percent" => percent = Some(value),
                "--token" => token = Some(value),
                other => return Err(format!("unknown internal display flag: {other}")),
            }
            index += 2;
        }
        let value = request_clawd(
            Command::SystemDisplayControl,
            json!({
                "session": session,
                "action": action,
                "output": output,
                "from": from,
                "width": width,
                "height": height,
                "refresh": refresh,
                "scale": scale,
                "x": x,
                "y": y,
                "transform": transform,
                "adaptive_sync": adaptive_sync,
                "source": source,
                "backlight": backlight,
                "percent": percent,
                "token": token,
                "confirm": confirm,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__usb" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal USB command required".to_string())?;
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal USB command requires COS_SESSION".to_string())?;
        let mut device = None;
        let mut state = None;
        let mut rule_id = None;
        let mut token = None;
        let mut confirm = false;
        let mut index = 2;
        while index < args.len() {
            if args[index] == "--confirm" {
                if confirm {
                    return Err("duplicate internal USB --confirm".to_string());
                }
                confirm = true;
                index += 1;
                continue;
            }
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("{} requires a value", args[index]))?
                .clone();
            match args[index].as_str() {
                "--device" => device = Some(value),
                "--state" => state = Some(value),
                "--rule-id" => rule_id = Some(value),
                "--token" => token = Some(value),
                other => return Err(format!("unknown internal USB flag: {other}")),
            }
            index += 2;
        }
        let value = request_clawd(
            Command::SystemUsbControl,
            json!({
                "session": session,
                "action": action,
                "device": device,
                "state": state,
                "rule_id": rule_id,
                "token": token,
                "confirm": confirm,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    if name == "__location" {
        let action = args
            .get(1)
            .ok_or_else(|| "internal location command required".to_string())?;
        let session = env::var("COS_SESSION")
            .map_err(|_| "internal location command requires COS_SESSION".to_string())?;
        let mut accuracy = None;
        let mut index = 2;
        while index < args.len() {
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("{} requires a value", args[index]))?
                .clone();
            match args[index].as_str() {
                "--accuracy" if accuracy.is_none() => accuracy = Some(value),
                "--accuracy" => return Err("duplicate internal location --accuracy".to_string()),
                other => return Err(format!("unknown internal location flag: {other}")),
            }
            index += 2;
        }
        let value = request_clawd(
            Command::SystemLocationQuery,
            json!({
                "session": session,
                "action": action,
                "accuracy": accuracy,
            }),
        )?;
        return Ok(Some(value.to_string()));
    }

    // "app" namespace → route to declared app runtimes
    if name == "app" {
        return dispatch_app(&args[1..], stdin_data);
    }

    // Built-in OS primitives
    match name.as_str() {
        "sys" => dispatch_builtin(args, "sys", sysinfo::run),
        "service" => dispatch_builtin(args, "service", service::run),
        "checkpoint" => dispatch_builtin(args, "checkpoint", checkpoint::run),
        "credential" => dispatch_builtin(args, "credential", credential::run),
        "cron" => dispatch_builtin(args, "cron", run_cron),
        "triggers" => dispatch_builtin(args, "triggers", run_triggers),
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

fn run_app_command(
    app_name: &str,
    command: &str,
    args: &[String],
    app: &apps::App,
    stdin_data: Option<Vec<u8>>,
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
        if let Err(denial) = caps::require(caps::Verb::AGENT_INVOKE, caps::Scope::name(app_name)) {
            return Err(denial.summary());
        }
    }

    let result =
        bridge::run_app_with_stdin(&app.dir, command, args, &data, &apps, stdin_data);

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

/// `cos app <name> <desktop.exec> [files...]` — launch an app's desktop
/// GUI surface. The GUI itself is "World A" (the app draws its own
/// window in any toolkit), so this does **not** require `agent.invoke`;
/// the kernel only interposes when the app later exercises a declared
/// capability verb. We still audit the launch and route through the
/// bridge so the process is kernel-spawned with `COS_APP_ID` set.
fn launch_app_gui(
    app_name: &str,
    exec: &str,
    files: &[String],
    app: &apps::App,
) -> Result<Option<String>, String> {
    let start = Instant::now();
    let audit = audit_path();
    let data = data_dir();
    let apps = apps_dir().to_string_lossy().to_string();

    match bridge::launch_gui(&app.dir, exec, files, &data, &apps) {
        Ok(()) => {
            audit::log_entry(&audit, app_name, exec, files, start, "ok", None);
            Ok(None)
        }
        Err(e) => {
            audit::log_entry(&audit, app_name, exec, files, start, "error", Some(&e));
            Err(e)
        }
    }
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

/// Special-case dispatcher for `cos agent` that turns a bare
/// invocation (no subcommand) on an interactive TTY into either
/// `setup` (when the agent has not been configured yet) or `chat`
/// (when it has). Falls through to the standard help-table behavior
/// for non-TTY callers — scripts piping `cos agent | jq` still see
/// the machine-readable command list — and for explicit `--help`.
fn dispatch_agent(args: &[String]) -> Result<Option<String>, String> {
    // Explicit help should not be hijacked.
    let explicit_help =
        args.len() >= 2 && matches!(args[1].as_str(), "--help" | "-h" | "help" | "--schema");
    if !explicit_help && args.len() == 1 {
        use std::io::IsTerminal;
        let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
        if interactive {
            let config = crate::config::current_snapshot();
            let cfg = &config.agent;
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
        let output = crate::cli_catalog::namespace_help(app_name)
            .ok_or_else(|| format!("no public help catalogue for: cos {app_name}"))?;
        return Ok(Some(output.to_string()));
    }

    // cos <primitive> --schema → show all command schemas for this primitive
    if args.len() == 2 && args[1] == "--schema" {
        return cli_help::show_builtin_schema(app_name);
    }

    let command = &args[1];
    let cmd_args: Vec<String> = args[2..].to_vec();

    // If --schema is in args, return schema instead of executing
    if cmd_args.contains(&"--schema".to_string()) {
        return cli_help::show_command_schema(app_name, command);
    }

    let start = std::time::Instant::now();
    let audit_p = audit_path();

    let result = handler(command, &cmd_args);

    match &result {
        Ok(v) => {
            audit::log_entry(&audit_p, app_name, command, &cmd_args, start, "ok", None);
            // A handler that has already written its human-facing output
            // to stdout (e.g. `cos agent ask` printing the plain-text
            // answer) signals "nothing more to render" by returning
            // `Value::Null`. Without this special case the dispatcher
            // would print a stray `null` line after the answer. No
            // existing CLI command currently returns Value::Null as its
            // top-level result, so this is safe to apply uniformly.
            if v.is_null() {
                Ok(None)
            } else {
                Ok(Some(v.to_string()))
            }
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
            // Same shape as `dispatch_app`: failures stay failures
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
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/router.rs"));
}
