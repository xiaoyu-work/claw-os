//! One reviewed desktop effect at a time for a bound GUI.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use claw_display_control::{Epoch, ProcessIdentity, wire::LaunchTarget};
use serde::Serialize;
use serde_json::{Value, json};

use crate::approvals::{
    self, DESKTOP_LAUNCH_APPROVAL, DESKTOP_POWER_APPROVAL, GrantDuration, RequestStatus,
};
use crate::caps::manifest::ScopeBinding;
use crate::caps::{Cap, CapSet, Scope, Verb};
use crate::clawd::app_sessions::gui::Registration;
use crate::clawd::authority::GrantView;
use crate::clawd::{power, protocol::BrokerError};
use crate::paths::RoutedPathContext;
use crate::provenance::{PackageKind, TrustSource, VerifyOptions};

const REVIEW_LIFETIME: Duration = Duration::from_secs(120);
const INSTALLED_APPS: &str = "/usr/lib/cos/apps";

struct Review {
    id: String,
    session: String,
    digest: String,
    effect: Effect,
    deadline: Instant,
    grant: crate::clawd::authority::GrantId,
    generation: u64,
}

#[derive(Serialize)]
enum Effect {
    Launch {
        desktop_id: String,
        target: LaunchTarget,
    },
    Power(String),
}

impl Effect {
    fn capability(&self) -> Cap {
        match self {
            Self::Launch { desktop_id, .. } => {
                Cap::new(Verb::DESKTOP_LAUNCH, Scope::name(desktop_id))
            }
            Self::Power(_) => Cap::new(Verb::SYS_POWER, Scope::Wild),
        }
    }

    fn review(&self) -> (&'static str, String) {
        match self {
            Self::Launch { desktop_id, target } => (
                DESKTOP_LAUNCH_APPROVAL,
                format!("Open signed App {} ({desktop_id}) once", target.app_id),
            ),
            Self::Power(action) => (
                DESKTOP_POWER_APPROVAL,
                format!("Request system {action} once"),
            ),
        }
    }
}

pub(super) enum Reply {
    Message(Value),
    Power(PowerDispatch),
}

fn status(state: &'static str) -> Reply {
    Reply::Message(json!({"status": state}))
}

pub(super) struct PowerDispatch {
    review: Review,
    generation: u32,
}

impl PowerDispatch {
    pub(super) async fn dispatch(
        self,
        window: Arc<Window>,
        process: ProcessIdentity,
        context: RoutedPathContext,
    ) -> Result<Value, BrokerError> {
        let Effect::Power(action) = &self.review.effect else {
            return Err(BrokerError::authorization(
                "expected a reviewed power effect",
            ));
        };
        let action = action.clone();
        let id = self.review.id.clone();
        let owner = window.registration.owner();
        let session = window.registration.session_id.clone();
        power::request_power_action(&action, owner, &session, Some(&id), move || {
            context
                .scope_sync(|| {
                    window.require_live_confirmation(&process, &self.review, self.generation)
                })
                .map_err(BrokerError::authorization)
        })
        .await?;
        Ok(json!({"status": "submitted"}))
    }
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
            .map_err(|_| "desktop review lock poisoned")?;
        self.retire_review(&mut review)
    }

    fn retire_review(&self, review: &mut Option<Review>) -> Result<(), String> {
        if let Some(record) = review {
            approvals::retire_operation_decision(
                self.registration.owner(),
                &record.session,
                &record.id,
                &record.digest,
                "desktop request has ended",
            )?;
            review.take();
        }
        Ok(())
    }

    fn live(&self, process: &ProcessIdentity, cap: &Cap) -> Result<GrantView, String> {
        if self.closed.load(Ordering::Acquire) {
            return Err("the desktop review window has closed".into());
        }
        let view = self.registration.snapshot(process)?;
        if view.subject.task_id.is_some()
            || view.subject.activity.is_some()
            || view.subject.workspace.is_some()
        {
            return Err("desktop reviews require an independent interactive desktop".into());
        }
        let launch = &self.registration.launch;
        if !matches!(launch.package().source(), TrustSource::Publisher { .. }) {
            return Err("desktop requests require a signed GUI".into());
        }
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
            return Err("this signed GUI does not declare the requested desktop action".into());
        }
        let caps = CapSet::from_caps([cap.clone()]);
        if launch.ceiling().clamp(&caps).0 != caps {
            return Err("the action is outside the requesting App's authority ceiling".into());
        }
        crate::approvals::app_policy::require(self.registration.owner(), launch.app_id(), &caps)?;
        Ok(view)
    }

    pub(super) fn request(
        &self,
        process: &ProcessIdentity,
        verb: Verb,
        session: &str,
        action: &str,
        selected: Option<&str>,
        review_id: Option<&str>,
    ) -> Result<Reply, String> {
        if session != self.registration.session_id {
            return Err("desktop request belongs to another GUI".into());
        }
        let mut review = self
            .review
            .lock()
            .map_err(|_| "desktop review lock poisoned")?;
        if self.closed.load(Ordering::Acquire) {
            return Err("the desktop review window has closed".into());
        }
        match (action, selected, review_id) {
            ("request", Some(selection), None) => {
                let cap = selected_capability(verb, selection)?;
                if review
                    .as_ref()
                    .is_some_and(|r| Instant::now() >= r.deadline)
                {
                    self.retire_review(&mut review)?;
                }
                if review.is_some() {
                    return Err(
                        "another desktop action is awaiting a decision in this window".into(),
                    );
                }
                let view = self.live(process, &cap)?;
                let effect = if verb == Verb::DESKTOP_LAUNCH {
                    Effect::Launch {
                        desktop_id: selection.to_owned(),
                        target: resolve_target(selection)?,
                    }
                } else {
                    Effect::Power(selection.to_owned())
                };
                let (prefix, reason) = effect.review();
                let nonce = Epoch::random().map_err(|e| e.to_string())?.label();
                let digest = crate::crypto::sha256_hex(
                    &serde_json::to_vec(&(
                        prefix,
                        &nonce,
                        self.registration.owner(),
                        &self.registration.session_id,
                        self.registration.control.epoch,
                        &effect,
                    ))
                    .map_err(|error| error.to_string())?,
                );
                let session = format!("{prefix}{nonce}");
                let id = approvals::submit_owned_with_context_for_operation(
                    cap.verb,
                    cap.scope,
                    session.clone(),
                    reason,
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
                    effect,
                    deadline: Instant::now() + REVIEW_LIFETIME.min(view.expires_in),
                    grant: view.id,
                    generation: view.generation,
                });
                Ok(Reply::Message(
                    json!({"status": "pending", "review_id": id}),
                ))
            }
            ("complete" | "cancel", None, Some(id)) => {
                let record = review
                    .as_ref()
                    .filter(|r| r.id == id && r.effect.capability().verb == verb)
                    .ok_or("no matching desktop request is retained by this window")?;
                if action == "cancel" || Instant::now() >= record.deadline {
                    self.retire_review(&mut review)?;
                    return Ok(status("cancelled"));
                }
                let cap = record.effect.capability();
                let view = self.live(process, &cap)?;
                if view.id != record.grant || view.generation != record.generation {
                    return Err("the requesting GUI authority changed".into());
                }
                let owner = self.registration.owner();
                match approvals::status_for_owner(&record.id, Some(owner)) {
                    RequestStatus::Pending | RequestStatus::Resolving => {
                        return Ok(status("pending"));
                    }
                    RequestStatus::Denied => {
                        self.retire_review(&mut review)?;
                        return Ok(status("denied"));
                    }
                    RequestStatus::Approved => {}
                    _ => return Err("the desktop decision is no longer available".into()),
                }
                let installed = if let Effect::Launch { desktop_id, target } = &record.effect {
                    let installed = resolve_target(desktop_id)?;
                    if installed.app_id != target.app_id
                        || installed.digest != target.digest
                        || installed.publisher != target.publisher
                    {
                        self.retire_review(&mut review)?;
                        return Err(
                            "the signed desktop launch target changed; select it again".into()
                        );
                    }
                    Some(installed)
                } else {
                    None
                };
                let evidence = approvals::consume_grant_set_once_for_owner_operation_evidence(
                    &record.session,
                    &[cap],
                    Some(owner),
                    Some(&record.digest),
                )?
                .ok_or("the exact desktop confirmation expired or was revoked")?;
                let [approval] = evidence.as_slice() else {
                    return Err("desktop approval binding is invalid".into());
                };
                let record = review.take().expect("checked review retained under lock");
                if approval.duration != GrantDuration::Once {
                    return Err("desktop confirmation must be single-use".into());
                }
                self.require_live_confirmation(process, &record, approval.generation)?;
                let (Some(installed), Effect::Launch { desktop_id, target }) =
                    (installed, &record.effect)
                else {
                    return Ok(Reply::Power(PowerDispatch {
                        review: record,
                        generation: approval.generation,
                    }));
                };
                let deadline = record.deadline.min(Instant::now() + Duration::from_secs(2));
                let result = self.registration.control.launch(installed, deadline);
                crate::provenance::audit(
                    "desktop.menu_launch",
                    json!({
                        "owner_uid": owner,
                        "source_app": self.registration.launch.app_id(),
                        "epoch": self.registration.control.epoch.label(),
                        "review_id": record.id,
                        "desktop_id": desktop_id,
                        "app_id": target.app_id,
                        "content_digest": target.digest,
                        "submitted": result.is_ok(),
                    }),
                );
                result.map_err(|error| {
                    format!("desktop launch was not confirmed; do not retry automatically: {error}")
                })?;
                Ok(status("submitted"))
            }
            _ => Err(
                "expected request with one selection, or complete/cancel with one review ID".into(),
            ),
        }
    }

    fn require_live_confirmation(
        &self,
        process: &ProcessIdentity,
        review: &Review,
        generation: u32,
    ) -> Result<(), String> {
        let view = self.live(process, &review.effect.capability())?;
        if self.closed.load(Ordering::Acquire)
            || approvals::generations::current(Some(self.registration.owner()), &review.session)?
                != generation
            || view.id != review.grant
            || view.generation != review.generation
            || Instant::now() >= review.deadline
        {
            return Err("desktop confirmation is no longer live".into());
        }
        Ok(())
    }
}

fn selected_capability(verb: Verb, selection: &str) -> Result<Cap, String> {
    match verb {
        Verb::DESKTOP_LAUNCH
            if selection.len() <= 255
                && selection
                    .bytes()
                    .next()
                    .is_some_and(|b| b.is_ascii_alphanumeric())
                && selection
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')) =>
        {
            Ok(Cap::new(verb, Scope::name(selection)))
        }
        Verb::SYS_POWER if power::power_methods(selection).is_some() => {
            Ok(Cap::new(verb, Scope::Wild))
        }
        _ => Err("invalid desktop AppID or power action".into()),
    }
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
