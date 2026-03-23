use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};
use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;

use crate::apps;
use crate::bridge;
use crate::sysinfo;

const VERSION: &str = "0.3.0";

fn apps_dir() -> PathBuf {
    PathBuf::from(env::var("COS_APPS_DIR").unwrap_or_else(|_| "/usr/lib/cos/apps".into()))
}

fn data_dir() -> String {
    env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into())
}

/// Start the HTTP API server.
pub async fn serve(host: &str, port: u16) -> Result<(), String> {
    let app = Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/apps", get(list_apps))
        .route("/api/v1/{app}/{command}", post(run_command));

    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .map_err(|e| format!("invalid address: {e}"))?;

    eprintln!("[cos-api] v{VERSION} listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("bind failed: {e}"))?;

    axum::serve(listener, app)
        .await
        .map_err(|e| format!("server error: {e}"))
}

async fn health() -> Json<Value> {
    Json(json!({"status": "ok"}))
}

async fn list_apps() -> Json<Value> {
    let discovered = apps::discover(&apps_dir());
    let mut app_list: Vec<Value> = discovered
        .iter()
        .map(|(name, app)| {
            json!({
                "name": name,
                "description": app.manifest.description,
                "commands": app.manifest.commands.keys().collect::<Vec<_>>(),
            })
        })
        .collect();

    app_list.push(json!({
        "name": "sys",
        "description": "System information — hardware, OS, environment, resources",
        "commands": ["info", "env", "resources", "uptime"],
    }));

    Json(json!({"apps": app_list}))
}

/// Convert a JSON request body into CLI-style args.
fn body_to_args(body: &Value) -> Vec<String> {
    let Some(obj) = body.as_object() else {
        return vec![];
    };

    // Explicit "args" key takes precedence
    if let Some(args_val) = obj.get("args") {
        if let Some(arr) = args_val.as_array() {
            return arr.iter().map(|v| value_to_string(v)).collect();
        }
        return vec![value_to_string(args_val)];
    }

    let positional_fields = ["path", "url", "key", "name", "query", "command"];
    let mut positional = Vec::new();
    let mut flags = Vec::new();

    for (key, value) in obj {
        if value.is_null() {
            continue;
        }
        if let Some(b) = value.as_bool() {
            if b {
                flags.push(format!("--{key}"));
            }
        } else if positional_fields.contains(&key.as_str()) {
            positional.push(value_to_string(value));
        } else if let Some(arr) = value.as_array() {
            for item in arr {
                flags.push(format!("--{key}"));
                flags.push(value_to_string(item));
            }
        } else {
            flags.push(format!("--{key}"));
            flags.push(value_to_string(value));
        }
    }

    positional.extend(flags);
    positional
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

async fn run_command(
    Path((app_name, command)): Path<(String, String)>,
    body: Option<Json<Value>>,
) -> (StatusCode, Json<Value>) {
    let body_val = body.map(|b| b.0).unwrap_or(json!({}));

    // Built-in sys app
    if app_name == "sys" {
        let args = body_to_args(&body_val);
        match sysinfo::run(&command, &args) {
            Ok(v) => return (StatusCode::OK, Json(v)),
            Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))),
        }
    }

    let discovered = apps::discover(&apps_dir());

    let Some(app) = discovered.get(&app_name) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown app: {app_name}")})),
        );
    };

    if !app.manifest.commands.contains_key(&command) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown command: {app_name} {command}")})),
        );
    }

    let args = body_to_args(&body_val);
    let data = data_dir();
    let apps = apps_dir().to_string_lossy().to_string();

    match bridge::run_python_app(&app.dir, &command, &args, &data, &apps) {
        Ok(Some(output)) => match serde_json::from_str::<Value>(&output) {
            Ok(v) => {
                let status = if v.get("error").is_some() {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::OK
                };
                (status, Json(v))
            }
            Err(_) => (StatusCode::OK, Json(json!({"output": output}))),
        },
        Ok(None) => (StatusCode::OK, Json(json!({}))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e})),
        ),
    }
}
