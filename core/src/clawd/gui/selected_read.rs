//! Protected, operation-bound selected file reads. App proposals are not grants.

use std::collections::{BTreeMap, BTreeSet};
use std::os::fd::AsFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use claw_display_control::{workload::InstanceProcess, Epoch, InstanceId, ProcessIdentity};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::caps::{Cap, Scope, Verb};
use crate::clawd::app_sessions::gui::{kernel_peer, Registration};
use crate::clawd::authority::{self, Audience, Decision, GrantId, GrantView, Presentation};
use crate::clawd::client_identity::ClientIdentity;
use crate::clawd::protocol::BrokerError;
use crate::worker::native_fs::{FileCandidate, ReadLeaseCheck, ReadPermit, ReadProvider};

use super::inputs::{FileReadRequest, FileReadStatusRequest};

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Stage {
    Pending,
    Selected,
    Denied,
    Stale,
    Expired,
    Revoked,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct Status {
    version: u8,
    selection_id: String,
    node: u64,
    access: &'static str,
    state: Stage,
    approval_request_id: String,
    target_display: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_in_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

struct Proposal {
    id: String,
    candidate: Arc<FileCandidate>,
    capability: Cap,
    digest: String,
    approval_session: String,
    approval_request: String,
    display: String,
    stage: Stage,
    authority: Option<Arc<ReadAuthority>>,
}

impl Proposal {
    fn status(&self) -> Status {
        Status {
            version: 1,
            selection_id: self.id.clone(),
            node: self.candidate.node(),
            access: "read",
            state: self.stage,
            approval_request_id: self.approval_request.clone(),
            target_display: self.display.clone(),
            expires_in_ms: self.authority.as_ref().map(|authority| {
                authority
                    .deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64
            }),
            reason: match self.stage {
                Stage::Denied => Some("owner_or_authority_denied"),
                Stage::Stale => Some("source_changed"),
                Stage::Expired => Some("read_lease_expired"),
                Stage::Revoked => Some("read_lease_revoked"),
                _ => None,
            },
        }
    }
}

struct ReadAuthority {
    registration: Arc<Registration>,
    process: ProcessIdentity,
    gui: GrantId,
    gui_generation: u64,
    grant: GrantId,
    capability: Cap,
    candidate: Arc<FileCandidate>,
    approval_session: String,
    generation: u32,
    deadline: Instant,
    stopped: Arc<AtomicBool>,
}

impl ReadLeaseCheck for ReadAuthority {
    fn require(&self) -> Result<(), String> {
        if self.stopped.load(Ordering::Acquire) || Instant::now() >= self.deadline {
            return Err("selected read lease ended".into());
        }
        crate::paths::RoutedPathContext::for_owner(
            self.registration.owner(),
            self.registration.home.clone(),
        )
        .scope_sync(|| {
            let gui = self.registration.snapshot(&self.process)?;
            if gui.id != self.gui
                || gui.generation != self.gui_generation
                || self
                    .registration
                    .selected_read_capability(self.candidate.relative())?
                    != self.capability
                || crate::approvals::generations::current(
                    Some(self.registration.owner()),
                    &self.approval_session,
                )? != self.generation
            {
                return Err("selected read authority, declaration or decision changed".into());
            }
            let _authorized = authority::exercise_gui_file_read(
                self.grant,
                &self.capability,
                Presentation {
                    uid: self.registration.owner(),
                    pid: self.process.pid() as u32,
                    start_time_ticks: Some(self.process.start_ticks()),
                    audience: Audience::GuiResource,
                    route: "gui.file.read.status",
                    session_id: Some(self.registration.session_id.clone()),
                },
            )?;
            Ok(())
        })
    }
}

impl Drop for ReadAuthority {
    fn drop(&mut self) {
        authority::authority().revoke(self.grant);
    }
}

pub(super) struct SelectedReads {
    registration: Arc<Registration>,
    instance: InstanceId,
    process: ProcessIdentity,
    containment: InstanceProcess,
    group: std::os::fd::OwnedFd,
    provider: ReadProvider,
    metadata_ceiling: Instant,
    gui: GrantId,
    gui_generation: u64,
    stopped: Arc<AtomicBool>,
    proposals: Mutex<BTreeMap<String, Proposal>>,
    approval_sessions: Mutex<BTreeSet<String>>,
    checker: Mutex<Option<std::thread::JoinHandle<()>>>,
    checker_error: Mutex<Option<String>>,
}

impl SelectedReads {
    pub(super) fn new(
        registration: Arc<Registration>,
        instance: InstanceId,
        process: ProcessIdentity,
        group: std::os::fd::OwnedFd,
        provider: ReadProvider,
        metadata_ceiling: Instant,
        gui: &GrantView,
    ) -> Result<Arc<Self>, String> {
        let containment = InstanceProcess::receive(
            process.pidfd().map_err(|error| error.to_string())?,
            group.try_clone().map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        if process.uid() != registration.owner()
            || gui.subject.app_id.as_deref() != Some(registration.launch.app_id())
        {
            return Err("selected read endpoint has another GUI identity".into());
        }
        let endpoint = Arc::new(Self {
            registration,
            instance,
            process,
            containment,
            group,
            provider,
            metadata_ceiling,
            gui: gui.id,
            gui_generation: gui.generation,
            stopped: Arc::new(AtomicBool::new(false)),
            proposals: Mutex::new(BTreeMap::new()),
            approval_sessions: Mutex::new(BTreeSet::new()),
            checker: Mutex::new(None),
            checker_error: Mutex::new(None),
        });
        let weak = Arc::downgrade(&endpoint);
        let context = crate::paths::RoutedPathContext::for_owner(
            endpoint.registration.owner(),
            endpoint.registration.home.clone(),
        );
        let checked = weak.clone();
        // Full selected-file checks must not run on the two-second metadata heartbeat.
        let thread = std::thread::Builder::new()
            .name("gui-file-checks".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    loop {
                        let Some(endpoint) = weak.upgrade() else {
                            return;
                        };
                        if endpoint.stopped.load(Ordering::Acquire) {
                            return;
                        }
                        if let Err(error) = context.clone().scope_sync(|| endpoint.check_selected()) {
                            endpoint.checker_failed(error);
                            return;
                        }
                        drop(endpoint);
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }));
                if let (Err(_), Some(endpoint)) = (result, checked.upgrade()) {
                    endpoint.checker_failed("Root selected-file checker panicked".into());
                }
            })
            .map_err(|error| format!("start Root selected-file checks: {error}"))?;
        *endpoint
            .checker
            .lock()
            .map_err(|_| "selected-file checker custody poisoned")? = Some(thread);
        Ok(endpoint)
    }

    fn fresh(&self) -> Result<GrantView, String> {
        if self.stopped.load(Ordering::Acquire) || Instant::now() >= self.metadata_ceiling {
            return Err("selected read window has retired".into());
        }
        let view = self.registration.snapshot(&self.process)?;
        if view.id != self.gui || view.generation != self.gui_generation {
            return Err("selected read window authority changed".into());
        }
        Ok(view)
    }

    pub(super) fn authorize_request(
        &self,
        authority: &Decision,
        client: &ClientIdentity,
    ) -> Result<(), String> {
        let peer = kernel_peer(client)?;
        if authority.owner_uid() != self.registration.owner()
            || authority.session_id() != Some(self.registration.session_id.as_str())
            || authority.app_id() != Some(self.registration.launch.app_id())
            || !self
                .containment
                .is_peer(&peer)
                .map_err(|error| error.to_string())?
        {
            return Err("selected read request is outside this exact GUI instance".into());
        }
        let meta = Cap::new(
            Verb::FS_META,
            Scope::path(format!("{}/**", self.registration.home.display())),
        );
        let _authorized = authority.require(meta)?;
        self.fresh()?;
        Ok(())
    }

    fn request(&self, node: u64) -> Result<Value, BrokerError> {
        let gui = self.fresh().map_err(BrokerError::authorization)?;
        let candidate = self
            .provider
            .candidate(node)
            .map_err(BrokerError::authorization)?;
        candidate.revalidate().map_err(BrokerError::authorization)?;
        let capability = self
            .registration
            .selected_read_capability(candidate.relative())
            .map_err(BrokerError::authorization)?;
        let mut proposals = self
            .proposals
            .lock()
            .map_err(|_| BrokerError::unavailable("selected reads poisoned"))?;
        self.fresh().map_err(BrokerError::authorization)?;
        if let Some(proposal) = proposals.values().find(|proposal| {
            proposal.candidate.node() == node
                && proposal.candidate.identity() == candidate.identity()
                && proposal.candidate.stamp() == candidate.stamp()
                && matches!(proposal.stage, Stage::Pending | Stage::Selected)
        }) {
            return serde_json::to_value(proposal.status())
                .map_err(|error| BrokerError::unavailable(error.to_string()));
        }
        if proposals.len() >= 32 {
            return Err(BrokerError::unavailable(
                "window selected-read proposal limit reached",
            ));
        }
        use std::os::unix::ffi::OsStrExt;
        let bytes = base64::engine::general_purpose::STANDARD
            .encode(candidate.relative().as_os_str().as_bytes());
        let digest = hex::encode(Sha256::digest(
            serde_json::to_vec(&serde_json::json!([
                "gui.selected-file-read.v1",
                self.registration.owner(),
                self.registration.session_id,
                Epoch(self.instance.0).label(),
                self.registration.authority_alias.label(),
                self.registration.control.epoch.label(),
                self.registration.gui_entry,
                self.registration.selector,
                candidate.node(),
                candidate.identity(),
                candidate.stamp(),
                candidate.generation(),
                bytes,
                capability,
                gui.id.audit_ref().as_str(),
                gui.generation,
            ]))
            .map_err(|error| BrokerError::unavailable(error.to_string()))?,
        ));
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let id = format!("sr-{nonce}");
        let approval_session = format!("gui-read-{}-{nonce}", Epoch(self.instance.0).label());
        let display = candidate.display().map_err(BrokerError::authorization)?;
        {
            let mut sessions = self.approval_sessions.lock().map_err(|_| {
                BrokerError::unavailable("selected-file session catalogue poisoned")
            })?;
            if sessions.len() >= 32 {
                return Err(BrokerError::unavailable(
                    "window selected-file session limit reached",
                ));
            }
            sessions.insert(approval_session.clone());
        }
        let approval_request = crate::approvals::submit_owned_with_context_for_operation(
            Verb::FS_READ, capability.scope.clone(), approval_session.clone(),
            format!("Read ONLY selected file {display}; window {}, native node {node}. No directory, write, delete or execute access",
                Epoch(self.instance.0).label()),
            Some(format!("GUI {} {} selected file read", self.registration.launch.app_id(), self.registration.selector)),
            Some(self.registration.owner()), None, Some(&digest),
        ).map_err(BrokerError::unavailable)?;
        let proposal = Proposal {
            id: id.clone(),
            candidate,
            capability,
            digest,
            approval_session,
            approval_request,
            display,
            stage: Stage::Pending,
            authority: None,
        };
        let result = serde_json::to_value(proposal.status())
            .map_err(|error| BrokerError::unavailable(error.to_string()))?;
        proposals.insert(id, proposal);
        Ok(result)
    }

    fn status(&self, id: &str, node: u64) -> Result<Value, BrokerError> {
        self.fresh().map_err(BrokerError::authorization)?;
        let mut proposals = self
            .proposals
            .lock()
            .map_err(|_| BrokerError::unavailable("selected reads poisoned"))?;
        let proposal = proposals
            .get_mut(id)
            .filter(|proposal| proposal.candidate.node() == node)
            .ok_or_else(|| {
                BrokerError::authorization(
                    "selected read status belongs to another window or target",
                )
            })?;
        if proposal.stage == Stage::Pending {
            if proposal.candidate.revalidate().is_err() {
                proposal.stage = Stage::Stale;
            } else {
                use crate::approvals::RequestStatus;
                match crate::approvals::status_for_owner(
                    &proposal.approval_request,
                    Some(self.registration.owner()),
                ) {
                    RequestStatus::Pending | RequestStatus::Resolving => {}
                    RequestStatus::Denied => proposal.stage = Stage::Denied,
                    RequestStatus::Unknown | RequestStatus::Consumed => {
                        proposal.stage = Stage::Revoked
                    }
                    RequestStatus::Approved => {
                        // A consumed or unverifiable decision must never leave a
                        // request apparently pending, let alone imply selection.
                        proposal.stage = Stage::Revoked;
                        let approval = crate::approvals::redeem_matching_grant_for_owner_operation(
                            &proposal.approval_session,
                            Verb::FS_READ,
                            &proposal.capability.scope,
                            Some(self.registration.owner()),
                            None,
                            Some(&proposal.digest),
                            true,
                        )
                        .map_err(BrokerError::unavailable)?;
                        match approval {
                            Some(approval) => {
                                if let Err(error) = self.select(proposal, &approval) {
                                    tracing::warn!(%error, node, "protected selected-file read was not installed");
                                    proposal.stage = if proposal.candidate.revalidate().is_err() {
                                        Stage::Stale
                                    } else {
                                        Stage::Denied
                                    };
                                }
                            }
                            None => proposal.stage = Stage::Expired,
                        }
                    }
                }
            }
        } else if proposal.stage == Stage::Selected {
            let valid = proposal.authority.as_ref().is_some_and(|read| {
                read.require().is_ok() && proposal.candidate.revalidate().is_ok()
            });
            if !valid {
                proposal.stage = if proposal
                    .authority
                    .as_ref()
                    .is_some_and(|read| Instant::now() >= read.deadline)
                {
                    Stage::Expired
                } else {
                    Stage::Revoked
                };
                self.provider.stop();
            }
        }
        serde_json::to_value(proposal.status())
            .map_err(|error| BrokerError::unavailable(error.to_string()))
    }

    fn select(
        &self,
        proposal: &mut Proposal,
        approval: &crate::approvals::ConsumedGrant,
    ) -> Result<(), String> {
        let started = Instant::now();
        let gui = self.fresh()?;
        proposal.candidate.revalidate()?;
        if self
            .registration
            .selected_read_capability(proposal.candidate.relative())?
            != proposal.capability
        {
            return Err("selected read declaration changed".into());
        }
        let lifetime = approval
            .expires_in()
            .min(gui.expires_in)
            .min(self.metadata_ceiling.saturating_duration_since(started));
        let deadline = started + lifetime;
        let authority_started = Instant::now();
        let remaining = deadline.saturating_duration_since(authority_started);
        if remaining < Duration::from_millis(250) {
            return Err("selected read authority is expiring".into());
        }
        let (grant, authorized) = authority::authorize_gui_file_read(
            authority::GuiReadApproval {
                process: &self.process,
                gui: &gui,
                capability: &proposal.capability,
                approval_session: &proposal.approval_session,
                operation_digest: &proposal.digest,
                lifetime: remaining,
            },
            approval,
        )?;
        let deadline = deadline.min(authority_started + grant.expires_in);
        let read = Arc::new(ReadAuthority {
            registration: self.registration.clone(),
            process: self.process.clone(),
            gui: self.gui,
            gui_generation: self.gui_generation,
            grant: grant.id,
            capability: proposal.capability.clone(),
            candidate: proposal.candidate.clone(),
            approval_session: proposal.approval_session.clone(),
            generation: approval.generation,
            deadline,
            stopped: self.stopped.clone(),
        });
        let permit = ReadPermit::new(
            proposal.candidate.clone(),
            &authorized,
            &proposal.capability,
            deadline,
            read.clone(),
        )?;
        self.provider.install(&proposal.id, permit)?;
        proposal.authority = Some(read.clone());
        if read.require().is_err() || proposal.candidate.revalidate().is_err() {
            self.stop();
            return Err(
                "selected read changed during installation; checked retirement required".into(),
            );
        }
        proposal.stage = Stage::Selected;
        Ok(())
    }

    pub(super) fn health(&self) -> Result<(), String> {
        if let Some(error) = self
            .checker_error
            .lock()
            .map_err(|_| "selected-file checker result poisoned")?
            .clone()
        {
            return Err(error);
        }
        if self.stopped.load(Ordering::Acquire) {
            return Err("selected-file checks retired".into());
        }
        let checker = self
            .checker
            .lock()
            .map_err(|_| "selected-file checker custody poisoned")?;
        if checker.as_ref().is_none_or(|checker| checker.is_finished()) {
            return Err("Root selected-file checker ended unexpectedly".into());
        }
        Ok(())
    }

    fn checker_failed(&self, error: String) {
        tracing::warn!(%error, "Root selected-file checks failed");
        if let Ok(mut failure) = self.checker_error.lock() {
            *failure = Some(error);
        }
        self.stop();
    }

    fn check_selected(&self) -> Result<(), String> {
        if self.stopped.load(Ordering::Acquire) {
            return Err("selected read endpoint retired".into());
        }
        let selected = self
            .proposals
            .lock()
            .map_err(|_| "selected files poisoned")?
            .values()
            .filter_map(|proposal| proposal.authority.clone())
            .collect::<Vec<_>>();
        for read in selected {
            if read.require().is_err() || read.candidate.revalidate().is_err() {
                self.stop();
                return Err(
                    "selected read authority or source ended; checked retirement required".into(),
                );
            }
        }
        Ok(())
    }

    pub(super) fn release_after_death(&self, deadline: Instant) -> Result<(), String> {
        if !self.stopped.load(Ordering::Acquire)
            || self
                .containment
                .active()
                .map_err(|error| error.to_string())?
            || !claw_display_control::workload::cgroup_is_empty(self.group.as_fd())
                .map_err(|error| error.to_string())?
        {
            return Err("selected read source pins require checked instance death".into());
        }
        {
            let mut checker = self
                .checker
                .lock()
                .map_err(|_| "selected-file checker custody poisoned")?;
            if let Some(worker) = checker.as_ref() {
                while !worker.is_finished() {
                    if Instant::now() >= deadline {
                        return Err("Root selected-file checks still running; custody retained".into());
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                checker
                    .take()
                    .ok_or("selected-file checker custody lost")?
                    .join()
                    .map_err(|_| "Root selected-file checker join failed")?;
            }
        }
        let decisions = self.lock_until(deadline)?.values().map(|proposal| {
            (proposal.approval_session.clone(), proposal.approval_request.clone(), proposal.digest.clone())
        }).collect::<Vec<_>>();
        for (session, request, digest) in decisions {
            if Instant::now() >= deadline {
                return Err("selected read decisions have not retired before the deadline".into());
            }
            crate::approvals::retire_operation_decision(
                self.registration.owner(), &session, &request, &digest,
                "selected-file window has retired",
            )?;
            let scope = crate::approvals::RevocationScope::Session {
                uid: Some(self.registration.owner()), session: session.clone(),
            };
            let generation = crate::approvals::generations::revoke(&scope)?;
            crate::clawd::audit::record_approval_revocation(&scope, &session, generation);
        }
        self.lock_until(deadline)?.clear();
        self.approval_sessions
            .lock()
            .map_err(|_| "selected-file session catalogue poisoned")?
            .clear();
        Ok(())
    }

    fn lock_until(
        &self,
        deadline: Instant,
    ) -> Result<std::sync::MutexGuard<'_, BTreeMap<String, Proposal>>, String> {
        loop {
            match self.proposals.try_lock() {
                Ok(proposals) => return Ok(proposals),
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err("selected reads poisoned".into())
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return Err("selected read requests have not quiesced".into());
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }

    pub(super) fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.provider.stop();
        if let Ok(proposals) = self.proposals.try_lock() {
            for read in proposals
                .values()
                .filter_map(|proposal| proposal.authority.as_ref())
            {
                authority::authority().revoke(read.grant);
            }
        }
    }

    pub(super) fn matches_approval(
        &self,
        scope: &crate::approvals::RevocationScope,
        deadline: Instant,
    ) -> Result<bool, String> {
        if scope.owner_uid() != Some(self.registration.owner()) {
            return Ok(false);
        }
        match scope {
            crate::approvals::RevocationScope::Owner { .. } => Ok(true),
            crate::approvals::RevocationScope::Session { session, .. } => loop {
                match self.approval_sessions.try_lock() {
                    Ok(sessions) => return Ok(sessions.contains(session)),
                    Err(std::sync::TryLockError::Poisoned(_)) => {
                        return Err("selected-file session catalogue poisoned".into())
                    }
                    Err(std::sync::TryLockError::WouldBlock) => {
                        if Instant::now() >= deadline {
                            return Err(
                                "selected-file session catalogue busy; custody retained".into(),
                            );
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
            },
        }
    }
}

pub(crate) async fn request_file_read(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, BrokerError> {
    let request: FileReadRequest = serde_json::from_value(params)
        .map_err(|_| BrokerError::execution("invalid selected read request"))?;
    let endpoint = super::manager::selected_reads(authority, client)?;
    let node = request.node.get();
    let context = crate::paths::RoutedPathContext::capture();
    tokio::task::spawn_blocking(move || context.scope_sync(|| endpoint.request(node)))
        .await
        .map_err(|error| {
            BrokerError::unavailable(format!("selected read request failed: {error}"))
        })?
}

pub(crate) async fn file_read_status(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, BrokerError> {
    let request: FileReadStatusRequest = serde_json::from_value(params)
        .map_err(|_| BrokerError::execution("invalid selected read status"))?;
    let endpoint = super::manager::selected_reads(authority, client)?;
    let node = request.node.get();
    let id = request.selection_id.as_str().to_string();
    let context = crate::paths::RoutedPathContext::capture();
    tokio::task::spawn_blocking(move || context.scope_sync(|| endpoint.status(&id, node)))
        .await
        .map_err(|error| {
            BrokerError::unavailable(format!("selected read status failed: {error}"))
        })?
}
