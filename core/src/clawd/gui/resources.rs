//! Window-bound owner consent and custody for the filtered Home metadata view.

use std::fs::File;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use claw_display_control::{Epoch, InstanceId, ProcessIdentity};
use sha2::{Digest, Sha256};

use crate::approvals::ConsumedGrant;
use crate::caps::{manifest::ScopeBinding, Cap, CapSet, Scope, Verb};
use crate::clawd::app_sessions::gui::Registration;
use crate::clawd::authority::GrantView;
use crate::clawd::protocol::BrokerError;
use crate::display_session::runtime::{Runtime, Socket};
use crate::worker::native_fs::NativeFsView;
use crate::worker::{LaunchPolicy, LaunchResources, Mount, MountClass};

struct Declaration {
    cap: Cap,
    digest: String,
    identity: (u64, u64),
}

fn declares_home_metadata(selected: &crate::caps::manifest::GuiEntry<'_>, scope: &Scope) -> bool {
    !selected.legacy_needs && selected.launch_needs().any(|need| {
        need.verb == Verb::FS_META && match &need.scope {
            ScopeBinding::OwnerPath { path } => path == "**",
            ScopeBinding::Fixed { scope: declared } => declared == scope,
            _ => false,
        }
    })
}

fn beneath_home(scope: &Scope, home: &Path) -> bool {
    matches!(scope, Scope::Path(path)
        if Path::new(path.strip_suffix("/**").unwrap_or(path)).starts_with(home))
}

fn refuse_unsupported_home_needs(
    selected: &crate::caps::manifest::GuiEntry<'_>, home: &Path, caps: &CapSet,
) -> Result<(), String> {
    for need in selected.launch_needs().filter(|need|
        matches!(need.verb, Verb::FS_DELETE | Verb::FS_WATCH | Verb::FS_EXEC))
    {
        let scopes = match &need.scope {
            ScopeBinding::OwnerPath { path } => vec![crate::caps::manifest::resolve_owner_path(
                need.verb, path, &crate::caps::args::PathContext { home: home.to_path_buf(), cwd: None })?],
            ScopeBinding::Fixed { scope } => vec![scope.clone()],
            ScopeBinding::Wild => caps.iter().filter(|cap| cap.verb == need.verb)
                .map(|cap| cap.scope.clone()).collect(),
            _ => continue,
        };
        if scopes.iter().any(|scope| scope.is_wildcard() || beneath_home(scope, home)) {
            return Err(format!("mandatory Home {} requires native resource integration that is not available", need.verb.as_str()));
        }
    }
    Ok(())
}

fn home_scope(registration: &Registration) -> Result<Scope, String> {
    let home = registration
        .home
        .to_str()
        .ok_or("authenticated GUI Home is not UTF-8")?;
    if !registration.home.is_absolute()
        || registration
            .home
            .canonicalize()
            .map_err(|error| error.to_string())?
            != registration.home
    {
        return Err("GUI metadata requires the canonical authenticated owner Home".into());
    }
    Ok(Scope::path(format!("{}/**", home.trim_end_matches('/'))))
}

fn declaration(
    registration: &Registration,
    instance: InstanceId,
) -> Result<Option<Declaration>, String> {
    let scope = home_scope(registration)?;
    let cap = Cap::new(Verb::FS_META, scope.clone());
    if !registration.caps.iter().any(|granted| granted == &cap) {
        let bare = Cap::new(
            Verb::FS_META,
            Scope::path(
                registration
                    .home
                    .to_str()
                    .ok_or("authenticated GUI Home is not UTF-8")?,
            ),
        );
        if registration.caps.iter().any(|granted| granted == &bare) {
            let selected = registration
                .launch
                .manifest()
                .gui_entry(registration.gui_entry.as_deref())?;
            if selected.legacy_needs || selected.launch_needs().any(|need| {
                need.verb == Verb::FS_META
                    && matches!(&need.scope, ScopeBinding::Fixed { scope } if scope == &bare.scope)
            }) {
                return Err(
                    "a bare Home scope cannot authorize recursive metadata browsing".into(),
                );
            }
        }
        return Ok(None);
    }
    let selected = registration
        .launch
        .manifest()
        .gui_entry(registration.gui_entry.as_deref())?;
    let explicit = declares_home_metadata(&selected, &scope);
    if !explicit {
        if !selected.legacy_needs
            && selected.optional_needs.iter().any(|need| {
                need.verb == Verb::FS_META
                    && need.when.is_none()
                    && match &need.scope {
                        ScopeBinding::OwnerPath { path } => path == "**",
                        ScopeBinding::Fixed { scope: declared } => declared == &scope,
                        _ => false,
                    }
            })
        {
            // Optional startup authority is not a request for a new prompt.
            return Ok(None);
        }
        return Err(
            "Home metadata requires an explicit bound need of this signed GUI surface".into(),
        );
    }
    let binding = registration.launch.bind(&[selected.entry.to_string()])?;
    if binding.is_developer() {
        return Err("Home metadata requires a signed installed GUI surface".into());
    }
    refuse_unsupported_home_needs(&selected, &registration.home, &registration.caps)?;
    let metadata = {
        let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(registration.owner())?;
        std::fs::metadata(&registration.home).map_err(|error| error.to_string())?
    };
    if !metadata.is_dir() || metadata.uid() != registration.owner() {
        return Err("GUI Home metadata source does not belong to its authenticated owner".into());
    }
    let identity = (metadata.dev(), metadata.ino());
    let material = serde_json::to_vec(&serde_json::json!([
        "gui.home-metadata.v1",
        registration.owner(),
        registration.session_id,
        Epoch(instance.0).label(),
        registration.authority_alias.label(),
        registration.control.epoch.label(),
        registration.gui_entry,
        selected.entry,
        registration.selector,
        registration.home,
        identity,
        registration.launcher.pid(),
        registration.launcher.start_ticks(),
    ]))
    .map_err(|error| error.to_string())?;
    Ok(Some(Declaration {
        cap,
        digest: hex::encode(Sha256::digest(material)),
        identity,
    }))
}

/// Run before the manager changes Reserved to Starting. No grant is consumed
/// until the same immutable registration actually prepares its one window.
pub(super) fn admit(registration: &Registration, instance: InstanceId) -> Result<(), BrokerError> {
    let Some(declaration) =
        declaration(registration, instance).map_err(BrokerError::authorization)?
    else {
        return Ok(());
    };
    let owner = Some(registration.owner());
    let approved = crate::approvals::has_approved_grant_for_context_operation(
        &registration.session_id,
        &declaration.cap,
        owner,
        None,
        Some(&declaration.digest),
    )
    .map_err(BrokerError::unavailable)?;
    if approved {
        return Ok(());
    }
    let pending = crate::approvals::list_pending_for_owner(owner)
        .into_iter()
        .find(|request| {
            request.session == registration.session_id
                && request.verb == declaration.cap.verb.as_str()
                && request.scope == declaration.cap.scope
                && request.operation_digest.as_deref() == Some(declaration.digest.as_str())
        });
    let id = match pending {
        Some(request) => request.id,
        None => crate::approvals::submit_owned_with_context_for_operation(
            declaration.cap.verb, declaration.cap.scope, registration.session_id.clone(),
            "Browse Home names and metadata for this GUI window; no file content read, write, delete or execution",
            Some(format!("GUI {} {} window {}", registration.launch.app_id(),
                registration.selector, Epoch(instance.0).label())),
            owner, None, Some(&declaration.digest),
        ).map_err(BrokerError::unavailable)?,
    };
    Err(BrokerError::authorization_required(
        "this GUI window requires protected owner consent to browse Home metadata",
        serde_json::json!({ "status":"approval_required", "approval_requests":[id] }),
    )
    .classified("approval_required"))
}

pub(super) struct Consent {
    declaration: Declaration,
    approval: ConsumedGrant,
    ceiling: Instant,
}

pub(super) fn consume(
    registration: &Registration,
    instance: InstanceId,
) -> Result<Option<Consent>, String> {
    let Some(declaration) = declaration(registration, instance)? else {
        return Ok(None);
    };
    let approval = crate::approvals::redeem_matching_grant_for_owner_operation(
        &registration.session_id,
        declaration.cap.verb,
        &declaration.cap.scope,
        Some(registration.owner()),
        None,
        Some(&declaration.digest),
        true,
    )?
    .ok_or("this GUI window has no exact protected Home metadata approval")?;
    let lifetime = approval.expires_in();
    if lifetime.is_zero() {
        return Err("Home metadata consent is already expired".into());
    }
    let ceiling = Instant::now()
        .checked_add(lifetime)
        .ok_or("Home metadata consent deadline overflow")?;
    Ok(Some(Consent {
        declaration,
        approval,
        ceiling,
    }))
}

pub(super) fn sandbox_caps(registration: &Registration, metadata_view: bool) -> Result<CapSet, String> {
    home_scope(registration)?;
    raw_mount_caps(&registration.home, &registration.caps, metadata_view)
}

fn raw_mount_caps(home: &Path, caps: &CapSet, metadata_view: bool) -> Result<CapSet, String> {
    let text = home.to_str().ok_or("GUI Home is not UTF-8")?;
    let scope = Scope::path(format!("{}/**", text.trim_end_matches('/')));
    let bare = Scope::path(text);
    Ok(CapSet::from_caps(
        caps
            .iter()
            .filter(|cap| !(cap.verb == Verb::FS_META && (cap.scope == scope || cap.scope == bare)))
            .filter(|cap| !(metadata_view && matches!(cap.verb,
                Verb::FS_META | Verb::FS_READ | Verb::FS_WRITE | Verb::FS_DELETE | Verb::FS_WATCH | Verb::FS_EXEC)
                && beneath_home(&cap.scope, home)))
            .cloned(),
    ))
}

/// Resource projection only: neither a declaration nor successful validation
/// supplies the separate owner/window consent required before FUSE admission.
pub(super) fn validate_optional_mounts(
    app: &crate::apps::App, entry: Option<&str>, home: &Path, caps: &CapSet,
) -> Result<(), String> {
    let selected = app.manifest.gui_entry(entry)?;
    let scope = Scope::path(format!("{}/**", home.to_str().ok_or("GUI Home is not UTF-8")?.trim_end_matches('/')));
    let controlled = declares_home_metadata(&selected, &scope)
        && caps.iter().any(|cap| cap == &Cap::new(Verb::FS_META, scope.clone()));
    if controlled { refuse_unsupported_home_needs(&selected, home, caps)?; }
    // Missing paths may resolve to no mounts; eligibility still binds the scope.
    if !controlled && caps.iter().any(|cap| {
        matches!(cap.verb, Verb::FS_META | Verb::FS_READ | Verb::FS_WRITE | Verb::FS_DELETE | Verb::FS_WATCH | Verb::FS_EXEC)
            && matches!(&cap.scope, Scope::Path(path) if Path::new(path) == home || cap.scope.covers(&scope))
    }) {
        return Err("GUI Home resources require the filtered view, never a raw Home bind".into());
    }
    let projected = if controlled { raw_mount_caps(home, caps, true)? } else { caps.clone() };
    let mounts = crate::worker::derive::granted_path_mounts(&projected)?;
    if mounts.iter().any(|mount| {
        matches!(mount.class, MountClass::Input | MountClass::Output)
            && home.starts_with(&mount.target)
    }) {
        return Err("GUI Home resources require the filtered view, never a raw Home bind".into());
    }
    Ok(())
}

pub(super) fn refuse_raw_home(
    policy: &LaunchPolicy,
    home: &Path,
    metadata: bool,
) -> Result<(), String> {
    if policy.mounts.iter().any(|mount| {
        matches!(mount.class, MountClass::Input | MountClass::Output)
            && (home.starts_with(&mount.target) || metadata && mount.target.starts_with(home))
    }) {
        return Err("GUI Home resources require the filtered view and exact selected-file consent, never a raw Home bind".into());
    }
    Ok(())
}

pub(super) struct HomeMetadata {
    consent: Consent,
    path: PathBuf,
    directory: Option<File>,
    identity: Option<(u64, u64)>,
    view: Option<NativeFsView>,
    selected_reads: OnceLock<Arc<crate::clawd::gui::selected_read::SelectedReads>>,
    created: bool,
    removed: bool,
}

impl HomeMetadata {
    pub(super) fn reserve(consent: Consent, runtime: &Runtime) -> Result<Self, String> {
        let root = runtime.socket(Socket::Broker);
        let path = root
            .parent()
            .ok_or("GUI runtime has no parent")?
            .join("home-metadata");
        Ok(Self {
            consent,
            path,
            directory: None,
            identity: None,
            view: None,
            selected_reads: OnceLock::new(),
            created: false,
            removed: false,
        })
    }

    pub(super) fn mount(&mut self, registration: &Registration) -> Result<(), String> {
        self.check(registration)?;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&self.path)
            .map_err(|error| error.to_string())?;
        self.created = true;
        self.directory = Some(File::open(&self.path).map_err(|error| error.to_string())?);
        let metadata = self
            .directory
            .as_ref()
            .ok_or("Home metadata directory was not retained")?
            .metadata()
            .map_err(|error| error.to_string())?;
        self.identity = Some((metadata.dev(), metadata.ino()));
        self.view = Some(NativeFsView::create_home_metadata(
            registration.owner(),
            &registration.home,
            &self.path,
            self.consent.declaration.identity,
            self.consent.ceiling,
        )?);
        Ok(())
    }

    pub(super) fn attach(
        &self,
        policy: &mut LaunchPolicy,
        registration: &Registration,
    ) -> Result<(), String> {
        if self.view.is_none() {
            return Err("GUI Home metadata provider is not mounted".into());
        }
        refuse_raw_home(policy, &registration.home, true)?;
        // Home must precede the already-private nested App data bind.
        policy.mounts.insert(
            0,
            Mount::read_write(&self.path, &registration.home, MountClass::Output),
        );
        Ok(())
    }

    fn check(&self, registration: &Registration) -> Result<(), String> {
        let authorization = &self.consent.approval.authorization;
        let generation = crate::approvals::generations::current(
            Some(registration.owner()),
            &registration.session_id,
        )?;
        if generation != self.consent.approval.generation
            || authorization.owner_uid != Some(registration.owner())
            || authorization.session != registration.session_id
            || authorization.capability != self.consent.declaration.cap
            || authorization.operation_digest.as_deref()
                != Some(self.consent.declaration.digest.as_str())
            || authorization.context.is_some()
            || authorization.execution.is_some()
            || Instant::now() >= self.consent.ceiling
            || self.consent.approval.expires_in().is_zero()
            || !registration.caps.covers(&self.consent.declaration.cap)
        {
            return Err("this window's Home metadata consent expired or was revoked".into());
        }
        Ok(())
    }

    pub(super) fn bind(
        &self,
        registration: &Arc<Registration>,
        instance: InstanceId,
        process: &ProcessIdentity,
        resources: &LaunchResources,
        grant: &GrantView,
    ) -> Result<Arc<crate::clawd::gui::selected_read::SelectedReads>, String> {
        self.check(registration)?;
        let view = self
            .view
            .as_ref()
            .ok_or("GUI Home metadata provider is unavailable")?;
        view.bind(process, resources.gui_cgroup()?)?;
        self.refresh(registration, grant)?;
        let selected = crate::clawd::gui::selected_read::SelectedReads::new(
            registration.clone(), instance, process.clone(), resources.gui_cgroup()?,
            view.read_provider()?, self.consent.ceiling, grant,
        )?;
        self.selected_reads.set(selected.clone()).map_err(|_| "selected read endpoint already bound")?;
        Ok(selected)
    }

    pub(super) fn refresh(
        &self,
        registration: &Registration,
        grant: &GrantView,
    ) -> Result<(), String> {
        self.check(registration)?;
        if !grant.caps.covers(&self.consent.declaration.cap) {
            return Err("live GUI authority no longer authorizes Home metadata".into());
        }
        let view = self
            .view
            .as_ref()
            .ok_or("GUI Home metadata provider is unavailable")?;
        view.health()?;
        view.renew_home_metadata(grant.expires_in.min(Duration::from_secs(2)))
    }

    pub(super) fn stop(&self) -> Result<(), String> {
        if let Some(selected) = self.selected_reads.get() { selected.stop(); }
        if let Some(view) = &self.view {
            view.revoke()?;
        }
        Ok(())
    }

    pub(super) fn health(&self) -> Result<(), String> {
        if let Some(selected) = self.selected_reads.get() { selected.health()?; }
        self.view
            .as_ref()
            .ok_or("GUI Home metadata provider is unavailable")?
            .health()
    }

    pub(super) fn retire(&mut self, deadline: Instant) -> Result<(), String> {
        if let Some(selected) = self.selected_reads.get() {
            selected.stop();
            selected.release_after_death(deadline)?;
        }
        if let Some(view) = &mut self.view {
            view.retire_checked(deadline)?;
        }
        NativeFsView::retire_pending_metadata(deadline)?;
        self.view.take();
        if let Some(selected) = self.selected_reads.get() { selected.finish_mutation_session()?; }
        if self.created && !self.removed {
            if self.directory.is_none() {
                crate::provenance::fsec::require_secure_location(&self.path, &[0])
                    .map_err(|error| error.to_string())?;
                self.directory = Some(File::open(&self.path).map_err(|error| error.to_string())?);
            }
            let directory = self
                .directory
                .as_ref()
                .ok_or("Home metadata directory custody is unavailable")?;
            let retained = directory.metadata().map_err(|error| error.to_string())?;
            let expected = *self
                .identity
                .get_or_insert((retained.dev(), retained.ino()));
            if retained.nlink() == 0 {
                self.removed = true;
                return Ok(());
            }
            let current =
                std::fs::symlink_metadata(&self.path).map_err(|error| error.to_string())?;
            if (current.dev(), current.ino()) != expected || !current.is_dir() || current.uid() != 0
            {
                return Err(
                    "GUI Home metadata directory identity changed; custody retained".into(),
                );
            }
            std::fs::remove_dir(&self.path).map_err(|error| error.to_string())?;
            if directory
                .metadata()
                .map_err(|error| error.to_string())?
                .nlink()
                != 0
            {
                return Err("GUI Home metadata directory remains linked".into());
            }
            self.removed = true;
        }
        Ok(())
    }
}
