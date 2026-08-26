//! `GET /api/models` — configured LLM provider and model metadata.

use std::time::Duration;

use axum::{Json, extract::State, http::StatusCode};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};

use crate::state::AppState;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;

struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Model {
    pub id: String,
    pub provider: String,
    pub label: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ModelsResponse {
    pub ready: bool,
    pub provider: String,
    pub model: String,
    pub label: String,
    pub models: Vec<Model>,
}

pub async fn list(
    State(_state): State<AppState>,
) -> Result<Json<ModelsResponse>, (StatusCode, Json<Value>)> {
    let status = run_json(&["agent", "setup", "llm", "--status"]).await?;
    let catalogue = run_json(&["agent", "setup", "llm", "--providers"]).await?;
    build_response(&status, &catalogue)
        .map(Json)
        .map_err(|error| (StatusCode::BAD_GATEWAY, Json(json!({ "error": error }))))
}

async fn run_json(args: &[&str]) -> Result<Value, (StatusCode, Json<Value>)> {
    let binary = std::env::var("COS_BIN").unwrap_or_else(|_| "cos".to_string());
    let mut command = Command::new(binary);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": format!("failed to run cos: {error}") })),
        )
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "model status stdout pipe is unavailable" })),
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "model status stderr pipe is unavailable" })),
        )
    })?;
    let communicate = async {
        tokio::try_join!(
            child.wait(),
            read_bounded(stdout, MAX_OUTPUT_BYTES),
            read_bounded(stderr, MAX_OUTPUT_BYTES),
        )
    };
    let (status, stdout, stderr) = match tokio::time::timeout(COMMAND_TIMEOUT, communicate).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            return Err((
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": format!("failed to read model status: {error}") })),
            ));
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err((
                StatusCode::GATEWAY_TIMEOUT,
                Json(json!({ "error": "model status command timed out" })),
            ));
        }
    };
    if stdout.truncated || stderr.truncated {
        return Err((
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": "model status response was too large" })),
        ));
    }
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr.bytes);
        return Err((
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "error": stderr.trim().chars().take(512).collect::<String>(),
            })),
        ));
    }
    serde_json::from_slice(&stdout.bytes).map_err(|error| {
        (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("invalid model status JSON: {error}") })),
        )
    })
}

async fn read_bounded<R>(mut reader: R, limit: usize) -> std::io::Result<CapturedOutput>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        let retained = remaining.min(read);
        bytes.extend_from_slice(&buffer[..retained]);
        truncated |= retained < read;
    }
    Ok(CapturedOutput { bytes, truncated })
}

fn build_response(status: &Value, catalogue: &Value) -> Result<ModelsResponse, String> {
    let provider = status
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let model = status
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let ready = status
        .get("ready")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let provider_entry = catalogue
        .get("providers")
        .and_then(Value::as_array)
        .and_then(|providers| {
            providers
                .iter()
                .find(|entry| entry.get("name").and_then(Value::as_str) == Some(provider.as_str()))
        });
    let provider_label = provider_entry
        .and_then(|entry| entry.get("label"))
        .and_then(Value::as_str)
        .filter(|label| !label.trim().is_empty())
        .unwrap_or(provider.as_str())
        .to_string();

    let mut models = provider_entry
        .and_then(|entry| entry.get("models"))
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("name").and_then(Value::as_str))
                .filter(|name| !name.trim().is_empty())
                .map(|name| Model {
                    id: name.to_string(),
                    provider: provider.clone(),
                    label: name.to_string(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !model.is_empty() && !models.iter().any(|entry| entry.id == model) {
        models.insert(
            0,
            Model {
                id: model.clone(),
                provider: provider.clone(),
                label: model.clone(),
            },
        );
    }

    let label = match (provider_label.is_empty(), model.is_empty()) {
        (true, true) => "Agent not configured".to_string(),
        (false, true) => provider_label,
        (true, false) => model.clone(),
        (false, false) => format!("{provider_label} · {model}"),
    };
    Ok(ModelsResponse {
        ready,
        provider,
        model,
        label,
        models,
    })
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/routes/models.rs"
    ));
}
