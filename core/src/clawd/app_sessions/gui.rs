use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use claw_display_control::{Epoch, ProcessIdentity};

use crate::bridge::AppLaunch;
use crate::caps::{Cap, CapSet, Scope, Verb};
use crate::display_session::registry::{GuiControl, Registry};
use crate::display_session::SHELL_APP;

#[cfg(test)]
use crate::provenance::verify::test_profile::{Guard as ProfileGuard, Span, Step};

use super::{authority, ClientIdentity};

mod selected_source;

pub(crate) struct Draft {
    launch: AppLaunch,
    gui_entry: Option<String>,
    entry: String,
    selector: String,
    launcher: ProcessIdentity,
    client: ClientIdentity,
    home: PathBuf,
    control: GuiControl,
    delegation: BoundDelegation,
    pub(super) desktop_shell: bool,
    pub(super) system_menu_app: bool,
    pub(super) owner_settings: bool,
    pub(super) owner_launch: bool,
}

pub(crate) struct Registration {
    pub launch: AppLaunch,
    pub gui_entry: Option<String>,
    entry: String,
    pub selector: String,
    pub launcher: ProcessIdentity,
    pub client: ClientIdentity,
    pub home: PathBuf,
    pub control: GuiControl,
    pub(crate) desktop_shell: bool,
    pub(crate) owner_settings: bool,
    pub(crate) owner_launch: bool,
    pub session_id: String,
    pub caps: CapSet,
    pub optional_needs_ungranted: Vec<usize>,
    optional_approval_expiry: Option<u64>,
    pub registered_at: Instant,
    pub authority_alias: Epoch,
    delegation: BoundDelegation,
}

struct BoundDelegation {
    parent: Option<String>,
    session: String,
    generation: u32,
    inherited_invoke: bool,
    inherited_ceiling: CapSet,
}

impl Draft {
    pub(super) fn new(
        package: Arc<crate::provenance::VerifiedPackage>,
        gui_entry: Option<&str>,
        selector: &str,
        client: &ClientIdentity,
        launcher_authority: &super::LauncherAuthority,
        delegation: &super::Delegation,
    ) -> Result<Self, String> {
        let launch = AppLaunch::new(package)?;
        crate::bridge::gui_args::prepare(launch.manifest().runtime, selector, &[])?;
        let selected = launch.manifest().gui_entry(gui_entry)?;
        if selected.exec != selector {
            return Err("GUI selector differs from the verified manifest".to_string());
        }
        let entry = selected.entry.to_string();
        launch.gui_entrypoint(gui_entry)?;
        let _binding = launch.bind(std::slice::from_ref(&entry))?;
        let launcher = kernel_peer(client)?;
        let display = Registry::get()
            .map_err(|error| error.to_string())?
            .for_process(&launcher)
            .map_err(|error| error.to_string())?;
        let system_launcher = gui_entry.is_none()
            && launcher_authority.parent.is_none()
            && display
                .lock()
                .map_err(|_| "display registry lock poisoned")?
                .starts_gui_launcher(&launcher)
                .map_err(|error| error.to_string())?;
        let desktop_shell = system_launcher && launch.app_id() == SHELL_APP;
        if desktop_shell {
            if entry != "bin/claw-desktop-shell" || selector != "--gui"
                || launch.manifest().gui_entry(None)?.legacy_needs
            {
                return Err("system desktop shell has an invalid default entry".into());
            }
            verify_system_gui(&launch)?;
        }
        let system_menu_app = system_launcher && !desktop_shell
            && matches!(launch.app_id(), "cosmic-edit" | "cosmic-files" | "cosmic-launcher"
                | "cosmic-notifications" | "cosmic-player" | "cosmic-screenshot"
                | "cosmic-settings" | "cosmic-store" | "cosmic-term")
            && verify_system_gui(&launch).is_ok();
        let owner_settings = system_menu_app && launch.app_id() == "cosmic-settings"
            && entry == "bin/cosmic-settings" && selector == "--gui";
        let owner_launch = launcher_authority.parent.is_none()
            && launcher_authority.activity.is_none()
            && launcher_authority.workspace.is_none()
            && client.extension_host.is_none()
            && (system_launcher || client.attended_local);
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
            gui_entry: gui_entry.map(str::to_string),
            entry,
            selector: selector.to_string(),
            launcher,
            client: client.clone(),
            home,
            control,
            desktop_shell,
            system_menu_app,
            owner_settings,
            owner_launch,
            delegation: BoundDelegation {
                parent: launcher_authority.parent.clone(),
                session: delegation.grant_session.clone(),
                generation,
                inherited_invoke,
                inherited_ceiling: delegation.ceiling.clone(),
            },
        })
    }

    pub fn finish(
        self,
        session_id: String,
        caps: CapSet,
        optional_needs_ungranted: Vec<usize>,
        optional_approval_expiry: Option<u64>,
    ) -> Result<Registration, String> {
        Ok(Registration {
            launch: self.launch,
            gui_entry: self.gui_entry,
            entry: self.entry,
            selector: self.selector,
            launcher: self.launcher,
            client: self.client,
            home: self.home,
            control: self.control,
            desktop_shell: self.desktop_shell,
            owner_settings: self.owner_settings,
            owner_launch: self.owner_launch,
            session_id,
            caps,
            optional_needs_ungranted,
            optional_approval_expiry,
            registered_at: Instant::now(),
            authority_alias: Epoch::random().map_err(|error| error.to_string())?,
            delegation: self.delegation,
        })
    }
}

fn verify_system_gui(launch: &AppLaunch) -> Result<(), String> {
    use crate::provenance::{
        envelope::ENVELOPE_FILE, fsec, trust::VENDOR_TRUST_ROOT, TrustSource, TrustTier,
    };
    use std::path::Path;

    let package = launch.package();
    let trust = crate::provenance::trust_store();
    let publisher = Path::new(VENDOR_TRUST_ROOT).join("clawos-app.json");
    if package.dir() != Path::new("/usr/lib/cos/apps").join(launch.app_id())
        || launch.manifest().runtime != crate::caps::manifest::Runtime::Binary
        || package.tier() != TrustTier::Vendor
        || !matches!(package.source(), TrustSource::Publisher { key_id }
            if trust.key(key_id).is_some_and(|key| key.source == publisher))
    {
        return Err("system desktop role requires the installed vendor-signed GUI".into());
    }
    fsec::require_secure_location(package.dir(), &[0]).map_err(|error| error.to_string())?;
    for path in package.files().map(|file| file.path.as_str()).chain([ENVELOPE_FILE]) {
        let node = fsec::lstat(&package.dir().join(path)).map_err(|error| error.to_string())?;
        if node.uid != 0 || node.is_symlink || node.is_group_or_world_writable() {
            return Err("system desktop package contains an unprotected resource".into());
        }
    }
    package.assert_current(&trust).map_err(|error| error.to_string())
}

pub(super) fn owner_settings_cap(cap: &Cap) -> bool {
    match (cap.verb, &cap.scope) {
        (Verb::SYS_PERMISSIONS, Scope::Name(name)) => name == "manage",
        (Verb::SYS_OBSERVE, Scope::Name(name)) => matches!(name.as_str(),
            "regional" | "accessibility" | "audio" | "display" | "network" | "bluetooth" | "bluetooth.service" | "power"),
        (Verb::IPC_SUBSCRIBE | Verb::IPC_INVOKE, Scope::Name(name)) => matches!(name.as_str(),
            "desktop.power.profile" | "desktop.preferences.compositor" | "desktop.preferences.keyboard-catalog"
            | "desktop.preferences.workspaces" | "desktop.preferences.appearance" | "desktop.preferences.panels"
            | "desktop.preferences.idle" | "desktop.preferences.audio" | "desktop.preferences.accessibility"
            | "desktop.preferences.clock" | "desktop.preferences.shortcuts" | "desktop.preferences.window-rules"
            | "desktop.preferences.background" | "desktop.preferences.regional"),
        (Verb::NET_MANAGE, Scope::Name(name)) => matches!(name.as_str(), "ethernet" | "wifi" | "vpn" | "airplane"),
        _ => desktop_shell_optional_cap(cap)
            || matches!((cap.verb, &cap.scope),
                (Verb::UI_ACCESSIBILITY | Verb::DEVICE_BLUETOOTH, Scope::Name(name)) if name == "control")
            || cap == &Cap::new(Verb::SYS_LANGUAGE, Scope::name("self")),
    }
}

pub(super) fn desktop_shell_startup_caps() -> CapSet {
    let notifications = crate::clawd::notifications::gui::RESOURCE;
    CapSet::from_caps([
        Cap::new(Verb::AGENT_INVOKE, Scope::name(SHELL_APP)),
        Cap::unscoped(Verb::UI_PANEL),
        Cap::new(Verb::SYS_OBSERVE, Scope::name("desktop")),
        Cap::new(Verb::DESKTOP_WINDOW, Scope::name("control")),
        Cap::new(Verb::IPC_SUBSCRIBE, Scope::name(notifications)),
        Cap::new(Verb::IPC_INVOKE, Scope::name(notifications)),
        Cap::new(Verb::IPC_INVOKE, Scope::name("desktop.session.lock")),
    ])
}

pub(super) fn desktop_shell_optional_cap(cap: &Cap) -> bool {
    let Scope::Name(name) = &cap.scope else {
        return false;
    };
    matches!((cap.verb, name.as_str()),
        (Verb::SYS_OBSERVE, "audio" | "display")
        | (Verb::DEVICE_AUDIO, "output")
        | (Verb::DEVICE_MICROPHONE, "input")
        | (Verb::DEVICE_MEDIA_ROUTE, "pipewire")
        | (Verb::DEVICE_DISPLAY, "manage"))
}

impl Registration {
    pub fn owner(&self) -> u32 {
        self.control.owner_uid
    }

    pub fn belongs_to_session(&self, session: &str) -> bool {
        self.session_id == session || self.delegation.session == session
    }

    /// Validate a selected native file against this surface's own declared
    /// read ceiling. This is eligibility, never a content selection or grant.
    pub(crate) fn selected_read_capability(&self, relative: &std::path::Path) -> Result<Cap, String> {
        self.selected_file_capability(relative, Verb::FS_READ)
    }

    pub(crate) fn selected_file_capability(&self, relative: &std::path::Path, verb: Verb) -> Result<Cap, String> {
        #[cfg(test)]
        let _profile = ProfileGuard::enter();
        #[cfg(test)]
        let _needs = Span::new(Step::Needs);
        use crate::caps::manifest::ScopeBinding;
        if !matches!(verb, Verb::FS_READ | Verb::FS_WRITE) {
            return Err("native file selection supports only independent read or write".into());
        }
        if relative.as_os_str().is_empty() || relative.components().any(|part|
            !matches!(part, std::path::Component::Normal(_)))
        { return Err("selected read requires a retained view-relative file".into()); }
        let selected = self.launch.manifest().gui_entry(self.gui_entry.as_deref())?;
        if selected.legacy_needs {
            return Err("selected read requires an explicit need of this signed surface".into());
        }
        let target = self.home.join(relative);
        let paths = crate::caps::args::PathContext { home: self.home.clone(), cwd: None };
        let mut candidates = Vec::new();
        for need in selected.launch_needs().chain(selected.optional_needs.iter().copied()
            .filter(|need| selected.optional_need_is_bound(need)))
        {
            if need.verb != verb { continue; }
            let scopes = match &need.scope {
                ScopeBinding::OwnerPath { path } =>
                    vec![crate::caps::manifest::resolve_owner_path(need.verb, path, &paths)?],
                ScopeBinding::Fixed { scope: Scope::Path(path) } if std::path::Path::new(path).is_absolute() =>
                    vec![Scope::path(path.clone()).canonicalized()],
                ScopeBinding::Wild => self.caps.iter()
                    .filter(|cap| cap.verb == verb && matches!(cap.scope, Scope::Path(_)))
                    .map(|cap| cap.scope.clone()).collect(),
                _ => continue,
            };
            for scope in scopes {
                let Scope::Path(pattern) = &scope else { continue; };
                let root = pattern.strip_suffix("/**").unwrap_or(pattern);
                if root.contains(['*', '?', '[', ']', '{', '}'])
                    || !std::path::Path::new(root).starts_with(&self.home)
                { continue; }
                let covered = if pattern.ends_with("/**") {
                    target.starts_with(root)
                } else {
                    target.as_os_str() == std::path::Path::new(root).as_os_str()
                };
                if covered { candidates.push(Cap::new(verb, scope)); }
            }
        }
        candidates.sort_by_key(|cap| std::cmp::Reverse(match &cap.scope {
            Scope::Path(path) => path.len(), _ => 0,
        }));
        let cap = candidates.into_iter().next().ok_or("selected file is outside this surface's declared read needs")?;
        #[cfg(test)]
        drop(_needs);
        #[cfg(test)]
        let _runtime_before = Span::new(Step::RuntimeBefore);
        let expected = crate::provenance::runtime::package_for(self.owner(), &self.session_id)?
            .ok_or("App session has no bound package identity")?;
        if expected != self.launch.package_ref() {
            return Err("selected file registration has another running package identity".into());
        }
        #[cfg(test)]
        drop(_runtime_before);
        let current = selected_source::reverify(
            self.launch.package(),
            &crate::provenance::trust_store(),
        )?;
        #[cfg(test)]
        let _withdrawal_package = Span::new(Step::WithdrawalPackage);
        crate::clawd::trust_withdrawal::check_package(self.owner(), &current)?;
        #[cfg(test)]
        drop(_withdrawal_package);
        #[cfg(test)]
        let _withdrawal_session = Span::new(Step::WithdrawalSession);
        crate::clawd::trust_withdrawal::check_session(self.owner(), &self.session_id)?;
        #[cfg(test)]
        drop(_withdrawal_session);
        #[cfg(test)]
        let _runtime_after = Span::new(Step::RuntimeAfter);
        if crate::provenance::runtime::package_for(self.owner(), &self.session_id)?.as_ref()
            != Some(&expected)
        {
            return Err("selected file package identity changed during validation".into());
        }
        #[cfg(test)]
        drop(_runtime_after);
        #[cfg(test)]
        let _ceiling_policy = Span::new(Step::CeilingPolicy);
        let ceiling = current.ceiling();
        let caps = CapSet::from_caps([cap.clone()]);
        if ceiling.clamp(&caps).0 != caps {
            return Err("selected read is outside the installed package's authority ceiling".into());
        }
        crate::approvals::app_policy::require(self.owner(), self.launch.app_id(), &caps)?;
        Ok(cap)
    }

    fn require_delegation(&self) -> Result<(), String> {
        if self.optional_approval_remaining()?.is_some_and(|remaining| remaining.is_zero()) {
            return Err("GUI optional approval expired; fresh registration is required".into());
        }
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
                if self
                    .caps
                    .iter()
                    .filter(|cap| self.delegation.inherited_ceiling.covers(cap))
                    .any(|cap| !current.caps.covers(cap))
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

    fn optional_approval_remaining(&self) -> Result<Option<std::time::Duration>, String> {
        let Some(expiry) = self.optional_approval_expiry else {
            return Ok(None);
        };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| format!("GUI approval clock: {error}"))?;
        Ok(Some(std::time::Duration::from_secs(expiry).saturating_sub(now)))
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
        crate::clawd::trust_withdrawal::check_package(self.owner(), self.launch.package())?;
        let selected = self.launch.manifest().gui_entry(self.gui_entry.as_deref())?;
        if selected.entry != self.entry || selected.exec != self.selector {
            return Err("GUI target differs from the Root-held registration".into());
        }
        self.launch.gui_entrypoint(self.gui_entry.as_deref())?;
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
        crate::clawd::trust_withdrawal::check_package(self.owner(), self.launch.package())?;
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
        crate::clawd::trust_withdrawal::check_session(self.owner(), &self.session_id)?;
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
        let mut view = authority::authority()
            .consume(
                view.id,
                &view.caps.iter().cloned().collect::<Vec<_>>(),
                &presentation,
            )
            .map_err(|error| error.to_string())?;
        if let Some(remaining) = self.optional_approval_remaining()? {
            view.expires_in = view.expires_in.min(remaining);
        }
        Ok(view)
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
    if matches!(command, super::Command::GuiFileReadRequest | super::Command::GuiFileReadStatus
        | super::Command::GuiFileWriteRequest | super::Command::GuiFileWriteStatus
        | super::Command::GuiFileSaveReserve
        | super::Command::GuiFileCreateRequest | super::Command::GuiFileCreateStatus
        | super::Command::GuiFileCreateReserve) {
        return Ok(command.route());
    }
    if matches!(command, super::Command::GuiPackageIntent | super::Command::GuiCaptureControl
        | super::Command::GuiMediaExchange | super::Command::GuiAudioControl
        | super::Command::GuiFilePicker | super::Command::GuiNotificationExchange
        | super::Command::GuiDesktopLaunch | super::Command::GuiPowerReview
        | super::Command::GuiDesktopLock) {
        return Ok(command.route());
    }
    super::relayable_route(command)
}
