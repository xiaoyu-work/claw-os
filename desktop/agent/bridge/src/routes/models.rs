//! `GET /api/models` — configured LLM provider and model metadata.

use std::time::Duration;

use axum::{Json, extract::State};
use cos_agent_protocol::{ModelSummary, ModelsResponse};
use serde::{Deserialize, de::DeserializeOwned};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};

use crate::{api_error::ApiError, state::AppState};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;

struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

#[derive(Debug, Default, Deserialize)]
struct ModelStatus {
    #[serde(default)]
    ready: bool,
    #[serde(default)]
    provider: String,
    #[serde(default)]
    model: String,
}

#[derive(Debug, Default, Deserialize)]
struct ModelCatalogue {
    #[serde(default)]
    providers: Vec<ProviderEntry>,
}

#[derive(Debug, Deserialize)]
struct ProviderEntry {
    name: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    models: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    name: String,
}

pub async fn list(State(_state): State<AppState>) -> Result<Json<ModelsResponse>, ApiError> {
    let status: ModelStatus = run_json(&["agent", "setup", "text", "--status"]).await?;
    let catalogue: ModelCatalogue = run_json(&["agent", "setup", "text", "--providers"]).await?;
    Ok(Json(build_response(&status, &catalogue)))
}

async fn run_json<T: DeserializeOwned>(args: &[&str]) -> Result<T, ApiError> {
    let binary = std::env::var("COS_BIN").unwrap_or_else(|_| "cos".to_string());
    let mut command = Command::new(binary);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|error| ApiError::service_unavailable(format!("failed to run cos: {error}")))?;
    let stdout = child.stdout.take().ok_or_else(|| {
        ApiError::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            cos_agent_protocol::ErrorCode::Internal,
            "model status stdout pipe is unavailable",
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        ApiError::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            cos_agent_protocol::ErrorCode::Internal,
            "model status stderr pipe is unavailable",
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
            return Err(ApiError::bad_gateway(format!(
                "failed to read model status: {error}"
            )));
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(ApiError::new(
                axum::http::StatusCode::GATEWAY_TIMEOUT,
                cos_agent_protocol::ErrorCode::Timeout,
                "model status command timed out",
            ));
        }
    };
    if stdout.truncated || stderr.truncated {
        return Err(ApiError::bad_gateway("model status response was too large"));
    }
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr.bytes);
        return Err(ApiError::bad_gateway(
            stderr.trim().chars().take(512).collect::<String>(),
        ));
    }
    serde_json::from_slice(&stdout.bytes)
        .map_err(|error| ApiError::bad_gateway(format!("invalid model status JSON: {error}")))
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

fn build_response(status: &ModelStatus, catalogue: &ModelCatalogue) -> ModelsResponse {
    let provider = status.provider.trim().to_string();
    let model = status.model.trim().to_string();
    let ready = status.ready;

    let provider_entry = catalogue
        .providers
        .iter()
        .find(|entry| entry.name == provider);
    let provider_label = provider_entry
        .map(|entry| entry.label.trim())
        .filter(|label| !label.is_empty())
        .unwrap_or(provider.as_str())
        .to_string();

    let mut models = provider_entry
        .map(|entry| {
            entry
                .models
                .iter()
                .map(|row| row.name.trim())
                .filter(|name| !name.is_empty())
                .map(|name| ModelSummary {
                    id: name.to_owned(),
                    provider: provider.clone(),
                    label: name.to_owned(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !model.is_empty() && !models.iter().any(|entry| entry.id == model) {
        models.insert(
            0,
            ModelSummary {
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
    ModelsResponse {
        ready,
        provider,
        model,
        label,
        models,
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/routes/models.rs"
    ));
}
