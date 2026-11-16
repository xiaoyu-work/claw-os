//! Instance-owned playback exchange. Only separately authorized clients dispatch controls.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use claw_display_control::ProcessIdentity;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::clawd::app_sessions::gui::Registration;
use crate::clawd::client_identity::ClientIdentity;
use crate::clawd::protocol::BrokerError;
use crate::clawd::wire::bounded::{Text, TextList, Token};
use crate::clawd::wire::requests::MediaPlayerAction;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Exchange {
    session: Token,
    #[serde(default)]
    reply: Option<Reply>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    request_id: Token<32>,
    result: ResultValue,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ResultValue {
    Applied,
    Status {
        status: PlaybackStatus,
        title: Option<Text<4096>>,
        album: Option<Text<4096>>,
        url: Option<Text<8192>>,
        artist: TextList<64, 1024>,
        length_micros: Option<i64>,
    },
    Failed {
        error: Text<2048>,
    },
}

#[derive(Debug, Deserialize, Serialize)]
enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

impl ResultValue {
    fn project(self, action: MediaPlayerAction) -> Result<Value, String> {
        let value = match self {
            Self::Applied if action != MediaPlayerAction::Status => json!({"ok": true}),
            Self::Status {
                status,
                title,
                album,
                url,
                artist,
                length_micros,
            } if action == MediaPlayerAction::Status => json!({
                "status": status, "title": title, "album": album, "url": url,
                "artist": artist, "length_micros": length_micros,
            }),
            Self::Failed { error } => return Err(format!("native playback: {}", error.as_str())),
            _ => return Err("native playback reply does not match the admitted action".into()),
        };
        if value.to_string().len() > super::MAX_OUTPUT {
            return Err("Media Player metadata exceeds the limit".into());
        }
        Ok(value)
    }
}

struct Invocation {
    id: String,
    action: MediaPlayerAction,
    deadline: Instant,
    cancelled: AtomicBool,
    dispatch: Mutex<Option<oneshot::Sender<oneshot::Sender<bool>>>>,
    result: Mutex<Option<oneshot::Sender<Result<ResultValue, String>>>>,
}

#[derive(Default)]
struct Pending {
    queue: VecDeque<Arc<Invocation>>,
    active: Option<Arc<Invocation>>,
    last: Option<String>,
}

pub(crate) struct Window {
    registration: Arc<Registration>,
    process: OnceLock<ProcessIdentity>,
    stopped: AtomicBool,
    ready: AtomicBool,
    pending: Mutex<Pending>,
    serial: tokio::sync::Mutex<()>,
    changed: tokio::sync::Notify,
    capacity: Arc<tokio::sync::Semaphore>,
}

impl Window {
    pub(crate) fn new(registration: Arc<Registration>) -> Arc<Self> {
        Arc::new(Self {
            registration,
            process: OnceLock::new(),
            stopped: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            pending: Mutex::new(Pending::default()),
            serial: tokio::sync::Mutex::new(()),
            changed: tokio::sync::Notify::new(),
            capacity: Arc::new(tokio::sync::Semaphore::new(8)),
        })
    }

    pub(crate) fn bind(&self, process: ProcessIdentity) -> Result<(), String> {
        self.registration.snapshot(&process)?;
        self.process
            .set(process)
            .map_err(|_| "playback window is already bound".into())
    }

    fn live(&self) -> Result<(), String> {
        if self.stopped.load(Ordering::Acquire) {
            return Err("playback window has stopped".into());
        }
        self.registration
            .snapshot(self.process.get().ok_or("playback window is not bound")?)?;
        Ok(())
    }

    pub(crate) fn ready(&self) -> bool {
        self.ready.load(Ordering::Acquire) && !self.stopped.load(Ordering::Acquire)
    }

    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        if let Ok(mut state) = self.pending.lock() {
            let mut requests = state.queue.drain(..).collect::<Vec<_>>();
            requests.extend(state.active.take());
            for request in requests {
                request.cancelled.store(true, Ordering::Release);
                if let Ok(mut result) = request.result.lock() {
                    if let Some(result) = result.take() {
                        let _ = result.send(Err(
                            "playback window retired; an admitted effect must not be replayed"
                                .into(),
                        ));
                    }
                }
            }
        }
        self.changed.notify_waiters();
    }

    fn remove(&self, request: &Invocation) {
        request.cancelled.store(true, Ordering::Release);
        if let Ok(mut state) = self.pending.lock() {
            state.queue.retain(|entry| entry.id != request.id);
            if state
                .active
                .as_ref()
                .is_some_and(|entry| entry.id == request.id)
            {
                state.active.take();
                state.last = Some(request.id.clone());
            }
        }
        self.changed.notify_waiters();
    }

    pub(crate) async fn execute(
        self: &Arc<Self>,
        action: MediaPlayerAction,
        timeout: Duration,
        mut authorize: impl FnMut() -> Result<(), String>,
    ) -> Result<Value, String> {
        self.live()?;
        let slot = self
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| "playback window is at its bounded command capacity")?;
        let (dispatch, proposed) = oneshot::channel();
        let (result, completed) = oneshot::channel();
        let request = Arc::new(Invocation {
            id: uuid::Uuid::new_v4().simple().to_string(),
            action,
            deadline: Instant::now()
                .checked_add(timeout)
                .ok_or("playback deadline overflow")?,
            cancelled: AtomicBool::new(false),
            dispatch: Mutex::new(Some(dispatch)),
            result: Mutex::new(Some(result)),
        });
        {
            let mut state = self
                .pending
                .lock()
                .map_err(|_| "playback queue is poisoned")?;
            self.live()?;
            state.queue.push_back(request.clone());
        }
        let _custody = InvocationGuard {
            window: self.clone(),
            request,
            _slot: slot,
        };
        self.changed.notify_one();
        tokio::time::timeout(timeout, async {
            let permit: oneshot::Sender<bool> = proposed
                .await
                .map_err(|_| "playback window ended before dispatch")?;
            // The caller's live authority stays in this broker request, not in App state.
            if let Err(error) = authorize() {
                let _ = permit.send(false);
                return Err(error);
            }
            permit
                .send(true)
                .map_err(|_| "playback dispatch was cancelled")?;
            let reply = completed
                .await
                .map_err(|_| "playback result is unconfirmed; do not replay")??;
            self.live()?;
            authorize()?;
            reply.project(action)
        })
        .await
        .map_err(|_| "playback result timed out; do not automatically replay".to_string())?
    }

    pub(crate) async fn exchange(
        &self,
        params: Value,
        client: &ClientIdentity,
    ) -> Result<Value, BrokerError> {
        let request: Exchange = serde_json::from_value(params)
            .map_err(|_| BrokerError::execution("invalid closed playback exchange"))?;
        if request.session.as_str() != self.registration.session_id
            || client.require_uid().map_err(BrokerError::authorization)?
                != self.registration.owner()
        {
            return Err(BrokerError::authorization(
                "playback exchange belongs to another window",
            ));
        }
        let _serial = self
            .serial
            .try_lock()
            .map_err(|_| BrokerError::unavailable("this window already has a playback exchange"))?;
        self.live().map_err(BrokerError::authorization)?;
        if let Some(reply) = request.reply {
            let mut state = self
                .pending
                .lock()
                .map_err(|_| BrokerError::unavailable("playback queue is poisoned"))?;
            if state
                .active
                .as_ref()
                .is_some_and(|entry| entry.id == reply.request_id.as_str())
            {
                let active = state.active.take().expect("matched playback invocation");
                state.last = Some(active.id.clone());
                let result = active
                    .result
                    .lock()
                    .map_err(|_| BrokerError::unavailable("playback reply is poisoned"))?
                    .take();
                if let Some(result) = result {
                    let _ = result.send(Ok(reply.result));
                }
            } else if state.last.as_deref() != Some(reply.request_id.as_str()) {
                return Err(BrokerError::execution(
                    "playback reply is not this window's admitted invocation",
                ));
            }
        }
        self.ready.store(true, Ordering::Release);
        let wait_until = tokio::time::Instant::now() + Duration::from_millis(1500);
        loop {
            self.live().map_err(BrokerError::authorization)?;
            let next = {
                let mut state = self
                    .pending
                    .lock()
                    .map_err(|_| BrokerError::unavailable("playback queue is poisoned"))?;
                if state.active.is_some() {
                    return Err(BrokerError::execution(
                        "the active playback invocation requires its exact reply",
                    ));
                }
                state.queue.pop_front()
            };
            if let Some(next) = next {
                if next.cancelled.load(Ordering::Acquire) || Instant::now() >= next.deadline {
                    continue;
                }
                let dispatch = next
                    .dispatch
                    .lock()
                    .map_err(|_| BrokerError::unavailable("playback dispatch is poisoned"))?
                    .take()
                    .ok_or_else(|| {
                        BrokerError::execution("playback invocation was already dispatched")
                    })?;
                let (permit, approved) = oneshot::channel();
                if dispatch.send(permit).is_err() {
                    continue;
                }
                let allowed = tokio::time::timeout_at(next.deadline.into(), approved)
                    .await
                    .is_ok_and(|result| result == Ok(true));
                if !allowed {
                    continue;
                }
                self.live().map_err(BrokerError::authorization)?;
                let mut state = self
                    .pending
                    .lock()
                    .map_err(|_| BrokerError::unavailable("playback queue is poisoned"))?;
                if next.cancelled.load(Ordering::Acquire) || Instant::now() >= next.deadline {
                    continue;
                }
                let command = json!({"request_id": next.id, "action": next.action});
                state.active = Some(next.clone());
                crate::provenance::audit(
                    "gui.media.dispatch",
                    json!({
                        "session_id": self.registration.session_id,
                        "app_id": self.registration.launch.app_id(),
                        "request_id": next.id, "action": next.action,
                    }),
                );
                return Ok(json!({"version":1, "command":command}));
            }
            if tokio::time::timeout_at(wait_until, self.changed.notified())
                .await
                .is_err()
            {
                self.live().map_err(BrokerError::authorization)?;
                return Ok(json!({"version":1,"command":null}));
            }
        }
    }
}

struct InvocationGuard {
    window: Arc<Window>,
    request: Arc<Invocation>,
    _slot: tokio::sync::OwnedSemaphorePermit,
}

impl Drop for InvocationGuard {
    fn drop(&mut self) {
        self.window.remove(&self.request);
    }
}

pub(crate) fn cli(mut params: serde_json::Map<String, Value>) -> Result<Value, String> {
    if params.contains_key("session") {
        return Err("playback exchange derives its session from the bound Host".into());
    }

    let session = std::env::var("COS_SESSION").map_err(|_| "playback requires its GUI session")?;
    params.insert("session".into(), json!(session));
    let request: Exchange = serde_json::from_value(Value::Object(params))
        .map_err(|_| "invalid closed playback exchange")?;
    let response = crate::clawd::client::request_gui_resource_blocking(
        crate::clawd::protocol::Request::build(
            crate::clawd::routes::Command::GuiMediaExchange,
            serde_json::to_value(request).map_err(|error| error.to_string())?,
        ),
    )
    .map_err(|error| format!("playback exchange is unconfirmed: {error}"))?;
    if response.ok {
        response
            .result
            .ok_or_else(|| "playback exchange result is missing".into())
    } else {
        Err(response
            .error
            .map(|error| error.message)
            .unwrap_or_else(|| "playback exchange refused".into()))
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/media_player/gui.rs"
    ));
}
