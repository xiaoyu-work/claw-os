//! Non-blocking observational event fanout and mediated proposed actions.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit, Semaphore};

use crate::agent::runtime::hooks::{
    Hook, HookContext, HookOutcome, ToolDecision, ToolResultSummary,
};
use crate::agent::tools::exposure::ToolExposureContext;
use crate::agent::tools::registry::ToolRegistry;
use crate::extension_host::abi::{EventPayload, MonotonicDeadlineNs, ShutdownReason};
use crate::extension_host::client::ExtensionHostClient;
use crate::extension_host::protocol::{
    AgentExtensionAudit, AgentExtensionRegistration, LifecycleAction,
};

use super::capability_ref::{ActionReferenceBinding, CapabilityReferenceStore, ReferenceContext};
use super::manifest::{EventKind, ExtensionManifest};
use super::registry::{installed_root, ExtensionRegistry, RegisteredExtension};

const DISABLE_AFTER_DROPS: usize = 8;
const ACTION_TIMEOUT: Duration = Duration::from_secs(30);
const TRUST_RECHECK_INTERVAL: Duration = Duration::from_secs(5);
const FINISH_TIMEOUT: Duration = Duration::from_secs(8);
const FINISH_DRAIN_TIMEOUT: Duration = Duration::from_secs(6);
const DETACH_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(1);
const DETACH_RETRY_DELAY: Duration = Duration::from_millis(50);
const CONTAINMENT_ESCALATION_RESERVE: Duration = Duration::from_millis(500);
const MAX_DETACH_ATTEMPTS: usize = 3;

struct AbortOnDrop(Option<tokio::task::AbortHandle>);

impl AbortOnDrop {
    fn new(handle: tokio::task::AbortHandle) -> Self {
        Self(Some(handle))
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

pub struct ExtensionRuntime {
    hook_name: Option<String>,
    hooks: Option<crate::agent::runtime::hooks::HookRegistry>,
    controls: Vec<ExtensionControl>,
    attempt_observer: Arc<dyn crate::agent::llm::attempt_observer::ProviderAttemptObserver>,
}

struct ExtensionControl {
    sender: mpsc::Sender<ExtensionWork>,
    worker: Option<tokio::task::JoinHandle<()>>,
    client: Arc<ExtensionHostClient>,
    binding: crate::extension_host::abi::AbiBinding,
    id: String,
    manifest_digest: String,
    package_digest: String,
    capability_generation: String,
    completion_subscribed: bool,
    terminal: Arc<AtomicBool>,
    detach_acknowledged: Arc<AtomicBool>,
    terminal_failure: Arc<Mutex<Option<String>>>,
}

enum ExtensionWork {
    Event {
        payload: EventPayload,
        _permit: OwnedSemaphorePermit,
    },
    Finish {
        completion: Option<EventPayload>,
        reason: ShutdownReason,
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
    sender: mpsc::Sender<ExtensionWork>,
    event_slots: Arc<Semaphore>,
    client: Arc<ExtensionHostClient>,
    accepting: Arc<AtomicBool>,
    security_disabled: Arc<AtomicBool>,
    terminal: Arc<AtomicBool>,
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
        hooks: crate::agent::runtime::hooks::HookRegistry,
    ) -> Self {
        if configured.is_empty() {
            return Self {
                hook_name: None,
                hooks: None,
                controls: Vec::new(),
                attempt_observer: Arc::new(
                    crate::agent::llm::attempt_observer::NoopProviderAttemptObserver,
                ),
            };
        }
        let Some(client) = crate::extension_host::client::current() else {
            tracing::warn!(
                "Agent extensions were configured but no task extension host is available"
            );
            return Self {
                hook_name: None,
                hooks: None,
                controls: Vec::new(),
                attempt_observer: Arc::new(
                    crate::agent::llm::attempt_observer::NoopProviderAttemptObserver,
                ),
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

        let mut sinks = Vec::new();
        let mut controls = Vec::new();
        for extension in registry.registered.into_values() {
            match activate_one(extension, exposure, tools.clone(), client.clone()).await {
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
                hooks: None,
                controls,
                attempt_observer: Arc::new(
                    crate::agent::llm::attempt_observer::NoopProviderAttemptObserver,
                ),
            };
        }
        let hook_name = format!("agent-extension-observer-{}", uuid::Uuid::new_v4().simple());
        let observer = Arc::new(ExtensionObserver {
            name: hook_name.clone(),
            sinks,
        });
        hooks.register(observer.clone());
        observer.publish(EventPayload::SessionStart {
            source: exposure.client().source.as_str().to_string(),
            attended: exposure.is_attended_local(),
            delegated: exposure.client().source == crate::session::SessionSource::DelegatedAgent,
        });
        Self {
            hook_name: Some(hook_name),
            hooks: Some(hooks),
            controls,
            attempt_observer: observer,
        }
    }

    pub fn attempt_observer(
        &self,
    ) -> Arc<dyn crate::agent::llm::attempt_observer::ProviderAttemptObserver> {
        Arc::clone(&self.attempt_observer)
    }

    pub async fn finish(
        mut self,
        success: bool,
        turns: u32,
        answer: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), String> {
        if let Some(name) = self.hook_name.take() {
            if let Some(hooks) = self.hooks.as_ref() {
                hooks.unregister(&name);
            }
        }
        let answer = answer.unwrap_or_default();
        let payload = EventPayload::Completion {
            success,
            turns,
            answer_bytes: answer.len(),
            answer_digest: crate::crypto::sha256_hex(answer.as_bytes()),
            error: crate::audit_policy::optional_text_digest(error),
        };
        let started = tokio::time::Instant::now();
        let drain_deadline = started + FINISH_DRAIN_TIMEOUT;
        let deadline = started + FINISH_TIMEOUT;
        let mut pending = Vec::new();
        for control in &self.controls {
            if control.terminal.swap(true, Ordering::AcqRel) {
                continue;
            }
            let (done_tx, done_rx) = oneshot::channel();
            if control
                .sender
                .try_send(ExtensionWork::Finish {
                    completion: control.completion_subscribed.then(|| payload.clone()),
                    reason: ShutdownReason::TaskComplete,
                    done: done_tx,
                })
                .is_ok()
            {
                pending.push(done_rx);
            }
        }
        for done in pending {
            let _ = tokio::time::timeout_at(drain_deadline, done).await;
        }
        let mut controls = Vec::new();
        for mut control in self.controls.drain(..) {
            if let Some(mut worker) = control.worker.take() {
                if tokio::time::timeout_at(drain_deadline, &mut worker)
                    .await
                    .is_err()
                {
                    worker.abort();
                    control.client.emit_agent_extension(
                        LifecycleAction::Shutdown,
                        &control.id,
                        &control.manifest_digest,
                        audit_metadata(
                            &control.package_digest,
                            &control.capability_generation,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                        ),
                        false,
                        Duration::ZERO,
                        Some("extension worker exceeded the finish drain deadline"),
                    );
                }
            }
            controls.push(control);
        }
        let retry_deadline = deadline
            .checked_sub(CONTAINMENT_ESCALATION_RESERVE)
            .unwrap_or(drain_deadline);
        let retries = futures_util::future::join_all(controls.iter().map(|control| async move {
            if control.detach_acknowledged.load(Ordering::Acquire) {
                Ok(())
            } else {
                retry_detach(control, ShutdownReason::Disabled, retry_deadline).await
            }
        }))
        .await;
        let detach_failures = controls
            .iter()
            .zip(retries)
            .filter_map(|(control, result)| result.is_err().then_some(control.id.clone()))
            .collect::<Vec<_>>();
        let lifecycle_failures = controls
            .iter()
            .filter_map(|control| {
                control
                    .terminal_failure
                    .lock()
                    .ok()
                    .and_then(|failure| failure.clone())
            })
            .collect::<Vec<_>>();
        if detach_failures.is_empty() && lifecycle_failures.is_empty() {
            return Ok(());
        }
        let ids = detach_failures
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let escalation = if detach_failures.is_empty() {
            Ok(())
        } else if remaining.is_zero() {
            Err("extension lifecycle budget expired before containment escalation".to_string())
        } else {
            tokio::time::timeout(
                remaining,
                controls[0].client.request_supervisor_containment_teardown(),
            )
            .await
            .map_err(|_| "extension containment escalation timed out".to_string())
            .and_then(|result| result)
        };
        let detail = match escalation {
            Ok(()) => "extension host shutdown requested; supervisor cleanup is mandatory",
            Err(ref error) => error.as_str(),
        };
        for control in &controls {
            if !detach_failures.contains(&control.id) {
                continue;
            }
            control.client.emit_agent_extension(
                LifecycleAction::Shutdown,
                &control.id,
                &control.manifest_digest,
                audit_metadata(
                    &control.package_digest,
                    &control.capability_generation,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                ),
                false,
                Duration::ZERO,
                Some(detail),
            );
        }
        let prior = lifecycle_failures.join("; ");
        Err(match (ids.is_empty(), prior.is_empty()) {
            (false, false) => format!(
                "Agent extension detach was not acknowledged for [{ids}]; {detail}; {prior}"
            ),
            (false, true) => {
                format!("Agent extension detach was not acknowledged for [{ids}]; {detail}")
            }
            (true, false) => prior,
            (true, true) => "Agent extension containment teardown failed".to_string(),
        })
    }
}

async fn activate_one(
    extension: RegisteredExtension,
    exposure: &ToolExposureContext,
    tools: Arc<ToolRegistry>,
    client: Arc<ExtensionHostClient>,
) -> Result<(ExtensionSink, ExtensionControl), String> {
    let RegisteredExtension {
        manifest,
        manifest_digest,
        package,
    } = extension;
    assert_package_current(&package)?;
    for policy in &manifest.action_policies {
        tools.validate_extension_action_policy(&policy.tool, &policy.policy_id)?;
    }
    let registration = AgentExtensionRegistration {
        extension_id: manifest.identity.id.clone(),
        extension_version: manifest.identity.version.clone(),
        package_digest: package.content_digest().to_string(),
        manifest_digest: manifest_digest.clone(),
        content_digest: manifest.identity.content_digest.clone(),
    };
    let started = Instant::now();
    let binding = client.attach_agent_extension(registration).await;
    let audit = audit_metadata(
        package.content_digest(),
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
        &manifest_digest,
        audit.clone(),
        binding.is_ok(),
        started.elapsed(),
        binding.as_ref().err().map(String::as_str),
    );
    let binding = binding?;
    client.emit_agent_extension(
        LifecycleAction::Ready,
        &manifest.identity.id,
        &manifest_digest,
        audit,
        true,
        started.elapsed(),
        None,
    );
    let (sender, receiver) = mpsc::channel(manifest.limits.queue_capacity + 1);
    let event_slots = Arc::new(Semaphore::new(manifest.limits.queue_capacity));
    let refs = Arc::new(CapabilityReferenceStore::new(
        manifest.action_policies.len(),
    ));
    let accepting = Arc::new(AtomicBool::new(true));
    let security_disabled = Arc::new(AtomicBool::new(false));
    let terminal = Arc::new(AtomicBool::new(false));
    let detach_acknowledged = Arc::new(AtomicBool::new(false));
    let terminal_failure = Arc::new(Mutex::new(None));
    let sink = ExtensionSink {
        id: manifest.identity.id.clone(),
        manifest_digest: manifest_digest.clone(),
        package_digest: package.content_digest().to_string(),
        capability_generation: exposure.capability_generation().to_string(),
        subscriptions: manifest.subscriptions.iter().copied().collect(),
        sender: sender.clone(),
        event_slots,
        client: client.clone(),
        accepting: accepting.clone(),
        security_disabled: security_disabled.clone(),
        terminal: terminal.clone(),
        consecutive_drops: Arc::new(AtomicUsize::new(0)),
    };
    let completion_subscribed = manifest.subscriptions.contains(&EventKind::Completion);
    let id = manifest.identity.id.clone();
    let control_manifest_digest = manifest_digest.clone();
    let control_package_digest = package.content_digest().to_string();
    let capability_generation = exposure.capability_generation().to_string();
    let worker = tokio::spawn(run_extension(
        manifest,
        manifest_digest,
        package.content_digest().to_string(),
        package,
        binding.clone(),
        exposure.clone(),
        tools,
        client.clone(),
        refs,
        accepting,
        security_disabled,
        detach_acknowledged.clone(),
        terminal_failure.clone(),
        receiver,
    ));
    Ok((
        sink,
        ExtensionControl {
            sender,
            worker: Some(worker),
            client,
            binding,
            id,
            manifest_digest: control_manifest_digest,
            package_digest: control_package_digest,
            capability_generation,
            completion_subscribed,
            terminal,
            detach_acknowledged,
            terminal_failure,
        },
    ))
}

#[allow(clippy::too_many_arguments)]
async fn run_extension(
    manifest: ExtensionManifest,
    manifest_digest: String,
    package_digest: String,
    package: Arc<crate::provenance::VerifiedPackage>,
    binding: crate::extension_host::abi::AbiBinding,
    exposure: ToolExposureContext,
    tools: Arc<ToolRegistry>,
    client: Arc<ExtensionHostClient>,
    refs: Arc<CapabilityReferenceStore>,
    accepting: Arc<AtomicBool>,
    security_disabled: Arc<AtomicBool>,
    detach_acknowledged: Arc<AtomicBool>,
    terminal_failure: Arc<Mutex<Option<String>>>,
    mut work: mpsc::Receiver<ExtensionWork>,
) {
    let mut trust_check = tokio::time::interval(TRUST_RECHECK_INTERVAL);
    loop {
        tokio::select! {
            item = work.recv() => {
                match item {
                    Some(ExtensionWork::Event { payload, .. }) => {
                        if !should_process_event(&security_disabled) {
                            continue;
                        }
                        if let Err(error) = process_event(
                            &manifest, &manifest_digest, &package_digest, &package, &binding,
                            &exposure, &tools, &client, &refs, &security_disabled, payload,
                        ).await {
                            disable_extension(
                                &manifest, &manifest_digest, &package_digest, &binding,
                                &exposure, &client, &accepting, &security_disabled,
                                &detach_acknowledged, &terminal_failure, &error,
                            ).await;
                            break;
                        }
                    }
                    Some(ExtensionWork::Finish { completion, reason, done }) => {
                        if let Some(payload) = completion {
                            if !security_disabled.load(Ordering::Acquire) {
                                if let Err(error) = process_event(
                                    &manifest, &manifest_digest, &package_digest, &package, &binding,
                                    &exposure, &tools, &client, &refs, &security_disabled, payload,
                                ).await {
                                    accepting.store(false, Ordering::Release);
                                    security_disabled.store(true, Ordering::Release);
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
                                }
                            }
                        }
                        detach_or_escalate(
                            &manifest.identity.id,
                            &manifest_digest,
                            &package_digest,
                            exposure.capability_generation(),
                            &binding,
                            &client,
                            &detach_acknowledged,
                            &terminal_failure,
                            reason,
                        )
                        .await;
                        let _ = done.send(());
                        break;
                    }
                    None => break,
                }
            },
            _ = trust_check.tick() => {
                if !security_disabled.load(Ordering::Acquire) {
                    if let Err(error) = assert_package_current(&package) {
                        disable_extension(
                            &manifest, &manifest_digest, &package_digest, &binding,
                            &exposure, &client, &accepting, &security_disabled,
                            &detach_acknowledged, &terminal_failure, &error,
                        ).await;
                        break;
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn disable_extension(
    manifest: &ExtensionManifest,
    manifest_digest: &str,
    package_digest: &str,
    binding: &crate::extension_host::abi::AbiBinding,
    exposure: &ToolExposureContext,
    client: &ExtensionHostClient,
    accepting: &AtomicBool,
    security_disabled: &AtomicBool,
    detach_acknowledged: &AtomicBool,
    terminal_failure: &Mutex<Option<String>>,
    error: &str,
) {
    accepting.store(false, Ordering::Release);
    security_disabled.store(true, Ordering::Release);
    client.emit_agent_extension(
        LifecycleAction::Disable,
        &manifest.identity.id,
        manifest_digest,
        audit_metadata(
            package_digest,
            exposure.capability_generation(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ),
        false,
        Duration::ZERO,
        Some(error),
    );
    detach_or_escalate(
        &manifest.identity.id,
        manifest_digest,
        package_digest,
        exposure.capability_generation(),
        binding,
        client,
        detach_acknowledged,
        terminal_failure,
        ShutdownReason::Disabled,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn detach_or_escalate(
    id: &str,
    manifest_digest: &str,
    package_digest: &str,
    capability_generation: &str,
    binding: &crate::extension_host::abi::AbiBinding,
    client: &ExtensionHostClient,
    detach_acknowledged: &AtomicBool,
    terminal_failure: &Mutex<Option<String>>,
    reason: ShutdownReason,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    let retry_deadline = deadline
        .checked_sub(CONTAINMENT_ESCALATION_RESERVE)
        .unwrap_or(deadline);
    let result = retry_with_deadline(retry_deadline, || {
        detach_once(
            id,
            manifest_digest,
            package_digest,
            capability_generation,
            binding,
            client,
            detach_acknowledged,
            reason,
            retry_deadline,
        )
    })
    .await;
    let Err(detach_error) = result else {
        return;
    };
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    let escalation =
        tokio::time::timeout(remaining, client.request_supervisor_containment_teardown())
            .await
            .map_err(|_| "extension containment escalation timed out".to_string())
            .and_then(|result| result);
    let detail = match escalation {
        Ok(()) => "extension host shutdown requested; supervisor cleanup is mandatory".to_string(),
        Err(error) => error,
    };
    client.emit_agent_extension(
        LifecycleAction::Shutdown,
        id,
        manifest_digest,
        audit_metadata(
            package_digest,
            capability_generation,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ),
        false,
        Duration::ZERO,
        Some(&detail),
    );
    if let Ok(mut failure) = terminal_failure.lock() {
        *failure = Some(format!(
            "Agent extension `{id}` detach failed: {detach_error}; {detail}"
        ));
    }
}

#[allow(clippy::too_many_arguments)]
async fn detach_once(
    id: &str,
    manifest_digest: &str,
    package_digest: &str,
    capability_generation: &str,
    binding: &crate::extension_host::abi::AbiBinding,
    client: &ExtensionHostClient,
    detach_acknowledged: &AtomicBool,
    reason: ShutdownReason,
    deadline: tokio::time::Instant,
) -> Result<(), String> {
    if detach_acknowledged.load(Ordering::Acquire) {
        return Ok(());
    }
    let attempt_deadline = deadline.min(tokio::time::Instant::now() + DETACH_ATTEMPT_TIMEOUT);
    let detached = tokio::time::timeout_at(
        attempt_deadline,
        client.detach_agent_extension(id.to_string(), binding.clone(), reason),
    )
    .await;
    let result = match detached {
        Ok(result) => classify_detach_response(result, detach_acknowledged),
        Err(_) => Err("extension detach exceeded its priority lifecycle deadline".to_string()),
    };
    client.emit_agent_extension(
        LifecycleAction::Shutdown,
        id,
        manifest_digest,
        audit_metadata(
            package_digest,
            capability_generation,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ),
        result.is_ok(),
        Duration::ZERO,
        result.as_ref().err().map(String::as_str),
    );
    result
}

fn classify_detach_response(
    response: Result<bool, String>,
    detach_acknowledged: &AtomicBool,
) -> Result<(), String> {
    match response {
        Ok(true) => {
            detach_acknowledged.store(true, Ordering::Release);
            Ok(())
        }
        Ok(false) => {
            Err("extension host could not acknowledge exact child termination".to_string())
        }
        Err(error) => Err(error),
    }
}

async fn retry_detach(
    control: &ExtensionControl,
    reason: ShutdownReason,
    deadline: tokio::time::Instant,
) -> Result<(), String> {
    retry_with_deadline(deadline, || {
        detach_once(
            &control.id,
            &control.manifest_digest,
            &control.package_digest,
            &control.capability_generation,
            &control.binding,
            &control.client,
            &control.detach_acknowledged,
            reason,
            deadline,
        )
    })
    .await
}

async fn retry_with_deadline<F, Fut>(
    deadline: tokio::time::Instant,
    mut attempt: F,
) -> Result<(), String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let mut last_error = "extension detach was not acknowledged".to_string();
    for attempt_index in 0..MAX_DETACH_ATTEMPTS {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        match attempt().await {
            Ok(()) => return Ok(()),
            Err(error) => last_error = error,
        }
        if attempt_index + 1 == MAX_DETACH_ATTEMPTS || tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(DETACH_RETRY_DELAY).await;
    }
    Err(last_error)
}

fn should_process_event(security_disabled: &AtomicBool) -> bool {
    !security_disabled.load(Ordering::Acquire)
}

fn assert_package_current(package: &crate::provenance::VerifiedPackage) -> Result<(), String> {
    package
        .assert_current(&crate::provenance::trust_store())
        .map_err(|error| format!("Agent extension package is no longer trusted: {error}"))
}

#[allow(clippy::too_many_arguments)]
async fn process_event(
    manifest: &ExtensionManifest,
    manifest_digest: &str,
    package_digest: &str,
    package: &crate::provenance::VerifiedPackage,
    binding: &crate::extension_host::abi::AbiBinding,
    exposure: &ToolExposureContext,
    tools: &ToolRegistry,
    client: &ExtensionHostClient,
    refs: &Arc<CapabilityReferenceStore>,
    security_disabled: &AtomicBool,
    payload: EventPayload,
) -> Result<(), String> {
    assert_package_current(package)?;
    let event_id = uuid::Uuid::new_v4().simple().to_string();
    let timeout = Duration::from_millis(manifest.limits.event_timeout_ms);
    let deadline = MonotonicDeadlineNs::after(timeout)?;
    let reference_context = ReferenceContext {
        owner_uid: exposure.owner_uid(),
        session_id: exposure.authority_session_id(),
        task_id: exposure.task_id().unwrap_or_default(),
        extension_id: &manifest.identity.id,
        manifest_digest,
        capability_generation: exposure.capability_generation(),
        event_id: &event_id,
        deadline,
    };
    let reference_lease = refs.issue_event(
        &reference_context,
        &manifest.requested_capabilities,
        &manifest.action_policies,
    )?;
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
    let result = match client
        .send_agent_extension_event_classified(
            manifest.identity.id.clone(),
            binding.clone(),
            event_id.clone(),
            deadline,
            payload,
            reference_lease.references().to_vec(),
        )
        .await
    {
        Ok(result) => result,
        Err(error) if error.is_busy() => {
            client.emit_agent_extension(
                LifecycleAction::BackpressureDrop,
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
                false,
                started.elapsed(),
                Some(error.message()),
            );
            return Ok(());
        }
        Err(error) => return Err(error.message().to_string()),
    };
    deadline.remaining()?;
    let mut prepared_actions = Vec::with_capacity(result.proposed_actions.len());
    let mut reference_bindings = Vec::with_capacity(result.proposed_actions.len());
    for action in &result.proposed_actions {
        let policy = manifest
            .action_policies
            .iter()
            .find(|policy| {
                policy.requested_index == action.capability_ref.requested_index
                    && policy.tool == action.tool
            })
            .ok_or_else(|| "extension action was not declared in the manifest".to_string())?;
        let expected_capability = manifest
            .requested_capabilities
            .get(policy.requested_index)
            .ok_or_else(|| "extension action named an unknown capability index".to_string())?;
        let ceiling = crate::caps::CapSet::from_caps([expected_capability.clone()]);
        let action_exposure = exposure.attenuated_for_extension(&manifest.identity.id, &ceiling);
        let call = crate::agent::llm::ToolCall {
            id: action.action_id.clone(),
            name: action.tool.clone(),
            input: action.input.clone(),
        };
        let mut normalized = action.clone();
        normalized.input = crate::agent::runtime::turn::effective_tool_input(
            &call,
            exposure.conversation_session_id(),
            &action_exposure,
        );
        let prepared = tools.prepare_extension_proposal(
            &action_exposure,
            &manifest.identity.id,
            package_digest,
            manifest_digest,
            &event_id,
            &normalized,
            &policy.policy_id,
            expected_capability,
        )?;
        let binding = prepared.binding();
        reference_bindings.push(ActionReferenceBinding {
            reference: action.capability_ref.clone(),
            action_id: binding.action_id.clone(),
            tool: binding.tool.clone(),
            policy_id: binding.policy_id.clone(),
            input_digest: binding.input_digest.clone(),
            capability: binding.capability.clone(),
            operation_digest: binding.operation_digest.clone(),
        });
        prepared_actions.push((prepared, binding, action.capability_ref.handle.clone()));
    }
    reference_lease.consume_all(&reference_bindings)?;
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
    if security_disabled.load(Ordering::Acquire) {
        return Ok(());
    }
    for (prepared, action, capability_ref) in prepared_actions {
        assert_package_current(package)?;
        let ceiling = crate::caps::CapSet::from_caps([action.capability.clone()]);
        let action_exposure = exposure.attenuated_for_extension(&manifest.identity.id, &ceiling);
        let started = Instant::now();
        let approval = tools.approval().clone();
        let mut task = tokio::spawn(prepared.execute(
            action_exposure,
            approval,
            "policy: Agent extension proposed action",
        ));
        let mut abort_on_drop = AbortOnDrop::new(task.abort_handle());
        let action_deadline = tokio::time::Instant::now() + ACTION_TIMEOUT;
        let result = loop {
            tokio::select! {
                result = &mut task => {
                    abort_on_drop.disarm();
                    break match result {
                        Ok(result) => result,
                        Err(error) => crate::agent::tools::ToolResult::err(format!(
                            "Agent extension proposed action task failed: {error}"
                        )),
                    };
                }
                _ = tokio::time::sleep_until(action_deadline) => {
                    task.abort();
                    break crate::agent::tools::ToolResult::err(format!(
                        "Agent extension proposed action timed out after {}s",
                        ACTION_TIMEOUT.as_secs()
                    ));
                }
                _ = tokio::time::sleep(TRUST_RECHECK_INTERVAL) => {
                    if let Err(error) = assert_package_current(package) {
                        task.abort();
                        break crate::agent::tools::ToolResult::err(error);
                    }
                }
            }
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
                Some(&action.action_id),
                Some(&action.tool),
                Some(&capability_ref),
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
        if !self.accepting.load(Ordering::Acquire)
            || self.security_disabled.load(Ordering::Acquire)
            || self.terminal.load(Ordering::Acquire)
            || !self.subscriptions.contains(&payload.kind())
        {
            return;
        }
        let permit = match self.event_slots.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                self.record_backpressure_drop();
                return;
            }
        };
        match self.sender.try_send(ExtensionWork::Event {
            payload,
            _permit: permit,
        }) {
            Ok(()) => {
                self.consecutive_drops.store(0, Ordering::Release);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.accepting.store(false, Ordering::Release);
                self.security_disabled.store(true, Ordering::Release);
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.record_backpressure_drop();
            }
        }
    }

    fn record_backpressure_drop(&self) {
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
        if register_backpressure_drop(&self.consecutive_drops, &self.accepting)
            && !self.terminal.swap(true, Ordering::AcqRel)
        {
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
            let (done, _ignored) = oneshot::channel();
            let _ = self.sender.try_send(ExtensionWork::Finish {
                completion: None,
                reason: ShutdownReason::Disabled,
                done,
            });
        }
    }
}

impl Hook for ExtensionObserver {
    fn name(&self) -> &str {
        &self.name
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
        self.publish(EventPayload::PostTool {
            turn_index: ctx.turn_index,
            tool: tool_call.name.clone(),
            tool_use_id_digest: crate::crypto::sha256_hex(tool_call.id.as_bytes()),
            success: result.success,
            latency_ms: result.latency_ms,
            result_bytes: result.bytes_returned,
            result_digest: result.result_digest.clone(),
            error: crate::audit_policy::optional_text_digest(result.error.as_deref()),
        });
        HookOutcome::Continue
    }
}

impl crate::agent::llm::attempt_observer::ProviderAttemptObserver for ExtensionObserver {
    fn observe_switch(&self, _record: &crate::agent::llm::provider_chain::ProviderSwitch) {}

    fn observe_start(&self, record: &crate::agent::llm::attempt_observer::ProviderAttemptStart) {
        self.publish(EventPayload::PreModelCall {
            turn_index: record.turn_index,
            attempt_id: record.attempt_id.clone(),
            provider: record.provider.clone(),
            model: record.model.clone(),
        });
    }

    fn observe_finish(&self, record: &crate::agent::llm::attempt_observer::ProviderAttemptFinish) {
        let (success, error_class) = match record.outcome {
            crate::agent::llm::attempt_observer::ProviderAttemptOutcome::Success => (true, None),
            crate::agent::llm::attempt_observer::ProviderAttemptOutcome::Error(class) => {
                (false, Some(class.to_string()))
            }
            crate::agent::llm::attempt_observer::ProviderAttemptOutcome::Cancelled => {
                (false, Some("cancelled".to_string()))
            }
        };
        self.publish(EventPayload::PostModelCall {
            turn_index: record.start.turn_index,
            attempt_id: record.start.attempt_id.clone(),
            provider: record.start.provider.clone(),
            model: record.start.model.clone(),
            success,
            latency_ms: record.latency_ms,
            input_tokens: record.usage.input_tokens,
            output_tokens: record.usage.output_tokens,
            error_class,
        });
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

fn register_backpressure_drop(drops: &AtomicUsize, accepting: &AtomicBool) -> bool {
    let drops = drops.fetch_add(1, Ordering::AcqRel) + 1;
    drops >= DISABLE_AFTER_DROPS && accepting.swap(false, Ordering::AcqRel)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent_extensions/runtime.rs"
    ));
}
