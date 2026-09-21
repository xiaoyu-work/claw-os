use std::collections::HashMap;
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

#[derive(Clone, Debug)]
pub(super) struct BackendInfo {
    pub home: PathBuf,
    pub provider: String,
    pub model: String,
    pub models: Vec<String>,
}

#[derive(Clone, Debug)]
pub(super) struct ConversationMessage {
    pub role: String,
    pub text: String,
}

#[derive(Clone, Debug)]
pub(super) struct Conversation {
    pub id: String,
    pub title: String,
    pub archived: bool,
    pub history_truncated: bool,
    pub messages: Vec<ConversationMessage>,
}

#[derive(Clone, Debug)]
pub(super) struct ConversationSummary {
    pub id: String,
    pub title: String,
    pub archived: bool,
}

#[derive(Clone, Debug)]
pub(super) struct ConversationList {
    pub conversations: Vec<ConversationSummary>,
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub(super) struct Job {
    pub id: String,
    pub session_id: String,
    pub activity_id: Option<String>,
    pub prompt: String,
    pub status: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub response: Option<String>,
    pub error: Option<String>,
    pub requested_model: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub turns_used: Option<u32>,
}

impl Job {
    pub fn is_terminal(&self) -> bool {
        matches!(self.status.as_str(), "ok" | "error" | "cancelled")
    }
}

#[derive(Clone, Debug)]
pub(super) struct TaskSummary {
    pub id: String,
    pub title: String,
    pub status: String,
    pub created_at: String,
    pub session_id: Option<String>,
    pub activity_id: Option<String>,
    pub waiting_on: usize,
    pub cancel_requested: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct NotificationAction {
    pub label: String,
    pub uri: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct NotificationDelivery {
    pub channel: String,
    pub state: String,
    pub attempts: u32,
    pub last_error_code: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct NotificationItem {
    pub id: String,
    pub source: String,
    pub kind: String,
    pub severity: String,
    pub title: String,
    pub body: String,
    pub delivery_policy: String,
    pub state: String,
    pub occurrences: u32,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub task_id: Option<String>,
    pub session_id: Option<String>,
    pub job_id: Option<String>,
    pub actions: Vec<NotificationAction>,
    pub deliveries: Vec<NotificationDelivery>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct NotificationPage {
    pub notifications: Vec<NotificationItem>,
    pub unread: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct NotificationPreferences {
    pub web_enabled: bool,
    pub desktop_enabled: bool,
    pub ntfy_enabled: bool,
    pub web_min_severity: String,
    pub desktop_min_severity: String,
    pub ntfy_min_severity: String,
    pub muted_kinds: Vec<String>,
    pub dnd_start_minute_utc: Option<u16>,
    pub dnd_end_minute_utc: Option<u16>,
    pub critical_bypasses_dnd: bool,
    pub retention_days: u16,
    pub ntfy_server: String,
    pub ntfy_topic: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NotificationMutation {
    Read,
    Acknowledge,
    Dismiss,
}

#[derive(Clone, Debug)]
pub(super) struct StreamFrame {
    pub cursor: u64,
    pub records: Vec<Value>,
    pub terminal: bool,
    pub job: Job,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ApprovalRequest {
    pub id: String,
    pub verb: String,
    pub scope: Value,
    pub reason: String,
    pub status: String,
    pub session: String,
    pub requested_at: u64,
    pub requester: Option<String>,
    pub risk: Option<String>,
    pub decided_at: Option<u64>,
    pub duration: Option<String>,
    pub note: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) struct SkillSummary {
    pub id: String,
    pub description: String,
    pub origin: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ReviewDecision {
    ApproveOnce,
    Deny,
}

#[async_trait]
pub(super) trait Backend: Send + Sync {
    fn info(&self) -> &BackendInfo;

    async fn create_conversation(&self) -> Result<Conversation, String>;
    async fn get_conversation(&self, id: &str) -> Result<Conversation, String>;
    async fn list_conversations(&self) -> Result<ConversationList, String>;
    async fn update_conversation(
        &self,
        id: &str,
        title: Option<&str>,
        archived: Option<bool>,
    ) -> Result<Conversation, String>;
    async fn fork_conversation(&self, id: &str) -> Result<Conversation, String>;
    async fn revert_conversation(&self, id: &str, user_turns: u32) -> Result<Conversation, String>;
    async fn submit(
        &self,
        prompt: &str,
        session_id: &str,
        use_memory: bool,
        max_turns: Option<u32>,
        model: &str,
    ) -> Result<Job, String>;
    async fn stream(&self, task_id: &str, cursor: u64) -> Result<StreamFrame, String>;
    async fn cancel(&self, task_id: &str) -> Result<(), String>;
    async fn list_tasks(&self) -> Result<Vec<TaskSummary>, String>;
    async fn get_task(&self, task_id: &str) -> Result<Job, String>;
    async fn retry_task(&self, task_id: &str) -> Result<Job, String>;
    async fn list_notifications(&self, include_dismissed: bool)
        -> Result<NotificationPage, String>;
    async fn mutate_notification(
        &self,
        id: &str,
        mutation: NotificationMutation,
    ) -> Result<NotificationItem, String>;
    async fn notification_preferences(&self) -> Result<NotificationPreferences, String>;
    async fn set_notification_preferences(
        &self,
        preferences: &NotificationPreferences,
    ) -> Result<NotificationPreferences, String>;
    async fn approvals(&self, ids: &[String]) -> Result<Vec<ApprovalRequest>, String>;
    async fn list_approvals(&self) -> Result<Vec<ApprovalRequest>, String>;
    async fn review(&self, id: &str, decision: ReviewDecision) -> Result<(), String>;
    async fn skills(&self) -> Result<Vec<SkillSummary>, String>;
}

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
        .map_err(|_| "Claw model readiness check failed".to_string())??;
        let models = super::models::catalog(Arc::new(config.agent.clone()), ready).await?;
        Ok(Self {
            info: BackendInfo {
                home,
                provider: config.agent.provider.clone(),
                model: config.agent.model.clone(),
                models,
            },
            socket: crate::clawd::config::socket_path(),
        })
    }

    async fn call(&self, command: Command, params: Value) -> Result<Value, String> {
        request(&self.socket, command, params).await
    }
}

#[async_trait]
impl Backend for BrokerBackend {
    fn info(&self) -> &BackendInfo {
        &self.info
    }

    async fn create_conversation(&self) -> Result<Conversation, String> {
        parse_conversation(
            self.call(Command::AgentConversationCreate, json!({}))
                .await?,
        )
    }

    async fn get_conversation(&self, id: &str) -> Result<Conversation, String> {
        parse_conversation(
            self.call(
                Command::AgentConversationGet,
                json!({ "id": id, "limit": 1_000 }),
            )
            .await?,
        )
    }

    async fn list_conversations(&self) -> Result<ConversationList, String> {
        let value = self
            .call(Command::AgentConversationList, json!({ "limit": 100 }))
            .await?;
        let values = value
            .get("conversations")
            .and_then(Value::as_array)
            .ok_or("Claw returned no conversation list")?;
        Ok(ConversationList {
            conversations: values
                .iter()
                .map(parse_conversation_summary)
                .collect::<Result<_, _>>()?,
            truncated: value
                .get("conversations_truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    async fn update_conversation(
        &self,
        id: &str,
        title: Option<&str>,
        archived: Option<bool>,
    ) -> Result<Conversation, String> {
        let mut params = json!({ "id": id });
        if let Some(title) = title {
            params["title"] = json!(title);
        }
        if let Some(archived) = archived {
            params["archived"] = json!(archived);
        }
        parse_conversation(self.call(Command::AgentConversationUpdate, params).await?)
    }

    async fn fork_conversation(&self, id: &str) -> Result<Conversation, String> {
        parse_conversation(
            self.call(Command::AgentConversationFork, json!({ "id": id }))
                .await?,
        )
    }

    async fn revert_conversation(&self, id: &str, user_turns: u32) -> Result<Conversation, String> {
        parse_conversation(
            self.call(
                Command::AgentConversationRevert,
                json!({ "id": id, "user_turns": user_turns }),
            )
            .await?,
        )
    }

    async fn submit(
        &self,
        prompt: &str,
        session_id: &str,
        use_memory: bool,
        max_turns: Option<u32>,
        model: &str,
    ) -> Result<Job, String> {
        let mut params = json!({
            "prompt": prompt,
            "session_id": session_id,
            "use_memory": use_memory,
            "model": model,
        });
        if let Some(max_turns) = max_turns {
            params["max_turns"] = json!(max_turns);
        }
        parse_job(self.call(Command::TaskSubmit, params).await?)
    }

    async fn stream(&self, task_id: &str, cursor: u64) -> Result<StreamFrame, String> {
        let value = self
            .call(
                Command::TaskStream,
                json!({ "id": task_id, "cursor": cursor, "timeout_ms": 1_000 }),
            )
            .await?;
        let next = value
            .get("cursor")
            .and_then(Value::as_u64)
            .ok_or("Claw task stream omitted its cursor")?;
        let records = value
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .ok_or("Claw task stream omitted its events")?;
        if next < cursor || (!records.is_empty() && next == cursor) || records.len() > 16_384 {
            return Err("Claw task stream returned an invalid cursor or event count".into());
        }
        let terminal = value
            .get("terminal")
            .and_then(Value::as_bool)
            .ok_or("Claw task stream omitted its terminal state")?;
        let job = parse_job(
            value
                .get("job")
                .cloned()
                .ok_or("Claw task stream omitted its job")?,
        )?;
        if job.id != task_id {
            return Err("Claw task stream changed task identity".into());
        }
        Ok(StreamFrame {
            cursor: next,
            records,
            terminal,
            job,
        })
    }

    async fn cancel(&self, task_id: &str) -> Result<(), String> {
        let value = self
            .call(Command::TaskCancel, json!({ "id": task_id }))
            .await?;
        if value.get("id").and_then(Value::as_str) != Some(task_id) {
            return Err("Claw cancelled a different task".into());
        }
        if value.get("cancelled").and_then(Value::as_bool) != Some(true)
            && value.get("cancel_requested").and_then(Value::as_bool) != Some(true)
        {
            return Err("The task was already terminal before cancellation".into());
        }
        Ok(())
    }

    async fn list_tasks(&self) -> Result<Vec<TaskSummary>, String> {
        let value = self
            .call(Command::TaskList, json!({ "limit": 100, "summary": true }))
            .await?;
        let values = value
            .get("jobs")
            .and_then(Value::as_array)
            .ok_or("Claw task list omitted jobs")?;
        if values.len() > 100 {
            return Err("Claw task list exceeded its terminal bound".into());
        }
        values.iter().map(parse_task_summary).collect()
    }

    async fn get_task(&self, task_id: &str) -> Result<Job, String> {
        validate_token(task_id, "task id")?;
        let job = parse_job(
            self.call(Command::TaskGet, json!({ "id": task_id }))
                .await?,
        )?;
        if job.id != task_id {
            return Err("Claw returned a different task".into());
        }
        Ok(job)
    }

    async fn retry_task(&self, task_id: &str) -> Result<Job, String> {
        validate_token(task_id, "task id")?;
        let job = parse_job(
            self.call(Command::TaskRetry, json!({ "id": task_id }))
                .await?,
        )?;
        if job.id == task_id || job.status != "pending" {
            return Err("Claw returned an invalid retry task".into());
        }
        Ok(job)
    }

    async fn list_notifications(
        &self,
        include_dismissed: bool,
    ) -> Result<NotificationPage, String> {
        let value = self
            .call(
                Command::NotificationList,
                json!({ "include_dismissed": include_dismissed, "limit": 100 }),
            )
            .await?;
        let values = value
            .get("notifications")
            .and_then(Value::as_array)
            .ok_or("Claw notification list omitted notifications")?;
        if values.len() > 100 {
            return Err("Claw notification list exceeded its terminal bound".into());
        }
        Ok(NotificationPage {
            notifications: values
                .iter()
                .map(parse_notification)
                .collect::<Result<_, _>>()?,
            unread: value
                .get("unread")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .ok_or("Claw notification list omitted unread count")?,
        })
    }

    async fn mutate_notification(
        &self,
        id: &str,
        mutation: NotificationMutation,
    ) -> Result<NotificationItem, String> {
        validate_token(id, "notification id")?;
        let command = match mutation {
            NotificationMutation::Read => Command::NotificationRead,
            NotificationMutation::Acknowledge => Command::NotificationAcknowledge,
            NotificationMutation::Dismiss => Command::NotificationDismiss,
        };
        let notification = parse_notification(&self.call(command, json!({ "id": id })).await?)?;
        if notification.id != id {
            return Err("Claw mutated a different notification".into());
        }
        Ok(notification)
    }

    async fn notification_preferences(&self) -> Result<NotificationPreferences, String> {
        parse_notification_preferences(
            &self
                .call(Command::NotificationPreferencesGet, json!({}))
                .await?,
        )
    }

    async fn set_notification_preferences(
        &self,
        preferences: &NotificationPreferences,
    ) -> Result<NotificationPreferences, String> {
        let value = self
            .call(
                Command::NotificationPreferencesSet,
                json!({
                    "web_enabled": preferences.web_enabled,
                    "desktop_enabled": preferences.desktop_enabled,
                    "ntfy_enabled": preferences.ntfy_enabled,
                    "web_min_severity": preferences.web_min_severity,
                    "desktop_min_severity": preferences.desktop_min_severity,
                    "ntfy_min_severity": preferences.ntfy_min_severity,
                    "muted_kinds": preferences.muted_kinds,
                    "dnd_start_minute_utc": preferences.dnd_start_minute_utc,
                    "dnd_end_minute_utc": preferences.dnd_end_minute_utc,
                    "critical_bypasses_dnd": preferences.critical_bypasses_dnd,
                    "retention_days": preferences.retention_days,
                    "ntfy_server": preferences.ntfy_server,
                    "ntfy_topic": preferences.ntfy_topic,
                }),
            )
            .await?;
        parse_notification_preferences(&value)
    }

    async fn approvals(&self, ids: &[String]) -> Result<Vec<ApprovalRequest>, String> {
        let value = self
            .call(Command::PermissionPending, json!({ "limit": 1_000 }))
            .await?;
        let requests = value
            .get("requests")
            .and_then(Value::as_array)
            .ok_or("Claw approval list omitted requests")?;
        ids.iter()
            .map(|id| {
                let request = requests
                    .iter()
                    .find(|request| request.get("id").and_then(Value::as_str) == Some(id))
                    .ok_or_else(|| format!("approval {id} is no longer pending"))?;
                parse_approval(request, "pending")
            })
            .collect()
    }

    async fn list_approvals(&self) -> Result<Vec<ApprovalRequest>, String> {
        let pending = self
            .call(Command::PermissionPending, json!({ "limit": 32 }))
            .await?;
        let recent = self
            .call(Command::PermissionRecent, json!({ "limit": 32 }))
            .await?;
        let pending = pending
            .get("requests")
            .and_then(Value::as_array)
            .ok_or("Claw pending approval list omitted requests")?;
        let recent = recent
            .get("requests")
            .and_then(Value::as_array)
            .ok_or("Claw recent approval list omitted requests")?;
        if pending.len() > 32 || recent.len() > 32 {
            return Err("Claw approval list exceeded its terminal bound".into());
        }
        let mut approvals = pending
            .iter()
            .map(|request| parse_approval(request, "pending"))
            .chain(recent.iter().map(|request| {
                let status = request
                    .get("decision")
                    .and_then(|decision| decision.get("outcome"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                parse_approval(request, status)
            }))
            .collect::<Result<Vec<_>, _>>()?;
        if approvals.is_empty() {
            return Ok(approvals);
        }
        let ids = approvals
            .iter()
            .map(|approval| approval.id.as_str())
            .collect::<Vec<_>>();
        let value = self
            .call(Command::PermissionStatus, json!({ "ids": ids }))
            .await?;
        let statuses = value
            .get("statuses")
            .and_then(Value::as_array)
            .ok_or("Claw approval status list omitted statuses")?
            .iter()
            .map(|status| {
                Ok((
                    required_string(status, "id")?,
                    required_string(status, "status")?,
                ))
            })
            .collect::<Result<HashMap<_, _>, String>>()?;
        for approval in &mut approvals {
            approval.status = statuses
                .get(&approval.id)
                .cloned()
                .ok_or_else(|| format!("Claw omitted status for approval {}", approval.id))?;
        }
        Ok(approvals)
    }

    async fn review(&self, id: &str, decision: ReviewDecision) -> Result<(), String> {
        validate_token(id, "approval id")?;
        let pending = self
            .call(Command::PermissionStatus, json!({ "ids": [id] }))
            .await?;
        if !has_approval_status(&pending, id, "pending") {
            return Err("the root approval request is no longer pending".into());
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
            .map_err(|_| "the installed Claw approval helper is unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or("approval helper stdout is unavailable")?;
        let stderr = child
            .stderr
            .take()
            .ok_or("approval helper stderr is unavailable")?;
        let result = tokio::time::timeout(Duration::from_secs(120), async {
            tokio::try_join!(
                read_helper_output(stdout),
                read_helper_output(stderr),
                async {
                    child
                        .wait()
                        .await
                        .map_err(|_| "could not wait for approval authorization".to_string())
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
                return Err("approval authorization timed out".into());
            }
        };
        if !status.success() {
            return Err("approval authorization was denied or cancelled".into());
        }
        let response: Value = serde_json::from_slice(&stdout)
            .map_err(|_| "approval helper returned invalid JSON".to_string())?;
        if response.get("id").and_then(Value::as_str) != Some(id)
            || response.get("decision").and_then(Value::as_str) != Some(expected)
        {
            return Err("approval helper returned a mismatched decision".into());
        }
        let status = self
            .call(Command::PermissionStatus, json!({ "ids": [id] }))
            .await?;
        if !has_approval_status(&status, id, expected) {
            return Err("the root approval decision could not be confirmed".into());
        }
        Ok(())
    }

    async fn skills(&self) -> Result<Vec<SkillSummary>, String> {
        let loaded =
            tokio::task::spawn_blocking(crate::agent::skills::loader::load_catalog_default)
                .await
                .map_err(|_| "Claw Skill catalogue is unavailable".to_string())?;
        if loaded.skills.len() + loaded.errors.len() + loaded.disabled.len() > 1_000 {
            return Err("Claw Skill catalogue exceeds its terminal bound".into());
        }
        Ok(loaded
            .skills
            .values()
            .map(|skill| SkillSummary {
                id: skill.id.clone(),
                description: skill.manifest.description.clone().unwrap_or_default(),
                origin: match skill.origin {
                    crate::agent::skills::loader::SkillOrigin::BuiltIn => "system",
                    crate::agent::skills::loader::SkillOrigin::User => "user",
                    crate::agent::skills::loader::SkillOrigin::Local => "repo",
                },
            })
            .collect())
    }
}

async fn request(
    socket: &std::path::Path,
    command: Command,
    params: Value,
) -> Result<Value, String> {
    let response = tokio::time::timeout(
        Duration::from_secs(35),
        crate::clawd::client::request(socket, Request::new(command, params)),
    )
    .await
    .map_err(|_| "Claw broker request timed out; its outcome is unknown".to_string())??;
    if response.ok {
        response
            .result
            .ok_or_else(|| "Claw broker returned no result".to_string())
    } else {
        Err(response
            .error
            .map(|error| format!("{}: {}", error.code, error.message))
            .unwrap_or_else(|| "Claw broker returned no error".to_string()))
    }
}

fn parse_conversation(value: Value) -> Result<Conversation, String> {
    let value = value
        .get("conversation")
        .filter(|value| value.is_object())
        .ok_or("Claw returned no conversation")?;
    let messages = value
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|message| {
            let role = message.get("role")?.as_str()?.to_string();
            let text = message
                .get("text")
                .or_else(|| message.get("content"))
                .and_then(Value::as_str)?
                .to_string();
            Some(ConversationMessage { role, text })
        })
        .collect();
    Ok(Conversation {
        id: required_string(value, "id")?,
        title: value
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("New conversation")
            .to_string(),
        archived: value
            .get("archived")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        history_truncated: value
            .get("messages_truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        messages,
    })
}

fn parse_conversation_summary(value: &Value) -> Result<ConversationSummary, String> {
    Ok(ConversationSummary {
        id: required_string(value, "id")?,
        title: value
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("New conversation")
            .to_string(),
        archived: value
            .get("archived")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn parse_job(value: Value) -> Result<Job, String> {
    Ok(Job {
        id: required_string(&value, "id")?,
        session_id: required_string(&value, "session_id")?,
        activity_id: optional_string(&value, "activity_id"),
        prompt: required_string(&value, "prompt")?,
        status: required_string(&value, "status")?,
        created_at: required_string(&value, "created_at")?,
        started_at: optional_string(&value, "started_at"),
        finished_at: optional_string(&value, "finished_at"),
        response: optional_string(&value, "response"),
        error: optional_string(&value, "error"),
        requested_model: optional_string(&value, "requested_model"),
        provider: optional_string(&value, "provider"),
        model: optional_string(&value, "model"),
        turns_used: value
            .get("turns_used")
            .and_then(Value::as_u64)
            .map(|turns| {
                u32::try_from(turns).map_err(|_| "Claw task turn count is too large".to_string())
            })
            .transpose()?,
    })
}

fn parse_task_summary(value: &Value) -> Result<TaskSummary, String> {
    Ok(TaskSummary {
        id: required_string(value, "id")?,
        title: required_string(value, "title")?,
        status: required_string(value, "status")?,
        created_at: required_string(value, "created_at")?,
        session_id: optional_string(value, "session_id"),
        activity_id: optional_string(value, "activity_id"),
        waiting_on: value
            .get("waiting_on")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
        cancel_requested: value
            .get("cancel_requested")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        error: optional_string(value, "error"),
    })
}

fn parse_approval(value: &Value, status: &str) -> Result<ApprovalRequest, String> {
    let decision = value.get("decision").filter(|value| value.is_object());
    Ok(ApprovalRequest {
        id: required_string(value, "id")?,
        verb: required_string(value, "verb")?,
        scope: value
            .get("scope")
            .cloned()
            .ok_or("approval request omitted scope")?,
        reason: required_string(value, "reason")?,
        status: status.to_string(),
        session: required_string(value, "session")?,
        requested_at: value
            .get("requested_at")
            .and_then(Value::as_u64)
            .ok_or("approval request omitted requested_at")?,
        requester: optional_string(value, "requester"),
        risk: optional_string(value, "risk"),
        decided_at: decision
            .and_then(|decision| decision.get("decided_at"))
            .and_then(Value::as_u64),
        duration: decision.and_then(|decision| optional_string(decision, "duration")),
        note: decision.and_then(|decision| optional_string(decision, "note")),
    })
}

fn parse_notification(value: &Value) -> Result<NotificationItem, String> {
    let actions = value
        .get("actions")
        .and_then(Value::as_array)
        .ok_or("notification omitted actions")?
        .iter()
        .map(|action| {
            Ok(NotificationAction {
                label: required_string(action, "label")?,
                uri: required_string(action, "uri")?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let deliveries = value
        .get("deliveries")
        .and_then(Value::as_array)
        .ok_or("notification omitted deliveries")?
        .iter()
        .map(|delivery| {
            Ok(NotificationDelivery {
                channel: required_string(delivery, "channel")?,
                state: required_string(delivery, "state")?,
                attempts: required_u32(delivery, "attempts")?,
                last_error_code: optional_string(delivery, "last_error_code"),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(NotificationItem {
        id: required_string(value, "id")?,
        source: required_string(value, "source")?,
        kind: required_string(value, "kind")?,
        severity: required_string(value, "severity")?,
        title: required_string(value, "title")?,
        body: required_string(value, "body")?,
        delivery_policy: required_string(value, "delivery_policy")?,
        state: required_string(value, "state")?,
        occurrences: required_u32(value, "occurrences")?,
        created_at_ms: value
            .get("created_at_ms")
            .and_then(Value::as_i64)
            .ok_or("notification omitted created_at_ms")?,
        updated_at_ms: value
            .get("updated_at_ms")
            .and_then(Value::as_i64)
            .ok_or("notification omitted updated_at_ms")?,
        task_id: optional_string(value, "task_id"),
        session_id: optional_string(value, "session_id"),
        job_id: optional_string(value, "job_id"),
        actions,
        deliveries,
    })
}

fn parse_notification_preferences(value: &Value) -> Result<NotificationPreferences, String> {
    Ok(NotificationPreferences {
        web_enabled: required_bool(value, "web_enabled")?,
        desktop_enabled: required_bool(value, "desktop_enabled")?,
        ntfy_enabled: required_bool(value, "ntfy_enabled")?,
        web_min_severity: required_string(value, "web_min_severity")?,
        desktop_min_severity: required_string(value, "desktop_min_severity")?,
        ntfy_min_severity: required_string(value, "ntfy_min_severity")?,
        muted_kinds: value
            .get("muted_kinds")
            .and_then(Value::as_array)
            .ok_or("notification preferences omitted muted_kinds")?
            .iter()
            .map(|kind| {
                kind.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| "notification muted kind is not a string".to_string())
            })
            .collect::<Result<_, _>>()?,
        dnd_start_minute_utc: optional_u16(value, "dnd_start_minute_utc")?,
        dnd_end_minute_utc: optional_u16(value, "dnd_end_minute_utc")?,
        critical_bypasses_dnd: required_bool(value, "critical_bypasses_dnd")?,
        retention_days: required_u16(value, "retention_days")?,
        ntfy_server: required_string(value, "ntfy_server")?,
        ntfy_topic: optional_string(value, "ntfy_topic"),
    })
}

fn required_string(value: &Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("Claw response omitted {key}"))
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn required_bool(value: &Value, key: &str) -> Result<bool, String> {
    value
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("Claw response omitted {key}"))
}

fn required_u32(value: &Value, key: &str) -> Result<u32, String> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("Claw response omitted {key}"))
        .and_then(|value| {
            u32::try_from(value).map_err(|_| format!("Claw response {key} is too large"))
        })
}

fn required_u16(value: &Value, key: &str) -> Result<u16, String> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("Claw response omitted {key}"))
        .and_then(|value| {
            u16::try_from(value).map_err(|_| format!("Claw response {key} is too large"))
        })
}

fn optional_u16(value: &Value, key: &str) -> Result<Option<u16>, String> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .map(|value| u16::try_from(value).map_err(|_| format!("Claw response {key} is too large")))
        .transpose()
}

fn validate_token(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':' | b'.'))
    {
        return Err(format!("invalid {field}"));
    }

    Ok(())
}

fn has_approval_status(value: &Value, id: &str, expected: &str) -> bool {
    value
        .get("statuses")
        .and_then(Value::as_array)
        .is_some_and(|statuses| {
            statuses.iter().any(|status| {
                status.get("id").and_then(Value::as_str) == Some(id)
                    && status.get("status").and_then(Value::as_str) == Some(expected)
            })
        })
}

async fn read_helper_output(reader: impl AsyncRead + Unpin) -> Result<Vec<u8>, String> {
    const MAX_HELPER_BYTES: u64 = 16 * 1024;
    let mut output = Vec::new();
    reader
        .take(MAX_HELPER_BYTES + 1)
        .read_to_end(&mut output)
        .await
        .map_err(|_| "could not read approval helper response".to_string())?;
    if output.len() > MAX_HELPER_BYTES as usize {
        return Err("approval helper response exceeds its size limit".into());
    }
    Ok(output)
}
