//! Edit ↔ AI bridge.
//!
//! Every AI-touching operation in cosmic-edit routes through the
//! kernel-level `apps/doc` over the `cos app doc <op>` boundary so
//! capability gating, audit logging and budget accounting all happen
//! exactly once, in the place the kernel expects them. The editor
//! process never speaks to a model directly.
//!
//! Async + `Result<_, String>` shaped because both the MCP server and
//! any future in-editor UI (e.g. a Cmd+K palette) want flat
//! human-presentable errors, not `io::Error`.

use std::path::{Path, PathBuf};

use serde_json::Value;

fn cos_bin() -> String {
    std::env::var("CLAW_COS_BIN").unwrap_or_else(|_| "cos".into())
}

async fn invoke_app(app_id: &str, op: &str, extra: &[&str]) -> Result<Value, String> {
    let bin = cos_bin();
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.args(["app", app_id, op]);
    cmd.args(extra);

    let output = cmd
        .output()
        .await
        .map_err(|e| format!("failed to invoke {bin}: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "cos produced no output ({})\n{}",
            output.status, stderr
        ));
    }
    let value: Value = serde_json::from_str(trimmed)
        .map_err(|e| format!("bad JSON from cos: {e}\n---\n{trimmed}"))?;
    if let Some(err) = value.get("error").and_then(|v| v.as_str()) {
        return Err(err.to_string());
    }
    Ok(value)
}

fn extract_string_field(value: &Value, field: &str) -> Result<String, String> {
    value
        .get(field)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("AI response missing '{field}' field"))
}

fn path_arg(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("non-UTF-8 path cannot cross the bridge: {path:?}"))
}

pub async fn summarize(path: PathBuf) -> Result<String, String> {
    let p = path_arg(&path)?;
    let value = invoke_app("doc", "summarize", &["--file", p]).await?;
    extract_string_field(&value, "summary")
}

pub async fn explain(path: PathBuf) -> Result<String, String> {
    let p = path_arg(&path)?;
    let value = invoke_app("doc", "explain", &["--file", p]).await?;
    extract_string_field(&value, "text")
}

pub async fn rewrite(path: PathBuf, instruction: String) -> Result<String, String> {
    let p = path_arg(&path)?;
    let value = invoke_app(
        "doc",
        "rewrite",
        &["--file", p, "--instruction", &instruction],
    )
    .await?;
    extract_string_field(&value, "text")
}
