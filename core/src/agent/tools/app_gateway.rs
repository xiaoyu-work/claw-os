//! Trusted invocation policy for manifest-declared App MCP services.
//!
//! This module owns the boundary between an authenticated Claw caller and an
//! App service. Tool arguments select work, never identity or authority.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::caps::manifest::Manifest;
use crate::caps::{Cap, Scope, Verb};
use crate::extension_host::protocol::ExtensionBinding;

pub const CALL_CONTEXT_META_KEY: &str = "claw-os.dev/call-context";
pub const CALL_CONTEXT_WIRE_VERSION: u8 = 1;
pub const MAX_CALL_DEPTH: u8 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpPrincipalKind {
    SystemAgent,
    App,
    AppAgent,
    ExternalAgent,
    LocalCli,
}

impl McpPrincipalKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SystemAgent => "system-agent",
            Self::App => "app",
            Self::AppAgent => "app-agent",
            Self::ExternalAgent => "external-agent",
            Self::LocalCli => "local-cli",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpPrincipal {
    pub kind: McpPrincipalKind,
    pub id: String,
    pub owner_uid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
}

impl McpPrincipal {
    pub fn system_agent(owner_uid: u32, session_id: impl Into<String>) -> Self {
        Self {
            kind: McpPrincipalKind::SystemAgent,
            id: session_id.into(),
            owner_uid,
            app_id: None,
        }
    }

    fn validate(&self) -> Result<(), String> {
        if !valid_workload_id(&self.id, 256) {
            return Err("MCP caller has an invalid workload id".to_string());
        }
        match self.kind {
            McpPrincipalKind::App | McpPrincipalKind::AppAgent => {
                if !self.app_id.as_deref().is_some_and(valid_app_id) {
                    return Err("App MCP caller has no valid App identity".to_string());
                }
            }
            McpPrincipalKind::SystemAgent
            | McpPrincipalKind::ExternalAgent
            | McpPrincipalKind::LocalCli => {
                if self.app_id.is_some() {
                    return Err("non-App MCP caller asserted an App identity".to_string());
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpCallContext {
    pub wire_version: u8,
    pub call_id: String,
    pub trace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_call_id: Option<String>,
    pub depth: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub caller: McpPrincipal,
}

impl McpCallContext {
    pub fn for_extension_system_agent(
        binding: &ExtensionBinding,
        timeout: Duration,
    ) -> Result<Self, String> {
        binding.validate_fresh_worker(
            std::process::id(),
            crate::proc::read_start_time_ticks_pub(std::process::id()),
        )?;
        let session_id = binding
            .session_id
            .as_deref()
            .ok_or_else(|| "system Agent App call has no bound session".to_string())?;
        let context = Self::root_system_agent(
            binding.owner_uid,
            session_id,
            Some(binding.task_id.clone()),
            timeout,
            Some(binding.expires_at_ms),
        )?;
        context.validate_system_agent_binding(binding)?;
        Ok(context)
    }

    pub fn for_current_system_agent(timeout: Duration) -> Result<Self, String> {
        let session = crate::proc::current_session_info_for_caps()
            .ok_or_else(|| "system Agent App call has no registered session".to_string())?;
        crate::caps::enforcement::require_current_session_identity(
            &session.session_id,
            session.pid,
        )
        .map_err(|error| format!("system Agent session identity is invalid: {error}"))?;
        let owner_uid = crate::paths::current_owner_uid_override().unwrap_or_else(current_uid);
        if owner_uid == 0 {
            return Err("system Agent App calls require a non-root owner".to_string());
        }
        Self::root_system_agent(owner_uid, &session.session_id, None, timeout, None)
    }

    fn root_system_agent(
        owner_uid: u32,
        session_id: &str,
        task_id: Option<String>,
        timeout: Duration,
        lease_deadline: Option<u64>,
    ) -> Result<Self, String> {
        if timeout.is_zero() {
            return Err("MCP App call timeout must be positive".to_string());
        }
        let now = crate::agentd::grant::now_ms();
        let requested_deadline =
            now.saturating_add(timeout.as_millis().min(u128::from(u64::MAX)) as u64);
        let deadline =
            lease_deadline.map_or(requested_deadline, |lease| requested_deadline.min(lease));
        if deadline <= now {
            return Err("MCP App call deadline has expired".to_string());
        }
        let call_id = format!("call-{}", uuid::Uuid::new_v4().simple());
        let context = Self {
            wire_version: CALL_CONTEXT_WIRE_VERSION,
            trace_id: call_id.clone(),
            call_id,
            parent_call_id: None,
            depth: 0,
            deadline_unix_ms: Some(deadline),
            session_id: Some(session_id.to_string()),
            task_id,
            caller: McpPrincipal::system_agent(owner_uid, session_id),
        };
        context.validate()?;
        Ok(context)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.wire_version != CALL_CONTEXT_WIRE_VERSION {
            return Err("MCP call context has an unsupported wire version".to_string());
        }
        if !valid_call_id(&self.call_id) || !valid_call_id(&self.trace_id) {
            return Err("MCP call context has an invalid call or trace id".to_string());
        }
        if self
            .parent_call_id
            .as_deref()
            .is_some_and(|value| !valid_call_id(value))
            || self.depth > MAX_CALL_DEPTH
            || self.deadline_unix_ms == Some(0)
            || self
                .session_id
                .as_deref()
                .is_some_and(|value| !valid_workload_id(value, 128))
            || self
                .task_id
                .as_deref()
                .is_some_and(|value| !valid_workload_id(value, 128))
        {
            return Err("MCP call context has invalid lineage".to_string());
        }
        self.caller.validate()
    }

    pub fn remaining(&self, limit: Duration) -> Result<Duration, String> {
        let remaining = self
            .deadline_unix_ms
            .and_then(|deadline| deadline.checked_sub(crate::agentd::grant::now_ms()))
            .filter(|remaining| *remaining > 0)
            .ok_or_else(|| "MCP App call deadline has expired".to_string())?;
        Ok(limit.min(Duration::from_millis(remaining)))
    }

    pub fn validate_system_agent_binding(&self, binding: &ExtensionBinding) -> Result<(), String> {
        self.validate_system_agent_binding_inner(binding, true)
    }

    pub fn validate_system_agent_audit_binding(
        &self,
        binding: &ExtensionBinding,
    ) -> Result<(), String> {
        self.validate_system_agent_binding_inner(binding, false)
    }

    fn validate_system_agent_binding_inner(
        &self,
        binding: &ExtensionBinding,
        require_live_deadline: bool,
    ) -> Result<(), String> {
        self.validate()?;
        binding.validate_shape()?;
        let session_id = binding
            .session_id
            .as_deref()
            .ok_or_else(|| "extension binding has no system Agent session".to_string())?;
        if self.caller.kind != McpPrincipalKind::SystemAgent
            || self.caller.owner_uid != binding.owner_uid
            || self.caller.id != session_id
            || self.caller.app_id.is_some()
            || self.session_id.as_deref() != Some(session_id)
            || self.task_id.as_deref() != Some(binding.task_id.as_str())
            || self.parent_call_id.is_some()
            || self.depth != 0
            || self.deadline_unix_ms.is_none_or(|deadline| {
                (require_live_deadline && deadline <= crate::agentd::grant::now_ms())
                    || deadline > binding.expires_at_ms
            })
        {
            return Err("MCP call context does not match the authenticated task".to_string());
        }
        Ok(())
    }
}

pub fn authorize_manifest(manifest: &Manifest, caller: &McpPrincipal) -> Result<(), String> {
    caller.validate()?;
    let Some(service) = manifest.mcp.as_ref() else {
        return if caller.kind == McpPrincipalKind::SystemAgent {
            Ok(())
        } else {
            Err(format!(
                "legacy App `{}` has no caller access policy",
                manifest.id
            ))
        };
    };
    let allowed = match caller.kind {
        McpPrincipalKind::SystemAgent => service.access.system_agent,
        McpPrincipalKind::LocalCli => service.access.local_cli,
        McpPrincipalKind::App | McpPrincipalKind::AppAgent => caller
            .app_id
            .as_ref()
            .is_some_and(|app| service.access.apps.iter().any(|allowed| allowed == app)),
        McpPrincipalKind::ExternalAgent => service.access.external_agents,
    };
    if !allowed {
        return Err(format!(
            "MCP caller is not allowed to address App `{}`",
            manifest.id
        ));
    }
    Ok(())
}

pub fn invoke_target(app_id: &str, tool: &str) -> Result<String, String> {
    if !valid_app_id(app_id) || !valid_tool_name(tool) {
        return Err("invalid MCP App invocation target".to_string());
    }
    Ok(format!("{app_id}/{tool}"))
}

pub fn invoke_cap(app_id: &str, tool: &str) -> Result<Cap, String> {
    Ok(Cap::new(
        Verb::AGENT_INVOKE,
        Scope::name(invoke_target(app_id, tool)?),
    ))
}

fn valid_app_id(value: &str) -> bool {
    value.len() <= 128 && valid_identifier(value, false)
}

fn valid_tool_name(value: &str) -> bool {
    value.len() <= 128 && valid_identifier(value, true) && !value.contains("..")
}

fn valid_identifier(value: &str, allow_dot: bool) -> bool {
    let mut bytes = value.bytes();
    bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        && bytes.all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'-')
                || (allow_dot && byte == b'.')
        })
}

fn valid_call_id(value: &str) -> bool {
    let mut bytes = value.bytes();
    !value.is_empty()
        && value.len() <= 128
        && bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && bytes
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn valid_workload_id(value: &str, limit: usize) -> bool {
    let mut bytes = value.bytes();
    !value.is_empty()
        && value.len() <= limit
        && bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && bytes.all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'_' | b':' | b'@' | b'/' | b'+' | b'%' | b'-')
        })
}

#[cfg(unix)]
fn current_uid() -> u32 {
    unsafe { libc::geteuid() as u32 }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/app_gateway.rs"
    ));
}
