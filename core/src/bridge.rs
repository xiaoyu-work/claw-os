use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::policy::{self, OpType};

/// Infer the policy OpType from a Python app command name.
fn infer_op_type(command: &str) -> OpType {
    match command {
        "read" | "ls" | "stat" | "search" | "recent" | "query" | "tables" | "schema"
        | "databases" | "get" | "list" | "info" | "tail" | "has" | "which" | "__schema__" => {
            OpType::Read
        }

        "write" | "mkdir" | "tag" | "set" | "exec" | "send" => OpType::Write,

        "rm" | "del" | "clear" | "dump" => OpType::Delete,

        "run" | "script" | "start" | "stop" | "ps" | "submit" => OpType::Exec,

        "fetch" | "download" => OpType::Net,

        "need" | "install" => OpType::System,

        // Unknown commands default to Exec (conservative but not overly restrictive)
        _ => OpType::Exec,
    }
}

/// Run a Python app's main.py via subprocess.
///
/// Spawns `python3 <app_dir>/main.py` with the command and args passed
/// via a JSON payload on stdin. The app writes JSON to stdout.
///
/// Returns the raw JSON string from stdout, or an error.
pub fn run_python_app(
    app_dir: &Path,
    command: &str,
    args: &[String],
    data_dir: &str,
    apps_dir: &str,
) -> Result<Option<String>, String> {
    let op = infer_op_type(command);
    policy::require(op).map_err(|v| v.to_string())?;

    let main_py = app_dir.join("main.py");
    if !main_py.is_file() {
        return Err(format!("app has no main.py at {}", main_py.display()));
    }

    // Build a small Python wrapper that imports main.py and calls run().
    // This avoids modifying the Python apps — they keep their existing interface.
    let wrapper = format!(
        r#"
import importlib.util, json, sys, os
os.environ.setdefault("COS_DATA_DIR", {data_dir})
os.environ.setdefault("COS_APPS_DIR", {apps_dir})
spec = importlib.util.spec_from_file_location("app", {main_py})
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
result = mod.run({command}, {args})
if result is not None:
    json.dump(result, sys.stdout)
    print()
"#,
        data_dir = serde_json::to_string(data_dir)
            .map_err(|e| format!("failed to serialize data_dir: {e}"))?,
        apps_dir = serde_json::to_string(apps_dir)
            .map_err(|e| format!("failed to serialize apps_dir: {e}"))?,
        main_py = serde_json::to_string(&main_py.to_string_lossy().to_string())
            .map_err(|e| format!("failed to serialize main_py path: {e}"))?,
        command = serde_json::to_string(command)
            .map_err(|e| format!("failed to serialize command: {e}"))?,
        args = serde_json::to_string(args).map_err(|e| format!("failed to serialize args: {e}"))?,
    );

    let python = if cfg!(windows) { "python" } else { "python3" };

    let mut child = Command::new(python)
        .arg("-c")
        .arg(&wrapper)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Agent-native: suppress all interactive prompts
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("CI", "true")
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("PIP_NO_INPUT", "1")
        .env("NPM_CONFIG_YES", "true")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        // Pass config values so Python apps use config.json instead of hardcoded defaults
        .envs(crate::config::as_env_vars())
        .spawn()
        .map_err(|e| format!("failed to spawn python3: {e}"))?;

    let status = child
        .wait()
        .map_err(|e| format!("python3 wait failed: {e}"))?;

    let mut stdout = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }

    let mut stderr = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }

    if !status.success() {
        // Try to extract a JSON error from stdout first.
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&stdout) {
            if v.get("error").is_some() {
                return Ok(Some(stdout.trim().to_string()));
            }
        }
        let msg = if stderr.is_empty() {
            format!("exit code {}", status.code().unwrap_or(-1))
        } else {
            stderr.trim().to_string()
        };
        return Err(msg);
    }

    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}
