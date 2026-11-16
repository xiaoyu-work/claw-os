//! One interactive portal request in the custody of one private GUI endpoint.

use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use claw_display_control::ProcessIdentity;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::caps::{Cap, Scope, Verb};
use crate::clawd::app_sessions::gui::Registration;
use crate::clawd::authority::{Decision, GrantRef, GrantView};
use crate::clawd::client_identity::ClientIdentity;
use crate::clawd::desktop::DesktopEnvironment;
use crate::clawd::protocol::BrokerError;
use crate::clawd::wire::bounded::Token;

use super::portal::{Outcome, Report, REPORT_BYTES};

const CANCEL_TIMEOUT: Duration = Duration::from_secs(16);
const EXIT_TIMEOUT: Duration = Duration::from_secs(2);
static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Request {
    #[serde(rename = "request")]
    Start {
        session: Token,
        request_id: Token<32>,
        modal: bool,
    },
    Image {
        session: Token,
        request_id: Token<32>,
        modal: bool,
    },
    ReadImage {
        session: Token,
        request_id: Token<32>,
        offset: u64,
    },
    Status {
        session: Token,
        request_id: Token<32>,
    },
    Cancel {
        session: Token,
        request_id: Token<32>,
    },
}

impl Request {
    fn identity(&self) -> Result<(&str, &str), String> {
        let (session, id) = match self {
            Self::Start {
                session,
                request_id,
                ..
            }
            | Self::Image {
                session,
                request_id,
                ..
            }
            | Self::ReadImage {
                session,
                request_id,
                ..
            }
            | Self::Status {
                session,
                request_id,
            }
            | Self::Cancel {
                session,
                request_id,
            } => (session.as_str(), request_id.as_str()),
        };
        if id.len() != 32
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("Capture correlation must be 32 lowercase hexadecimal characters".into());
        }
        Ok((session, id))
    }
}

struct Entry {
    id: String,
    modal: bool,
    image: bool,
    bytes: Option<Vec<u8>>,
    active: bool,
    retired: bool,
    report: Option<Report>,
    slot: Option<tokio::sync::SemaphorePermit<'static>>,
}

impl Entry {
    fn check(&self, request: &Request) -> Result<(), String> {
        let (_, id) = request.identity()?;
        if self.id != id
            || matches!(request, Request::Start { modal, .. } | Request::Image { modal, .. } if *modal != self.modal)
            || matches!(request, Request::Start { .. } if self.image)
            || matches!(request, Request::Image { .. } | Request::ReadImage { .. } if !self.image)
        {
            return Err("this window retains one Capture correlation and immutable modal choice; no request was replayed".into());
        }
        Ok(())
    }
}

struct Binding {
    request_id: String,
    image: bool,
    grant: GrantRef,
    generation: u64,
    deadline: Instant,
}

pub(crate) struct Window {
    registration: Arc<Registration>,
    process: OnceLock<ProcessIdentity>,
    stopped: AtomicBool,
    cancellation: tokio::sync::watch::Sender<bool>,
    entry: Mutex<Option<Entry>>,
}

tokio::task_local! { static WINDOW: Arc<Window>; }

/// Only the Root GUI broker installs this context, after its per-message
/// process/cgroup checks. Ordinary dispatch and App/MCP relays have none.
pub(crate) async fn within<F: std::future::Future>(window: Arc<Window>, future: F) -> F::Output {
    WINDOW.scope(window, future).await
}

impl Window {
    pub(crate) fn new(registration: Arc<Registration>) -> Arc<Self> {
        let (cancellation, _) = tokio::sync::watch::channel(false);
        Arc::new(Self {
            registration,
            process: OnceLock::new(),
            stopped: AtomicBool::new(false),
            cancellation,
            entry: Mutex::new(None),
        })
    }

    pub(crate) fn bind(&self, process: ProcessIdentity) -> Result<(), String> {
        self.process
            .set(process)
            .map_err(|_| "Capture GUI process is already bound".into())
    }

    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.cancellation.send_replace(true);
        if let Ok(mut entry) = self.entry.lock() {
            if let Some(entry) = entry.as_mut() {
                entry.bytes.take();
                if !entry.active && entry.retired {
                    entry.slot.take();
                }
            }
        }
    }

    pub(crate) fn quiescent(&self) -> bool {
        self.entry.lock().is_ok_and(|entry| {
            entry
                .as_ref()
                .is_none_or(|entry| !entry.active && entry.retired)
        })
    }

    pub(crate) fn health(&self) -> Result<(), String> {
        let entry = self
            .entry
            .lock()
            .map_err(|_| "Capture custody lock is poisoned")?;
        if entry
            .as_ref()
            .is_some_and(|entry| !entry.active && !entry.retired)
        {
            return Err(
                "Capture portal/helper retirement is unconfirmed; GUI custody is retained".into(),
            );
        }
        Ok(())
    }

    fn snapshot(&self) -> Result<GrantView, String> {
        if self.stopped.load(Ordering::Acquire) {
            return Err("Capture GUI has stopped".into());
        }
        self.registration
            .snapshot(self.process.get().ok_or("Capture GUI is not bound")?)
    }

    fn live(&self, binding: &Binding) -> Result<(), String> {
        if Instant::now() >= binding.deadline {
            return Err("Capture GUI lease expired".into());
        }
        let view = self.snapshot()?;
        if view.id.audit_ref() != binding.grant || view.generation != binding.generation {
            return Err("Capture GUI grant changed".into());
        }
        Ok(())
    }

    fn eligible(&self) -> Result<(), String> {
        use crate::caps::manifest::ScopeBinding;
        let selected = self
            .registration
            .launch
            .manifest()
            .gui_entry(self.registration.gui_entry.as_deref())?;
        let declared = !selected.legacy_needs
            && selected
                .launch_needs()
                .chain(
                    selected
                        .optional_needs
                        .iter()
                        .copied()
                        .filter(|need| selected.optional_need_is_bound(need)),
                )
                .any(|need| {
                    need.verb == Verb::DESKTOP_CAPTURE
                        && match &need.scope {
                            ScopeBinding::Fixed { scope } => scope.covers(&Scope::name("screen")),
                            ScopeBinding::Wild => true,
                            _ => false,
                        }
                });
        if declared {
            Ok(())
        } else {
            Err("interactive capture requires this signed GUI's explicit desktop.capture:screen declaration".into())
        }
    }

    fn control(
        self: &Arc<Self>,
        request: Request,
        client: &ClientIdentity,
        authority: &Decision,
    ) -> Result<Value, BrokerError> {
        let (session, id) = request.identity().map_err(BrokerError::execution)?;
        if session != self.registration.session_id
            || authority.session_id() != Some(session)
            || authority.owner_uid() != self.registration.owner()
            || client.require_uid().map_err(BrokerError::authorization)?
                != self.registration.owner()
            || authority.app_id() != Some(self.registration.launch.app_id())
        {
            return Err(BrokerError::authorization(
                "Capture request is outside this exact GUI owner/App/session",
            ));
        }
        self.eligible().map_err(BrokerError::authorization)?;
        let observed = Instant::now();
        let view = self.snapshot().map_err(BrokerError::authorization)?;
        if view.id.audit_ref() != *authority.grant_ref()
            || view.generation != authority.generation()
        {
            return Err(BrokerError::authorization(
                "Capture request does not hold this GUI's current grant",
            ));
        }
        let _authorized = authority
            .require(Cap::new(Verb::DESKTOP_CAPTURE, Scope::name("screen")))
            .map_err(BrokerError::authorization)?;
        let mut entry = self
            .entry
            .lock()
            .map_err(|_| BrokerError::unavailable("Capture custody lock is poisoned"))?;
        if let Some(retained) = entry.as_ref() {
            retained.check(&request).map_err(BrokerError::execution)?;
            if let Request::ReadImage { offset, .. } = request {
                let bytes = retained
                    .bytes
                    .as_ref()
                    .filter(|_| retained.retired && !retained.active)
                    .ok_or_else(|| BrokerError::unavailable("Capture PNG is not ready"))?;
                let offset = usize::try_from(offset)
                    .map_err(|_| BrokerError::execution("invalid PNG offset"))?;
                if offset > bytes.len() {
                    return Err(BrokerError::execution("invalid PNG offset"));
                }
                let end = offset.saturating_add(128 * 1024).min(bytes.len());
                return Ok(json!({
                    "version":1,"request_id":retained.id,"offset":offset,"done":end == bytes.len(),
                    "bytes":&bytes[offset..end],
                }));
            }
            if matches!(request, Request::Cancel { .. }) {
                self.cancellation.send_replace(true);
            }
            return Ok(projection(retained, *self.cancellation.borrow()));
        }
        let (modal, image) = match request {
            Request::Start { modal, .. } => (modal, false),
            Request::Image { modal, .. } => (modal, true),
            _ => {
                return Err(BrokerError::execution(
                    "Capture correlation is not retained by this window",
                ))
            }
        };
        let slot = SLOTS.try_acquire().map_err(|_| {
            BrokerError::unavailable("interactive Capture is at its bounded capacity")
        })?;
        if self.stopped.load(Ordering::Acquire) {
            return Err(BrokerError::authorization("Capture GUI has stopped"));
        }
        let binding = Binding {
            request_id: id.into(),
            image,
            grant: view.id.audit_ref(),
            generation: view.generation,
            deadline: observed
                .checked_add(if image {
                    view.expires_in.min(super::CAPTURE_TIMEOUT)
                } else {
                    view.expires_in
                })
                .ok_or_else(|| BrokerError::unavailable("Capture lease overflow"))?,
        };
        *entry = Some(Entry {
            id: id.into(),
            modal,
            image,
            bytes: None,
            active: true,
            retired: false,
            report: None,
            slot: Some(slot),
        });
        match self.start(modal, image, authority) {
            Ok((child, gate, dispatched)) => {
                let window = self.clone();
                let context = crate::paths::RoutedPathContext::for_owner(
                    self.registration.owner(),
                    self.registration.home.clone(),
                );
                tokio::spawn(context.scope(async move {
                    window.monitor(child, gate, dispatched, binding).await;
                }));
            }
            Err(error) => {
                let retained = entry
                    .as_mut()
                    .ok_or_else(|| BrokerError::unavailable("Capture custody disappeared"))?;
                retained.active = false;
                retained.retired = true;
                retained.report = Some(Report::failed(&error, true));
                retained.slot.take();
            }
        }
        Ok(projection(
            entry
                .as_ref()
                .ok_or_else(|| BrokerError::unavailable("Capture custody disappeared"))?,
            *self.cancellation.borrow(),
        ))
    }

    fn start(
        &self,
        modal: bool,
        image: bool,
        authority: &Decision,
    ) -> Result<(tokio::process::Child, tokio::net::UnixStream, bool), String> {
        let process = self.process.get().ok_or("Capture GUI is not bound")?;
        let environment = DesktopEnvironment::for_user(
            self.registration.owner(),
            process.gid(),
            self.registration.home.clone(),
            self.registration.launcher.pid() as u32,
        )?;
        super::validate_session_bus(self.registration.owner())?;
        let (gate, peer) = UnixStream::pair().map_err(|error| error.to_string())?;
        gate.set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let mut dispatch = gate.try_clone().map_err(|error| error.to_string())?;
        let gate = tokio::net::UnixStream::from_std(gate).map_err(|error| error.to_string())?;
        let fd = peer.as_raw_fd();
        let mut command = super::helper_command(
            &[
                if image {
                    "--capture-image-helper"
                } else {
                    "--capture-gui-helper"
                }
                .into(),
                modal.to_string(),
                fd.to_string(),
            ],
            &environment,
        )?;
        command
            .process_group(0)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for key in ["WAYLAND_DISPLAY", "DISPLAY", "XAUTHORITY"] {
            command.env_remove(key);
        }
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut command = tokio::process::Command::from(command);
        command.kill_on_drop(true);
        let child = command
            .spawn()
            .map_err(|error| format!("start OS Capture helper: {error}"))?;
        drop(peer);
        // A helper with no dispatch byte cannot contact the portal. Keep child
        // custody even if this final live spend or the one-byte handoff fails.
        let dispatched = authority
            .require(Cap::new(Verb::DESKTOP_CAPTURE, Scope::name("screen")))
            .is_ok()
            && dispatch.write_all(&[1]).is_ok();
        Ok((child, gate, dispatched))
    }

    async fn monitor(
        self: Arc<Self>,
        mut child: tokio::process::Child,
        gate: tokio::net::UnixStream,
        dispatched: bool,
        binding: Binding,
    ) {
        let (mut input, mut control) = gate.into_split();
        let read = async {
            let length = input.read_u32().await.map_err(|error| error.to_string())? as usize;
            if length == 0 || length > REPORT_BYTES {
                return Err("invalid Capture helper report bound".to_string());
            }
            let mut bytes = vec![0; length];
            input
                .read_exact(&mut bytes)
                .await
                .map_err(|error| error.to_string())?;
            let report: Report =
                serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
            if let Outcome::Image { length } = report.outcome {
                let length = usize::try_from(length).map_err(|_| "Capture PNG length overflow")?;
                if length == 0 || length > super::MAX_PNG_BYTES {
                    return Ok((
                        Report::failed("Capture PNG exceeds its bound", report.portal_retired),
                        None,
                    ));
                }
                let mut png = vec![0; length];
                if input.read_exact(&mut png).await.is_err()
                    || !png.starts_with(b"\x89PNG\r\n\x1a\n")
                {
                    return Ok((
                        Report::failed(
                            "Capture PNG transfer was incomplete or invalid",
                            report.portal_retired,
                        ),
                        None,
                    ));
                }
                Ok((report, Some(png)))
            } else {
                Ok((report, None))
            }
        };
        tokio::pin!(read);
        let mut cancellation = self.cancellation.subscribe();
        let mut tick = tokio::time::interval(Duration::from_millis(200));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut cancel_deadline = None;
        let mut timed_out = false;
        let (mut report, mut image) = loop {
            if !dispatched {
                break (Report::failed("Capture was not dispatched", true), None);
            }
            if cancel_deadline.is_none() && (*cancellation.borrow() || self.live(&binding).is_err())
            {
                timed_out = binding.image && Instant::now() >= binding.deadline;
                cancel_deadline = Some(tokio::time::Instant::now() + CANCEL_TIMEOUT);
                if !tokio::time::timeout(EXIT_TIMEOUT, control.write_u8(2))
                    .await
                    .is_ok_and(|result| result.is_ok())
                {
                    break (
                        Report::failed("Capture cancellation transport is unconfirmed", false),
                        None,
                    );
                }
            }
            tokio::select! {
                result = &mut read => break result.unwrap_or_else(|_| (Report::failed("OS Capture helper ended without a checked portal report", false), None)),
                _ = tick.tick() => {}
                _ = cancellation.changed(), if cancel_deadline.is_none() => {}
                _ = async { tokio::time::sleep_until(cancel_deadline.expect("cancel deadline")).await; },
                    if cancel_deadline.is_some() => break (Report::failed("OS Capture cancellation deadline expired; cleanup is pending", false), None),
            }
        };
        if timed_out {
            report = Report::failed("Non-interactive Capture timed out", report.portal_retired);
            image = None;
        }
        drop(control);
        let exit_deadline = tokio::time::Instant::now() + EXIT_TIMEOUT;
        let mut killed = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(error) => {
                    tracing::error!(%error, "Capture helper retirement is unconfirmed");
                }
            }
            if !killed && tokio::time::Instant::now() >= exit_deadline {
                if let Err(error) = child.start_kill() {
                    tracing::error!(%error, "Capture helper stop is unconfirmed");
                }
                killed = true;
            }
            // SIGKILL alone is never exit proof, nor portal Close proof.
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let retired = report.portal_retired;
        crate::provenance::audit(
            "gui.capture.finished",
            json!({
                "session_id": self.registration.session_id,
                "app_id": self.registration.launch.app_id(),
                "request_id": binding.request_id,
                "portal_outcome": report.outcome.label(),
                "portal_retired": report.portal_retired, "helper_exited": true,
                "helper_success": status.success(),
                "retired": retired,
            }),
        );
        if let Ok(mut entry) = self.entry.lock() {
            if let Some(entry) = entry.as_mut() {
                entry.active = false;
                entry.retired = retired;
                entry.report = Some(report);
                if !self.stopped.load(Ordering::Acquire) {
                    entry.bytes = image;
                }
                if retired && entry.bytes.is_none() {
                    entry.slot.take();
                }
            }
        }
    }
}

fn projection(entry: &Entry, cancelling: bool) -> Value {
    let mut value = if !entry.active && entry.retired {
        serde_json::to_value(
            &entry
                .report
                .as_ref()
                .expect("retired Capture report")
                .outcome,
        )
        .expect("bounded Capture projection")
    } else {
        json!({"status": if entry.report.is_some() { "cleanup_pending" } else if cancelling { "cancelling" } else { "pending" }})
    };
    value["version"] = json!(1);
    value["request_id"] = json!(entry.id);
    value
}

pub(crate) async fn control(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, BrokerError> {
    let window = WINDOW.try_with(Arc::clone).map_err(|_| {
        BrokerError::authorization(
            "interactive capture requires the exact Root-held private GUI endpoint",
        )
    })?;
    let request = serde_json::from_value(params)
        .map_err(|_| BrokerError::execution("invalid closed Capture request"))?;
    window.control(request, client, authority)
}

pub(crate) fn cli(
    action: &str,
    mut params: serde_json::Map<String, Value>,
) -> Result<Value, String> {
    if !matches!(
        action,
        "request" | "status" | "cancel" | "image" | "read_image"
    ) || params.contains_key("session")
        || params.contains_key("action")
    {
        return Err("GUI Capture derives its action and session from the bound Host".into());
    }
    let session =
        std::env::var("COS_SESSION").map_err(|_| "GUI Capture requires its bound session")?;
    params.insert("session".into(), json!(session));
    params.insert("action".into(), json!(action));
    let request: Request = serde_json::from_value(Value::Object(params))
        .map_err(|_| "invalid closed GUI Capture request")?;
    request.identity()?;
    let response = crate::clawd::client::request_gui_resource_blocking(crate::clawd::protocol::Request::new(
        crate::clawd::routes::Command::GuiCaptureControl, serde_json::to_value(request).map_err(|error| error.to_string())?,
    )).map_err(|error| format!("GUI Capture response is unconfirmed: {error}; do not create a new correlation to replay it"))?;
    if response.ok {
        response
            .result
            .ok_or_else(|| "GUI Capture reply is missing".into())
    } else {
        Err(response
            .error
            .map(|error| error.message)
            .unwrap_or_else(|| "GUI Capture was refused".into()))
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/capture/gui.rs"
    ));
}
