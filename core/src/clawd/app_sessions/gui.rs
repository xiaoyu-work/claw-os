use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use claw_display_control::{Epoch, ProcessIdentity};

use crate::bridge::AppLaunch;
use crate::caps::{Cap, CapSet, Scope, Verb};
use crate::display_session::registry::{GuiControl, Registry};

use super::{authority, ClientIdentity};

pub(crate) struct Draft {
    launch: AppLaunch,
    selector: String,
    launcher: ProcessIdentity,
    client: ClientIdentity,
    home: PathBuf,
    control: GuiControl,
    delegation: BoundDelegation,
}

pub(crate) struct Registration {
    pub launch: AppLaunch,
    pub selector: String,
    pub launcher: ProcessIdentity,
    pub client: ClientIdentity,
    pub home: PathBuf,
    pub control: GuiControl,
    pub session_id: String,
    pub caps: CapSet,
    pub registered_at: Instant,
    pub authority_alias: Epoch,
    delegation: BoundDelegation,
}

struct BoundDelegation {
    parent: Option<String>,
    session: String,
    generation: u32,
    inherited_invoke: bool,
}

impl Draft {
    pub(super) fn new(
        package: Arc<crate::provenance::VerifiedPackage>,
        selector: &str,
        client: &ClientIdentity,
        launcher_authority: &super::LauncherAuthority,
        delegation: &super::Delegation,
    ) -> Result<Self, String> {
        let launch = AppLaunch::new(package)?;
        crate::bridge::gui_args::prepare(launch.manifest().runtime, selector, &[])?;
        if launch
            .manifest()
            .desktop
            .as_ref()
            .map(|desktop| desktop.exec.as_str())
            != Some(selector)
        {
            return Err("GUI selector differs from the verified manifest".to_string());
        }
        let launcher = kernel_peer(client)?;
        let display = Registry::get()
            .map_err(|error| error.to_string())?
            .for_process(&launcher)
            .map_err(|error| error.to_string())?;
        let control = GuiControl::of(&display).map_err(|error| error.to_string())?;
        let home = crate::paths::verified_home_for_uid(control.owner_uid)?;
        let generation =
            crate::paths::RoutedPathContext::for_owner(control.owner_uid, home.clone())
                .scope_sync(|| {
                    crate::approvals::generations::current(
                        Some(control.owner_uid),
                        &delegation.grant_session,
                    )
                })?;
        let inherited_invoke = delegation
            .ceiling
            .covers(&Cap::new(Verb::AGENT_INVOKE, Scope::name(launch.app_id())));
        Ok(Self {
            launch,
            selector: selector.to_string(),
            launcher,
            client: client.clone(),
            home,
            control,
            delegation: BoundDelegation {
                parent: launcher_authority.parent.clone(),
                session: delegation.grant_session.clone(),
                generation,
                inherited_invoke,
            },
        })
    }

    pub fn finish(self, session_id: String, caps: CapSet) -> Result<Registration, String> {
        Ok(Registration {
            launch: self.launch,
            selector: self.selector,
            launcher: self.launcher,
            client: self.client,
            home: self.home,
            control: self.control,
            session_id,
            caps,
            registered_at: Instant::now(),
            authority_alias: Epoch::random().map_err(|error| error.to_string())?,
            delegation: self.delegation,
        })
    }
}

impl Registration {
    pub fn owner(&self) -> u32 {
        self.control.owner_uid
    }

    pub fn belongs_to_session(&self, session: &str) -> bool {
        self.session_id == session || self.delegation.session == session
    }

    fn require_delegation(&self) -> Result<(), String> {
        crate::paths::RoutedPathContext::for_owner(self.owner(), self.home.clone()).scope_sync(
            || {
                let current = super::launcher_authority(
                    &crate::proc::registry_sessions(),
                    self.launcher.pid() as u32,
                    Some(self.launcher.start_ticks()),
                    &self.home,
                )?;
                let generation = crate::approvals::generations::current(
                    Some(self.owner()),
                    &self.delegation.session,
                )?;
                if current.parent != self.delegation.parent
                    || generation != self.delegation.generation
                {
                    return Err(
                        "GUI launcher's authority or revocation generation changed".to_string()
                    );
                }
                let invoke = Cap::new(Verb::AGENT_INVOKE, Scope::name(self.launch.app_id()));
                if self.caps.iter().any(|cap| !current.caps.covers(cap))
                    || (self.delegation.inherited_invoke && !current.caps.covers(&invoke))
                {
                    return Err(
                        "GUI launcher no longer delegates the bound operation needs".to_string()
                    );
                }
                Ok(())
            },
        )
    }

    pub fn require_client(&self, client: &ClientIdentity) -> Result<(), String> {
        if kernel_peer(client)? != self.launcher {
            return Err("GUI instance belongs to another authenticated launcher".to_string());
        }
        Ok(())
    }

    pub fn require_launch(&self, handle: &str, client: &ClientIdentity) -> Result<(), String> {
        self.require_client(client)?;
        self.require_delegation()?;
        let grant = super::require_launch_grant(client, handle, &self.session_id, self.owner())?;
        if grant.issued_ago > super::BIND_WINDOW
            || grant.subject.app_id.as_deref() != Some(self.launch.app_id())
            || !grant.audience.contains(authority::Audience::GuiResource)
        {
            return Err("GUI registration is expired or has no GUI lifetime authority".to_string());
        }
        Ok(())
    }

    pub async fn bind_owned(
        &self,
        handle: &str,
        child: &std::process::Child,
        resources: &crate::worker::LaunchResources,
    ) -> Result<ProcessIdentity, String> {
        self.require_launch(handle, &self.client)?;
        let process = ProcessIdentity::child(child).map_err(|error| error.to_string())?;
        if process.uid() != self.owner() {
            return Err("GUI child has the wrong execution owner".to_string());
        }
        claw_display_control::workload::InstanceProcess::receive(
            process.pidfd().map_err(|error| error.to_string())?,
            resources.gui_cgroup()?,
        )
        .map_err(|error| error.to_string())?;
        let (package, ceiling) =
            super::installed_app_for_session(self.owner(), &self.session_id, self.launch.app_id())?;
        let _ = package;
        let session_id = self.session_id.clone();
        let child_pid = child.id();
        let caps = crate::paths::with_user_override(self.owner(), self.home.clone(), async {
            crate::proc::bind_session_process(&session_id, child_pid)?;
            crate::proc::session_info_by_id(&session_id)
                .and_then(|session| session.caps)
                .ok_or_else(|| "GUI registration lost its capability row".to_string())
        })
        .await?;
        let caps = super::clamp_to_ceiling(&ceiling, self.launch.app_id(), &caps, "gui-bind");
        if caps != self.caps {
            return Err("GUI authority changed before process binding".to_string());
        }
        super::issue_session_grant_with_gui(
            handle,
            &self.session_id,
            Some(self.launch.app_id()),
            self.owner(),
            child_pid,
            &caps,
            Some(&ceiling),
            true,
        )?;
        crate::provenance::runtime::bind_process(self.owner(), &self.session_id, child_pid);
        Ok(process)
    }

    pub fn snapshot(&self, process: &ProcessIdentity) -> Result<authority::GrantView, String> {
        process
            .assert_current()
            .map_err(|error| error.to_string())?;
        self.launcher
            .assert_current()
            .map_err(|error| error.to_string())?;
        self.require_delegation()?;
        let trust = crate::provenance::trust_store();
        self.launch
            .package()
            .assert_current(&trust)
            .map_err(|error| error.to_string())?;
        crate::provenance::runtime::assert_live_instance(self.owner(), &self.session_id, &trust)?;
        let presentation = authority::Presentation {
            uid: self.owner(),
            pid: process.pid() as u32,
            start_time_ticks: Some(process.start_ticks()),
            audience: authority::Audience::GuiResource,
            route: "gui.resource-lease",
            session_id: Some(self.session_id.clone()),
        };
        let view = authority::authority()
            .resolve_session(&self.session_id, &presentation)
            .map_err(|error| error.to_string())?;
        if view.subject.app_id.as_deref() != Some(self.launch.app_id())
            || view.uses_remaining.is_some()
            || view.caps != self.caps
        {
            return Err("GUI session authority is not the bound operation authority".to_string());
        }
        // Session grants are unbounded; this rechecks the real grant and App policy,
        // rather than treating a cached CapSet or a successful preflight as authority.
        authority::authority()
            .consume(
                view.id,
                &view.caps.iter().cloned().collect::<Vec<_>>(),
                &presentation,
            )
            .map_err(|error| error.to_string())
    }

    pub fn clear_after_retirement(&self) {
        authority::authority().revoke_session(&self.session_id);
        crate::proc::deregister_session(&self.session_id);
        crate::provenance::runtime::deregister(self.owner(), &self.session_id);
    }
}

pub(crate) fn kernel_peer(client: &ClientIdentity) -> Result<ProcessIdentity, String> {
    let pid = client
        .pid
        .filter(|pid| *pid > 1)
        .ok_or("GUI launcher PID is unavailable")?;
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if raw < 0 {
        return Err(format!(
            "capture GUI launcher pidfd: {}",
            std::io::Error::last_os_error()
        ));
    }
    let descriptor = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    let process =
        ProcessIdentity::from_pidfd(descriptor.as_fd()).map_err(|error| error.to_string())?;
    if client.uid != Some(process.uid())
        || client.execution_uid.is_some()
        || client.gid != Some(process.gid())
        || client.start_time_ticks != Some(process.start_ticks())
    {
        return Err("GUI launcher does not match its kernel peer identity".to_string());
    }
    Ok(process)
}

pub(crate) fn provider_route(command: super::Command) -> Result<&'static super::Route, String> {
    super::relayable_route(command)
}
