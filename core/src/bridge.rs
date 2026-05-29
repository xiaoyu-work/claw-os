use std::path::Path;
use std::process::{Command, Stdio};

use crate::caps::manifest::Runtime;

/// Build the Python launcher script shared by [`run_python_app`] (one-
/// shot operations) and [`launch_gui`] (long-lived desktop surface).
///
/// The script makes the `claw_os_sdk` + `cos_runtime` packages
/// importable, loads the app's `main.py`, and calls `run(command,
/// args)`. The GUI path passes the manifest's `desktop.exec` value as
/// `command`; an op invocation passes the operation name.
fn python_wrapper(
    main_py: &Path,
    command: &str,
    args: &[String],
    data_dir: &str,
    apps_dir: &str,
) -> Result<String, String> {
    Ok(format!(
        r#"
import importlib.util, json, sys, os
os.environ.setdefault("COS_DATA_DIR", {data_dir})
os.environ.setdefault("COS_APPS_DIR", {apps_dir})
# Make the claw_os_sdk + cos_runtime packages importable from every
# app, so Python apps can `from cos_runtime import policy` (capability
# checks) and `from claw_os_sdk import ai` (AI features) without
# bundling either tree into each app. Honour an explicit override;
# otherwise fall back to the common production install path and the
# in-repo dev-checkout paths.
_sdk_override = os.environ.get("COS_SDK_PYTHON_DIR")
_sdk_candidates = []
if _sdk_override:
    _sdk_candidates.append(_sdk_override)
_sdk_candidates.append("/usr/lib/cos/python")
_apps_root = os.environ.get("COS_APPS_DIR") or ""
if _apps_root:
    _sdk_candidates.append(
        os.path.normpath(
            os.path.join(_apps_root, os.pardir, "claw-os-sdk", "python", "src")
        )
    )
    _sdk_candidates.append(
        os.path.normpath(
            os.path.join(_apps_root, os.pardir, "cos-runtime", "python", "src")
        )
    )
_wanted = ("claw_os_sdk", "cos_runtime")
for _cand in _sdk_candidates:
    if not _cand or not os.path.isdir(_cand):
        continue
    if any(os.path.isdir(os.path.join(_cand, _pkg)) for _pkg in _wanted):
        if _cand not in sys.path:
            sys.path.insert(0, _cand)
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
    ))
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
    let main_py = app_dir.join("main.py");
    if !main_py.is_file() {
        return Err(format!("app has no main.py at {}", main_py.display()));
    }

    let wrapper = python_wrapper(&main_py, command, args, data_dir, apps_dir)?;

    let python = if cfg!(windows) { "python" } else { "python3" };

    // The directory name is the canonical app id (the manifest loader
    // enforces this — see apps::discover). The Python helpers read
    // `COS_APP_ID` to identify which app is calling them.
    let app_id = app_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    let child = Command::new(python)
        .arg("-c")
        .arg(&wrapper)
        .stdin(Stdio::null())
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
        .env("COS_APP_ID", &app_id)
        // Pass config values so Python apps use config.json instead of hardcoded defaults
        .envs(crate::config::as_env_vars())
        .spawn()
        .map_err(|e| format!("failed to spawn python3: {e}"))?;

    // wait_with_output() drains stdout and stderr in background threads
    // BEFORE the child can fill the kernel pipe buffer (Linux default
    // 64KB). The previous pattern of `child.wait()` first and then
    // reading the streams deadlocks for any verb that emits more than
    // 64KB to stdout — e.g. fs.read of a multi-MB file, pkg.list, a
    // wide db.query — because the child blocks on write() while we
    // block on wait().
    let output = child
        .wait_with_output()
        .map_err(|e| format!("python3 wait failed: {e}"))?;
    let status = output.status;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

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
    // that ship without app.json default to the Python runtime — this
    // lets ad-hoc `main.py` apps in development still run.
    let manifest_path = app_dir.join("app.json");
    let (runtime, entry) = if manifest_path.is_file() {
        let body = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("read {}: {}", manifest_path.display(), e))?;
        let manifest = crate::apps::AppManifest::from_json(&body)
            .map_err(|e| format!("parse {}: {}", manifest_path.display(), e))?;
        // Reject app launches whose `ai.tools[]` references a tool
        // the kernel doesn't know. Catches typoed allowlists before
        // the model ever sees a tool definition. The catalog is
        // passed in so the caps crate stays free of an `ai`
        // dependency (would create a cycle).
        let catalog = crate::ai::tools::list_names();
        manifest
            .validate_tools_against_catalog(&catalog)
            .map_err(|e| format!("parse {}: {}", manifest_path.display(), e))?;
        let rt = manifest.runtime;
        let entry = manifest
            .entry
            .unwrap_or_else(|| rt.default_entry().to_string());
        (rt, entry)
    } else {
        (Runtime::Python, Runtime::Python.default_entry().to_string())
    };

    if matches!(runtime, Runtime::Python) {
        // Pythonic apps always run through the shared wrapper which
        // loads `main.py`. A custom entry name is currently unsupported
        // for the python runtime; surface a clear error rather than
        // silently ignoring it.
        if entry != "main.py" {
            return Err(format!(
                "python runtime currently requires entry='main.py' (got '{entry}'); \
                 file an issue if you need a per-app entry override"
            ));
        }
        return run_python_app(app_dir, command, args, data_dir, apps_dir);
    }

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

    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("COS_COMMAND", command)
        .env("COS_ARGS_JSON", &args_json)
        .env("COS_DATA_DIR", data_dir)
        .env("COS_APPS_DIR", apps_dir)
        .env(
            "COS_APP_ID",
            app_dir.file_name().and_then(|s| s.to_str()).unwrap_or(""),
        )
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

    // wait_with_output() avoids the deadlock that occurs when the
    // child writes more than ~64KB to stdout / stderr while we wait
    // — pipe fills, child blocks on write, parent blocks on wait. See
    // run_python_app above for the same fix.
    let output = child
        .wait_with_output()
        .map_err(|e| format!("{runtime:?} app wait failed: {e}"))?;
    let status = output.status;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

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

/// Launch an app's **desktop GUI surface**.
///
/// Unlike [`run_app`] (one-shot, stdout captured as a JSON envelope),
/// this is a long-lived foreground launch: the app entry is spawned
/// with `COS_APP_GUI=1`, given the manifest's `desktop.exec` value
/// (default `--gui`) as its `COS_COMMAND`, inherits the parent's stdio,
/// and runs its own event loop until the window closes.
///
/// Identity (`COS_APP_ID`) is set exactly as for the headless path, so
/// audit / consent / policy enforcement apply unchanged. This is the
/// reason the generated `.desktop` routes through `cos app <id> --gui`
/// instead of exec-ing the app binary directly.
///
/// `exec` is the command the entry receives (the manifest's
/// `desktop.exec`); `files` are the file paths passed by the launcher
/// (`%F`). Returns once the GUI process exits.
pub fn launch_gui(
    app_dir: &Path,
    exec: &str,
    files: &[String],
    data_dir: &str,
    apps_dir: &str,
) -> Result<(), String> {
    let manifest_path = app_dir.join("app.json");
    let (runtime, entry) = if manifest_path.is_file() {
        let body = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("read {}: {}", manifest_path.display(), e))?;
        let manifest = crate::apps::AppManifest::from_json(&body)
            .map_err(|e| format!("parse {}: {}", manifest_path.display(), e))?;
        let rt = manifest.runtime;
        let entry = manifest
            .entry
            .unwrap_or_else(|| rt.default_entry().to_string());
        (rt, entry)
    } else {
        (Runtime::Python, Runtime::Python.default_entry().to_string())
    };

    let app_id = app_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    let mut cmd = if matches!(runtime, Runtime::Python) {
        let main_py = app_dir.join("main.py");
        if !main_py.is_file() {
            return Err(format!("app has no main.py at {}", main_py.display()));
        }
        let wrapper = python_wrapper(&main_py, exec, files, data_dir, apps_dir)?;
        let python = if cfg!(windows) { "python" } else { "python3" };
        let mut c = Command::new(python);
        c.arg("-c").arg(wrapper);
        c
    } else {
        let entry_path = app_dir.join(&entry);
        if !entry_path.is_file() {
            return Err(format!("app entry not found: {}", entry_path.display()));
        }
        match runtime {
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
        }
    };

    let args_json =
        serde_json::to_string(files).map_err(|e| format!("failed to serialize files: {e}"))?;

    // A GUI draws on Wayland/X, not stdout. Inherit the parent's stdio
    // so the app's own logging is visible and so it stays attached as a
    // long-lived foreground process until the window is closed.
    let status = cmd
        .stdin(Stdio::null())
        .env("COS_APP_ID", &app_id)
        .env("COS_APP_GUI", "1")
        .env("COS_COMMAND", exec)
        .env("COS_ARGS_JSON", &args_json)
        .env("COS_DATA_DIR", data_dir)
        .env("COS_APPS_DIR", apps_dir)
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .envs(crate::config::as_env_vars())
        .status()
        .map_err(|e| format!("failed to launch {runtime:?} GUI: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "GUI `{app_id}` exited with code {}",
            status.code().unwrap_or(-1)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_entries_are_runtime_aware() {
        assert_eq!(Runtime::Python.default_entry(), "main.py");
        assert_eq!(Runtime::Node.default_entry(), "main.js");
        // Shell + Binary just need to be non-empty.
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
            r#"{"id":"x","version":"0","name":"X","runtime":"python","entry":"alt.py"}"#,
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
            r#"{"id":"x","version":"0","name":"X","runtime":"rust"}"#,
        )
        .unwrap();
        let err = run_app(&tmp, "ls", &[], "/tmp", "/tmp").unwrap_err();
        // serde rejects unknown runtime values at parse time.
        assert!(
            err.contains("unknown variant") || err.contains("runtime"),
            "expected runtime parse error, got: {err}"
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
            r#"{"id":"x","version":"0","name":"X","runtime":"node"}"#,
        )
        .unwrap();
        let err = run_app(&tmp, "ls", &[], "/tmp", "/tmp").unwrap_err();
        assert!(
            err.contains("app entry not found"),
            "expected entry-missing error, got: {err}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Regression: bridge previously did `child.wait()` BEFORE reading
    /// stdout/stderr. When the child wrote more than the Linux pipe
    /// buffer (~64KB) to stdout, the child blocked on write() while
    /// the parent blocked on wait() — `cos` process hung forever. The
    /// fix routes both run_python_app and run_app through
    /// `wait_with_output`, which drains the streams in background
    /// threads.
    ///
    /// This test asks a tiny Python app to emit a JSON payload well
    /// above 64KB. Before the fix this test would never return; we
    /// add a generous-but-not-infinite outer timeout to make a
    /// regression a quick CI failure instead of a hang.
    #[cfg(unix)]
    #[test]
    fn run_python_app_handles_stdout_larger_than_pipe_buffer() {
        // Skip if python3 isn't on PATH (some minimal CI images).
        if std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let tmp = std::env::temp_dir().join("cos-bridge-test-bigstdout");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        // ~256 KB of payload — comfortably over the 64KB pipe buffer.
        std::fs::write(
            tmp.join("main.py"),
            "def run(command, args):\n    return {\"data\": \"x\" * 262144}\n",
        )
        .unwrap();

        // Hard timeout: any deadlock regresses this into a 10s failure
        // rather than a session-killing hang.
        let (tx, rx) = std::sync::mpsc::channel();
        let app_dir = tmp.clone();
        let t = std::thread::spawn(move || {
            let r = run_python_app(&app_dir, "noop", &[], "/tmp", "/tmp");
            let _ = tx.send(r);
        });
        let result = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("run_python_app deadlocked on >64KB stdout");
        let _ = t.join();
        let out = result.expect("run_python_app errored").expect("got json");
        assert!(out.len() >= 262_144, "payload truncated, got {} bytes", out.len());
        assert!(out.contains("\"data\""), "json missing data field");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
