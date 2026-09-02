use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::tools::mcp::integration::McpServerSpec;
use crate::agent::tools::mcp::protocol::{CallToolResult, ToolDescriptor};
use crate::clawd::wire::RequestId;

pub const PROTOCOL_VERSION: u32 = 8;
pub const MAX_CONTROL_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_CONTROL_CONNECTIONS: usize = 8;
pub const MAX_REQUEST_TIMEOUT_MS: u64 = 180_000;
pub const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 130_000;
pub const READY_TIMEOUT_MS: u64 = 15_000;
pub const EXTENSION_HOST_GROUP: &str = "extension-host";
pub const BROKER_SOCKET_ENV: &str = "COS_EXTENSION_BROKER_SOCKET";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedPath {
    pub path: String,
    pub device: u64,
    pub inode: u64,
    pub owner_uid: u32,
    pub mode: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBootstrap {
    pub protocol: u32,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub owner_uid: u32,
    pub extension_uid: u32,
    pub execution_gid: u32,
    pub enforce_groups: bool,
    pub worker_pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_start_time_ticks: Option<u64>,
    pub lease_nonce: String,
    pub expires_at_ms: u64,
    pub capability_generation: String,
    pub control_socket: String,
    pub broker_socket: String,
    pub approved_paths: Vec<ApprovedPath>,
    pub agent_extensions: Vec<crate::provenance::verify::PackageVerificationReceipt>,
}

impl HostBootstrap {
    pub fn into_current_binding(self) -> Result<ExtensionBinding, String> {
        if self.protocol != PROTOCOL_VERSION
            || self.extension_uid != unsafe { libc::geteuid() as u32 }
            || self.execution_gid != unsafe { libc::getegid() as u32 }
            || self.extension_uid == self.owner_uid
            || self.owner_uid == 0
            || self.extension_uid == 0
            || self.execution_gid == 0
        {
            return Err("extension-host bootstrap identity is invalid".to_string());
        }
        if crate::agentd::grant::now_ms() > self.expires_at_ms {
            return Err("extension-host bootstrap lease has expired".to_string());
        }
        let Some(worker_start_time_ticks) = self.worker_start_time_ticks else {
            return Err("extension-host bootstrap omitted worker start time".to_string());
        };
        if crate::proc::read_start_time_ticks_pub(self.worker_pid) != Some(worker_start_time_ticks)
        {
            return Err("extension-host bootstrap worker identity is stale".to_string());
        }
        let host_pid = std::process::id();
        let binding = ExtensionBinding {
            protocol: self.protocol,
            task_id: self.task_id,
            session_id: self.session_id,
            owner_uid: self.owner_uid,
            extension_uid: self.extension_uid,
            owner_gid: self.execution_gid,
            capability_generation: self.capability_generation,
            approved_paths: self.approved_paths,
            agent_extensions: self.agent_extensions,
            worker_pid: self.worker_pid,
            worker_start_time_ticks: self.worker_start_time_ticks,
            host_pid,
            host_start_time_ticks: crate::proc::read_start_time_ticks_pub(host_pid),
            lease_nonce: self.lease_nonce,
            expires_at_ms: self.expires_at_ms,
            control_socket: self.control_socket,
            broker_socket: self.broker_socket,
        };
        binding.validate_host(host_pid, binding.host_start_time_ticks)?;
        Ok(binding)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExtensionKind {
    Host,
    App,
    Mcp,
    AgentExtension,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LifecycleAction {
    Attach,
    Initialize,
    Ready,
    Event,
    Result,
    BackpressureDrop,
    Disable,
    Action,
    Call,
    Cancel,
    Crash,
    Connect,
    Protocol,
    RemoteCallFailure,
    Timeout,
    Detach,
    Shutdown,
    TaskComplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuditStage {
    Gateway,
    Host,
}

impl AuditStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::Host => "host",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpInvocationAudit {
    pub policy_identity: String,
    pub server_identity: String,
    pub handle_digest: String,
    pub descriptor_digest: String,
    pub capability_generation: String,
    pub untrusted_remote_name: crate::audit_policy::TextDigest,
}

impl McpInvocationAudit {
    pub fn validate(&self) -> Result<(), String> {
        let identity = |value: &str| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        };
        let digest = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        };
        if !identity(&self.policy_identity)
            || !identity(&self.server_identity)
            || !digest(&self.handle_digest)
            || !digest(&self.descriptor_digest)
            || self.capability_generation.len() != 16
            || !self
                .capability_generation
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || self.untrusted_remote_name.bytes > 4096
            || self.untrusted_remote_name.digest.is_empty()
        {
            return Err("MCP invocation audit identity is invalid".to_string());
        }
        Ok(())
    }
}

impl LifecycleAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Attach => "attach",
            Self::Initialize => "initialize",
            Self::Ready => "ready",
            Self::Event => "event",
            Self::Result => "result",
            Self::BackpressureDrop => "backpressure-drop",
            Self::Disable => "disable",
            Self::Action => "action",
            Self::Call => "call",
            Self::Cancel => "cancel",
            Self::Crash => "crash",
            Self::Connect => "connect",
            Self::Protocol => "protocol",
            Self::RemoteCallFailure => "remote-call-failure",
            Self::Timeout => "timeout",
            Self::Detach => "detach",
            Self::Shutdown => "shutdown",
            Self::TaskComplete => "task-complete",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExtensionErrorCategory {
    Connect,
    Timeout,
    Crash,
    RemoteCallFailure,
    #[default]
    Protocol,
}

impl ExtensionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::App => "app",
            Self::Mcp => "mcp",
            Self::AgentExtension => "agent-extension",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentExtensionRegistration {
    pub extension_id: String,
    pub extension_version: String,
    pub package_digest: String,
    pub manifest_digest: String,
    pub content_digest: String,
}

impl AgentExtensionRegistration {
    pub fn validate(&self) -> Result<(), String> {
        if self.extension_id.is_empty()
            || self.extension_id.len() > 128
            || !self.extension_id.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'_' | b'.')
            })
            || semver::Version::parse(&self.extension_version)
                .map(|version| version.to_string() != self.extension_version)
                .unwrap_or(true)
        {
            return Err("Agent extension registration identity is invalid".to_string());
        }
        if !crate::provenance::envelope::is_sha256_ref(&self.package_digest)
            || !crate::provenance::envelope::is_sha256_ref(&self.content_digest)
            || self.manifest_digest.len() != 64
            || !self
                .manifest_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err("Agent extension registration digest is invalid".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentExtensionResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default)]
    pub proposed_actions: Vec<super::abi::ProposedAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentExtensionAudit {
    pub package_digest: String,
    pub capability_generation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_kind: Option<crate::agent_extensions::manifest::EventKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<crate::audit_policy::TextDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<crate::audit_policy::TextDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id: Option<crate::audit_policy::TextDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_ref: Option<crate::audit_policy::TextDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_depth: Option<usize>,
}

impl AgentExtensionAudit {
    pub fn validate(&self) -> Result<(), String> {
        if !crate::provenance::envelope::is_sha256_ref(&self.package_digest)
            || self.capability_generation.len() != 16
            || !self
                .capability_generation
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err("Agent extension audit digest is invalid".to_string());
        }
        if self.tool.as_ref().is_some_and(|tool| {
            tool.is_empty()
                || tool.len() > 128
                || !tool
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        }) || self.queue_depth.is_some_and(|depth| depth > 32)
        {
            return Err("Agent extension audit metadata is invalid".to_string());
        }
        for text in [
            self.event_id.as_ref(),
            self.output.as_ref(),
            self.action_id.as_ref(),
            self.capability_ref.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            if text.bytes > 64 * 1024 || text.digest.is_empty() {
                return Err("Agent extension audit text digest is invalid".to_string());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionBinding {
    pub protocol: u32,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub owner_uid: u32,
    pub extension_uid: u32,
    pub owner_gid: u32,
    pub capability_generation: String,
    pub approved_paths: Vec<ApprovedPath>,
    pub agent_extensions: Vec<crate::provenance::verify::PackageVerificationReceipt>,
    pub worker_pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_start_time_ticks: Option<u64>,
    pub host_pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_start_time_ticks: Option<u64>,
    pub lease_nonce: String,
    pub expires_at_ms: u64,
    pub control_socket: String,
    pub broker_socket: String,
}

impl ExtensionBinding {
    pub fn validate_shape(&self) -> Result<(), String> {
        if self.protocol != PROTOCOL_VERSION {
            return Err(format!(
                "extension-host protocol mismatch: binding speaks v{}, runtime speaks v{}",
                self.protocol, PROTOCOL_VERSION
            ));
        }
        if self.task_id.is_empty() || self.task_id.len() > 128 {
            return Err("extension-host binding has an invalid task id".to_string());
        }
        if self
            .session_id
            .as_deref()
            .is_some_and(|value| value.is_empty() || value.len() > 128)
        {
            return Err("extension-host binding has an invalid session id".to_string());
        }
        if self.owner_uid == 0
            || self.extension_uid == 0
            || self.extension_uid == self.owner_uid
            || (61_000..=61_063).contains(&self.owner_uid)
            || !(61_000..=61_063).contains(&self.extension_uid)
            || self.owner_gid == 0
            || self.worker_pid <= 1
            || self.host_pid <= 1
            || self.worker_start_time_ticks.is_none()
            || self.host_start_time_ticks.is_none()
        {
            return Err("extension-host binding names a privileged or invalid process".to_string());
        }
        if self.lease_nonce.len() != 32
            || !self
                .lease_nonce
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("extension-host binding has an invalid lease nonce".to_string());
        }
        if self.control_socket.is_empty() || self.broker_socket.is_empty() {
            return Err("extension-host binding omitted a channel path".to_string());
        }
        if self.capability_generation.len() != 16
            || !self
                .capability_generation
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || self.approved_paths.is_empty()
            || self.approved_paths.len() > 64
        {
            return Err("extension-host binding has invalid runtime authority".to_string());
        }
        for approved in &self.approved_paths {
            if approved.path.is_empty()
                || !approved.path.starts_with('/')
                || approved.inode == 0
                || (approved.owner_uid != 0 && approved.owner_uid != self.owner_uid)
                || approved.mode & 0o022 != 0
            {
                return Err("extension-host binding has an invalid approved path".to_string());
            }
        }
        if self.agent_extensions.len() > 64 {
            return Err("extension-host binding has too many Agent extension receipts".to_string());
        }
        for receipt in &self.agent_extensions {
            receipt.validate()?;
            if receipt.kind != crate::provenance::PackageKind::AgentExtension {
                return Err(
                    "extension-host binding carries a non-extension package receipt".to_string(),
                );
            }
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String, String> {
        let bytes = serde_json::to_vec(self)
            .map_err(|error| format!("encode extension-host binding: {error}"))?;
        Ok(crate::crypto::sha256_hex(&bytes))
    }

    pub fn validate_worker(&self, pid: u32, start_time_ticks: Option<u64>) -> Result<(), String> {
        self.validate_shape()?;
        if self.worker_pid != pid || self.worker_start_time_ticks != start_time_ticks {
            return Err("extension-host binding belongs to a different worker".to_string());
        }
        Ok(())
    }

    pub fn validate_fresh_worker(
        &self,
        pid: u32,
        start_time_ticks: Option<u64>,
    ) -> Result<(), String> {
        self.validate_worker(pid, start_time_ticks)?;
        if crate::agentd::grant::now_ms() > self.expires_at_ms {
            return Err("extension-host binding lease has expired".to_string());
        }
        Ok(())
    }

    pub fn validate_host(&self, pid: u32, start_time_ticks: Option<u64>) -> Result<(), String> {
        self.validate_shape()?;
        if self.host_pid != pid || self.host_start_time_ticks != start_time_ticks {
            return Err("extension-host binding belongs to a different host".to_string());
        }
        if crate::agentd::grant::now_ms() > self.expires_at_ms {
            return Err("extension-host binding lease has expired".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlRequest {
    pub protocol: u32,
    pub id: RequestId,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub lease_nonce: String,
    pub binding_digest: String,
    pub timeout_ms: u64,
    pub action: HostAction,
}

impl ControlRequest {
    pub fn new(binding: &ExtensionBinding, action: HostAction, timeout_ms: u64) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            id: RequestId::generate(),
            task_id: binding.task_id.clone(),
            session_id: binding.session_id.clone(),
            lease_nonce: binding.lease_nonce.clone(),
            binding_digest: binding
                .digest()
                .expect("validated extension binding is serializable"),
            timeout_ms: timeout_ms.clamp(1, MAX_REQUEST_TIMEOUT_MS),
            action,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum HostAction {
    Ping,
    RunApp {
        app_id: String,
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
    AppOpen {
        app_id: String,
    },
    AppCall {
        app_id: String,
        tool: String,
        #[serde(default)]
        arguments: Value,
    },
    AppClose {
        app_id: String,
    },
    McpAttach {
        spec: McpServerSpec,
    },
    McpCall {
        server: String,
        tool: String,
        descriptor_digest: String,
        audit: McpInvocationAudit,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arguments: Option<Value>,
    },
    McpDetach {
        server: String,
    },
    AgentExtensionAttach {
        registration: AgentExtensionRegistration,
    },
    AgentExtensionEvent {
        extension_id: String,
        binding: super::abi::AbiBinding,
        event_id: String,
        deadline_monotonic_ns: super::abi::MonotonicDeadlineNs,
        payload: super::abi::EventPayload,
        capability_refs: Vec<crate::agent_extensions::capability_ref::CapabilityReference>,
    },
    AgentExtensionDetach {
        extension_id: String,
        binding: super::abi::AbiBinding,
        reason: super::abi::ShutdownReason,
    },
    Cancel {
        request_id: RequestId,
    },
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlResponse {
    pub protocol: u32,
    pub id: RequestId,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<HostResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_category: Option<ExtensionErrorCategory>,
}

impl ControlResponse {
    pub fn ok(id: RequestId, result: HostResult) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            id,
            ok: true,
            result: Some(result),
            error: None,
            error_category: None,
        }
    }

    pub fn error(
        id: RequestId,
        category: ExtensionErrorCategory,
        message: impl Into<String>,
    ) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            id,
            ok: false,
            result: None,
            error: Some(clamp_text(&message.into(), 2048)),
            error_category: Some(category),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum HostResult {
    Ready {
        pid: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_time_ticks: Option<u64>,
        dumpable: bool,
        seccomp_mode: u32,
    },
    AppOutput {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
    AppOpened {
        tool_count: usize,
    },
    AppCall {
        value: CallToolResult,
    },
    AppClosed {
        closed: bool,
    },
    McpAttached {
        tools: Vec<ToolDescriptor>,
    },
    McpCall {
        value: CallToolResult,
    },
    McpDetached {
        detached: bool,
    },
    AgentExtensionReady {
        binding: Box<super::abi::AbiBinding>,
    },
    AgentExtensionEvent {
        value: AgentExtensionResult,
    },
    AgentExtensionDetached {
        detached: bool,
    },
    Cancelled,
    Shutdown,
}

pub fn clamp_text(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}
