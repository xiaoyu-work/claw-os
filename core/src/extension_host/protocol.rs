use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::tools::mcp::integration::McpServerSpec;
use crate::agent::tools::mcp::protocol::{CallToolResult, ToolDescriptor};
use crate::clawd::wire::RequestId;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_CONTROL_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_CONTROL_CONNECTIONS: usize = 8;
pub const MAX_REQUEST_TIMEOUT_MS: u64 = 180_000;
pub const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 130_000;
pub const READY_TIMEOUT_MS: u64 = 15_000;
pub const EXTENSION_HOST_GROUP: &str = "extension-host";
pub const BROKER_SOCKET_ENV: &str = "COS_EXTENSION_BROKER_SOCKET";

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
    pub owner_gid: u32,
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
        if self.owner_uid == 0 || self.owner_gid == 0 || self.worker_pid <= 1 || self.host_pid <= 1 {
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
        Ok(())
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
