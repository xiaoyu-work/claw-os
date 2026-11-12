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
use crate::worker::native_fs::{
    FileCandidate, ReadLeaseCheck, ReadPermit, ReadProvider, WriteLeaseCheck, WritePermit,
};

use super::inputs::{
    FileReadRequest, FileReadStatusRequest, FileSaveReservationRequest, FileWriteRequest,
    FileWriteStatusRequest,
};

#[cfg(test)]
struct TestTiming(&'static str, Instant);

#[cfg(test)]
impl Drop for TestTiming {
    fn drop(&mut self) {
        let thread = std::thread::current();
        let lane = match thread.name() {
            Some("gui-file-checks") => "checker",
            Some("claw-native-fs") => "native_io",
            _ => "controller",
        };
        eprintln!(
            "private selected-file timing sourceStep={} sourceLane={lane} elapsedMs={}",
            self.0, self.1.elapsed().as_millis(),
        );
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Access {
    Read,
    Write,
}

impl Access {
    fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }
    fn verb(self) -> Verb {
        match self {
            Self::Read => Verb::FS_READ,
            Self::Write => Verb::FS_WRITE,
        }
    }
    fn prefix(self) -> &'static str {
        match self {
            Self::Read => "sr",
            Self::Write => "sw",
        }
    }
    fn route(self) -> &'static str {
        match self {
            Self::Read => "gui.file.read.status",
            Self::Write => "gui.file.write.status",
        }
    }
    fn capability(
        self,
        registration: &Registration,
        relative: &std::path::Path,
    ) -> Result<Cap, String> {
        #[cfg(test)]
        let _timing = TestTiming("root_declared_capability", Instant::now());
        match self {
            Self::Read => registration.selected_read_capability(relative),
            Self::Write => registration.selected_file_capability(relative, Verb::FS_WRITE),
        }
    }
}

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
    access: Access,
    id: String,
    candidate: Arc<FileCandidate>,
    capability: Cap,
    digest: String,
    approval_session: String,
    approval_request: String,
    display: String,
    stage: Stage,
    authority: Option<Arc<SelectionAuthority>>,
}

impl Proposal {
    fn status(&self) -> Status {
        Status {
            version: 1,
            selection_id: self.id.clone(),
            node: self.candidate.node(),
            access: self.access.name(),
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
                Stage::Expired => Some("file_lease_expired"),
                Stage::Revoked => Some("file_lease_revoked"),
                _ => None,
            },
        }
    }
}

struct SelectionAuthority {
    access: Access,
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

impl SelectionAuthority {
    fn authorize_capability(&self) -> Result<authority::Authorized, String> {
        if self.stopped.load(Ordering::Acquire) || Instant::now() >= self.deadline {
            return Err("selected read lease ended".into());
        }
        crate::paths::RoutedPathContext::for_owner(
            self.registration.owner(),
            self.registration.home.clone(),
        )
        .scope_sync(|| {
            let gui = {
                #[cfg(test)]
                let _timing = TestTiming("root_registration_snapshot", Instant::now());
                self.registration.snapshot(&self.process)?
            };
            if gui.id != self.gui
                || gui.generation != self.gui_generation
                || self
                    .access
                    .capability(&self.registration, self.candidate.relative())?
                    != self.capability
                || crate::approvals::generations::current(
                    Some(self.registration.owner()),
                    &self.approval_session,
                )? != self.generation
            {
                return Err("selected read authority, declaration or decision changed".into());
            }
            let presentation = Presentation {
                uid: self.registration.owner(),
                pid: self.process.pid() as u32,
                start_time_ticks: Some(self.process.start_ticks()),
                audience: Audience::GuiResource,
                route: self.access.route(),
                session_id: Some(self.registration.session_id.clone()),
            };
            match self.access {
                Access::Read => {
                    authority::exercise_gui_file_read(self.grant, &self.capability, presentation)
                }
                Access::Write => {
                    authority::exercise_gui_file(self.grant, &self.capability, presentation)
                }
            }
        })
    }
}

impl ReadLeaseCheck for SelectionAuthority {
    fn require(&self) -> Result<(), String> {
        self.authorize_capability().map(|_| ())
    }
}

impl WriteLeaseCheck for SelectionAuthority {
    fn authorize(&self) -> Result<authority::Authorized, String> {
        self.authorize_capability()
    }
}

impl Drop for SelectionAuthority {
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
    mutation_session: Mutex<Option<crate::session::SessionId>>,
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
            mutation_session: Mutex::new(None),
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

    fn request(&self, node: u64, access: Access) -> Result<Value, BrokerError> {
        let gui = self.fresh().map_err(BrokerError::authorization)?;
        let candidate = self
            .provider
            .candidate(node)
            .map_err(BrokerError::authorization)?;
        candidate.revalidate().map_err(BrokerError::authorization)?;
        if access == Access::Write {
            candidate
                .require_inverse_path()
                .map_err(BrokerError::authorization)?;
        }
        let capability = access
            .capability(&self.registration, candidate.relative())
            .map_err(BrokerError::authorization)?;
        let mut proposals = self
            .proposals
            .lock()
            .map_err(|_| BrokerError::unavailable("selected reads poisoned"))?;
        self.fresh().map_err(BrokerError::authorization)?;
        if let Some(proposal) = proposals.values().find(|proposal| {
            proposal.access == access
                && proposal.candidate.node() == node
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
                access.name(),
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
        let id = format!("{}-{nonce}", access.prefix());
        let approval_session = format!(
            "gui-{}-{}-{nonce}",
            access.name(),
            Epoch(self.instance.0).label()
        );
        let display = candidate.display().map_err(BrokerError::authorization)?;
        let purpose = if access == Access::Read {
            format!("Read ONLY selected file {display}; window {}, native node {node}. No directory, write, delete or execute access", Epoch(self.instance.0).label())
        } else {
            format!("Write ONLY selected existing file {display}, including a Root-reserved atomic replacement; window {}, native node {node}. No content read, sibling/directory write, delete or execute access", Epoch(self.instance.0).label())
        };
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
            access.verb(),
            capability.scope.clone(),
            approval_session.clone(),
            purpose,
            Some(format!(
                "GUI {} {} selected file {}",
                self.registration.launch.app_id(),
                self.registration.selector,
                access.name()
            )),
            Some(self.registration.owner()),
            None,
            Some(&digest),
        )
        .map_err(BrokerError::unavailable)?;
        let proposal = Proposal {
            access,
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

    fn status(&self, id: &str, node: u64, access: Access) -> Result<Value, BrokerError> {
        self.fresh().map_err(BrokerError::authorization)?;
        let mut proposals = self
            .proposals
            .lock()
            .map_err(|_| BrokerError::unavailable("selected reads poisoned"))?;
        let proposal = proposals
            .get_mut(id)
            .filter(|proposal| proposal.access == access && proposal.candidate.node() == node)
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
                            proposal.access.verb(),
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
        if proposal
            .access
            .capability(&self.registration, proposal.candidate.relative())?
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
        let binding = authority::GuiReadApproval {
            process: &self.process,
            gui: &gui,
            capability: &proposal.capability,
            approval_session: &proposal.approval_session,
            operation_digest: &proposal.digest,
            lifetime: remaining,
        };
        let (grant, authorized) = match proposal.access {
            Access::Read => authority::authorize_gui_file_read(binding, approval)?,
            Access::Write => authority::authorize_gui_file(binding, approval)?,
        };
        let deadline = deadline.min(authority_started + grant.expires_in);
        let read = Arc::new(SelectionAuthority {
            access: proposal.access,
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
        match proposal.access {
            Access::Read => {
                let permit = ReadPermit::new(
                    proposal.candidate.clone(),
                    &authorized,
                    &proposal.capability,
                    deadline,
                    read.clone(),
                )?;
                self.provider.install(&proposal.id, permit)?;
            }
            Access::Write => {
                let session = self.write_session()?;
                let permit = WritePermit::new(
                    proposal.candidate.clone(),
                    &authorized,
                    &proposal.capability,
                    deadline,
                    read.clone(),
                    session,
                )?;
                self.provider.install_write(&proposal.id, permit)?;
            }
        }
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
            if read.require().is_err() || read.candidate.health_validate().is_err() {
                self.stop();
                return Err(
                    "selected read authority or source ended; checked retirement required".into(),
                );
            }
        }
        self.provider.check_histories()?;
        Ok(())
    }

    fn write_session(&self) -> Result<crate::session::SessionId, String> {
        let mut session = self
            .mutation_session
            .lock()
            .map_err(|_| "native mutation session poisoned")?;
        if let Some(session) = &*session {
            return Ok(session.clone());
        }
        let context = crate::paths::RoutedPathContext::for_owner(
            self.registration.owner(),
            self.registration.home.clone(),
        );
        let id = context.clone().scope_sync(|| {
            crate::session::create(format!(
                "GUI native file effects {} window {}",
                self.registration.launch.app_id(),
                Epoch(self.instance.0).label()
            ))
            .map_err(|error| error.to_string())
        })?;
        *session = Some(id.clone());
        context.scope_sync(|| {
            if !crate::session::record_is_root_owned(&id) {
                return Err("GUI mutation evidence is not Root-owned".into());
            }
            crate::session::update_meta(&id, |meta| {
                meta.client = crate::session::SessionClient::new(
                    crate::session::SessionSource::App,
                    false,
                    true,
                );
            })
            .map_err(|error| error.to_string())?;
            Ok::<_, String>(())
        })?;
        Ok(id)
    }

    fn reserve(&self, id: &str, node: u64) -> Result<Value, BrokerError> {
        self.fresh().map_err(BrokerError::authorization)?;
        let proposals = self
            .proposals
            .lock()
            .map_err(|_| BrokerError::unavailable("selected files poisoned"))?;
        let proposal = proposals
            .get(id)
            .filter(|proposal| {
                proposal.access == Access::Write
                    && proposal.stage == Stage::Selected
                    && proposal.candidate.node() == node
            })
            .ok_or_else(|| {
                BrokerError::authorization("atomic save requires this exact selected writable file")
            })?;
        let authority = proposal
            .authority
            .as_ref()
            .ok_or_else(|| BrokerError::authorization("write selection has no authority"))?;
        let _authorized = authority
            .authorize_capability()
            .map_err(BrokerError::authorization)?;
        proposal
            .candidate
            .revalidate()
            .map_err(BrokerError::authorization)?;
        let reserved = self
            .provider
            .reserve_save(id)
            .map_err(BrokerError::authorization)?;
        Ok(serde_json::json!({
            "version":1,"reservation_id":reserved.id,"selection_id":id,"node":node,
            "parent_node":reserved.parent_node,"name":reserved.name,
            "expires_in_ms":reserved.deadline.saturating_duration_since(Instant::now())
                .as_millis().min(u128::from(u64::MAX)) as u64,
        }))
    }

    pub(super) fn finish_mutation_session(&self) -> Result<(), String> {
        let session = self
            .mutation_session
            .lock()
            .map_err(|_| "native mutation session poisoned")?;
        if let Some(session) = &*session {
            crate::paths::RoutedPathContext::for_owner(
                self.registration.owner(),
                self.registration.home.clone(),
            )
            .scope_sync(|| {
                if !crate::session::record_is_root_owned(session) {
                    return Err("GUI mutation session lost Root ownership; custody retained".into());
                }
                let meta = crate::session::get_meta(session).map_err(|error| error.to_string())?;
                if meta.owner_uid != Some(self.registration.owner()) {
                    return Err("GUI mutation session owner changed".into());
                }
                if !meta.status.is_active() {
                    return Ok(());
                }
                let unresolved = crate::session::journal::unresolved_mutations(
                    &crate::session::journal::Partition::Session(session.clone()),
                    self.registration.owner(),
                )
                .map_err(|error| error.to_string())?;
                crate::session::end(
                    session,
                    if unresolved.is_empty() {
                        crate::session::Status::Done
                    } else {
                        crate::session::Status::Failed
                    },
                )
                .map_err(|error| error.to_string())
            })?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn mutation_session_id(&self) -> Result<crate::session::SessionId, String> {
        self.mutation_session
            .lock()
            .map_err(|_| "native mutation session poisoned")?
            .clone()
            .ok_or_else(|| "GUI has no canonical native write session".into())
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
        let decisions = self
            .lock_until(deadline)?
            .values()
            .map(|proposal| {
                (
                    proposal.approval_session.clone(),
                    proposal.approval_request.clone(),
                    proposal.digest.clone(),
                )
            })
            .collect::<Vec<_>>();
        for (session, request, digest) in decisions {
            if Instant::now() >= deadline {
                return Err("selected read decisions have not retired before the deadline".into());
            }
            crate::approvals::retire_operation_decision(
                self.registration.owner(),
                &session,
                &request,
                &digest,
                "selected-file window has retired",
            )?;
            let scope = crate::approvals::RevocationScope::Session {
                uid: Some(self.registration.owner()),
                session: session.clone(),
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
    tokio::task::spawn_blocking(move || context.scope_sync(|| endpoint.request(node, Access::Read)))
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
    tokio::task::spawn_blocking(move || {
        context.scope_sync(|| endpoint.status(&id, node, Access::Read))
    })
    .await
    .map_err(|error| BrokerError::unavailable(format!("selected read status failed: {error}")))?
}

pub(crate) async fn request_file_write(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, BrokerError> {
    let request: FileWriteRequest = serde_json::from_value(params)
        .map_err(|_| BrokerError::execution("invalid selected write request"))?;
    let endpoint = super::manager::selected_reads(authority, client)?;
    let node = request.node.get();
    let context = crate::paths::RoutedPathContext::capture();
    tokio::task::spawn_blocking(move || {
        context.scope_sync(|| endpoint.request(node, Access::Write))
    })
    .await
    .map_err(|error| BrokerError::unavailable(format!("selected write request failed: {error}")))?
}

pub(crate) async fn file_write_status(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, BrokerError> {
    let request: FileWriteStatusRequest = serde_json::from_value(params)
        .map_err(|_| BrokerError::execution("invalid selected write status"))?;
    let endpoint = super::manager::selected_reads(authority, client)?;
    let node = request.node.get();
    let id = request.selection_id.as_str().to_string();
    let context = crate::paths::RoutedPathContext::capture();
    tokio::task::spawn_blocking(move || {
        context.scope_sync(|| endpoint.status(&id, node, Access::Write))
    })
    .await
    .map_err(|error| BrokerError::unavailable(format!("selected write status failed: {error}")))?
}

pub(crate) async fn reserve_file_save(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, BrokerError> {
    let request: FileSaveReservationRequest = serde_json::from_value(params)
        .map_err(|_| BrokerError::execution("invalid atomic save reservation"))?;
    let endpoint = super::manager::selected_reads(authority, client)?;
    let node = request.node.get();
    let id = request.selection_id.as_str().to_string();
    let context = crate::paths::RoutedPathContext::capture();
    tokio::task::spawn_blocking(move || context.scope_sync(|| endpoint.reserve(&id, node)))
        .await
        .map_err(|error| {
            BrokerError::unavailable(format!("atomic save reservation failed: {error}"))
        })?
}
