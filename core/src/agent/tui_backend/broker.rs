use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::clawd::protocol::Request;
use crate::clawd::routes::Command;
use crate::config::CosConfig;

use super::backend::{Backend, BackendInfo, Operation, ReviewDecision};
use super::protocol::{safe_text, token, RpcError};

pub(super) struct BrokerBackend {
    info: BackendInfo,
    socket: PathBuf,
}

impl BrokerBackend {
    pub async fn new(config: Arc<CosConfig>, uid: u32) -> Result<Self, String> {
        let readiness = config.clone();
        let (home, ready) = tokio::task::spawn_blocking(move || {
            let home = crate::paths::verified_home_for_uid(uid)?;
            let ready = crate::agent::setup::is_ready(&readiness.agent).is_ok();
            Ok::<_, String>((home, ready))
        })
        .await
        .map_err(|_| "Claw provider readiness check failed".to_string())??;
        let models = super::models::catalog(Arc::new(config.agent.clone()), ready).await?;
        Ok(Self {
            info: BackendInfo {
                config_home: home.join(".config").join("cos"),
                home,
                provider: config.agent.provider.clone(),
                model: config.agent.model.clone(),
                models,
                ready,
            },
            socket: crate::clawd::config::socket_path(),
        })
    }
}

#[async_trait]
impl Backend for BrokerBackend {
    fn info(&self) -> &BackendInfo {
        &self.info
    }

    async fn call(&self, operation: Operation, params: Value) -> Result<Value, RpcError> {
        let command = match operation {
            Operation::ConversationCreate => Command::AgentConversationCreate,
            Operation::ConversationGet => Command::AgentConversationGet,
            Operation::ConversationList => Command::AgentConversationList,
            Operation::ConversationUpdate => Command::AgentConversationUpdate,
            Operation::ConversationFork => Command::AgentConversationFork,
            Operation::ConversationRevert => Command::AgentConversationRevert,
            Operation::TaskSubmit => Command::TaskSubmit,
            Operation::TaskGet => Command::TaskGet,
            Operation::TaskList => Command::TaskList,
            Operation::TaskStream => Command::TaskStream,
            Operation::TaskCancel => Command::TaskCancel,
            Operation::PermissionPending => Command::PermissionPending,
            Operation::PermissionStatus => Command::PermissionStatus,
            Operation::SkillsList => return skills(&self.info.home).await,
        };
        request(&self.socket, command, params).await
    }

    async fn review(&self, id: &str, decision: ReviewDecision) -> Result<(), RpcError> {
        token(id, "approval id")?;
        if id.len() > 128
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(RpcError::params("Invalid approval id"));
        }
        let expected = match decision {
            ReviewDecision::ApproveOnce => "approved",
            ReviewDecision::Deny => "denied",
        };
        let mut command = tokio::process::Command::new("/usr/bin/pkexec");
        command
            .arg("/usr/local/bin/claw-approval-helper")
            .arg("--id")
            .arg(id)
            .arg("--decision")
            .arg(match decision {
                ReviewDecision::ApproveOnce => "approve",
                ReviewDecision::Deny => "deny",
            })
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if decision == ReviewDecision::ApproveOnce {
            command.arg("--duration").arg("once");
        }
        let mut child = command
            .spawn()
            .map_err(|_| RpcError::unavailable("The installed Claw polkit approval helper is unavailable; use `cos approval` to review the pending request"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RpcError::backend("Approval helper stdout is unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| RpcError::backend("Approval helper stderr is unavailable"))?;
        let result = tokio::time::timeout(Duration::from_secs(120), async {
            tokio::try_join!(
                read_helper_output(stdout),
                read_helper_output(stderr),
                async {
                    child
                        .wait()
                        .await
                        .map_err(|_| RpcError::failed("Could not wait for approval authorization"))
                }
            )
        })
        .await;
        let (stdout, _stderr, status) = match result {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                let _ = child.kill().await;
                return Err(error);
            }
            Err(_) => {
                let _ = child.kill().await;
                return Err(RpcError::failed(
                    "Approval authorization timed out; the root decision remains authoritative",
                ));
            }
        };
        if !status.success() {
            return Err(RpcError::failed(
                "Approval authorization was denied or cancelled; no TUI decision grants permission",
            ));
        }
        let value: Value = serde_json::from_slice(&stdout)
            .map_err(|_| RpcError::backend("The approval helper returned an invalid response"))?;
        if value.get("id").and_then(Value::as_str) != Some(id)
            || value.get("decision").and_then(Value::as_str) != Some(expected)
        {
            return Err(RpcError::backend(
                "The approval helper returned a mismatched decision",
            ));
        }
        Ok(())
    }
}

pub(super) async fn read_initial_conversation(id: &str) -> Result<Value, RpcError> {
    request(
        &crate::clawd::config::socket_path(),
        Command::AgentConversationGet,
        json!({ "id": id, "limit": 1 }),
    )
    .await
}

async fn request(
    socket: &std::path::Path,
    command: Command,
    params: Value,
) -> Result<Value, RpcError> {
    let response = tokio::time::timeout(
        Duration::from_secs(35),
        crate::clawd::client::request(socket, Request::new(command, params)),
    )
    .await
    .map_err(|_| RpcError::unavailable("Claw broker request timed out; its outcome is unknown"))?
    .map_err(RpcError::unavailable)?;
    if response.ok {
        response
            .result
            .ok_or_else(|| RpcError::backend("Claw broker returned no result"))
    } else {
        let error = response
            .error
            .ok_or_else(|| RpcError::backend("Claw broker returned no error"))?;
        Err(match error.code.as_str() {
            "unavailable" | "unknown_command" => RpcError::unavailable(error.message),
            "malformed" | "invalid_request" => RpcError::params(error.message),
            _ => RpcError::failed(error.message),
        })
    }
}

async fn read_helper_output(reader: impl AsyncRead + Unpin) -> Result<Vec<u8>, RpcError> {
    const MAX_HELPER_BYTES: u64 = 16 * 1024;
    let mut output = Vec::new();
    reader
        .take(MAX_HELPER_BYTES + 1)
        .read_to_end(&mut output)
        .await
        .map_err(|_| RpcError::failed("Could not read approval helper response"))?;
    if output.len() > MAX_HELPER_BYTES as usize {
        return Err(RpcError::backend(
            "Approval helper response exceeds its size limit",
        ));
    }
    Ok(output)
}

async fn skills(home: &std::path::Path) -> Result<Value, RpcError> {
    let loaded = tokio::task::spawn_blocking(crate::agent::skills::loader::load_catalog_default)
        .await
        .map_err(|_| RpcError::unavailable("Claw Skill catalogue is unavailable"))?;
    if loaded.skills.len() + loaded.errors.len() + loaded.disabled.len() > 1000 {
        return Err(RpcError::capacity());
    }
    let entries: Vec<Value> = loaded
        .skills
        .values()
        .map(|skill| {
            json!({
                "name": safe_text(&skill.id),
                "description": safe_text(skill.manifest.description.as_deref().unwrap_or("")),
                "path": skill.manifest_path,
                "scope": match skill.origin {
                    crate::agent::skills::loader::SkillOrigin::BuiltIn => "system",
                    crate::agent::skills::loader::SkillOrigin::User => "user",
                    crate::agent::skills::loader::SkillOrigin::Local => "repo",
                },
                "enabled": true,
                "pluginId": null,
            })
        })
        .collect();
    let errors: Vec<Value> = loaded
        .errors
        .iter()
        .chain(loaded.disabled.iter())
        .map(|(id, message)| {
            json!({
                "path": id,
                "message": safe_text(message),
            })
        })
        .collect();
    Ok(json!({ "data": [{ "cwd": home, "skills": entries, "errors": errors }] }))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tui_backend/broker.rs"
    ));
}
