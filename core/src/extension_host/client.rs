//! Worker-side client for one task's extension host.

use std::path::Path;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use serde_json::Value;
use tokio::net::UnixStream;
use tokio::sync::mpsc::UnboundedSender;

use crate::clawd::wire::RequestId;

use super::protocol::{
    ControlRequest, ControlResponse, ExtensionBinding, ExtensionErrorCategory, HostAction,
    HostResult, DEFAULT_REQUEST_TIMEOUT_MS, MAX_CONTROL_FRAME_BYTES, PROTOCOL_VERSION,
    READY_TIMEOUT_MS,
};

#[derive(Debug, Clone)]
struct ClientFault {
    category: ExtensionErrorCategory,
    message: String,
}

impl ClientFault {
    fn new(category: ExtensionErrorCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
        }
    }

    fn protocol(message: impl Into<String>) -> Self {
        Self::new(ExtensionErrorCategory::Protocol, message)
    }
}

impl std::fmt::Display for ClientFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

type ClientResult<T> = Result<T, ClientFault>;

static CURRENT: OnceLock<RwLock<Option<Arc<ExtensionHostClient>>>> = OnceLock::new();

fn slot() -> &'static RwLock<Option<Arc<ExtensionHostClient>>> {
    CURRENT.get_or_init(|| RwLock::new(None))
}

#[derive(Debug)]
pub struct ExtensionHostClient {
    binding: ExtensionBinding,
    binding_digest: String,
    lease_digest: String,
    audit: Option<UnboundedSender<crate::agentd::protocol::WorkerFrame>>,
}

pub struct InstallGuard {
    lease_nonce: String,
}

impl Drop for InstallGuard {
    fn drop(&mut self) {
        let current = slot().read().ok().and_then(|guard| guard.as_ref().cloned());
        if current
            .as_ref()
            .is_some_and(|client| client.binding.lease_nonce == self.lease_nonce)
        {
            if let Some(client) = current {
                client.emit(
                    super::protocol::ExtensionKind::Host,
                    super::protocol::LifecycleAction::TaskComplete,
                    "task-host",
                    None,
                    true,
                    Duration::ZERO,
                    None,
                );
            }
            if let Ok(mut guard) = slot().write() {
                if guard
                    .as_ref()
                    .is_some_and(|client| client.binding.lease_nonce == self.lease_nonce)
                {
                    *guard = None;
                }
            }
        }
    }
}

pub fn current() -> Option<Arc<ExtensionHostClient>> {
    slot().read().ok()?.as_ref().cloned()
}

pub fn is_available() -> bool {
    current().is_some()
}

pub async fn install(binding: ExtensionBinding) -> Result<InstallGuard, String> {
    install_with_audit(binding, None).await
}

pub async fn install_for_worker(
    binding: ExtensionBinding,
    audit: UnboundedSender<crate::agentd::protocol::WorkerFrame>,
) -> Result<InstallGuard, String> {
    install_with_audit(binding, Some(audit)).await
}

async fn install_with_audit(
    binding: ExtensionBinding,
    audit: Option<UnboundedSender<crate::agentd::protocol::WorkerFrame>>,
) -> Result<InstallGuard, String> {
    binding.validate_fresh_worker(
        std::process::id(),
        crate::proc::read_start_time_ticks_pub(std::process::id()),
    )?;
    let binding_digest = binding.digest()?;
    let lease_digest = crate::crypto::sha256_hex(binding.lease_nonce.as_bytes());
    let client = Arc::new(ExtensionHostClient {
        binding,
        binding_digest,
        lease_digest,
        audit,
    });
    client.wait_ready().await?;
    client.emit(
        super::protocol::ExtensionKind::Host,
        super::protocol::LifecycleAction::Ready,
        "task-host",
        None,
        true,
        Duration::ZERO,
        None,
    );
    let lease_nonce = client.binding.lease_nonce.clone();
    let mut guard = slot()
        .write()
        .map_err(|_| "extension-host client registry is poisoned".to_string())?;
    if guard.is_some() {
        return Err("an extension host is already installed in this worker".to_string());
    }
    *guard = Some(client);
    Ok(InstallGuard { lease_nonce })
}

impl ExtensionHostClient {
    pub fn binding(&self) -> &ExtensionBinding {
        &self.binding
    }

    async fn wait_ready(&self) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(READY_TIMEOUT_MS);
        loop {
            match self
                .request_with_timeout(HostAction::Ping, Duration::from_secs(2), false)
                .await
            {
                Ok(HostResult::Ready {
                    pid,
                    start_time_ticks,
                    dumpable,
                    seccomp_mode,
                }) if pid == self.binding.host_pid
                    && start_time_ticks == self.binding.host_start_time_ticks =>
                {
                    if dumpable || seccomp_mode != 2 {
                        return Err(
                            "extension host did not retain dumpable/seccomp hardening".to_string()
                        );
                    }
                    return Ok(());
                }
                Ok(_) => {
                    return Err("extension host returned an invalid ready response".to_string())
                }
                Err(error) if tokio::time::Instant::now() < deadline => {
                    tracing::debug!(%error, "waiting for extension host readiness");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(error) => {
                    return Err(format!(
                        "extension host did not become ready within {}ms: {error}",
                        READY_TIMEOUT_MS
                    ))
                }
            }
        }
    }

    pub async fn run_app(
        &self,
        app_id: String,
        command: String,
        args: Vec<String>,
    ) -> Result<Option<String>, String> {
        let started = std::time::Instant::now();
        let digest = app_manifest_digest(&app_id);
        let result = self
            .request(HostAction::RunApp {
                app_id: app_id.clone(),
                command,
                args,
            })
            .await
            .and_then(|result| match result {
                HostResult::AppOutput { output } => Ok(output),
                _ => Err(ClientFault::protocol(
                    "extension host returned the wrong App result",
                )),
            });
        self.emit_result(
            super::protocol::ExtensionKind::App,
            super::protocol::LifecycleAction::Attach,
            &app_id,
            digest.as_deref(),
            started.elapsed(),
            &result,
        );
        self.emit_result(
            super::protocol::ExtensionKind::App,
            action_for_result(&result),
            &app_id,
            digest.as_deref(),
            started.elapsed(),
            &result,
        );
        self.emit_result(
            super::protocol::ExtensionKind::App,
            super::protocol::LifecycleAction::Detach,
            &app_id,
            digest.as_deref(),
            started.elapsed(),
            &result,
        );
        result.map_err(|error| error.message)
    }

    pub async fn open_app(&self, app_id: String) -> Result<usize, String> {
        let started = std::time::Instant::now();
        let digest = app_manifest_digest(&app_id);
        let result = self
            .request(HostAction::AppOpen {
                app_id: app_id.clone(),
            })
            .await
            .and_then(|result| match result {
                HostResult::AppOpened { tool_count } => Ok(tool_count),
                _ => Err(ClientFault::protocol(
                    "extension host returned the wrong App-open result",
                )),
            });
        self.emit_result(
            super::protocol::ExtensionKind::App,
            super::protocol::LifecycleAction::Attach,
            &app_id,
            digest.as_deref(),
            started.elapsed(),
            &result,
        );
        if result.is_ok() {
            self.emit_result(
                super::protocol::ExtensionKind::App,
                super::protocol::LifecycleAction::Ready,
                &app_id,
                digest.as_deref(),
                started.elapsed(),
                &result,
            );
        }
        result.map_err(|error| error.message)
    }

    pub async fn call_app(
        &self,
        app_id: String,
        tool: String,
        arguments: Value,
        timeout: Duration,
    ) -> Result<crate::agent::tools::mcp::protocol::CallToolResult, String> {
        let started = std::time::Instant::now();
        let digest = app_manifest_digest(&app_id);
        let result = self
            .request_with_timeout(
                HostAction::AppCall {
                    app_id: app_id.clone(),
                    tool,
                    arguments,
                },
                timeout.saturating_add(Duration::from_secs(5)),
                true,
            )
            .await
            .and_then(|result| match result {
                HostResult::AppCall { value } => Ok(value),
                _ => Err(ClientFault::protocol(
                    "extension host returned the wrong App-call result",
                )),
            });
        self.emit_result(
            super::protocol::ExtensionKind::App,
            action_for_result(&result),
            &app_id,
            digest.as_deref(),
            started.elapsed(),
            &result,
        );
        result.map_err(|error| error.message)
    }

    pub async fn close_app(&self, app_id: String) -> Result<bool, String> {
        let started = std::time::Instant::now();
        let result = self
            .request(HostAction::AppClose {
                app_id: app_id.clone(),
            })
            .await
            .and_then(|result| match result {
                HostResult::AppClosed { closed } => Ok(closed),
                _ => Err(ClientFault::protocol(
                    "extension host returned the wrong App-close result",
                )),
            });
        self.emit_result(
            super::protocol::ExtensionKind::App,
            super::protocol::LifecycleAction::Detach,
            &app_id,
            app_manifest_digest(&app_id).as_deref(),
            started.elapsed(),
            &result,
        );
        result.map_err(|error| error.message)
    }

    pub async fn attach_mcp(
        &self,
        spec: crate::agent::tools::mcp::integration::McpServerSpec,
    ) -> Result<Vec<crate::agent::tools::mcp::protocol::ToolDescriptor>, String> {
        let started = std::time::Instant::now();
        let name = spec.name.clone();
        let digest = mcp_spec_digest(&spec);
        let result = self
            .request(HostAction::McpAttach { spec })
            .await
            .and_then(|result| match result {
                HostResult::McpAttached { tools } => Ok(tools),
                _ => Err(ClientFault::protocol(
                    "extension host returned the wrong MCP-attach result",
                )),
            });
        self.emit_result(
            super::protocol::ExtensionKind::Mcp,
            super::protocol::LifecycleAction::Attach,
            &name,
            Some(&digest),
            started.elapsed(),
            &result,
        );
        if result.is_ok() {
            self.emit_result(
                super::protocol::ExtensionKind::Mcp,
                super::protocol::LifecycleAction::Ready,
                &name,
                Some(&digest),
                started.elapsed(),
                &result,
            );
        }
        result.map_err(|error| error.message)
    }

    pub async fn call_mcp(
        &self,
        server: String,
        tool: String,
        descriptor_digest: String,
        arguments: Option<Value>,
        timeout: Duration,
        audit: super::protocol::McpInvocationAudit,
    ) -> Result<crate::agent::tools::mcp::protocol::CallToolResult, String> {
        let started = std::time::Instant::now();
        let result = self
            .request_with_timeout(
                HostAction::McpCall {
                    server: server.clone(),
                    tool,
                    descriptor_digest,
                    audit: audit.clone(),
                    arguments,
                },
                timeout.saturating_add(Duration::from_secs(5)),
                true,
            )
            .await
            .and_then(|result| match result {
                HostResult::McpCall { value } => Ok(value),
                _ => Err(ClientFault::protocol(
                    "extension host returned the wrong MCP-call result",
                )),
            });
        self.emit_mcp_result(
            action_for_result(&result),
            &server,
            super::protocol::AuditStage::Host,
            &audit,
            started.elapsed(),
            &result,
        );
        result.map_err(|error| error.message)
    }

    pub async fn detach_mcp(&self, server: String) -> Result<bool, String> {
        let started = std::time::Instant::now();
        let result = self
            .request(HostAction::McpDetach {
                server: server.clone(),
            })
            .await
            .and_then(|result| match result {
                HostResult::McpDetached { detached } => Ok(detached),
                _ => Err(ClientFault::protocol(
                    "extension host returned the wrong MCP-detach result",
                )),
            });
        self.emit_result(
            super::protocol::ExtensionKind::Mcp,
            super::protocol::LifecycleAction::Detach,
            &server,
            None,
            started.elapsed(),
            &result,
        );
        result.map_err(|error| error.message)
    }

    pub async fn attach_agent_extension(
        &self,
        registration: super::protocol::AgentExtensionRegistration,
    ) -> Result<super::abi::AbiBinding, String> {
        let result = self
            .request_with_timeout(
                HostAction::AgentExtensionAttach { registration },
                Duration::from_secs(15),
                false,
            )
            .await
            .and_then(|result| match result {
                HostResult::AgentExtensionReady { binding } => Ok(*binding),
                _ => Err(ClientFault::protocol(
                    "extension host returned the wrong Agent-extension attach result",
                )),
            });
        result.map_err(|error| error.message)
    }

    pub async fn send_agent_extension_event(
        &self,
        extension_id: String,
        binding: super::abi::AbiBinding,
        event_id: String,
        deadline: super::abi::MonotonicDeadlineNs,
        payload: super::abi::EventPayload,
        capability_refs: Vec<crate::agent_extensions::capability_ref::CapabilityReference>,
    ) -> Result<super::protocol::AgentExtensionResult, String> {
        let timeout = deadline.remaining()?;
        let result = self
            .request_with_timeout(
                HostAction::AgentExtensionEvent {
                    extension_id,
                    binding,
                    event_id,
                    deadline_monotonic_ns: deadline,
                    payload,
                    capability_refs,
                },
                timeout.saturating_add(Duration::from_secs(2)),
                false,
            )
            .await
            .and_then(|result| match result {
                HostResult::AgentExtensionEvent { value } => Ok(value),
                _ => Err(ClientFault::protocol(
                    "extension host returned the wrong Agent-extension event result",
                )),
            });
        result.map_err(|error| error.message)
    }

    pub async fn detach_agent_extension(
        &self,
        extension_id: String,
        binding: super::abi::AbiBinding,
        reason: super::abi::ShutdownReason,
    ) -> Result<bool, String> {
        let result = self
            .request_with_timeout(
                HostAction::AgentExtensionDetach {
                    extension_id,
                    binding,
                    reason,
                },
                Duration::from_secs(5),
                false,
            )
            .await
            .and_then(|result| match result {
                HostResult::AgentExtensionDetached { detached } => Ok(detached),
                _ => Err(ClientFault::protocol(
                    "extension host returned the wrong Agent-extension detach result",
                )),
            });
        result.map_err(|error| error.message)
    }

    async fn request(&self, action: HostAction) -> ClientResult<HostResult> {
        self.request_with_timeout(
            action,
            Duration::from_millis(DEFAULT_REQUEST_TIMEOUT_MS),
            true,
        )
        .await
    }

    async fn request_with_timeout(
        &self,
        action: HostAction,
        timeout: Duration,
        cancel_on_timeout: bool,
    ) -> ClientResult<HostResult> {
        self.binding
            .validate_worker(
                std::process::id(),
                crate::proc::read_start_time_ticks_pub(std::process::id()),
            )
            .map_err(ClientFault::protocol)?;
        let request = ControlRequest::new(
            &self.binding,
            action,
            timeout
                .saturating_sub(Duration::from_millis(250))
                .as_millis()
                .min(u128::from(u64::MAX)) as u64,
        );
        let request_id = request.id.clone();
        match tokio::time::timeout(timeout, self.exchange(request)).await {
            Ok(result) => result,
            Err(_) => {
                if cancel_on_timeout {
                    self.cancel_best_effort(request_id);
                }
                Err(ClientFault::new(
                    ExtensionErrorCategory::Timeout,
                    format!(
                        "extension host request timed out after {}ms",
                        timeout.as_millis()
                    ),
                ))
            }
        }
    }

    async fn exchange(&self, request: ControlRequest) -> ClientResult<HostResult> {
        let path = Path::new(&self.binding.control_socket);
        let mut stream = UnixStream::connect(path).await.map_err(|error| {
            ClientFault::new(
                ExtensionErrorCategory::Connect,
                format!("connect extension host {}: {error}", path.display()),
            )
        })?;
        verify_host_peer(&stream, &self.binding)
            .map_err(|error| ClientFault::new(ExtensionErrorCategory::Crash, error))?;
        let body = serde_json::to_vec(&request).map_err(|error| {
            ClientFault::protocol(format!("encode extension-host request: {error}"))
        })?;
        if body.len() > MAX_CONTROL_FRAME_BYTES {
            return Err(ClientFault::protocol(
                "extension-host request exceeds its frame limit",
            ));
        }
        crate::clawd::transport::frame::write_request_async(&mut stream, &body)
            .await
            .map_err(|error| {
                ClientFault::new(
                    ExtensionErrorCategory::Crash,
                    format!("write extension-host request: {error}"),
                )
            })?;
        let body = crate::clawd::transport::frame::read_response_async(
            &mut stream,
            MAX_CONTROL_FRAME_BYTES,
        )
        .await
        .map_err(|fault| {
            let category = response_fault_category(fault);
            ClientFault::new(
                category,
                format!("read extension-host response: {}", fault.message()),
            )
        })?;
        let response: ControlResponse = serde_json::from_slice(&body).map_err(|_| {
            ClientFault::protocol("extension-host response is not a valid envelope")
        })?;
        if response.protocol != PROTOCOL_VERSION {
            return Err(ClientFault::protocol(format!(
                "extension-host response protocol is v{}, expected v{}",
                response.protocol, PROTOCOL_VERSION
            )));
        }
        if response.id != request.id {
            return Err(ClientFault::protocol(
                "extension-host response did not correlate with the request",
            ));
        }
        if !response.ok {
            let category = response
                .error_category
                .unwrap_or(ExtensionErrorCategory::Protocol);
            return Err(ClientFault::new(
                category,
                response
                    .error
                    .unwrap_or_else(|| "extension host request failed".to_string()),
            ));
        }
        response
            .result
            .ok_or_else(|| ClientFault::protocol("extension host response omitted its result"))
    }

    fn cancel_best_effort(&self, request_id: RequestId) {
        let binding = self.binding.clone();
        std::thread::spawn(move || {
            let request = ControlRequest::new(
                &binding,
                HostAction::Cancel { request_id },
                DEFAULT_REQUEST_TIMEOUT_MS,
            );
            let _ = exchange_blocking(&binding, &request, false);
        });
    }

    fn shutdown_best_effort(&self) {
        let request = ControlRequest::new(
            &self.binding,
            HostAction::Shutdown,
            DEFAULT_REQUEST_TIMEOUT_MS,
        );
        let _ = exchange_blocking(&self.binding, &request, false);
    }

    #[allow(clippy::too_many_arguments)]
    fn emit(
        &self,
        kind: super::protocol::ExtensionKind,
        action: super::protocol::LifecycleAction,
        extension_id: &str,
        manifest_digest: Option<&str>,
        success: bool,
        latency: Duration,
        error: Option<&str>,
    ) {
        let (Some(audit), Some(session_id)) =
            (self.audit.as_ref(), self.binding.session_id.as_ref())
        else {
            return;
        };
        let _ = audit.send(crate::agentd::protocol::WorkerFrame::Audit {
            task_id: self.binding.task_id.clone(),
            record: Box::new(
                crate::agentd::protocol::RuntimeAuditRecord::ExtensionLifecycle {
                    session_id: session_id.clone(),
                    kind,
                    action,
                    extension_id: super::protocol::clamp_text(extension_id, 128),
                    binding_digest: self.binding_digest.clone(),
                    lease_digest: self.lease_digest.clone(),
                    stage: None,
                    mcp: None,
                    abi: None,
                    manifest_digest: manifest_digest.map(str::to_string),
                    success,
                    latency_ms: latency.as_millis().min(u128::from(u64::MAX)) as u64,
                    error: crate::audit_policy::optional_text_digest(error),
                },
            ),
        });
    }

    pub(crate) fn emit_mcp_gateway(
        &self,
        audit: &super::protocol::McpInvocationAudit,
        success: bool,
        error: Option<&str>,
    ) {
        self.emit_mcp(
            super::protocol::LifecycleAction::Call,
            &audit.server_identity,
            super::protocol::AuditStage::Gateway,
            audit,
            success,
            Duration::ZERO,
            error,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_mcp(
        &self,
        action: super::protocol::LifecycleAction,
        extension_id: &str,
        stage: super::protocol::AuditStage,
        mcp: &super::protocol::McpInvocationAudit,
        success: bool,
        latency: Duration,
        error: Option<&str>,
    ) {
        let (Some(audit), Some(session_id)) =
            (self.audit.as_ref(), self.binding.session_id.as_ref())
        else {
            return;
        };
        let _ = audit.send(crate::agentd::protocol::WorkerFrame::Audit {
            task_id: self.binding.task_id.clone(),
            record: Box::new(
                crate::agentd::protocol::RuntimeAuditRecord::ExtensionLifecycle {
                    session_id: session_id.clone(),
                    kind: super::protocol::ExtensionKind::Mcp,
                    action,
                    extension_id: super::protocol::clamp_text(extension_id, 128),
                    binding_digest: self.binding_digest.clone(),
                    lease_digest: self.lease_digest.clone(),
                    stage: Some(stage),
                    mcp: Some(mcp.clone()),
                    abi: None,
                    manifest_digest: Some(mcp.descriptor_digest.clone()),
                    success,
                    latency_ms: latency.as_millis().min(u128::from(u64::MAX)) as u64,
                    error: crate::audit_policy::optional_text_digest(error),
                },
            ),
        });
    }

    fn emit_mcp_result<T>(
        &self,
        action: super::protocol::LifecycleAction,
        extension_id: &str,
        stage: super::protocol::AuditStage,
        mcp: &super::protocol::McpInvocationAudit,
        latency: Duration,
        result: &ClientResult<T>,
    ) {
        self.emit_mcp(
            action,
            extension_id,
            stage,
            mcp,
            result.is_ok(),
            latency,
            result.as_ref().err().map(|error| error.message.as_str()),
        );
    }

    fn emit_result<T>(
        &self,
        kind: super::protocol::ExtensionKind,
        action: super::protocol::LifecycleAction,
        extension_id: &str,
        manifest_digest: Option<&str>,
        latency: Duration,
        result: &ClientResult<T>,
    ) {
        self.emit(
            kind,
            action,
            extension_id,
            manifest_digest,
            result.is_ok(),
            latency,
            result.as_ref().err().map(|error| error.message.as_str()),
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_agent_extension(
        &self,
        action: super::protocol::LifecycleAction,
        extension_id: &str,
        manifest_digest: &str,
        abi: super::protocol::AgentExtensionAudit,
        success: bool,
        latency: Duration,
        error: Option<&str>,
    ) {
        let (Some(audit), Some(session_id)) =
            (self.audit.as_ref(), self.binding.session_id.as_ref())
        else {
            return;
        };
        if abi.validate().is_err() {
            return;
        }
        let _ = audit.send(crate::agentd::protocol::WorkerFrame::Audit {
            task_id: self.binding.task_id.clone(),
            record: Box::new(
                crate::agentd::protocol::RuntimeAuditRecord::ExtensionLifecycle {
                    session_id: session_id.clone(),
                    kind: super::protocol::ExtensionKind::AgentExtension,
                    action,
                    extension_id: super::protocol::clamp_text(extension_id, 128),
                    binding_digest: self.binding_digest.clone(),
                    lease_digest: self.lease_digest.clone(),
                    stage: None,
                    mcp: None,
                    abi: Some(Box::new(abi)),
                    manifest_digest: Some(manifest_digest.to_string()),
                    success,
                    latency_ms: latency.as_millis().min(u128::from(u64::MAX)) as u64,
                    error: crate::audit_policy::optional_text_digest(error),
                },
            ),
        });
    }
}

fn response_fault_category(fault: crate::clawd::wire::Fault) -> ExtensionErrorCategory {
    match fault {
        crate::clawd::wire::Fault::TruncatedFrame => ExtensionErrorCategory::Crash,
        crate::clawd::wire::Fault::ReadTimeout
        | crate::clawd::wire::Fault::WriteTimeout
        | crate::clawd::wire::Fault::RouteTimeout => ExtensionErrorCategory::Timeout,
        _ => ExtensionErrorCategory::Protocol,
    }
}

fn action_for_result<T>(result: &ClientResult<T>) -> super::protocol::LifecycleAction {
    match result {
        Ok(_) => super::protocol::LifecycleAction::Call,
        Err(error) => match error.category {
            ExtensionErrorCategory::Connect => super::protocol::LifecycleAction::Connect,
            ExtensionErrorCategory::Timeout => super::protocol::LifecycleAction::Timeout,
            ExtensionErrorCategory::Crash => super::protocol::LifecycleAction::Crash,
            ExtensionErrorCategory::RemoteCallFailure => {
                super::protocol::LifecycleAction::RemoteCallFailure
            }
            ExtensionErrorCategory::Protocol => super::protocol::LifecycleAction::Protocol,
        },
    }
}

fn app_manifest_digest(app_id: &str) -> Option<String> {
    let root = std::path::PathBuf::from(
        std::env::var("COS_APPS_DIR").unwrap_or_else(|_| "/usr/lib/cos/apps".to_string()),
    );
    let app = crate::apps::find(&root, app_id)?;
    let bytes = std::fs::read(app.dir.join("app.json")).ok()?;
    Some(crate::crypto::sha256_hex(&bytes))
}

fn mcp_spec_digest(spec: &crate::agent::tools::mcp::integration::McpServerSpec) -> String {
    let mut env_keys = spec.env.keys().cloned().collect::<Vec<_>>();
    env_keys.sort();
    let value = serde_json::json!({
        "name": spec.name,
        "command": spec.command,
        "args": spec.args,
        "env_keys": env_keys,
        "cwd": spec.cwd,
        "timeout_secs": spec.timeout_secs,
        "url": spec.url,
        "bearer_env": spec.bearer_env,
    });
    crate::crypto::sha256_hex(value.to_string().as_bytes())
}

fn verify_host_peer(stream: &UnixStream, binding: &ExtensionBinding) -> Result<(), String> {
    let credentials = stream
        .peer_cred()
        .map_err(|error| format!("read extension-host peer credentials: {error}"))?;
    let pid = credentials
        .pid()
        .and_then(|pid| u32::try_from(pid).ok())
        .ok_or_else(|| "extension-host peer pid is unavailable".to_string())?;
    if credentials.uid() != binding.extension_uid || pid != binding.host_pid {
        return Err("extension-host socket belongs to a different process".to_string());
    }
    if crate::proc::read_start_time_ticks_pub(pid) != binding.host_start_time_ticks {
        return Err("extension-host process identity changed".to_string());
    }
    Ok(())
}

fn exchange_blocking(
    binding: &ExtensionBinding,
    request: &ControlRequest,
    read_response: bool,
) -> Result<(), String> {
    let path = Path::new(&binding.control_socket);
    let mut stream = std::os::unix::net::UnixStream::connect(path)
        .map_err(|error| format!("connect extension host {}: {error}", path.display()))?;
    let body = serde_json::to_vec(request)
        .map_err(|error| format!("encode extension-host request: {error}"))?;
    if body.len() > MAX_CONTROL_FRAME_BYTES {
        return Err("extension-host request exceeds its frame limit".to_string());
    }
    crate::clawd::transport::frame::write_request_blocking(&mut stream, &body)
        .map_err(|error| format!("write extension-host request: {error}"))?;
    if read_response {
        let _ = crate::clawd::transport::frame::read_response_blocking(
            &mut stream,
            MAX_CONTROL_FRAME_BYTES,
        )
        .map_err(|fault| format!("read extension-host response: {}", fault.message()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/extension_host/client.rs"
    ));
}
