use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::clawd::protocol::Request;
use crate::clawd::routes::Command;
use crate::config::CosConfig;

pub(super) use crate::agent::memory::maintenance::LearnedMemoryReset;

const PKEXEC_PATH: &str = "/usr/bin/pkexec";
const APPROVAL_HELPER_PATH: &str = "/usr/local/bin/claw-approval-helper";

#[derive(Clone, Debug)]
pub(super) struct BackendInfo {
    pub home: PathBuf,
    pub provider: String,
    pub model: String,
    pub models: Vec<String>,
    pub provider_ready: bool,
    pub model_catalog_warning: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) struct ConversationMessage {
    pub role: String,
    pub text: String,
}

#[derive(Clone, Debug)]
pub(super) struct ConversationJob {
    pub id: String,
    pub status: String,
    pub session_id: String,
}

#[derive(Clone, Debug)]
pub(super) struct Conversation {
    pub id: String,
    pub title: String,
    pub archived: bool,
    pub history_truncated: bool,
    pub messages: Vec<ConversationMessage>,
    pub jobs: Vec<ConversationJob>,
    pub jobs_truncated: bool,
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
    pub workspace: Option<String>,
    pub after_task_id: Option<String>,
    pub prompt: String,
    pub status: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub response: Option<String>,
    pub error: Option<String>,
    pub requested_model: Option<String>,
    pub requested_reasoning_effort: Option<String>,
    pub plan_only: bool,
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
    pub workspace: Option<String>,
    pub after_task_id: Option<String>,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ActivityResource {
    pub label: String,
    pub reference: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Activity {
    pub id: String,
    pub title: String,
    pub goal: String,
    pub completion_criteria: String,
    pub boundaries: String,
    pub resources: Vec<ActivityResource>,
    pub state: String,
    pub completion_note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ActivityJob {
    pub id: String,
    pub title: String,
    pub status: String,
    pub session_id: Option<String>,
    pub created_at: String,
    pub finished_at: Option<String>,
    pub response: Option<String>,
    pub error: Option<String>,
    pub waiting_on: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ActivityDetail {
    pub activity: Activity,
    pub jobs: Vec<ActivityJob>,
    pub sessions: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ActivityAttention {
    pub activity_id: String,
    pub activity_state: String,
    pub presentation: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ActivityControls {
    pub activity_id: String,
    pub execution_limits: Option<Value>,
    pub monetary_budget: Option<Value>,
    pub scheduling_policy: Option<Value>,
    pub capability_policy: Option<Value>,
}

impl ActivityControls {
    pub fn execution_revision(&self) -> Option<u64> {
        policy_revision(&self.execution_limits)
    }

    pub fn execution_enabled(&self) -> Option<bool> {
        policy_enabled(&self.execution_limits)
    }

    pub fn monetary_revision(&self) -> Option<u64> {
        policy_revision(&self.monetary_budget)
    }

    pub fn monetary_enabled(&self) -> Option<bool> {
        policy_enabled(&self.monetary_budget)
    }

    pub fn scheduling_revision(&self) -> Option<u64> {
        policy_revision(&self.scheduling_policy)
    }

    pub fn scheduling_priority(&self) -> Option<&str> {
        self.scheduling_policy
            .as_ref()
            .and_then(|policy| policy.get("priority"))
            .and_then(Value::as_str)
    }

    pub fn capability_revision(&self) -> Option<u64> {
        policy_revision(&self.capability_policy)
    }

    pub fn capability_enabled(&self) -> Option<bool> {
        policy_enabled(&self.capability_policy)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ActivityControlPolicy {
    ExecutionLimits,
    MonetaryBudget,
    CapabilityPolicy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ActivityEvidence {
    pub activity_id: String,
    pub presentation: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ActivityReview {
    pub activity_id: String,
    pub presentation: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ActivityOperationPreview {
    pub activity_id: String,
    pub presentation: String,
}

#[derive(Clone, Debug)]
pub(super) struct StreamFrame {
    pub cursor: u64,
    pub records: Vec<Value>,
    pub terminal: bool,
    pub job: Job,
}

pub(super) enum StreamError {
    Retryable(String),
    Fatal(String),
}

pub(super) enum InitialConnectionError {
    Retryable(String),
    Fatal(String),
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PlatformOverview {
    pub presentation: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AgentHookSettings {
    pub logging: bool,
    pub audit: bool,
    pub checkpoint: bool,
    pub updated_kind: Option<String>,
    pub changed: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct McpServerSummary {
    pub name: String,
    pub source: &'static str,
    pub enabled: bool,
    pub transport: &'static str,
    pub timeout_secs: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct McpOverview {
    pub servers: Vec<McpServerSummary>,
    pub discovery_enabled: bool,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ExtensionSummary {
    pub kind: &'static str,
    pub id: String,
    pub status: &'static str,
    pub trust: String,
    pub diagnostic: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ExtensionsOverview {
    pub entries: Vec<ExtensionSummary>,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum UsagePeriod {
    Daily,
    Weekly,
    Cumulative,
}

impl UsagePeriod {
    pub fn label(self) -> &'static str {
        match self {
            Self::Daily => "last 24 hours",
            Self::Weekly => "last 7 days",
            Self::Cumulative => "all retained usage",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct UsageBreakdown {
    pub name: String,
    pub totals: crate::agent::llm::usage::Totals,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct UsageOverview {
    pub period: UsagePeriod,
    pub total: crate::agent::llm::usage::Totals,
    pub providers: Vec<UsageBreakdown>,
    pub models: Vec<UsageBreakdown>,
    pub parse_errors: usize,
    pub log_lines: u64,
    pub log_bytes: u64,
    pub breakdown_truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct DebugOverview {
    pub daemon: String,
    pub daemon_status: String,
    pub started_at: String,
    pub uptime_ms: u64,
    pub provider: String,
    pub model: String,
    pub provider_ready: bool,
    pub model_count: usize,
    pub model_catalog_warning: Option<String>,
    pub max_turns: u32,
    pub reasoning_effort: Option<String>,
    pub compression_enabled: bool,
    pub memory_redaction_enabled: bool,
    pub progressive_tools_enabled: bool,
    pub tool_allow_count: Option<usize>,
    pub tool_deny_count: usize,
    pub configured_mcp_count: usize,
    pub mcp_discovery_enabled: bool,
    pub selected_extension_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ReviewDecision {
    ApproveOnce,
    Deny,
}

#[async_trait]
pub(super) trait Backend: Send + Sync {
    fn info(&self) -> &BackendInfo;

    async fn initial_conversation(
        &self,
        session_id: Option<&str>,
    ) -> Result<Conversation, InitialConnectionError>;
    async fn create_conversation(&self) -> Result<Conversation, String>;
    async fn get_conversation(&self, id: &str) -> Result<Conversation, String>;
    async fn list_conversations(&self) -> Result<ConversationList, String>;
    async fn update_conversation(
        &self,
        id: &str,
        title: Option<&str>,
        archived: Option<bool>,
    ) -> Result<Conversation, String>;
    async fn fork_conversation(
        &self,
        id: &str,
        before_user_turn: Option<u32>,
    ) -> Result<Conversation, String>;
    async fn revert_conversation(&self, id: &str, user_turns: u32) -> Result<Conversation, String>;
    async fn submit(
        &self,
        prompt: &str,
        attachments: &[crate::agent::attachments::AttachmentInput],
        session_id: &str,
        workspace: &str,
        after_task_id: Option<&str>,
        use_memory: bool,
        max_turns: Option<u32>,
        model: &str,
        reasoning_effort: Option<&str>,
        plan_only: bool,
    ) -> Result<Job, String>;
    async fn resolve_workspace(&self, path: Option<&str>) -> Result<String, String>;
    async fn stream(&self, task_id: &str, cursor: u64) -> Result<StreamFrame, StreamError>;
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
    async fn list_activities(&self, state: Option<&str>) -> Result<Vec<Activity>, String>;
    async fn get_activity(&self, id: &str) -> Result<ActivityDetail, String>;
    async fn create_activity(&self, title: &str, goal: &str) -> Result<Activity, String>;
    async fn transition_activity(
        &self,
        id: &str,
        state: &str,
        completion_note: Option<&str>,
    ) -> Result<Activity, String>;
    async fn run_activity(
        &self,
        id: &str,
        prompt: Option<&str>,
        workspace: &str,
    ) -> Result<Job, String>;
    async fn activity_attention(&self, id: &str) -> Result<ActivityAttention, String>;
    async fn activity_controls(&self, id: &str) -> Result<ActivityControls, String>;
    async fn set_activity_control(
        &self,
        id: &str,
        policy: ActivityControlPolicy,
        expected_revision: Option<u64>,
        draft: Value,
    ) -> Result<ActivityControls, String>;
    async fn set_activity_control_enabled(
        &self,
        id: &str,
        policy: ActivityControlPolicy,
        revision: u64,
        enabled: bool,
    ) -> Result<ActivityControls, String>;
    async fn set_activity_priority(
        &self,
        id: &str,
        expected_revision: Option<u64>,
        priority: &str,
    ) -> Result<ActivityControls, String>;
    async fn activity_evidence(&self, id: &str) -> Result<ActivityEvidence, String>;
    async fn activity_review(&self, id: &str) -> Result<ActivityReview, String>;
    async fn activity_operation_preview(
        &self,
        id: &str,
        app_id: &str,
        operation: &str,
        args: &[String],
    ) -> Result<ActivityOperationPreview, String>;
    async fn approvals(&self, ids: &[String]) -> Result<Vec<ApprovalRequest>, String>;
    async fn list_approvals(&self) -> Result<Vec<ApprovalRequest>, String>;
    async fn review(&self, id: &str, decision: ReviewDecision) -> Result<(), String>;
    async fn skills(&self) -> Result<Vec<SkillSummary>, String>;
    async fn platform_overview(&self) -> Result<PlatformOverview, String>;
    async fn reset_memories(&self) -> Result<LearnedMemoryReset, String>;
    async fn agent_hooks(&self) -> Result<AgentHookSettings, String>;
    async fn set_agent_hook(&self, kind: &str, enabled: bool)
        -> Result<AgentHookSettings, String>;
    async fn mcp_overview(&self) -> Result<McpOverview, String>;
    async fn extensions_overview(&self) -> Result<ExtensionsOverview, String>;
    async fn usage_overview(&self, period: UsagePeriod) -> Result<UsageOverview, String>;
    async fn debug_overview(&self) -> Result<DebugOverview, String>;
}

pub(super) struct BrokerBackend {
    info: BackendInfo,
    socket: PathBuf,
    config: Arc<CosConfig>,
    owner_uid: u32,
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
        let catalog = super::models::catalog(Arc::new(config.agent.clone()), ready).await?;
        Ok(Self {
            info: BackendInfo {
                home,
                provider: config.agent.provider.clone(),
                model: config.agent.model.clone(),
                models: catalog.models,
                provider_ready: ready,
                model_catalog_warning: catalog.warning,
            },
            socket: crate::clawd::config::socket_path(),
            config,
            owner_uid: uid,
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

    async fn initial_conversation(
        &self,
        session_id: Option<&str>,
    ) -> Result<Conversation, InitialConnectionError> {
        let (command, params, replay_safe) = match session_id {
            Some(id) => (
                Command::AgentConversationGet,
                json!({ "id": id, "limit": 1_000 }),
                true,
            ),
            None => (Command::AgentConversationCreate, json!({}), false),
        };
        let value = initial_request(&self.socket, command, params, replay_safe).await?;
        parse_conversation(value).map_err(InitialConnectionError::Fatal)
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

    async fn fork_conversation(
        &self,
        id: &str,
        before_user_turn: Option<u32>,
    ) -> Result<Conversation, String> {
        let mut params = json!({ "id": id });
        if let Some(before_user_turn) = before_user_turn {
            params["before_user_turn"] = json!(before_user_turn);
        }
        parse_conversation(self.call(Command::AgentConversationFork, params).await?)
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
        attachments: &[crate::agent::attachments::AttachmentInput],
        session_id: &str,
        workspace: &str,
        after_task_id: Option<&str>,
        use_memory: bool,
        max_turns: Option<u32>,
        model: &str,
        reasoning_effort: Option<&str>,
        plan_only: bool,
    ) -> Result<Job, String> {
        let mut params = json!({
            "prompt": prompt,
            "session_id": session_id,
            "workspace": workspace,
            "use_memory": use_memory,
            "model": model,
        });
        if !attachments.is_empty() {
            params["attachments"] =
                serde_json::to_value(attachments).map_err(|error| error.to_string())?;
        }
        if let Some(max_turns) = max_turns {
            params["max_turns"] = json!(max_turns);
        }
        if let Some(reasoning_effort) = reasoning_effort {
            params["reasoning_effort"] = json!(reasoning_effort);
        }
        if plan_only {
            params["plan_only"] = json!(true);
        }
        if let Some(after_task_id) = after_task_id {
            params["after_task_id"] = json!(after_task_id);
        }
        parse_job(self.call(Command::TaskSubmit, params).await?)
    }

    async fn resolve_workspace(&self, path: Option<&str>) -> Result<String, String> {
        let mut params = json!({});
        if let Some(path) = path {
            params["path"] = json!(path);
        }
        let value = self.call(Command::TaskWorkspaceResolve, params).await?;
        required_string(&value, "workspace")
    }

    async fn stream(
        &self,
        task_id: &str,
        cursor: u64,
    ) -> Result<StreamFrame, StreamError> {
        let request = Request::new(
            Command::TaskStream,
            json!({ "id": task_id, "cursor": cursor, "timeout_ms": 1_000 }),
        );
        let response = tokio::time::timeout(
            Duration::from_secs(35),
            crate::clawd::client::request(&self.socket, request),
        )
        .await
        .map_err(|_| {
            StreamError::Retryable("Claw broker task stream timed out".to_string())
        })?
        .map_err(|error| StreamError::Retryable(error.to_string()))?;
        let value = if response.ok {
            response
                .result
                .ok_or_else(|| StreamError::Fatal("Claw broker returned no result".to_string()))?
        } else {
            return Err(StreamError::Fatal(
                response
                    .error
                    .map(|error| format!("{}: {}", error.code, error.message))
                    .unwrap_or_else(|| "Claw broker returned no error".to_string()),
            ));
        };
        let next = value
            .get("cursor")
            .and_then(Value::as_u64)
            .ok_or_else(|| StreamError::Fatal("Claw task stream omitted its cursor".to_string()))?;
        let records = value
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| StreamError::Fatal("Claw task stream omitted its events".to_string()))?;
        if next < cursor || (!records.is_empty() && next == cursor) || records.len() > 16_384 {
            return Err(StreamError::Fatal(
                "Claw task stream returned an invalid cursor or event count".to_string(),
            ));
        }
        let terminal = value
            .get("terminal")
            .and_then(Value::as_bool)
            .ok_or_else(|| {
                StreamError::Fatal("Claw task stream omitted its terminal state".to_string())
            })?;
        let job = parse_job(
            value
                .get("job")
                .cloned()
                .ok_or_else(|| {
                    StreamError::Fatal("Claw task stream omitted its job".to_string())
                })?,
        )
        .map_err(StreamError::Fatal)?;
        if job.id != task_id {
            return Err(StreamError::Fatal(
                "Claw task stream changed task identity".to_string(),
            ));
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

    async fn list_activities(&self, state: Option<&str>) -> Result<Vec<Activity>, String> {
        let mut params = json!({ "limit": 100 });
        if let Some(state) = state {
            params["state"] = json!(state);
        }
        let value = self.call(Command::ActivityList, params).await?;
        let activities = value
            .get("activities")
            .and_then(Value::as_array)
            .ok_or("Claw Activity list omitted activities")?;
        if activities.len() > 100 {
            return Err("Claw Activity list exceeded its terminal bound".into());
        }
        activities.iter().map(parse_activity).collect()
    }

    async fn get_activity(&self, id: &str) -> Result<ActivityDetail, String> {
        validate_token(id, "Activity id")?;
        let value = self
            .call(Command::ActivityGet, json!({ "id": id, "limit": 100 }))
            .await?;
        let activity = parse_activity(
            value
                .get("activity")
                .ok_or("Claw Activity detail omitted activity")?,
        )?;
        if activity.id != id {
            return Err("Claw returned a different Activity".into());
        }
        let jobs = value
            .get("jobs")
            .and_then(Value::as_array)
            .ok_or("Claw Activity detail omitted jobs")?;
        let sessions = value
            .get("sessions")
            .and_then(Value::as_array)
            .ok_or("Claw Activity detail omitted sessions")?;
        if jobs.len() > 100 || sessions.len() > 100 {
            return Err("Claw Activity detail exceeded its terminal bound".into());
        }
        Ok(ActivityDetail {
            activity,
            jobs: jobs
                .iter()
                .map(parse_activity_job)
                .collect::<Result<_, _>>()?,
            sessions: sessions
                .iter()
                .map(|session| {
                    session
                        .as_str()
                        .map(str::to_string)
                        .ok_or_else(|| "Claw Activity session is not a string".to_string())
                })
                .collect::<Result<_, _>>()?,
        })
    }

    async fn create_activity(&self, title: &str, goal: &str) -> Result<Activity, String> {
        parse_activity(
            &self
                .call(
                    Command::ActivityCreate,
                    json!({ "title": title, "goal": goal }),
                )
                .await?,
        )
    }

    async fn transition_activity(
        &self,
        id: &str,
        state: &str,
        completion_note: Option<&str>,
    ) -> Result<Activity, String> {
        validate_token(id, "Activity id")?;
        let mut params = json!({ "id": id, "state": state });
        if let Some(note) = completion_note {
            params["completion_note"] = json!(note);
        }
        let activity = parse_activity(&self.call(Command::ActivityTransition, params).await?)?;
        if activity.id != id || activity.state != state {
            return Err("Claw returned a mismatched Activity transition".into());
        }
        Ok(activity)
    }

    async fn run_activity(
        &self,
        id: &str,
        prompt: Option<&str>,
        workspace: &str,
    ) -> Result<Job, String> {
        validate_token(id, "Activity id")?;
        let mut params = json!({ "id": id, "workspace": workspace });
        if let Some(prompt) = prompt {
            params["prompt"] = json!(prompt);
        }
        let job = parse_job(self.call(Command::ActivityRun, params).await?)?;
        if job.activity_id.as_deref() != Some(id) || job.workspace.as_deref() != Some(workspace) {
            return Err(
                "Claw returned an Activity task with mismatched identity or workspace".into(),
            );
        }
        Ok(job)
    }

    async fn activity_attention(&self, id: &str) -> Result<ActivityAttention, String> {
        validate_token(id, "Activity id")?;
        let value = self
            .call(
                Command::ActivityAttention,
                json!({ "id": id, "limit": 100 }),
            )
            .await?;
        let activity_id = required_string(&value, "activity_id")?;
        if activity_id != id {
            return Err("Claw returned attention for another Activity".into());
        }
        let activity_state = required_string(&value, "activity_state")?;
        let presentation = serde_json::to_string_pretty(&value)
            .map_err(|_| "Claw Activity attention could not be rendered".to_string())?;
        if presentation.len() > 512 * 1024 {
            return Err("Claw Activity attention exceeded its terminal bound".into());
        }
        Ok(ActivityAttention {
            activity_id,
            activity_state,
            presentation,
        })
    }

    async fn activity_controls(&self, id: &str) -> Result<ActivityControls, String> {
        validate_token(id, "Activity id")?;
        let execution = self
            .call(Command::ActivityExecutionLimitsGet, json!({ "id": id }))
            .await?;
        let monetary = self
            .call(Command::ActivityMonetaryBudgetGet, json!({ "id": id }))
            .await?;
        let scheduling = self
            .call(Command::ActivitySchedulingPolicyGet, json!({ "id": id }))
            .await?;
        let capability = self
            .call(Command::ActivityCapabilityPolicyGet, json!({ "id": id }))
            .await?;
        for response in [&execution, &monetary, &scheduling, &capability] {
            if response.get("activity_id").and_then(Value::as_str) != Some(id) {
                return Err("Claw returned controls for another Activity".into());
            }
        }
        Ok(ActivityControls {
            activity_id: id.to_string(),
            execution_limits: optional_object(&execution, "execution_limits")?,
            monetary_budget: optional_object(&monetary, "monetary_budget")?,
            scheduling_policy: optional_object(&scheduling, "scheduling_policy")?,
            capability_policy: optional_object(&capability, "capability_policy")?,
        })
    }

    async fn set_activity_control(
        &self,
        id: &str,
        policy: ActivityControlPolicy,
        expected_revision: Option<u64>,
        draft: Value,
    ) -> Result<ActivityControls, String> {
        validate_token(id, "Activity id")?;
        let (command, field) = match policy {
            ActivityControlPolicy::ExecutionLimits => {
                (Command::ActivityExecutionLimitsSet, "limits")
            }
            ActivityControlPolicy::MonetaryBudget => (Command::ActivityMonetaryBudgetSet, "budget"),
            ActivityControlPolicy::CapabilityPolicy => {
                (Command::ActivityCapabilityPolicySet, "policy")
            }
        };
        let mut params = json!({ "id": id });
        params[field] = draft;
        if let Some(revision) = expected_revision {
            params["expected_revision"] = json!(revision);
        }
        let value = self.call(command, params).await?;
        verify_activity_policy_ack(&value, id)?;
        self.activity_controls(id).await
    }

    async fn set_activity_control_enabled(
        &self,
        id: &str,
        policy: ActivityControlPolicy,
        revision: u64,
        enabled: bool,
    ) -> Result<ActivityControls, String> {
        validate_token(id, "Activity id")?;
        let command = match policy {
            ActivityControlPolicy::ExecutionLimits => Command::ActivityExecutionLimitsEnabled,
            ActivityControlPolicy::MonetaryBudget => Command::ActivityMonetaryBudgetEnabled,
            ActivityControlPolicy::CapabilityPolicy => Command::ActivityCapabilityPolicyEnabled,
        };
        let value = self
            .call(
                command,
                json!({
                    "id": id,
                    "expected_revision": revision,
                    "enabled": enabled,
                }),
            )
            .await?;
        verify_activity_policy_ack(&value, id)?;
        self.activity_controls(id).await
    }

    async fn set_activity_priority(
        &self,
        id: &str,
        expected_revision: Option<u64>,
        priority: &str,
    ) -> Result<ActivityControls, String> {
        validate_token(id, "Activity id")?;
        let mut params = json!({ "id": id, "priority": priority });
        if let Some(revision) = expected_revision {
            params["expected_revision"] = json!(revision);
        }
        let value = self
            .call(Command::ActivitySchedulingPolicySet, params)
            .await?;
        verify_activity_policy_ack(&value, id)?;
        self.activity_controls(id).await
    }

    async fn activity_evidence(&self, id: &str) -> Result<ActivityEvidence, String> {
        validate_token(id, "Activity id")?;
        let objects = self
            .call(Command::ActivityObjects, json!({ "id": id }))
            .await?;
        let receipts = self
            .call(Command::ActivityReceipts, json!({ "id": id, "limit": 100 }))
            .await?;
        let object_state = self
            .call(
                Command::ActivityObjectStateList,
                json!({ "id": id, "limit": 100 }),
            )
            .await?;
        for response in [&objects, &receipts, &object_state] {
            if response.get("activity_id").and_then(Value::as_str) != Some(id) {
                return Err("Claw returned evidence for another Activity".into());
            }
        }
        let object_rows = bounded_array(&objects, "objects", 32)?;
        let receipt_rows = bounded_array(&receipts, "receipts", 100)?;
        let state_rows = bounded_array(&object_state, "entries", 100)?;
        let file_plan_objects = object_rows
            .iter()
            .filter(|object| {
                object
                    .get("reference")
                    .and_then(Value::as_str)
                    .is_some_and(|reference| reference.starts_with("app://fs/change-plan?"))
            })
            .cloned()
            .collect::<Vec<_>>();
        let file_plan_receipts = receipt_rows
            .iter()
            .filter(|receipt| {
                receipt
                    .pointer("/report/result/preview")
                    .and_then(Value::as_str)
                    .is_some_and(|preview| preview.starts_with("App-reported file change plan;"))
            })
            .cloned()
            .collect::<Vec<_>>();
        let evidence = json!({
            "activity_id": id,
            "caveats": [
                "Object declarations and references are inert and grant no authority.",
                "Operation effects and receipts are App-declared or caller-reported, not confirmed effects.",
                "Object-state entries are annotations, not semantic truth.",
                "File plans remain App-owned proposals; inspect and apply them explicitly through the normal App gate."
            ],
            "objects": object_rows,
            "operation_previews": "Use /activity-preview ID APP OP | JSON_ARGS; previewing never executes.",
            "receipts": receipt_rows,
            "object_state": state_rows,
            "staged_file_plans": {
                "attached_objects": file_plan_objects,
                "reported_receipts": file_plan_receipts,
            },
        });
        let presentation = serde_json::to_string_pretty(&evidence)
            .map_err(|_| "Claw Activity evidence could not be rendered".to_string())?;
        if presentation.len() > 2 * 1024 * 1024 {
            return Err("Claw Activity evidence exceeded its terminal bound".into());
        }
        Ok(ActivityEvidence {
            activity_id: id.to_string(),
            presentation,
        })
    }

    async fn activity_review(&self, id: &str) -> Result<ActivityReview, String> {
        let evidence = self.activity_evidence(id).await?;
        let value: Value = serde_json::from_str(&evidence.presentation)
            .map_err(|_| "Claw Activity evidence could not be reviewed".to_string())?;
        let staged = value
            .get("staged_file_plans")
            .cloned()
            .ok_or("Claw Activity evidence omitted staged file plans")?;
        let review = json!({
            "activity_id": id,
            "authority": "Review is presentation only. Plans and diffs are App-reported proposals; no target is read or changed here.",
            "staged_file_plans": staged,
            "next": "Inspect or apply a selected plan explicitly through the normal Files App permission path."
        });
        let presentation = serde_json::to_string_pretty(&review)
            .map_err(|_| "Claw file-change review could not be rendered".to_string())?;
        if presentation.len() > 1024 * 1024 {
            return Err("Claw file-change review exceeded its terminal bound".into());
        }
        Ok(ActivityReview {
            activity_id: id.to_string(),
            presentation,
        })
    }

    async fn activity_operation_preview(
        &self,
        id: &str,
        app_id: &str,
        operation: &str,
        args: &[String],
    ) -> Result<ActivityOperationPreview, String> {
        validate_token(id, "Activity id")?;
        validate_token(app_id, "App id")?;
        validate_token(operation, "operation")?;
        if args.len() > 64 || args.iter().any(|arg| arg.len() > 8_192) {
            return Err("operation preview arguments exceed terminal bounds".into());
        }
        let value = self
            .call(
                Command::ActivityOperationPreview,
                json!({
                    "id": id,
                    "app_id": app_id,
                    "operation": operation,
                    "args": args,
                }),
            )
            .await?;
        if value.get("app_id").and_then(Value::as_str) != Some(app_id)
            || value.get("operation").and_then(Value::as_str) != Some(operation)
            || value.get("authorization_checked").and_then(Value::as_bool) != Some(false)
            || value.get("executed").and_then(Value::as_bool) != Some(false)
            || value.get("effects_confirmed").and_then(Value::as_bool) != Some(false)
        {
            return Err("Claw returned an unsafe or mismatched operation preview".into());
        }
        let presentation = serde_json::to_string_pretty(&value)
            .map_err(|_| "Claw operation preview could not be rendered".to_string())?;
        if presentation.len() > 256 * 1024 {
            return Err("Claw operation preview exceeded its terminal bound".into());
        }
        Ok(ActivityOperationPreview {
            activity_id: id.to_string(),
            presentation,
        })
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
        ensure_approval_runtime(Path::new(PKEXEC_PATH), Path::new(APPROVAL_HELPER_PATH))?;
        let mut command = tokio::process::Command::new(PKEXEC_PATH);
        command
            .arg(APPROVAL_HELPER_PATH)
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
            .map_err(|error| format!("could not launch Claw approval authorization: {error}"))?;
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

    async fn platform_overview(&self) -> Result<PlatformOverview, String> {
        let skills = self.skills().await?;
        let usage = self
            .call(Command::AgentUsage, json!({ "args": [] }))
            .await?;
        let memory = self
            .call(Command::MemorySessions, json!({ "limit": 1 }))
            .await?;
        let config = self.config.clone();
        let owner_uid = self.owner_uid;
        let local = tokio::task::spawn_blocking(move || {
            let configured_mcp = config
                .agent
                .mcp_servers
                .iter()
                .filter(|server| server.enabled)
                .map(|server| server.name.clone())
                .collect::<Vec<_>>();
            let discovered_mcp = if config.agent.agent_api_discovery_enabled {
                let paths = config
                    .agent
                    .agent_api_paths
                    .iter()
                    .map(PathBuf::from)
                    .collect::<Vec<_>>();
                let paths = (!paths.is_empty()).then_some(paths.as_slice());
                crate::agent::tools::mcp::discover::discover(paths)
                    .into_iter()
                    .map(|server| server.name)
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let apps_dir = std::env::var_os("COS_APPS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/usr/lib/cos/apps"));
            let apps = crate::apps::discover_all(&apps_dir);
            let verified_apps = apps.verified.keys().cloned().collect::<Vec<_>>();
            let quarantined_apps = apps.quarantined.keys().cloned().collect::<Vec<_>>();
            let extensions = crate::agent_extensions::registry::ExtensionRegistry::load_selected_for_owner(
                &crate::agent_extensions::registry::installed_root(),
                &config.agent.extensions,
                owner_uid,
            );
            let verified_extensions = extensions.registered.keys().cloned().collect::<Vec<_>>();
            let quarantined_extensions = extensions
                .quarantined
                .iter()
                .map(|extension| extension.id.clone())
                .collect::<Vec<_>>();
            (
                configured_mcp,
                discovered_mcp,
                verified_apps,
                quarantined_apps,
                verified_extensions,
                quarantined_extensions,
            )
        })
        .await
        .map_err(|_| "Claw platform discovery failed".to_string())?;
        let presentation = serde_json::to_string_pretty(&json!({
            "authority": "Read-only verified inventory. This panel starts no MCP server, executes no App, and activates no extension.",
            "skills": skills.into_iter().map(|skill| skill.id).collect::<Vec<_>>(),
            "mcp": {
                "configured_enabled": local.0,
                "verified_discovered": local.1,
            },
            "apps": {
                "verified": local.2,
                "quarantined": local.3,
            },
            "extensions": {
                "verified_selected": local.4,
                "quarantined_selected": local.5,
            },
            "memory_sessions": memory.get("n").cloned().unwrap_or(Value::Null),
            "usage": usage,
        }))
        .map_err(|_| "Claw platform overview could not be rendered".to_string())?;
        if presentation.len() > 512 * 1024 {
            return Err("Claw platform overview exceeded its terminal bound".into());
        }
        Ok(PlatformOverview { presentation })
    }

    async fn reset_memories(&self) -> Result<LearnedMemoryReset, String> {
        let report: LearnedMemoryReset = serde_json::from_value(
            self.call(Command::MemoryReset, json!({ "confirm": true }))
                .await?,
        )
        .map_err(|error| format!("invalid learned-memory reset response: {error}"))?;
        if !report.conversations_preserved {
            return Err(
                "invalid learned-memory reset response: conversation preservation was not confirmed"
                    .to_string(),
            );
        }
        Ok(report)
    }

    async fn agent_hooks(&self) -> Result<AgentHookSettings, String> {
        parse_agent_hook_settings(self.call(Command::AgentHooksGet, json!({})).await?)
    }

    async fn set_agent_hook(
        &self,
        kind: &str,
        enabled: bool,
    ) -> Result<AgentHookSettings, String> {
        parse_agent_hook_settings(
            self.call(
                Command::AgentHooksSet,
                json!({ "kind": kind, "enabled": enabled }),
            )
            .await?,
        )
    }

    async fn mcp_overview(&self) -> Result<McpOverview, String> {
        let config = self.config.clone();
        tokio::task::spawn_blocking(move || build_mcp_overview(&config))
            .await
            .map_err(|_| "Claw MCP inventory reader failed".to_string())
    }

    async fn extensions_overview(&self) -> Result<ExtensionsOverview, String> {
        let config = self.config.clone();
        let owner_uid = self.owner_uid;
        tokio::task::spawn_blocking(move || build_extensions_overview(&config, owner_uid))
            .await
            .map_err(|_| "Claw extension inventory reader failed".to_string())
    }

    async fn usage_overview(&self, period: UsagePeriod) -> Result<UsageOverview, String> {
        let mut args = vec!["overall".to_string()];
        let now = chrono::Utc::now();
        let since = match period {
            UsagePeriod::Daily => Some(now - chrono::Duration::days(1)),
            UsagePeriod::Weekly => Some(now - chrono::Duration::days(7)),
            UsagePeriod::Cumulative => None,
        };
        if let Some(since) = since {
            args.push("--since".into());
            args.push(since.to_rfc3339());
        }
        parse_usage_overview(
            period,
            self.call(Command::AgentUsage, json!({ "args": args }))
                .await?,
        )
    }

    async fn debug_overview(&self) -> Result<DebugOverview, String> {
        let health: RawDaemonHealth = serde_json::from_value(
            self.call(Command::DaemonHealth, json!({})).await?,
        )
        .map_err(|error| format!("invalid Claw daemon health response: {error}"))?;
        if health.daemon != "clawd" || health.status != "ok" {
            return Err("invalid Claw daemon health response: daemon is not healthy".to_string());
        }
        Ok(DebugOverview {
            daemon: health.daemon,
            daemon_status: health.status,
            started_at: health.started_at,
            uptime_ms: health.uptime_ms,
            provider: self.info.provider.clone(),
            model: self.info.model.clone(),
            provider_ready: self.info.provider_ready,
            model_count: self.info.models.len(),
            model_catalog_warning: self.info.model_catalog_warning.clone(),
            max_turns: self.config.agent.max_turns,
            reasoning_effort: self.config.agent.reasoning_effort.clone(),
            compression_enabled: self.config.agent.compress_enabled,
            memory_redaction_enabled: self.config.agent.redact_memory_enabled,
            progressive_tools_enabled: self.config.agent.progressive_tools_enabled,
            tool_allow_count: self.config.agent.tool_allow.as_ref().map(Vec::len),
            tool_deny_count: self.config.agent.tool_deny.len(),
            configured_mcp_count: self.config.agent.mcp_servers.len(),
            mcp_discovery_enabled: self.config.agent.agent_api_discovery_enabled,
            selected_extension_count: self.config.agent.extensions.len(),
        })
    }
}

#[derive(Deserialize)]
struct RawDaemonHealth {
    status: String,
    daemon: String,
    started_at: String,
    uptime_ms: u64,
}

const MAX_MCP_PRESENTATION_ENTRIES: usize = 256;

fn build_mcp_overview(config: &CosConfig) -> McpOverview {
    let mut servers = config
        .agent
        .mcp_servers
        .iter()
        .map(|server| McpServerSummary {
            name: server.name.clone(),
            source: "operator config",
            enabled: server.enabled,
            transport: "stdio",
            timeout_secs: server.timeout_secs,
        })
        .collect::<Vec<_>>();
    if config.agent.agent_api_discovery_enabled {
        let paths = config
            .agent
            .agent_api_paths
            .iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        let paths = (!paths.is_empty()).then_some(paths.as_slice());
        servers.extend(
            crate::agent::tools::mcp::discover::discover(paths)
                .into_iter()
                .map(|server| McpServerSummary {
                    name: server.name,
                    source: "verified discovery",
                    enabled: true,
                    transport: if server.url.is_some() { "http" } else { "stdio" },
                    timeout_secs: server.timeout_secs,
                }),
        );
    }
    let truncated = servers.len() > MAX_MCP_PRESENTATION_ENTRIES;
    servers.truncate(MAX_MCP_PRESENTATION_ENTRIES);
    McpOverview {
        servers,
        discovery_enabled: config.agent.agent_api_discovery_enabled,
        truncated,
    }
}

const MAX_EXTENSION_PRESENTATION_ENTRIES: usize = 512;

fn build_extensions_overview(config: &CosConfig, owner_uid: u32) -> ExtensionsOverview {
    let mut entries = Vec::new();
    let mut truncated = false;
    let mut push = |entry: ExtensionSummary| {
        if entries.len() < MAX_EXTENSION_PRESENTATION_ENTRIES {
            entries.push(entry);
        } else {
            truncated = true;
        }
    };

    let apps_dir = std::env::var_os("COS_APPS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/lib/cos/apps"));
    let apps = crate::apps::discover_all(&apps_dir);
    for (id, app) in apps.verified {
        push(ExtensionSummary {
            kind: "App",
            id,
            status: "verified",
            trust: app.trust_label().to_string(),
            diagnostic: None,
        });
    }
    for (id, app) in apps.quarantined {
        push(ExtensionSummary {
            kind: "App",
            id,
            status: "quarantined",
            trust: "quarantined".into(),
            diagnostic: app.quarantine_reason().map(str::to_string),
        });
    }

    let skills = crate::agent::skills::loader::load_catalog_default();
    for (id, skill) in skills.skills {
        push(ExtensionSummary {
            kind: "Skill",
            id,
            status: "verified",
            trust: skill.trust_label().to_string(),
            diagnostic: None,
        });
    }
    for (id, diagnostic) in skills.disabled {
        push(ExtensionSummary {
            kind: "Skill",
            id,
            status: "disabled",
            trust: "policy".into(),
            diagnostic: Some(diagnostic),
        });
    }
    for (id, diagnostic) in skills.errors {
        push(ExtensionSummary {
            kind: "Skill",
            id,
            status: "quarantined",
            trust: "quarantined".into(),
            diagnostic: Some(diagnostic),
        });
    }

    let extensions =
        crate::agent_extensions::registry::ExtensionRegistry::load_selected_for_owner(
            &crate::agent_extensions::registry::installed_root(),
            &config.agent.extensions,
            owner_uid,
        );
    for (id, extension) in extensions.registered {
        push(ExtensionSummary {
            kind: "Agent extension",
            id,
            status: "verified",
            trust: extension.package.source().as_str().to_string(),
            diagnostic: None,
        });
    }
    for extension in extensions.quarantined {
        push(ExtensionSummary {
            kind: "Agent extension",
            id: extension.id,
            status: "quarantined",
            trust: "quarantined".into(),
            diagnostic: Some(extension.diagnostic),
        });
    }

    ExtensionsOverview { entries, truncated }
}

const MAX_USAGE_BREAKDOWN_ENTRIES: usize = 20;

#[derive(Deserialize)]
struct RawUsageOverview {
    scope: String,
    total: crate::agent::llm::usage::Totals,
    by_provider: BTreeMap<String, crate::agent::llm::usage::Totals>,
    by_model: BTreeMap<String, crate::agent::llm::usage::Totals>,
    parse_errors: usize,
    log_lines: u64,
    log_bytes: u64,
    breakdown_truncated: bool,
}

pub(super) fn parse_usage_overview(
    period: UsagePeriod,
    value: Value,
) -> Result<UsageOverview, String> {
    let raw: RawUsageOverview = serde_json::from_value(value)
        .map_err(|error| format!("invalid Agent usage response: {error}"))?;
    if raw.scope != "overall" {
        return Err("invalid Agent usage response: expected overall scope".to_string());
    }
    let (providers, providers_truncated) = top_usage_breakdown(raw.by_provider);
    let (models, models_truncated) = top_usage_breakdown(raw.by_model);
    Ok(UsageOverview {
        period,
        total: raw.total,
        providers,
        models,
        parse_errors: raw.parse_errors,
        log_lines: raw.log_lines,
        log_bytes: raw.log_bytes,
        breakdown_truncated: raw.breakdown_truncated
            || providers_truncated
            || models_truncated,
    })
}

fn top_usage_breakdown(
    values: BTreeMap<String, crate::agent::llm::usage::Totals>,
) -> (Vec<UsageBreakdown>, bool) {
    let truncated = values.len() > MAX_USAGE_BREAKDOWN_ENTRIES;
    let mut values = values
        .into_iter()
        .map(|(name, totals)| UsageBreakdown { name, totals })
        .collect::<Vec<_>>();
    values.sort_by(|left, right| {
        right
            .totals
            .calls
            .cmp(&left.totals.calls)
            .then_with(|| left.name.cmp(&right.name))
    });
    values.truncate(MAX_USAGE_BREAKDOWN_ENTRIES);
    (values, truncated)
}

#[derive(Deserialize)]
struct RawAgentHookSetting {
    kind: String,
    enabled: bool,
}

#[derive(Deserialize)]
struct RawAgentHookSettings {
    applies_to: String,
    hooks: Vec<RawAgentHookSetting>,
    #[serde(default)]
    updated_kind: Option<String>,
    #[serde(default)]
    changed: Option<bool>,
}

pub(super) fn parse_agent_hook_settings(value: Value) -> Result<AgentHookSettings, String> {
    let raw: RawAgentHookSettings = serde_json::from_value(value)
        .map_err(|error| format!("invalid Agent hook settings response: {error}"))?;
    if raw.applies_to != "future_tasks" {
        return Err("invalid Agent hook settings response: unsupported apply scope".to_string());
    }
    if raw.updated_kind.is_some() != raw.changed.is_some() {
        return Err("invalid Agent hook settings response: incomplete update result".to_string());
    }
    if raw.hooks.len() != 3 {
        return Err("invalid Agent hook settings response: incomplete hook inventory".to_string());
    }
    let mut logging = None;
    let mut audit = None;
    let mut checkpoint = None;
    for hook in raw.hooks {
        let slot = match hook.kind.as_str() {
            "logging" => &mut logging,
            "audit" => &mut audit,
            "checkpoint" => &mut checkpoint,
            _ => {
                return Err(format!(
                    "invalid Agent hook settings response: unknown kind {}",
                    hook.kind
                ));
            }
        };
        if slot.replace(hook.enabled).is_some() {
            return Err(format!(
                "invalid Agent hook settings response: duplicate kind {}",
                hook.kind
            ));
        }
    }
    if raw
        .updated_kind
        .as_deref()
        .is_some_and(|kind| !matches!(kind, "logging" | "audit" | "checkpoint"))
    {
        return Err("invalid Agent hook settings response: unknown updated kind".to_string());
    }
    Ok(AgentHookSettings {
        logging: logging
            .ok_or_else(|| "invalid Agent hook settings response: missing logging".to_string())?,
        audit: audit
            .ok_or_else(|| "invalid Agent hook settings response: missing audit".to_string())?,
        checkpoint: checkpoint
            .ok_or_else(|| "invalid Agent hook settings response: missing checkpoint".to_string())?,
        updated_kind: raw.updated_kind,
        changed: raw.changed,
    })
}

pub(super) fn ensure_approval_runtime(pkexec: &Path, helper: &Path) -> Result<(), String> {
    if !pkexec.is_file() {
        return Err(format!(
            "Claw approval authorization is unavailable: {} is not installed",
            pkexec.display()
        ));
    }
    if !helper.is_file() {
        return Err(format!(
            "Claw approval authorization is unavailable: {} is not installed",
            helper.display()
        ));
    }
    Ok(())
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

async fn initial_request(
    socket: &Path,
    command: Command,
    params: Value,
    replay_safe: bool,
) -> Result<Value, InitialConnectionError> {
    let response = tokio::time::timeout(
        Duration::from_secs(35),
        crate::clawd::client::request(socket, Request::new(command, params)),
    )
    .await
    .map_err(|_| {
        if replay_safe {
            InitialConnectionError::Retryable("Claw broker request timed out".to_string())
        } else {
            InitialConnectionError::Fatal(
                "Claw conversation creation timed out; its outcome is unknown".to_string(),
            )
        }
    })?
    .map_err(|error| {
        if replay_safe || !error.may_have_dispatched() {
            InitialConnectionError::Retryable(error.to_string())
        } else {
            InitialConnectionError::Fatal(format!(
                "Claw conversation creation may have reached the broker: {error}"
            ))
        }
    })?;
    if response.ok {
        response
            .result
            .ok_or_else(|| InitialConnectionError::Fatal("Claw broker returned no result".into()))
    } else {
        Err(InitialConnectionError::Fatal(
            response
                .error
                .map(|error| format!("{}: {}", error.code, error.message))
                .unwrap_or_else(|| "Claw broker returned no error".to_string()),
        ))
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
    let jobs = value
        .get("jobs")
        .and_then(Value::as_array)
        .map_or(Ok(Vec::new()), |jobs| {
            if jobs.len() > 1_000 {
                return Err("Claw conversation job list exceeded its terminal bound".to_string());
            }
            jobs.iter()
                .map(|job| {
                    Ok(ConversationJob {
                        id: required_string(job, "id")?,
                        status: required_string(job, "status")?,
                        session_id: required_string(job, "session_id")?,
                    })
                })
                .collect::<Result<Vec<_>, String>>()
        })?;
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
        jobs,
        jobs_truncated: value
            .get("jobs_truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
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
        workspace: optional_string(&value, "workspace"),
        after_task_id: optional_string(&value, "after_task_id"),
        prompt: required_string(&value, "prompt")?,
        status: required_string(&value, "status")?,
        created_at: required_string(&value, "created_at")?,
        started_at: optional_string(&value, "started_at"),
        finished_at: optional_string(&value, "finished_at"),
        response: optional_string(&value, "response"),
        error: optional_string(&value, "error"),
        requested_model: optional_string(&value, "requested_model"),
        requested_reasoning_effort: optional_string(&value, "requested_reasoning_effort"),
        plan_only: value
            .get("plan_only")
            .and_then(Value::as_bool)
            .unwrap_or(false),
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
        workspace: optional_string(value, "workspace"),
        after_task_id: optional_string(value, "after_task_id"),
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

fn parse_activity(value: &Value) -> Result<Activity, String> {
    let resources = value
        .get("resources")
        .and_then(Value::as_array)
        .ok_or("Activity omitted resources")?
        .iter()
        .map(|resource| {
            Ok(ActivityResource {
                label: required_string(resource, "label")?,
                reference: required_string(resource, "reference")?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    if resources.len() > 32 {
        return Err("Activity resource list exceeded its terminal bound".into());
    }
    Ok(Activity {
        id: required_string(value, "id")?,
        title: required_string(value, "title")?,
        goal: required_string(value, "goal")?,
        completion_criteria: required_string(value, "completion_criteria")?,
        boundaries: required_string(value, "boundaries")?,
        resources,
        state: required_string(value, "state")?,
        completion_note: optional_string(value, "completion_note"),
        created_at: required_string(value, "created_at")?,
        updated_at: required_string(value, "updated_at")?,
    })
}

fn parse_activity_job(value: &Value) -> Result<ActivityJob, String> {
    Ok(ActivityJob {
        id: required_string(value, "id")?,
        title: required_string(value, "title")?,
        status: required_string(value, "status")?,
        session_id: optional_string(value, "session_id"),
        created_at: required_string(value, "created_at")?,
        finished_at: optional_string(value, "finished_at"),
        response: optional_string(value, "response"),
        error: optional_string(value, "error"),
        waiting_on: value
            .get("waiting_on")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
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

fn optional_object(value: &Value, key: &str) -> Result<Option<Value>, String> {
    match value.get(key) {
        Some(Value::Null) | None => Ok(None),
        Some(value) if value.is_object() => Ok(Some(value.clone())),
        Some(_) => Err(format!("Claw response {key} is not an object")),
    }
}

fn policy_revision(policy: &Option<Value>) -> Option<u64> {
    policy
        .as_ref()
        .and_then(|policy| policy.get("revision"))
        .and_then(Value::as_u64)
}

fn policy_enabled(policy: &Option<Value>) -> Option<bool> {
    policy
        .as_ref()
        .and_then(|policy| policy.get("enabled"))
        .and_then(Value::as_bool)
}

fn verify_activity_policy_ack(value: &Value, id: &str) -> Result<(), String> {
    if value.get("activity_id").and_then(Value::as_str) != Some(id)
        || value.get("revision").and_then(Value::as_u64).is_none()
    {
        return Err("Claw returned a mismatched Activity control acknowledgement".into());
    }
    Ok(())
}

fn bounded_array<'a>(value: &'a Value, key: &str, limit: usize) -> Result<&'a Vec<Value>, String> {
    let values = value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("Claw response omitted {key}"))?;
    if values.len() > limit {
        return Err(format!("Claw response {key} exceeded its terminal bound"));
    }
    Ok(values)
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
