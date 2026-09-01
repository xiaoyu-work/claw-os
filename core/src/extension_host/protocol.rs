use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::tools::mcp::integration::McpServerSpec;
use crate::agent::tools::mcp::protocol::{CallToolResult, ToolDescriptor};
use crate::clawd::wire::RequestId;

pub const PROTOCOL_VERSION: u32 = 4;
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
        if crate::proc::read_start_time_ticks_pub(self.worker_pid)
            != Some(worker_start_time_ticks)
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LifecycleAction {
    Attach,
    Ready,
    Call,
    Cancel,
    Crash,
    Timeout,
    Detach,
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
            Self::Ready => "ready",
            Self::Call => "call",
            Self::Cancel => "cancel",
            Self::Crash => "crash",
            Self::Timeout => "timeout",
            Self::Detach => "detach",
            Self::TaskComplete => "task-complete",
        }
    }
}

impl ExtensionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::App => "app",
            Self::Mcp => "mcp",
        }
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
}

impl ControlResponse {
    pub fn ok(id: RequestId, result: HostResult) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: RequestId, message: impl Into<String>) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            id,
            ok: false,
            result: None,
            error: Some(clamp_text(&message.into(), 2048)),
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
    Cancelled,
    Shutdown,
}

pub fn clamp_text(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}
