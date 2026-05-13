use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::policy::{self, OpType};

/// Runtime selector for [`run_app`]. Mirrors the `runtime` field in
/// `app.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runtime {
    Python,
    Node,
    Shell,
    Binary,
}

impl Runtime {
    pub fn parse(s: Option<&str>) -> Option<Self> {
        match s.unwrap_or("python").to_ascii_lowercase().as_str() {
            "python" | "py" => Some(Runtime::Python),
            "node" | "js" | "node.js" | "nodejs" => Some(Runtime::Node),
            "shell" | "bash" | "sh" => Some(Runtime::Shell),
            "binary" | "bin" | "exe" => Some(Runtime::Binary),
            _ => None,
        }
    }

    pub fn default_entry(self) -> &'static str {
        match self {
            Runtime::Python => "main.py",
            Runtime::Node => "main.js",
            Runtime::Shell => {
                if cfg!(windows) {
                    "main.bat"
                } else {
                    "main.sh"
                }
            }
            Runtime::Binary => {
                if cfg!(windows) {
                    "main.exe"
                } else {
                    "main"
                }
            }
        }
    }
}

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

/// Generic polyglot bridge: read `app_dir/app.json`, pick the runtime
/// based on the `runtime` field (default: python), and invoke the
/// app's entry point.
///
/// Non-Python runtimes get the command + args via env vars instead of
/// the Python wrapper:
///
/// * `COS_COMMAND`     — string (e.g. "ls")
/// * `COS_ARGS_JSON`   — JSON-encoded array of strings
/// * `COS_DATA_DIR`    — same as the python wrapper
/// * `COS_APPS_DIR`    — same
///
/// The app writes one JSON document to stdout. Empty stdout is
/// allowed and reported as `Ok(None)`. On non-zero exit, the
/// function follows the same JSON-error fallback rule as
/// [`run_python_app`]: if stdout parses as `{ "error": ... }` we
/// return that string; otherwise stderr (or the exit code) is
/// returned as an `Err`.
pub fn run_app(
    app_dir: &Path,
    command: &str,
    args: &[String],
    data_dir: &str,
    apps_dir: &str,
) -> Result<Option<String>, String> {
    // Load the manifest if present so we can pick a runtime. Apps
    // that ship without app.json are treated as legacy python
    // apps for back-compat.
    let manifest_path = app_dir.join("app.json");
    let (runtime, entry) = if manifest_path.is_file() {
        let body = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("read {}: {}", manifest_path.display(), e))?;
        let manifest: crate::apps::AppManifest = serde_json::from_str(&body)
            .map_err(|e| format!("parse {}: {}", manifest_path.display(), e))?;
        let rt = Runtime::parse(manifest.runtime.as_deref()).ok_or_else(|| {
            format!(
                "unknown runtime '{}' in {}",
                manifest.runtime.unwrap_or_default(),
                manifest_path.display()
            )
        })?;
        let entry = manifest
            .entry
            .unwrap_or_else(|| rt.default_entry().to_string());
        (rt, entry)
    } else {
        (Runtime::Python, Runtime::Python.default_entry().to_string())
    };

    if matches!(runtime, Runtime::Python) {
        // Override the entry-point file by symlink / copy is out of
        // scope; pythonic apps without main.py should opt into the
        // entry override at the manifest level. For now the python
        // path uses the existing wrapper which always loads main.py.
        // Custom entries fall back to the legacy wrapper if it
        // matches main.py exactly; otherwise we surface a clear
        // error.
        if entry != "main.py" {
            return Err(format!(
                "python runtime currently requires entry='main.py' (got '{entry}'); \
                 file an issue if you need a per-app entry override"
            ));
        }
        return run_python_app(app_dir, command, args, data_dir, apps_dir);
    }

    let op = infer_op_type(command);
    policy::require(op).map_err(|v| v.to_string())?;

    let entry_path = app_dir.join(&entry);
    if !entry_path.is_file() {
        return Err(format!("app entry not found: {}", entry_path.display()));
    }

    let args_json =
        serde_json::to_string(args).map_err(|e| format!("failed to serialize args: {e}"))?;

    let mut cmd = match runtime {
        Runtime::Node => {
            let mut c = Command::new("node");
            c.arg(&entry_path);
            c
        }
        Runtime::Shell => {
            if cfg!(windows) {
                let mut c = Command::new("cmd");
                c.arg("/c").arg(&entry_path);
                c
            } else {
                let mut c = Command::new("bash");
                c.arg(&entry_path);
                c
            }
        }
        Runtime::Binary => Command::new(&entry_path),
        Runtime::Python => unreachable!("python handled above"),
    };

    let mut child = cmd
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("COS_COMMAND", command)
        .env("COS_ARGS_JSON", &args_json)
        .env("COS_DATA_DIR", data_dir)
        .env("COS_APPS_DIR", apps_dir)
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("CI", "true")
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("PIP_NO_INPUT", "1")
        .env("NPM_CONFIG_YES", "true")
        .envs(crate::config::as_env_vars())
        .spawn()
        .map_err(|e| format!("failed to spawn {runtime:?} app: {e}"))?;

    let status = child
        .wait()
        .map_err(|e| format!("{runtime:?} app wait failed: {e}"))?;

    let mut stdout = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    let mut stderr = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }

    if !status.success() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout.trim()) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_parses_aliases() {
        assert_eq!(Runtime::parse(None), Some(Runtime::Python));
        assert_eq!(Runtime::parse(Some("python")), Some(Runtime::Python));
        assert_eq!(Runtime::parse(Some("PY")), Some(Runtime::Python));
        assert_eq!(Runtime::parse(Some("node")), Some(Runtime::Node));
        assert_eq!(Runtime::parse(Some("nodejs")), Some(Runtime::Node));
        assert_eq!(Runtime::parse(Some("shell")), Some(Runtime::Shell));
        assert_eq!(Runtime::parse(Some("bash")), Some(Runtime::Shell));
        assert_eq!(Runtime::parse(Some("binary")), Some(Runtime::Binary));
        assert_eq!(Runtime::parse(Some("bin")), Some(Runtime::Binary));
        assert_eq!(Runtime::parse(Some("exe")), Some(Runtime::Binary));
        assert!(Runtime::parse(Some("rust")).is_none());
        assert!(Runtime::parse(Some("")).is_none());
    }

    #[test]
    fn default_entries_are_runtime_aware() {
        assert_eq!(Runtime::Python.default_entry(), "main.py");
        assert_eq!(Runtime::Node.default_entry(), "main.js");
        // Shell + Binary are platform-conditional; just assert
        // non-empty rather than encode the host.
        assert!(!Runtime::Shell.default_entry().is_empty());
        assert!(!Runtime::Binary.default_entry().is_empty());
    }

    #[test]
    fn run_app_errors_when_app_dir_missing() {
        let tmp = std::env::temp_dir().join("cos-bridge-test-missing");
        let _ = std::fs::remove_dir_all(&tmp);
        let err = run_app(&tmp, "ls", &[], "/tmp", "/tmp").unwrap_err();
        // No app.json + no main.py → python branch surfaces
        // "app has no main.py" via run_python_app.
        assert!(
            err.contains("main.py") || err.contains("app.json"),
            "expected main.py / app.json reference, got: {err}"
        );
    }

    #[test]
    fn run_app_rejects_non_main_py_for_python() {
        let tmp = std::env::temp_dir().join("cos-bridge-test-pyentry");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("app.json"),
            r#"{"name":"x","version":"0","description":"","commands":{},"runtime":"python","entry":"alt.py"}"#,
        )
        .unwrap();
        let err = run_app(&tmp, "ls", &[], "/tmp", "/tmp").unwrap_err();
        assert!(
            err.contains("entry='main.py'"),
            "expected python-entry guard, got: {err}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn run_app_errors_on_unknown_runtime() {
        let tmp = std::env::temp_dir().join("cos-bridge-test-unknown");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("app.json"),
            r#"{"name":"x","version":"0","description":"","commands":{},"runtime":"rust"}"#,
        )
        .unwrap();
        let err = run_app(&tmp, "ls", &[], "/tmp", "/tmp").unwrap_err();
        assert!(
            err.contains("unknown runtime"),
            "expected unknown runtime error, got: {err}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn run_app_node_entry_missing_surfaces_clear_error() {
        let tmp = std::env::temp_dir().join("cos-bridge-test-node-missing");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("app.json"),
            r#"{"name":"x","version":"0","description":"","commands":{},"runtime":"node"}"#,
        )
        .unwrap();
        let err = run_app(&tmp, "ls", &[], "/tmp", "/tmp").unwrap_err();
        assert!(
            err.contains("app entry not found"),
            "expected entry-missing error, got: {err}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
