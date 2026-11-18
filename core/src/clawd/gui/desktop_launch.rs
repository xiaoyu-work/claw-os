//! One reviewed, exact signed App launch at a time for a bound desktop GUI.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use claw_display_control::{Epoch, ProcessIdentity, wire::LaunchTarget};
use serde_json::{Value, json};

use crate::approvals::{self, DESKTOP_LAUNCH_APPROVAL, GrantDuration, RequestStatus};
use crate::caps::manifest::ScopeBinding;
use crate::caps::{Cap, CapSet, Scope, Verb};
use crate::clawd::app_sessions::gui::Registration;
use crate::clawd::authority::GrantView;
use crate::clawd::wire::requests::DesktopMenuLaunch;
use crate::provenance::{PackageKind, TrustSource, VerifyOptions};

const REVIEW_LIFETIME: Duration = Duration::from_secs(120);
const INSTALLED_APPS: &str = "/usr/lib/cos/apps";

struct Review {
    id: String,
    session: String,
    digest: String,
    desktop_id: String,
    target: LaunchTarget,
    deadline: Instant,
    grant: crate::clawd::authority::GrantId,
    generation: u64,
}

pub(super) struct Window {
    registration: Arc<Registration>,
    review: Mutex<Option<Review>>,
    closed: AtomicBool,
}

impl Window {
    pub(super) fn new(registration: Arc<Registration>) -> Self {
        Self {
            registration,
            review: Mutex::new(None),
            closed: AtomicBool::new(false),
        }
    }

    pub(super) fn stop(&self) {
        self.closed.store(true, Ordering::Release);
    }

    pub(super) fn retire(&self) -> Result<(), String> {
        let mut review = self
            .review
            .lock()
            .map_err(|_| "desktop launch review lock poisoned")?;
        self.retire_review(&mut review)
    }

    fn retire_review(&self, review: &mut Option<Review>) -> Result<(), String> {
        if let Some(record) = review {
            approvals::retire_operation_decision(
                self.registration.owner(),
                &record.session,
                &record.id,
                &record.digest,
                "desktop menu request has ended",
            )?;
            review.take();
        }
        Ok(())
    }

    fn live(&self, process: &ProcessIdentity, desktop_id: &str) -> Result<GrantView, String> {
        if self.closed.load(Ordering::Acquire) {
            return Err("the desktop launch window has closed".into());
        }
        let view = self.registration.snapshot(process)?;
        if view.subject.task_id.is_some()
            || view.subject.activity.is_some()
            || view.subject.workspace.is_some()
        {
            return Err("desktop menu launches require an independent interactive desktop".into());
        }
        let launch = &self.registration.launch;
        if !matches!(launch.package().source(), TrustSource::Publisher { .. }) {
            return Err("desktop launch requests require a signed GUI".into());
        }
        let cap = capability(desktop_id);
        let selected = launch
            .manifest()
            .gui_entry(self.registration.gui_entry.as_deref())?;
        if !selected.optional_needs.iter().any(|need| {
            need.verb == cap.verb
                && need.when.is_none()
                && match &need.scope {
                    ScopeBinding::Wild => true,
                    ScopeBinding::Fixed { scope } => scope == &cap.scope,
                    _ => false,
                }
        }) {
            return Err(
                "this signed GUI does not declare desktop launch requests for that AppID".into(),
            );
        }
        let caps = CapSet::from_caps([cap]);
        if launch.ceiling().clamp(&caps).0 != caps {
            return Err("desktop launch is outside the requesting App's authority ceiling".into());
        }
        crate::approvals::app_policy::require(self.registration.owner(), launch.app_id(), &caps)?;
        Ok(view)
    }

    pub(super) fn request(
        &self,
        process: &ProcessIdentity,
        request: DesktopMenuLaunch,
    ) -> Result<Value, String> {
        if request.session.as_str() != self.registration.session_id {
            return Err("desktop launch request belongs to another GUI".into());
        }
        let mut review = self
            .review
            .lock()
            .map_err(|_| "desktop launch review lock poisoned")?;
        if self.closed.load(Ordering::Acquire) {
            return Err("the desktop launch window has closed".into());
        }
        match (request.action.as_str(), request.app_id, request.review_id) {
            ("request", Some(desktop_id), None) => {
                let desktop_id = desktop_id.as_str();
                if desktop_id.is_empty()
                    || desktop_id.len() > 255
                    || !desktop_id
                        .bytes()
                        .next()
                        .is_some_and(|b| b.is_ascii_alphanumeric())
                    || !desktop_id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
                {
                    return Err("invalid desktop AppID".into());
                }
                if review
                    .as_ref()
                    .is_some_and(|r| Instant::now() >= r.deadline)
                {
                    self.retire_review(&mut review)?;
                }
                if review.is_some() {
                    return Err(
                        "another desktop launch is awaiting a decision in this window".into(),
                    );
                }
                let view = self.live(process, desktop_id)?;
                let target = resolve_target(desktop_id)?;
                let nonce = Epoch::random().map_err(|e| e.to_string())?.label();
                let digest = crate::crypto::sha256_hex(
                    &serde_json::to_vec(&(
                        "desktop-menu-launch",
                        &nonce,
                        self.registration.owner(),
                        &self.registration.session_id,
                        self.registration.control.epoch,
                        desktop_id,
                        &target,
                    ))
                    .map_err(|error| error.to_string())?,
                );
                let session = format!("{DESKTOP_LAUNCH_APPROVAL}{nonce}");
                let id = approvals::submit_owned_with_context_for_operation(
                    Verb::DESKTOP_LAUNCH,
                    Scope::name(desktop_id),
                    session.clone(),
                    format!("Open signed App {} ({desktop_id}) once", target.app_id),
                    Some(format!(
                        "App {} / GUI {}",
                        self.registration.launch.app_id(),
                        self.registration.session_id
                    )),
                    Some(self.registration.owner()),
                    None,
                    Some(&digest),
                )?;
                *review = Some(Review {
                    id: id.clone(),
                    session,
                    digest,
                    desktop_id: desktop_id.to_owned(),
                    target,
                    deadline: Instant::now() + REVIEW_LIFETIME.min(view.expires_in),
                    grant: view.id,
                    generation: view.generation,
                });
                Ok(json!({"status": "pending", "review_id": id}))
            }
            ("complete" | "cancel", None, Some(id)) => {
                let record = review
                    .as_ref()
                    .filter(|r| r.id == id.as_str())
                    .ok_or("no matching desktop menu request is retained by this window")?;
                if request.action.as_str() == "cancel" || Instant::now() >= record.deadline {
                    self.retire_review(&mut review)?;
                    return Ok(json!({"status": "cancelled"}));
                }
                let view = self.live(process, &record.desktop_id)?;
                if view.id != record.grant || view.generation != record.generation {
                    return Err("the requesting GUI authority changed".into());
                }
                let owner = self.registration.owner();
                match approvals::status_for_owner(&record.id, Some(owner)) {
                    RequestStatus::Pending | RequestStatus::Resolving => {
                        return Ok(json!({"status": "pending"}));
                    }
                    RequestStatus::Denied => {
                        self.retire_review(&mut review)?;
                        return Ok(json!({"status": "denied"}));
                    }
                    RequestStatus::Approved => {}
                    _ => return Err("the desktop launch decision is no longer available".into()),
                }
                let installed = resolve_target(&record.desktop_id)?;
                if installed.app_id != record.target.app_id
                    || installed.digest != record.target.digest
                    || installed.publisher != record.target.publisher
                {
                    self.retire_review(&mut review)?;
                    return Err("the signed desktop launch target changed; select it again".into());
                }
                let evidence = approvals::consume_grant_set_once_for_owner_operation_evidence(
                    &record.session,
                    &[capability(&record.desktop_id)],
                    Some(owner),
                    Some(&record.digest),
                )?
                .ok_or("the exact desktop launch confirmation expired or was revoked")?;
                let [approval] = evidence.as_slice() else {
                    return Err("desktop launch approval binding is invalid".into());
                };
                let record = review.take().expect("checked review retained under lock");
                let current = self.live(process, &record.desktop_id)?;
                if approval.duration != GrantDuration::Once
                    || approvals::generations::current(Some(owner), &record.session)?
                        != approval.generation
                    || current.id != record.grant
                    || current.generation != record.generation
                    || Instant::now() >= record.deadline
                {
                    return Err("desktop launch confirmation is no longer live".into());
                }
                let deadline = record.deadline.min(Instant::now() + Duration::from_secs(2));
                let result = self.registration.control.launch(installed, deadline);
                crate::provenance::audit(
                    "desktop.menu_launch",
                    json!({
                        "owner_uid": owner,
                        "source_app": self.registration.launch.app_id(),
                        "epoch": self.registration.control.epoch.label(),
                        "review_id": record.id,
                        "desktop_id": record.desktop_id,
                        "app_id": record.target.app_id,
                        "content_digest": record.target.digest,
                        "submitted": result.is_ok(),
                    }),
                );
                result.map_err(|error| {
                    format!("desktop launch was not confirmed; do not retry automatically: {error}")
                })?;
                Ok(json!({"status": "submitted"}))
            }
            _ => {
                Err("expected request with one AppID, or complete/cancel with one review ID".into())
            }
        }
    }
}

fn capability(id: &str) -> Cap {
    Cap::new(Verb::DESKTOP_LAUNCH, Scope::name(id))
}

fn resolve_target(desktop_id: &str) -> Result<LaunchTarget, String> {
    let resource = format!("resources/usr/share/applications/{desktop_id}.desktop");
    let root = Path::new(INSTALLED_APPS);
    let trust = crate::provenance::trust_store();
    let mut found = None;
    for (index, entry) in std::fs::read_dir(root)
        .map_err(|error| error.to_string())?
        .enumerate()
    {
        if index >= 512 {
            return Err("the installed desktop catalogue exceeds its bound".into());
        }
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir()
            || !entry.path().join(&resource).is_file()
        {
            continue;
        }
        let id = entry
            .file_name()
            .into_string()
            .map_err(|_| "invalid installed AppID")?;
        let package = crate::provenance::verify::verify_package(
            &entry.path(),
            &VerifyOptions::new(PackageKind::App).expect_id(&id),
            &trust,
        )
        .map_err(|error| error.to_string())?;
        let TrustSource::Publisher { key_id } = package.source() else {
            return Err("the selected desktop entry is not publisher-signed".into());
        };
        if !package.resources().iter().any(|path| path == &resource) {
            continue;
        }
        if package
            .manifest_text()
            .ok()
            .and_then(|text| crate::caps::manifest::Manifest::from_json(&text).ok())
            .is_none_or(|manifest| manifest.id != id || manifest.gui_entry(None).is_err())
        {
            return Err("the selected signed App has no authenticated default GUI".into());
        }
        if found.is_some() {
            return Err("more than one signed App owns the selected desktop entry".into());
        }
        let target = LaunchTarget {
            app_id: id,
            digest: package.content_digest().to_owned(),
            publisher: key_id.to_owned(),
        };
        target.validate().map_err(|error| error.to_string())?;
        found = Some(target);
    }
    found.ok_or_else(|| "no installed signed Claw App owns that desktop entry".into())
}
