use std::env;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::json;

use crate::apps;
use crate::audit;
use crate::bridge;
use crate::sysinfo;

const VERSION: &str = "0.3.0";

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
        return show_apps();
    }

    let app_name = &args[0];
    let apps_dir = apps_dir();
    let discovered = apps::discover(&apps_dir);

    // Check if it's a known app
    if !discovered.contains_key(app_name.as_str()) {
        // Check built-in apps
        if app_name == "sys" {
            return dispatch_builtin_sys(args);
        }
        let names: Vec<&String> = discovered.keys().collect();
        return Err(format!(
            "unknown app: {app_name}. installed: {names:?}"
        ));
    }

    // One arg: show app help
    if args.len() == 1 {
        return show_app_help(app_name, &discovered[app_name.as_str()]);
    }

    let command = &args[1];
    let cmd_args: Vec<String> = args[2..].to_vec();
    let app = &discovered[app_name.as_str()];

    // Validate command exists
    if !app.manifest.commands.contains_key(command.as_str()) {
        let valid: Vec<&String> = app.manifest.commands.keys().collect();
        return Err(format!(
            "unknown command: {app_name} {command}. available: {valid:?}"
        ));
    }

    run_app_command(app_name, command, &cmd_args, app)
}

fn show_apps() -> Result<Option<String>, String> {
    let apps_dir = apps_dir();
    let discovered = apps::discover(&apps_dir);

    let mut app_list = Vec::new();
    for (name, app) in &discovered {
        app_list.push(json!({
            "name": name,
            "description": app.manifest.description,
            "commands": app.manifest.commands.keys().collect::<Vec<_>>(),
        }));
    }
    // Always include built-in sys
    app_list.push(json!({
        "name": "sys",
        "description": "System information — hardware, OS, environment, resources",
        "commands": ["info", "env", "resources", "uptime"],
    }));

    let output = json!({
        "name": "cos",
        "version": VERSION,
        "apps": app_list,
        "hint": "Run: cos <app>",
    });
    Ok(Some(output.to_string()))
}

fn show_app_help(name: &str, app: &apps::App) -> Result<Option<String>, String> {
    let output = json!({
        "app": name,
        "version": app.manifest.version,
        "description": app.manifest.description,
        "commands": app.manifest.commands,
        "hint": format!("Run: cos {name} <command> [args]"),
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

    let result = bridge::run_python_app(&app.dir, command, args, &data, &apps);

    match &result {
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
        }
        Err(e) => {
            audit::log_entry(&audit, app_name, command, args, start, "error", Some(e));
        }
    }

    result
}

fn dispatch_builtin_sys(args: &[String]) -> Result<Option<String>, String> {
    if args.len() == 1 {
        let output = json!({
            "app": "sys",
            "version": "0.1.0",
            "description": "System information — hardware, OS, environment, resources",
            "commands": {
                "info": "Show system information",
                "env": "Show environment variables",
                "resources": "Show disk, memory, and CPU usage",
                "uptime": "Show system uptime"
            },
            "hint": "Run: cos sys <command>",
        });
        return Ok(Some(output.to_string()));
    }

    let command = &args[1];
    let cmd_args: Vec<String> = args[2..].to_vec();
    let start = Instant::now();
    let audit = audit_path();

    let result = sysinfo::run(command, &cmd_args);

    match &result {
        Ok(v) => {
            audit::log_entry(&audit, "sys", command, &cmd_args, start, "ok", None);
            Ok(Some(v.to_string()))
        }
        Err(e) => {
            audit::log_entry(&audit, "sys", command, &cmd_args, start, "error", Some(e));
            Err(e.clone())
        }
    }
}
