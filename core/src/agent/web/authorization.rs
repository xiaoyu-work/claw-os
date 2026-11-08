//! Ephemeral browser presentation of the existing OS authentication authority.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use clawd_client::system_review::{
    PermissionChoice, ReviewAction, ReviewDecision, ReviewStatus, SystemReview,
};
use serde::Serialize;
use serde_json::json;
use tokio::sync::{oneshot, watch};
use zeroize::Zeroizing;

use super::auth::AuthenticatedToken;
use super::routes::clawd;
use crate::clawd::routes::Command;

#[cfg(target_os = "linux")]
mod polkit;

#[derive(Debug, thiserror::Error)]
enum Failure {
    #[error("OS authorization was cancelled.")]
    Cancelled,
    #[error("OS authorization timed out.")]
    Expired,
    #[error("{0}")]
    Unavailable(&'static str),
}

#[cfg(not(target_os = "linux"))]
mod polkit {
    use super::*;

    pub(super) async fn authorize(
        _session: Arc<Session>,
        _review: &SystemReview,
        _decision: &ReviewDecision,
    ) -> Result<(), Failure> {
        Err(Failure::Unavailable(
            "Web OS authentication requires Linux polkit.",
        ))
    }
}

const AUTHORIZATION_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_RESPONSE_BYTES: usize = 4096;

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    Starting,
    Prompt,
    Verifying,
    Finished,
    Failed,
    Cancelled,
    Expired,
}

impl Phase {
    fn terminal(self) -> bool {
        matches!(
            self,
            Self::Finished | Self::Failed | Self::Cancelled | Self::Expired
        )
    }
}

#[derive(Clone, Serialize)]
pub(super) struct Prompt {
    pub id: String,
    pub message: String,
}

#[derive(Clone, Serialize)]
pub(super) struct Snapshot {
    pub id: String,
    pub review_id: String,
    pub revision: String,
    pub label: String,
    pub target: String,
    pub choice: Option<PermissionChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<ReviewAction>,
    pub phase: Phase,
    pub prompt: Option<Prompt>,
    pub message: Option<String>,
}

struct Presentation {
    snapshot: Snapshot,
    response: Option<oneshot::Sender<Zeroizing<String>>>,
}

struct Session {
    owner_uid: u32,
    token_id: String,
    origin: String,
    presentation: Mutex<Presentation>,
    cancel: watch::Sender<bool>,
}

impl Session {
    fn snapshot(&self) -> Result<Snapshot, String> {
        self.presentation
            .lock()
            .map(|state| state.snapshot.clone())
            .map_err(|_| "OS authorization state is unavailable.".to_string())
    }

    fn finish(&self, phase: Phase, message: Option<String>) -> Result<(), String> {
        let mut state = self
            .presentation
            .lock()
            .map_err(|_| "OS authorization state is unavailable.".to_string())?;
        state.response = None;
        state.snapshot.phase = phase;
        state.snapshot.prompt = None;
        state.snapshot.message = message;
        Ok(())
    }

    async fn prompt(&self, message: String) -> Result<Zeroizing<String>, String> {
        if *self.cancel.borrow() {
            return Err("OS authorization was cancelled.".into());
        }
        let (sender, receiver) = oneshot::channel();
        {
            let mut state = self
                .presentation
                .lock()
                .map_err(|_| "OS authorization state is unavailable.".to_string())?;
            if state.response.is_some() {
                return Err("An OS authentication response is already pending.".into());
            }
            state.snapshot.phase = Phase::Prompt;
            state.snapshot.prompt = Some(Prompt {
                id: nonce()?,
                message: message.chars().take(512).collect(),
            });
            state.response = Some(sender);
        }
        let mut cancel = self.cancel.subscribe();
        tokio::select! {
            response = receiver => response.map_err(|_| "OS authentication prompt closed.".into()),
            _ = cancel.changed() => Err("OS authorization was cancelled.".into()),
        }
    }

    fn respond(&self, prompt_id: &str, response: Zeroizing<String>) -> Result<(), String> {
        if *self.cancel.borrow() {
            return Err("OS authorization was cancelled. Refresh its outcome.".into());
        }
        if response.len() > MAX_RESPONSE_BYTES
            || response
                .bytes()
                .any(|byte| matches!(byte, b'\0' | b'\r' | b'\n'))
        {
            return Err("The OS authentication response is invalid or too long.".into());
        }
        let mut state = self
            .presentation
            .lock()
            .map_err(|_| "OS authorization state is unavailable.".to_string())?;
        if state.snapshot.phase != Phase::Prompt
            || state
                .snapshot
                .prompt
                .as_ref()
                .map(|prompt| prompt.id.as_str())
                != Some(prompt_id)
        {
            return Err("This OS authentication prompt is no longer current.".into());
        }
        let sender = state
            .response
            .take()
            .ok_or("This OS authentication prompt was already answered.")?;
        state.snapshot.phase = Phase::Verifying;
        state.snapshot.prompt = None;
        sender.send(response).map_err(|_| {
            "The OS authentication prompt closed before receiving the response.".into()
        })
    }
}

#[derive(Default)]
pub(super) struct Controller {
    current: Mutex<Option<Arc<Session>>>,
}

impl Controller {
    pub async fn start(
        &self,
        caller: &AuthenticatedToken,
        origin: String,
        review: SystemReview,
        decision: ReviewDecision,
    ) -> Result<Snapshot, String> {
        if super::state::current_owner_uid()? != caller.uid {
            return Err("OS authorization requires the serving account's identity.".into());
        }
        protect_secret_memory()?;
        decision
            .validate_for(&review)
            .map_err(|_| "The displayed OS review is no longer valid.".to_string())?;
        let (choice, action, label, target) = match decision.action {
            ReviewAction::ApplyChoices => {
                let choice = decision
                    .choices
                    .first()
                    .ok_or("OS authorization requires an explicit permission choice.")?
                    .choice;
                let permission = review
                    .permissions
                    .first()
                    .ok_or("The OS review has no permission to authorize.")?;
                (
                    Some(choice),
                    None,
                    permission.label.clone(),
                    permission.scope.description.clone(),
                )
            }
            ReviewAction::ConfirmInstall
            | ReviewAction::ConfirmActivation
            | ReviewAction::ConfirmUpdate => (
                None,
                Some(decision.action),
                review.subject.name.clone(),
                format!(
                    "{} {}",
                    review.subject.app_id.as_deref().unwrap_or("App"),
                    review.subject.version.as_deref().unwrap_or("")
                ),
            ),
            ReviewAction::Cancel => {
                return Err("Cancellation does not need OS authentication.".into())
            }
        };
        let snapshot = Snapshot {
            id: nonce()?,
            review_id: review.id.clone(),
            revision: review.revision.to_string(),
            label,
            target,
            choice,
            action,
            phase: Phase::Starting,
            prompt: None,
            message: None,
        };
        let (cancel, _) = watch::channel(false);
        let session = Arc::new(Session {
            owner_uid: caller.uid,
            token_id: caller.token_id.clone(),
            origin,
            presentation: Mutex::new(Presentation {
                snapshot: snapshot.clone(),
                response: None,
            }),
            cancel,
        });
        {
            let mut current = self
                .current
                .lock()
                .map_err(|_| "OS authorization controller is unavailable.")?;
            if let Some(active) = current.as_ref() {
                if !active.snapshot()?.phase.terminal() {
                    return Err(
                        "Another OS authorization is already open. Finish or cancel it first."
                            .into(),
                    );
                }
            }
            *current = Some(session.clone());
        }
        tokio::spawn(async move {
            let result = polkit::authorize(session.clone(), &review, &decision).await;
            let refreshed = clawd::request(Command::SystemReviewShow, json!({"id": review.id}))
                .await
                .map_err(|_| "The authorization outcome could not be confirmed. Refresh the request before trying again.".to_string())
                .and_then(|value| serde_json::from_value::<SystemReview>(value)
                    .map_err(|_| "The OS returned an invalid authorization outcome.".to_string()))
                .and_then(|current| {
                    current.validate().map_err(|_| "The OS returned an invalid review.".to_string())?;
                    if current.id != review.id {
                        return Err("The OS returned a different authorization request.".into());
                    }
                    Ok(current.status)
                });
            let (phase, message) = match refreshed {
                Ok(ReviewStatus::Confirmed | ReviewStatus::Consumed | ReviewStatus::Completed) if result.is_ok() => (Phase::Finished, None),
                Ok(ReviewStatus::Confirmed | ReviewStatus::Consumed | ReviewStatus::Completed) => (Phase::Failed, Some(
                    "The OS shows this request as authorized, but this attempt was not confirmed. Refresh its recorded outcome; cancellation does not undo authorization.".into()
                )),
                Ok(ReviewStatus::Pending) => match result {
                    Ok(()) => (Phase::Failed, Some("Authorization was not confirmed. Refresh the request before trying again.".into())),
                    Err(Failure::Cancelled) => (Phase::Cancelled, Some("OS authorization was cancelled; no approval is assumed.".into())),
                    Err(Failure::Expired) => (Phase::Expired, Some("OS authorization timed out. Refresh the request to inspect its outcome.".into())),
                    Err(error) => (Phase::Failed, Some(error.to_string())),
                },
                Ok(_) => (Phase::Failed, Some("The OS request is no longer pending. Refresh to inspect its current state.".into())),
                Err(error) => (Phase::Failed, Some(error)),
            };
            if let Err(error) = session.finish(phase, message) {
                tracing::error!(%error, "could not publish OS authorization outcome");
            }
        });
        Ok(snapshot)
    }

    fn session(&self, id: &str, caller: &AuthenticatedToken) -> Result<Arc<Session>, String> {
        let session = self
            .current
            .lock()
            .map_err(|_| "OS authorization controller is unavailable.")?
            .as_ref()
            .cloned()
            .ok_or("OS authorization is no longer available.")?;
        if session.owner_uid != caller.uid
            || session.token_id != caller.token_id
            || session.snapshot()?.id != id
        {
            return Err("OS authorization does not belong to this Web session.".into());
        }
        Ok(session)
    }

    pub fn status(&self, id: &str, caller: &AuthenticatedToken) -> Result<Snapshot, String> {
        self.session(id, caller)?.snapshot()
    }

    pub fn respond(
        &self,
        id: &str,
        caller: &AuthenticatedToken,
        origin: &str,
        prompt_id: &str,
        response: Zeroizing<String>,
    ) -> Result<Snapshot, String> {
        let session = self.session(id, caller)?;
        if session.origin != origin {
            return Err("OS authorization response came from a different origin.".into());
        }
        session.respond(prompt_id, response)?;
        session.snapshot()
    }

    pub fn cancel(
        &self,
        id: &str,
        caller: &AuthenticatedToken,
        origin: &str,
    ) -> Result<Snapshot, String> {
        let session = self.session(id, caller)?;
        if session.origin != origin {
            return Err("OS authorization cancellation came from a different origin.".into());
        }
        session.cancel.send_replace(true);
        session.snapshot()
    }
}

fn nonce() -> Result<String, String> {
    let mut bytes = [0; 32];
    crate::credential::os_random_bytes(&mut bytes)
        .map_err(|_| "Could not create an OS authorization challenge.".to_string())?;
    Ok(hex::encode(bytes))
}

fn protect_secret_memory() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let limits = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limits) } != 0
            || unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0
        {
            return Err(
                "The Web process could not protect transient OS authentication memory.".into(),
            );
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("Web OS authentication requires Linux process protection.".into())
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/web/authorization.rs"
    ));
}
