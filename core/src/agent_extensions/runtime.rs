//! Non-blocking observational event fanout and mediated proposed actions.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot};

use crate::agent::runtime::hooks::{
    Hook, HookContext, HookOutcome, ToolDecision, ToolResultSummary, TurnSummary,
};
use crate::agent::tools::exposure::ToolExposureContext;
use crate::agent::tools::registry::ToolRegistry;
use crate::caps::CapSet;
use crate::extension_host::abi::{EventPayload, ShutdownReason};
use crate::extension_host::client::ExtensionHostClient;
use crate::extension_host::protocol::{
    AgentExtensionAudit, AgentExtensionRegistration, LifecycleAction,
};

use super::capability_ref::{CapabilityReferenceStore, ReferenceContext};
use super::manifest::{EventKind, ExtensionManifest};
use super::registry::{installed_root, ExtensionRegistry, RegisteredExtension};

const DISABLE_AFTER_DROPS: usize = 8;
const ACTION_TIMEOUT: Duration = Duration::from_secs(30);

pub struct ExtensionRuntime {
    hook_name: Option<String>,
    controls: Vec<ExtensionControl>,
}

struct ExtensionControl {
    control: mpsc::Sender<Control>,
    worker: tokio::task::JoinHandle<()>,
    timeout: Duration,
}

enum Control {
    Completion {
        payload: EventPayload,
        done: oneshot::Sender<()>,
    },
    Shutdown {
        done: oneshot::Sender<()>,
    },
}

#[derive(Clone)]
struct ExtensionSink {
    id: String,
    manifest_digest: String,
    package_digest: String,
    capability_generation: String,
    subscriptions: BTreeSet<EventKind>,
    sender: mpsc::Sender<EventPayload>,
    client: Arc<ExtensionHostClient>,
    binding: crate::extension_host::abi::AbiBinding,
    disabled: Arc<AtomicBool>,
    consecutive_drops: Arc<AtomicUsize>,
}

struct ExtensionObserver {
    name: String,
    sinks: Vec<ExtensionSink>,
}

impl ExtensionRuntime {
    pub async fn activate(
        configured: &[String],
        exposure: &mut ToolExposureContext,
        tools: Arc<ToolRegistry>,
    ) -> Self {
        if configured.is_empty() {
            return Self {
                hook_name: None,
                controls: Vec::new(),
            };
        }
        let Some(client) = crate::extension_host::client::current() else {
            tracing::warn!(
                "Agent extensions were configured but no task extension host is available"
            );
            return Self {
                hook_name: None,
                controls: Vec::new(),
            };
        };
        let registry = ExtensionRegistry::load_selected(&installed_root(), configured);
        for quarantined in &registry.quarantined {
            tracing::warn!(
                extension = %quarantined.id,
                diagnostic = %quarantined.diagnostic,
                "Agent extension activation quarantined"
            );
        }

        let refs = Arc::new(CapabilityReferenceStore::default());
        let mut sinks = Vec::new();
        let mut controls = Vec::new();
        for extension in registry.registered.into_values() {
            match activate_one(
                extension,
                exposure,
                tools.clone(),
                client.clone(),
                refs.clone(),
            )
            .await
            {
                Ok((sink, control)) => {
                    exposure.enable_extension(sink.id.clone());
                    sinks.push(sink);
                    controls.push(control);
                }
                Err(error) => {
                    tracing::warn!(%error, "Agent extension failed closed during activation");
                }
            }
        }
        if sinks.is_empty() {
            return Self {
                hook_name: None,
                controls,
            };
        }
        let hook_name = format!("agent-extension-observer-{}", uuid::Uuid::new_v4().simple());
        let observer = Arc::new(ExtensionObserver {
            name: hook_name.clone(),
            sinks,
        });
        crate::agent::runtime::hooks::global_registry().register(observer.clone());
        observer.publish(EventPayload::SessionStart {
            source: exposure.client().source.as_str().to_string(),
            attended: exposure.is_attended_local(),
            delegated: exposure.client().source == crate::session::SessionSource::DelegatedAgent,
        });
        Self {
            hook_name: Some(hook_name),
            controls,
        }
    }

    pub async fn finish(
        mut self,
        success: bool,
        turns: u32,
        answer: Option<&str>,
        error: Option<&str>,
    ) {
        if let Some(name) = self.hook_name.take() {
            crate::agent::runtime::hooks::global_registry().unregister(&name);
        }
        let answer = answer.unwrap_or_default();
        let payload = EventPayload::Completion {
            success,
            turns,
            answer_bytes: answer.len(),
            answer_digest: crate::crypto::sha256_hex(answer.as_bytes()),
            error: crate::audit_policy::optional_text_digest(error),
        };
        for control in &self.controls {
            let (done_tx, done_rx) = oneshot::channel();
            if control
                .control
                .send(Control::Completion {
                    payload: payload.clone(),
                    done: done_tx,
                })
                .await
                .is_ok()
            {
                let _ =
                    tokio::time::timeout(control.timeout + Duration::from_secs(2), done_rx).await;
            }
        }
        for control in self.controls.drain(..) {
            let (done_tx, done_rx) = oneshot::channel();
            let _ = control
                .control
                .send(Control::Shutdown { done: done_tx })
                .await;
            let _ = tokio::time::timeout(Duration::from_secs(4), done_rx).await;
            let mut worker = control.worker;
            if tokio::time::timeout(Duration::from_secs(1), &mut worker)
                .await
                .is_err()
            {
                worker.abort();
                tracing::warn!("Agent extension worker did not stop before task teardown");
            }
        }
    }
}

async fn activate_one(
    extension: RegisteredExtension,
    exposure: &ToolExposureContext,
    tools: Arc<ToolRegistry>,
    client: Arc<ExtensionHostClient>,
    refs: Arc<CapabilityReferenceStore>,
) -> Result<(ExtensionSink, ExtensionControl), String> {
    let manifest = extension.manifest;
    let registration = AgentExtensionRegistration {
        extension_id: manifest.identity.id.clone(),
        extension_version: manifest.identity.version.clone(),
        package_digest: extension.package.digest().to_string(),
        manifest_digest: extension.manifest_digest.clone(),
        content_digest: manifest.identity.content_digest.clone(),
    };
    let started = Instant::now();
    let binding = client
        .attach_agent_extension(registration, extension.package.snapshot())
        .await;
    let audit = audit_metadata(
        extension.package.digest(),
        exposure.capability_generation(),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    );
    client.emit_agent_extension(
        LifecycleAction::Initialize,
        &manifest.identity.id,
        &extension.manifest_digest,
        audit.clone(),
        binding.is_ok(),
        started.elapsed(),
        binding.as_ref().err().map(String::as_str),
    );
    let binding = binding?;
    client.emit_agent_extension(
        LifecycleAction::Ready,
        &manifest.identity.id,
        &extension.manifest_digest,
        audit,
        true,
        started.elapsed(),
        None,
    );
    let (sender, receiver) = mpsc::channel(manifest.limits.queue_capacity);
    let (control_tx, control_rx) = mpsc::channel(1);
    let disabled = Arc::new(AtomicBool::new(false));
    let sink = ExtensionSink {
        id: manifest.identity.id.clone(),
        manifest_digest: extension.manifest_digest.clone(),
        package_digest: extension.package.digest().to_string(),
        capability_generation: exposure.capability_generation().to_string(),
        subscriptions: manifest.subscriptions.iter().copied().collect(),
        sender,
        client: client.clone(),
        binding: binding.clone(),
        disabled: disabled.clone(),
        consecutive_drops: Arc::new(AtomicUsize::new(0)),
    };
    let timeout = Duration::from_millis(manifest.limits.event_timeout_ms);
    let worker = tokio::spawn(run_extension(
        manifest,
        extension.manifest_digest,
        extension.package.digest().to_string(),
        binding,
        exposure.clone(),
        tools,
        client,
        refs,
        disabled,
        receiver,
        control_rx,
    ));
    Ok((
        sink,
        ExtensionControl {
            control: control_tx,
            worker,
            timeout,
        },
    ))
}

#[allow(clippy::too_many_arguments)]
async fn run_extension(
    manifest: ExtensionManifest,
    manifest_digest: String,
    package_digest: String,
    binding: crate::extension_host::abi::AbiBinding,
    exposure: ToolExposureContext,
    tools: Arc<ToolRegistry>,
    client: Arc<ExtensionHostClient>,
    refs: Arc<CapabilityReferenceStore>,
    disabled: Arc<AtomicBool>,
    mut events: mpsc::Receiver<EventPayload>,
    mut controls: mpsc::Receiver<Control>,
) {
    loop {
        tokio::select! {
            biased;
            control = controls.recv() => match control {
                Some(Control::Completion { payload, done }) => {
                    if !disabled.load(Ordering::Acquire) {
                        let _ = process_event(
                            &manifest, &manifest_digest, &package_digest, &binding,
                            &exposure, &tools, &client, &refs, &disabled, payload,
                        ).await;
                    }
                    let _ = done.send(());
                }
                Some(Control::Shutdown { done }) => {
                    let _ = client.detach_agent_extension(
                        manifest.identity.id.clone(),
                        binding.clone(),
                        ShutdownReason::TaskComplete,
                    ).await;
                    client.emit_agent_extension(
                        LifecycleAction::Shutdown,
                        &manifest.identity.id,
                        &manifest_digest,
                        audit_metadata(
                            &package_digest,
                            exposure.capability_generation(),
                            None, None, None, None, None, None, None,
                        ),
                        true,
                        Duration::ZERO,
                        None,
                    );
                    let _ = done.send(());
                    break;
                }
                None => break,
            },
            event = events.recv() => {
                let Some(event) = event else { break; };
                if disabled.load(Ordering::Acquire) {
                    continue;
                }
                if let Err(error) = process_event(
                    &manifest, &manifest_digest, &package_digest, &binding,
                    &exposure, &tools, &client, &refs, &disabled, event,
                ).await {
                    disabled.store(true, Ordering::Release);
                    client.emit_agent_extension(
                        LifecycleAction::Disable,
                        &manifest.identity.id,
                        &manifest_digest,
                        audit_metadata(
                            &package_digest,
                            exposure.capability_generation(),
                            None, None, None, None, None, None, None,
                        ),
                        false,
                        Duration::ZERO,
                        Some(&error),
                    );
                    let _ = client.detach_agent_extension(
                        manifest.identity.id.clone(),
                        binding.clone(),
                        ShutdownReason::ProtocolFailure,
                    ).await;
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn process_event(
    manifest: &ExtensionManifest,
    manifest_digest: &str,
    package_digest: &str,
    binding: &crate::extension_host::abi::AbiBinding,
    exposure: &ToolExposureContext,
    tools: &ToolRegistry,
    client: &ExtensionHostClient,
    refs: &CapabilityReferenceStore,
    disabled: &AtomicBool,
    payload: EventPayload,
) -> Result<(), String> {
    let event_id = uuid::Uuid::new_v4().simple().to_string();
    let timeout = Duration::from_millis(manifest.limits.event_timeout_ms);
    let expires_at_ms = now_ms().saturating_add(manifest.limits.event_timeout_ms);
    let reference_context = ReferenceContext {
        owner_uid: exposure.owner_uid(),
        session_id: exposure.authority_session_id(),
        task_id: exposure.task_id().unwrap_or_default(),
        extension_id: &manifest.identity.id,
        manifest_digest,
        capability_generation: exposure.capability_generation(),
        event_id: &event_id,
        expires_at_ms,
    };
    let capability_refs = refs.issue(&reference_context, &manifest.requested_capabilities)?;
    let kind = payload.kind();
    let started = Instant::now();
    client.emit_agent_extension(
        LifecycleAction::Event,
        &manifest.identity.id,
        manifest_digest,
        audit_metadata(
            package_digest,
            exposure.capability_generation(),
            Some(kind),
            Some(&event_id),
            None,
            None,
            None,
            None,
            None,
        ),
        true,
        Duration::ZERO,
        None,
    );
    let result = client
        .send_agent_extension_event(
            manifest.identity.id.clone(),
            binding.clone(),
            event_id.clone(),
            payload,
            capability_refs,
            timeout,
        )
        .await?;
    client.emit_agent_extension(
        LifecycleAction::Result,
        &manifest.identity.id,
        manifest_digest,
        audit_metadata(
            package_digest,
            exposure.capability_generation(),
            Some(kind),
            Some(&event_id),
            result.output.as_deref(),
            None,
            None,
            None,
            None,
        ),
        true,
        started.elapsed(),
        None,
    );
    if disabled.load(Ordering::Acquire) {
        return Ok(());
    }
    for action in result.proposed_actions {
        let requested = manifest
            .requested_capabilities
            .get(action.capability_ref.requested_index)
            .ok_or_else(|| "extension action named an unknown capability index".to_string())?;
        let cap = refs.consume(&reference_context, &action.capability_ref)?;
        if &cap != requested {
            return Err(
                "extension action capability reference did not match its index".to_string(),
            );
        }
        let ceiling = CapSet::from_caps([cap]);
        let action_exposure = exposure.attenuated_for_extension(&manifest.identity.id, &ceiling);
        let call = crate::agent::llm::ToolCall {
            id: action.action_id.clone(),
            name: action.tool.clone(),
            input: action.input,
        };
        let input = crate::agent::runtime::turn::effective_tool_input(
            &call,
            exposure.conversation_session_id(),
            &action_exposure,
        );
        let started = Instant::now();
        let result = match tokio::time::timeout(
            ACTION_TIMEOUT,
            crate::caps::enforcement::with_capability_ceiling(
                ceiling,
                tools.execute(
                    &action_exposure,
                    &call.name,
                    input,
                    "policy: Agent extension proposed action",
                ),
            ),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => crate::agent::tools::ToolResult::err(format!(
                "Agent extension proposed action timed out after {}s",
                ACTION_TIMEOUT.as_secs()
            )),
        };
        client.emit_agent_extension(
            LifecycleAction::Action,
            &manifest.identity.id,
            manifest_digest,
            audit_metadata(
                package_digest,
                exposure.capability_generation(),
                Some(kind),
                Some(&event_id),
                Some(&result.content),
                Some(&call.id),
                Some(&call.name),
                Some(&action.capability_ref.handle),
                None,
            ),
            !result.is_error,
            started.elapsed(),
            result.is_error.then_some(result.content.as_str()),
        );
    }
    Ok(())
}

impl ExtensionObserver {
    fn publish(&self, payload: EventPayload) {
        for sink in &self.sinks {
            sink.publish(payload.clone());
        }
    }
}

impl ExtensionSink {
    fn publish(&self, payload: EventPayload) {
        if self.disabled.load(Ordering::Acquire) || !self.subscriptions.contains(&payload.kind()) {
            return;
        }
        match self.sender.try_send(payload) {
            Ok(()) => {
                self.consecutive_drops.store(0, Ordering::Release);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.disabled.store(true, Ordering::Release);
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                let queue_depth = self.sender.max_capacity() - self.sender.capacity();
                self.client.emit_agent_extension(
                    LifecycleAction::BackpressureDrop,
                    &self.id,
                    &self.manifest_digest,
                    audit_metadata(
                        &self.package_digest,
                        &self.capability_generation,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        Some(queue_depth),
                    ),
                    false,
                    Duration::ZERO,
                    Some("extension event queue is full"),
                );
                if register_backpressure_drop(&self.consecutive_drops, &self.disabled) {
                    self.client.emit_agent_extension(
                        LifecycleAction::Disable,
                        &self.id,
                        &self.manifest_digest,
                        audit_metadata(
                            &self.package_digest,
                            &self.capability_generation,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            Some(queue_depth),
                        ),
                        false,
                        Duration::ZERO,
                        Some("extension disabled after repeated backpressure"),
                    );
                    if let Ok(handle) = tokio::runtime::Handle::try_current() {
                        let client = self.client.clone();
                        let id = self.id.clone();
                        let binding = self.binding.clone();
                        handle.spawn(async move {
                            let _ = client
                                .detach_agent_extension(id, binding, ShutdownReason::Disabled)
                                .await;
                        });
                    }
                }
            }
        }
    }
}

impl Hook for ExtensionObserver {
    fn name(&self) -> &str {
        &self.name
    }

    fn pre_turn(&self, ctx: &HookContext) -> HookOutcome {
        self.publish(EventPayload::PreModelCall {
            turn_index: ctx.turn_index,
            provider: ctx.provider.clone(),
            model: ctx.model.clone(),
        });
        HookOutcome::Continue
    }

    fn post_turn(&self, ctx: &HookContext, summary: &TurnSummary) -> HookOutcome {
        self.publish(EventPayload::PostModelCall {
            turn_index: ctx.turn_index,
            success: summary.success,
            latency_ms: summary.latency_ms,
            input_tokens: summary.input_tokens,
            output_tokens: summary.output_tokens,
            error: crate::audit_policy::optional_text_digest(summary.error.as_deref()),
        });
        HookOutcome::Continue
    }

    fn pre_tool(&self, ctx: &HookContext, tool_call: &crate::agent::llm::ToolCall) -> ToolDecision {
        let input = serde_json::to_vec(&tool_call.input).unwrap_or_default();
        self.publish(EventPayload::PreTool {
            turn_index: ctx.turn_index,
            tool: tool_call.name.clone(),
            tool_use_id_digest: crate::crypto::sha256_hex(tool_call.id.as_bytes()),
            input_bytes: input.len(),
            input_digest: crate::crypto::sha256_hex(&input),
        });
        ToolDecision::Allow
    }

    fn post_tool(
        &self,
        ctx: &HookContext,
        tool_call: &crate::agent::llm::ToolCall,
        result: &ToolResultSummary,
    ) -> HookOutcome {
        let summary = format!(
            "{}:{}:{}",
            result.success, result.bytes_returned, result.latency_ms
        );
        self.publish(EventPayload::PostTool {
            turn_index: ctx.turn_index,
            tool: tool_call.name.clone(),
            tool_use_id_digest: crate::crypto::sha256_hex(tool_call.id.as_bytes()),
            success: result.success,
            latency_ms: result.latency_ms,
            result_bytes: result.bytes_returned,
            result_digest: crate::crypto::sha256_hex(summary.as_bytes()),
            error: crate::audit_policy::optional_text_digest(result.error.as_deref()),
        });
        HookOutcome::Continue
    }
}

#[allow(clippy::too_many_arguments)]
fn audit_metadata(
    package_digest: &str,
    capability_generation: &str,
    event_kind: Option<EventKind>,
    event_id: Option<&str>,
    output: Option<&str>,
    action_id: Option<&str>,
    tool: Option<&str>,
    capability_ref: Option<&str>,
    queue_depth: Option<usize>,
) -> AgentExtensionAudit {
    AgentExtensionAudit {
        package_digest: package_digest.to_string(),
        capability_generation: capability_generation.to_string(),
        event_kind,
        event_id: event_id.map(crate::audit_policy::text_digest),
        output: output.map(crate::audit_policy::text_digest),
        action_id: action_id.map(crate::audit_policy::text_digest),
        tool: tool.map(str::to_string),
        capability_ref: capability_ref.map(crate::audit_policy::text_digest),
        queue_depth,
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn register_backpressure_drop(drops: &AtomicUsize, disabled: &AtomicBool) -> bool {
    let drops = drops.fetch_add(1, Ordering::AcqRel) + 1;
    drops >= DISABLE_AFTER_DROPS && !disabled.swap(true, Ordering::AcqRel)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent_extensions/runtime.rs"
    ));
}
