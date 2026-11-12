//! Package-only intent mediation for a Root-held GUI. No GUI grant is widened.

use std::collections::BTreeMap;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use claw_display_control::{ProcessIdentity, workload::InstanceProcess};
use serde_json::{Value, json};

use crate::caps::{Cap, CapSet, Scope, Verb};
use crate::clawd::app_sessions::gui::Registration;
use crate::clawd::authority::{self, GrantView};
use crate::clawd::client_identity::ClientIdentity;
use crate::clawd::package_transactions::Operation;
use crate::clawd::protocol::BrokerError;
use crate::clawd::state::DaemonState;

use super::intent::{Mode, Request};

static RECOVERY_JOBS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
const MAX_INTENTS: usize = 32;
const MAX_REVIEW_TARGETS: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ReviewIdentity {
    Unattempted,
    Unconfirmed,
    Tracked(String),
}

#[derive(Clone)]
pub(super) struct ReviewTarget {
    pub capability: Cap,
    pub identity: ReviewIdentity,
    pub retired: bool,
}

#[derive(Clone)]
pub(super) struct IntentEntry {
    pub operation: Operation,
    pub digest: String,
    pub session: String,
    pub targets: Vec<ReviewTarget>,
    pub generation_retired: bool,
}

#[derive(Default)]
pub(super) struct IntentLedger {
    pub binding: Option<String>,
    pub entries: BTreeMap<String, IntentEntry>,
    pub target_count: usize,
    pub closing: bool,
    pub retired: bool,
}

impl IntentLedger {
    pub(super) fn bind(&mut self, binding: &str) -> Result<(), String> {
        if self.closing || self.retired {
            return Err("this package-intent window is closing".into());
        }
        match &self.binding {
            Some(current) if current != binding =>
                Err("package intent belongs to another GUI/process/package generation".into()),
            Some(_) => Ok(()),
            None => { self.binding = Some(binding.to_string()); Ok(()) }
        }
    }

    pub(super) fn reserve(
        &mut self, binding: &str, invocation: &str, operation: Operation, required: Vec<Cap>,
    ) -> Result<(), String> {
        self.bind(binding)?;
        if let Some(entry) = self.entries.get(invocation) {
            if entry.operation != operation
                || entry.targets.iter().map(|target| &target.capability).ne(required.iter())
            {
                return Err("package invocation cannot change its exact action/targets".into());
            }
            return Ok(());
        }
        if required.is_empty() || self.entries.len() >= MAX_INTENTS
            || required.len() > MAX_REVIEW_TARGETS.saturating_sub(self.target_count)
        {
            return Err("this window permits at most 32 distinct package intents and 128 total review targets; no additional review was created".into());
        }
        let digest = crate::crypto::sha256_hex(&serde_json::to_vec(&(
            "gui-package-intent-v2", binding, invocation, &operation,
        )).map_err(|error| error.to_string())?);
        self.target_count += required.len();
        self.entries.insert(invocation.to_string(), IntentEntry {
            operation,
            session: format!("package-intent:{digest}"),
            digest,
            targets: required.into_iter().map(|capability| ReviewTarget {
                capability, identity: ReviewIdentity::Unattempted, retired: false,
            }).collect(),
            generation_retired: false,
        });
        Ok(())
    }

    pub(super) fn fence(&mut self) -> Vec<(String, IntentEntry)> {
        self.closing = true;
        self.entries.iter().map(|(id, entry)| (id.clone(), entry.clone())).collect()
    }
}

struct WindowProcess {
    process: ProcessIdentity,
    containment: InstanceProcess,
    group: OwnedFd,
}

/// One Root-held registry per private GUI broker, never supplied by an App.
pub(crate) struct WindowIntents {
    registration: Arc<Registration>,
    process: OnceLock<WindowProcess>,
    stopped: AtomicBool,
    serial: tokio::sync::Mutex<()>,
    ledger: Mutex<IntentLedger>,
    retirement: Mutex<()>,
}

impl WindowIntents {
    pub(crate) fn new(registration: Arc<Registration>) -> Arc<Self> {
        Arc::new(Self {
            registration, process: OnceLock::new(), stopped: AtomicBool::new(false),
            serial: tokio::sync::Mutex::new(()), ledger: Mutex::new(IntentLedger::default()),
            retirement: Mutex::new(()),
        })
    }

    pub(crate) fn bind(&self, process: ProcessIdentity, group: OwnedFd) -> Result<(), String> {
        self.open()?;
        if process.uid() != self.registration.owner() {
            return Err("package-intent process has another owner".into());
        }
        self.registration.snapshot(&process)?;
        let containment = InstanceProcess::receive(
            process.pidfd().map_err(|error| error.to_string())?,
            group.try_clone().map_err(|error| error.to_string())?,
        ).map_err(|error| error.to_string())?;
        self.open()?;
        self.process.set(WindowProcess { process, containment, group })
            .map_err(|_| "package-intent process was already bound".into())
    }

    pub(crate) fn stop(&self) { self.stopped.store(true, Ordering::Release); }

    fn open(&self) -> Result<(), String> {
        if self.stopped.load(Ordering::Acquire) {
            Err("this package-intent window has stopped".into())
        } else { Ok(()) }
    }

    fn lock(&self) -> Result<MutexGuard<'_, IntentLedger>, String> {
        self.ledger.lock().map_err(|_| "package-intent registry is poisoned".into())
    }

    fn snapshot_locked(
        &self, ledger: &mut IntentLedger, process: &ProcessIdentity,
    ) -> Result<(GrantView, Instant), String> {
        self.open()?;
        let bound = self.process.get().ok_or("package-intent process custody is unavailable")?;
        if bound.process != *process {
            return Err("package intent belongs to another bound process".into());
        }
        // Freshness is rechecked after acquiring the proposal lock.
        let observed = Instant::now();
        let gui = self.registration.snapshot(process)?;
        let deadline = observed.checked_add(gui.expires_in).ok_or("GUI expiry overflow")?;
        let binding = crate::crypto::sha256_hex(&serde_json::to_vec(&(
            self.registration.owner(), &self.registration.session_id,
            self.registration.launch.app_id(), &self.registration.gui_entry,
            crate::provenance::runtime::PackageRef::of(self.registration.launch.package()),
            process.pid(), process.start_ticks(), gui.id.audit_ref().as_str(), gui.generation,
        )).map_err(|error| error.to_string())?);
        self.open()?;
        ledger.bind(&binding)?;
        Ok((gui, deadline))
    }

    fn entry(&self, invocation: &str) -> Result<IntentEntry, String> {
        self.open()?;
        let ledger = self.lock()?;
        if ledger.closing { return Err("package-intent window is closing".into()); }
        ledger.entries.get(invocation).cloned().ok_or_else(|| "package intent is not retained by this window".into())
    }

    fn pending(
        &self, process: &ProcessIdentity, request: &Request, create: bool,
    ) -> Result<Value, String> {
        let mut ledger = self.lock()?;
        self.snapshot_locked(&mut ledger, process)?;
        let entry = ledger.entries.get_mut(request.invocation_id.as_str())
            .ok_or("package intent is not retained by this window")?;
        eligible(&self.registration, &entry.targets.iter().map(|target|
            target.capability.clone()).collect::<Vec<_>>())?;
        let mut ids = Vec::new();
        let mut approved = 0;
        for target in &mut entry.targets {
            self.open()?;
            let existing = crate::approvals::find_pending_exact_for_operation(
                &entry.session, &target.capability, Some(self.registration.owner()), None, Some(&entry.digest),
            );
            if let Some(record) = existing {
                match &target.identity {
                    ReviewIdentity::Tracked(id) if id != &record.id =>
                        return Err("package review identity changed; custody retained".into()),
                    _ => target.identity = ReviewIdentity::Tracked(record.id.clone()),
                }
                ids.push(record.id);
            }
            if crate::approvals::has_approved_grant_for_context_operation(
                &entry.session, &target.capability, Some(self.registration.owner()), None, Some(&entry.digest),
            )? {
                if !matches!(target.identity, ReviewIdentity::Tracked(_)) {
                    return Err("approved package decision has no retained review identity; custody retained".into());
                }
                approved += 1;
                continue;
            }
            if matches!(target.identity, ReviewIdentity::Unconfirmed) {
                return Err("package review creation is unconfirmed; its identity must be reconciled, not recreated".into());
            }
            if create && matches!(target.identity, ReviewIdentity::Unattempted) {
                target.identity = ReviewIdentity::Unconfirmed;
                let id = crate::approvals::submit_owned_with_context_for_operation(
                    target.capability.verb, target.capability.scope.clone(), entry.session.clone(),
                    format!("GUI {} requests {:?} for {} in exact package intent {}. \
                        Critical package permission is not App delivery or all-App-stop consent.",
                        self.registration.launch.app_id(), request.action,
                        target.capability.scope, request.invocation_id.as_str()),
                    Some(format!("App {} / GUI {}", self.registration.launch.app_id(), self.registration.session_id)),
                    Some(self.registration.owner()), None, Some(&entry.digest),
                )?;
                target.identity = ReviewIdentity::Tracked(id.clone());
                ids.push(id);
            }
        }
        self.open()?;
        if approved == entry.targets.len() {
            return Ok(reply(request, "permission_ready"));
        }
        if ids.is_empty() {
            let mut result = reply(request, "unconfirmed");
            result["reason"] = json!("No current complete permission set is available. This intent retains its existing review identities; no package effect was replayed.");
            return Ok(result);
        }
        let mut result = reply(request, "permission_required");
        result["approval_requests"] = json!(ids);
        Ok(result)
    }

    /// Called only from the checked after-death GUI path, before cgroup removal.
    pub(crate) fn release_after_death(&self, group: OwnedFd, deadline: Instant) -> Result<(), String> {
        self.stop();
        let _retirement = lock_until(&self.retirement, deadline)?;
        let batch = {
            let mut ledger = lock_until(&self.ledger, deadline)?;
            ledger.fence()
        };
        // No proposal lock is held during custody, approval, generation or audit I/O.
        if !claw_display_control::workload::cgroup_is_empty(group.as_fd()).map_err(|error| error.to_string())? {
            return Err("package decisions require checked GUI cgroup death".into());
        }
        if let Some(bound) = self.process.get() {
            if bound.containment.active().map_err(|error| error.to_string())?
                || !claw_display_control::workload::cgroup_is_empty(bound.group.as_fd()).map_err(|error| error.to_string())?
            { return Err("package-intent process custody is still active".into()); }
        } else if !batch.is_empty() {
            return Err("package intent custody is unavailable; absence is not retirement proof".into());
        }
        for (invocation, entry) in batch {
            for (index, target) in entry.targets.iter().enumerate() {
                if target.retired { continue; }
                if Instant::now() >= deadline { return Err("package review retirement deadline expired".into()); }
                let id = match &target.identity {
                    ReviewIdentity::Unattempted => None,
                    ReviewIdentity::Tracked(id) => Some(id.clone()),
                    ReviewIdentity::Unconfirmed => {
                        let record = crate::approvals::find_pending_exact_for_operation(
                            &entry.session, &target.capability, Some(self.registration.owner()), None, Some(&entry.digest),
                        ).ok_or("package review creation remains unconfirmed; custody retained")?;
                        let mut ledger = lock_until(&self.ledger, deadline)?;
                        let retained = ledger.entries.get_mut(&invocation).ok_or("package retirement record disappeared")?;
                        retained.targets[index].identity = ReviewIdentity::Tracked(record.id.clone());
                        Some(record.id)
                    }
                };
                if let Some(id) = id {
                    crate::approvals::retire_operation_decision(
                        self.registration.owner(), &entry.session, &id, &entry.digest,
                        "package-intent window has retired",
                    )?;
                }
                let mut ledger = lock_until(&self.ledger, deadline)?;
                ledger.entries.get_mut(&invocation).ok_or("package retirement record disappeared")?
                    .targets[index].retired = true;
            }
            if !entry.generation_retired {
                let scope = crate::approvals::RevocationScope::Session {
                    uid: Some(self.registration.owner()), session: entry.session.clone(),
                };
                let generation = crate::approvals::generations::revoke(&scope)?;
                crate::clawd::audit::record_approval_revocation(&scope, &entry.session, generation);
                lock_until(&self.ledger, deadline)?.entries.get_mut(&invocation)
                    .ok_or("package retirement record disappeared")?.generation_retired = true;
            }
        }
        let mut ledger = lock_until(&self.ledger, deadline)?;
        if ledger.entries.values().any(|entry| !entry.generation_retired
            || entry.targets.iter().any(|target| !target.retired))
        { return Err("package review retirement remains incomplete".into()); }
        ledger.retired = true;
        Ok(())
    }
}

fn lock_until<T>(lock: &Mutex<T>, deadline: Instant) -> Result<MutexGuard<'_, T>, String> {
    loop {
        match lock.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::Poisoned(_)) => return Err("package-intent custody lock poisoned".into()),
            Err(std::sync::TryLockError::WouldBlock) => {
                if Instant::now() >= deadline { return Err("package-intent custody has not quiesced".into()); }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

fn eligible(registration: &Registration, required: &[Cap]) -> Result<(), String> {
    use crate::caps::manifest::ScopeBinding;
    let selected = registration.launch.manifest().gui_entry(registration.gui_entry.as_deref())?;
    if selected.legacy_needs {
        return Err("package intents require this signed GUI's explicit permission declaration".into());
    }
    let declared = selected.launch_needs().chain(selected.optional_needs.iter().copied()
        .filter(|need| selected.optional_need_is_bound(need))).collect::<Vec<_>>();
    for cap in required {
        if cap.verb != Verb::SYS_PACKAGE || !matches!(cap.scope, Scope::Name(_))
            || !declared.iter().any(|need| need.verb == Verb::SYS_PACKAGE && match &need.scope {
                ScopeBinding::Wild => true,
                ScopeBinding::Fixed { scope } => scope.covers(&cap.scope),
                _ => false,
            })
        { return Err("package intent is outside this exact GUI surface's declared package feature".into()); }
    }
    let caps = CapSet::from_caps(required.iter().cloned());
    if registration.launch.package().ceiling().clamp(&caps).0 != caps {
        return Err("package intent is outside the installed App's authority ceiling".into());
    }
    crate::approvals::app_policy::require(registration.owner(), registration.launch.app_id(), &caps)
}

/// Created only after the complete exact decision set was consumed by Root.
pub(crate) struct ApprovedIntent {
    window: Arc<WindowIntents>,
    registration: Arc<Registration>,
    process: ProcessIdentity,
    gui: GrantView,
    invocation: String,
    session: String,
    digest: String,
    generation: u32,
    required: Vec<Cap>,
    approvals: Vec<crate::approvals::ConsumedGrant>,
    deadline: Instant,
}

pub(crate) struct IntentBinding<'a> {
    pub registration: &'a Registration,
    pub process: &'a ProcessIdentity,
    pub gui: GrantView,
    pub required: &'a [Cap],
    pub deadline: Instant,
    pub generation: u32,
}

impl ApprovedIntent {
    pub(crate) fn validate(&self) -> Result<IntentBinding<'_>, String> {
        let (current, gui_deadline) = {
            let mut ledger = self.window.lock()?;
            let current = self.window.snapshot_locked(&mut ledger, &self.process)?;
            let entry = ledger.entries.get(&self.invocation)
                .ok_or("package intent custody disappeared before authorization")?;
            if entry.digest != self.digest || entry.session != self.session
                || entry.targets.iter().map(|target| &target.capability).ne(self.required.iter())
                || entry.targets.iter().any(|target| target.retired
                    || !matches!(target.identity, ReviewIdentity::Tracked(_)))
            { return Err("retained package intent changed before authorization".into()); }
            current
        };
        eligible(&self.registration, &self.required)?;
        if current.id != self.gui.id || current.generation != self.gui.generation
            || current.caps != self.gui.caps
            || self.approvals.len() != self.required.len()
            || crate::approvals::generations::current(Some(self.registration.owner()), &self.session)?
                != self.generation
        { return Err("package intent authority or protected decision changed".into()); }
        let observed = Instant::now();
        let mut deadline = self.deadline.min(gui_deadline);
        for (approval, capability) in self.approvals.iter().zip(&self.required) {
            let authorization = &approval.authorization;
            if authorization.owner_uid != Some(self.registration.owner())
                || authorization.session != self.session
                || authorization.capability != *capability
                || authorization.risk != crate::caps::Risk::Critical
                || authorization.context.is_some() || authorization.execution.is_some()
                || authorization.resume_request_id.is_some() || authorization.resumable_until.is_some()
                || authorization.operation_digest.as_deref() != Some(self.digest.as_str())
                || approval.uses_remaining != 0 || approval.generation != self.generation
                || approval.expires_in().is_zero()
            { return Err("consumed package decision does not match this live exact intent".into()); }
            deadline = deadline.min(observed.checked_add(approval.expires_in())
                .ok_or("package approval expiry overflow")?);
        }
        self.window.open()?;
        if Instant::now() >= deadline { return Err("package intent authority expired".into()); }
        Ok(IntentBinding {
            registration: &self.registration, process: &self.process, gui: current,
            required: &self.required, deadline, generation: self.generation,
        })
    }
}

fn reply(request: &Request, status: &str) -> Value {
    json!({"version": 1, "invocation_id": request.invocation_id.as_str(), "status": status})
}

/// Only the private GUI broker supplies these Root-held identities. The public
/// registry entry refuses direct dispatch, including App/Host proxy calls.
pub(crate) async fn request(
    daemon: &DaemonState,
    window: Arc<WindowIntents>,
    process: &ProcessIdentity,
    client: &ClientIdentity,
    params: Value,
) -> Result<Value, BrokerError> {
    let request: Request = serde_json::from_value(params)
        .map_err(|_| BrokerError::authorization("invalid typed package intent"))?;
    let operation = request.operation().map_err(BrokerError::authorization)?;
    let registration = window.registration.clone();
    if request.session.as_str() != registration.session_id || client.require_uid().ok() != Some(registration.owner()) {
        return Err(BrokerError::authorization("package intent is outside this exact GUI owner/session"));
    }
    let _serializer = window.serial.lock().await;
    {
        let mut ledger = window.lock().map_err(BrokerError::authorization)?;
        window.snapshot_locked(&mut ledger, process).map_err(BrokerError::authorization)?;
        if let Some(entry) = ledger.entries.get(request.invocation_id.as_str()) {
            if entry.operation != operation || entry.targets.iter()
                .map(|target| &target.capability).ne(request.capabilities().iter())
            {
                return Err(BrokerError::authorization("package invocation cannot change its exact action/targets"));
            }
        }
    }
    let required = request.capabilities();
    eligible(&registration, &required).map_err(BrokerError::authorization)?;
    let records = super::transaction_status(
        json!({"invocation_id": request.invocation_id.as_str()}), client).await?;
    let records = records["transactions"].as_array()
        .ok_or_else(|| BrokerError::unavailable("invalid retained package projection"))?;
    if !records.is_empty() {
        if records.len() != 1
            || records[0]["app_id"] != registration.launch.app_id()
            || records[0]["operation"] != serde_json::to_value(&operation).map_err(|error| BrokerError::unavailable(error.to_string()))?
        {
            return Err(BrokerError::authorization("package invocation conflicts with its retained owner/App/operation"));
        }
        if request.mode == Mode::Recover {
            let permit = RECOVERY_JOBS.try_acquire().map_err(|_| BrokerError::unavailable(
                "read-only package reconciliation is busy; query Root status before retrying recovery"))?;
            let params = json!({"request_id": records[0]["request_id"]});
            let client = client.clone();
            let context = crate::paths::RoutedPathContext::capture();
            // Recovery may wait for PUBLICATION. Never hold this GUI provider
            // open behind a controller that is waiting for its checked retirement.
            tokio::spawn(context.scope(async move {
                let _permit = permit;
                if let Err(error) = super::transaction_recover(params, &client).await {
                    tracing::warn!(%error, "retained GUI package reconciliation remains unconfirmed");
                }
            }));
        }
        let mut result = reply(&request, if request.mode == Mode::Recover {
            "recovery_requested"
        } else { "transaction" });
        result["transaction"] = records[0].clone();
        return Ok(result);
    }
    let (gui, gui_deadline) = {
        let mut ledger = window.lock().map_err(BrokerError::authorization)?;
        let current = window.snapshot_locked(&mut ledger, process).map_err(BrokerError::authorization)?;
        if matches!(request.mode, Mode::Review | Mode::Submit) {
            let binding = ledger.binding.clone().ok_or_else(|| BrokerError::authorization("package window binding is missing"))?;
            ledger.reserve(&binding, request.invocation_id.as_str(), operation.clone(), required.clone())
                .map_err(BrokerError::authorization)?;
        } else if !ledger.entries.contains_key(request.invocation_id.as_str()) {
            let mut result = reply(&request, "unconfirmed");
            result["reason"] = json!("No retained package transaction or window intent exists; no effect was replayed.");
            return Ok(result);
        }
        current
    };
    if request.mode != Mode::Submit {
        return window.pending(process, &request, request.mode == Mode::Review)
            .map_err(BrokerError::authorization);
    }
    let _admission = crate::clawd::app_mutation::admit_for_owner(
        registration.launch.app_id(), registration.owner()).map_err(BrokerError::authorization)?;
    window.open().map_err(BrokerError::authorization)?;
    let entry = window.entry(request.invocation_id.as_str()).map_err(BrokerError::authorization)?;
    if entry.targets.iter().any(|target| !matches!(target.identity, ReviewIdentity::Tracked(_))) {
        return window.pending(process, &request, true).map_err(BrokerError::authorization);
    }
    let generation = crate::approvals::generations::current(Some(registration.owner()), &entry.session)
        .map_err(BrokerError::authorization)?;
    let Some(approvals) = crate::approvals::consume_grant_set_once_for_owner_operation_evidence(
        &entry.session, &required, Some(registration.owner()), Some(&entry.digest)).map_err(BrokerError::authorization)?
    else {
        return window.pending(process, &request, true)
            .map_err(BrokerError::authorization);
    };
    let observed = Instant::now();
    let mut deadline = gui_deadline;
    for approval in &approvals {
        deadline = deadline.min(observed.checked_add(approval.expires_in())
            .ok_or_else(|| BrokerError::authorization("package approval expiry overflow"))?);
    }
    let approved = ApprovedIntent {
        window: window.clone(), registration, process: process.clone(), gui,
        invocation: request.invocation_id.as_str().to_string(),
        session: entry.session, digest: entry.digest, generation, required, approvals, deadline,
    };
    let authority = authority::package_intent::authorize(&approved, client)
        .map_err(BrokerError::authorization)?;
    let receipt = crate::clawd::package_transactions::accept(
        daemon, client, authority.decision(), operation, Some(request.invocation_id.as_str()),
    ).await.map_err(BrokerError::authorization)?;
    let mut result = reply(&request, "accepted");
    result["receipt"] = receipt;
    Ok(result)
}
