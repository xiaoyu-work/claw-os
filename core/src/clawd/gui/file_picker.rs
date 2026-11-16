//! Human file selection returns metadata, never a filesystem or execution grant.

use std::collections::HashMap;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use claw_display_control::ProcessIdentity;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zbus::zvariant::Value as PortalValue;

use crate::caps::{Cap, Scope, Verb};
use crate::clawd::app_sessions::gui::Registration;
use crate::clawd::authority::Decision;
use crate::clawd::client_identity::ClientIdentity;
use crate::clawd::desktop::{
    authenticated_helper_gate, owner_portal_request, protected_helper_command, DesktopEnvironment,
    PortalResult,
};
use crate::clawd::protocol::BrokerError;
use crate::clawd::wire::bounded::{Text, Token};

const REPORT_BYTES: usize = 16 * 1024;
static PICKERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Mode {
    #[default]
    File,
    Folder,
    Save,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Filter {
    Ron,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Request {
    Open {
        session: Token,
        request_id: Token<32>,
        title: Text<256>,
        modal: bool,
        #[serde(default)]
        mode: Mode,
        #[serde(default)]
        name: Option<Text<255>>,
        #[serde(default)]
        filter: Option<Filter>,
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
    fn identity(&self) -> (&str, &str) {
        match self {
            Self::Open {
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
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum Outcome {
    Selected { uri: Text<8192> },
    Cancelled,
    Failed { error: Text<2048> },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Report {
    outcome: Outcome,
    portal_retired: bool,
}

impl Report {
    fn failed(message: &str, portal_retired: bool) -> Self {
        Self {
            outcome: Outcome::Failed {
                error: Text::parse(message).expect("static picker diagnostic"),
            },
            portal_retired,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Options {
    title: Text<256>,
    modal: bool,
    mode: Mode,
    name: Option<Text<255>>,
    filter: Option<Filter>,
}

struct Entry {
    id: String,
    options: Options,
    active: bool,
    retired: bool,
    report: Option<Report>,
    cancelled: Arc<AtomicBool>,
    slot: Option<tokio::sync::SemaphorePermit<'static>>,
}

struct Binding {
    grant: crate::clawd::authority::GrantRef,
    generation: u64,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
}

pub(crate) struct Window {
    registration: Arc<Registration>,
    process: OnceLock<ProcessIdentity>,
    stopped: AtomicBool,
    cancel: tokio::sync::watch::Sender<u64>,
    entry: Mutex<Option<Entry>>,
}

tokio::task_local! { static WINDOW: Arc<Window>; }

pub(crate) async fn within<F: std::future::Future>(window: Arc<Window>, future: F) -> F::Output {
    WINDOW.scope(window, future).await
}

impl Window {
    pub(crate) fn new(registration: Arc<Registration>) -> Arc<Self> {
        let (cancel, _) = tokio::sync::watch::channel(0);
        Arc::new(Self {
            registration,
            process: OnceLock::new(),
            stopped: AtomicBool::new(false),
            cancel,
            entry: Mutex::new(None),
        })
    }

    pub(crate) fn bind(&self, process: ProcessIdentity) -> Result<(), String> {
        self.registration.snapshot(&process)?;
        self.process
            .set(process)
            .map_err(|_| "file picker is already bound".into())
    }

    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.cancel
            .send_modify(|value| *value = value.wrapping_add(1));
    }

    pub(crate) fn quiescent(&self) -> bool {
        self.entry.lock().is_ok_and(|entry| {
            entry
                .as_ref()
                .is_none_or(|entry| !entry.active && entry.retired)
        })
    }

    fn live(&self) -> Result<crate::clawd::authority::GrantView, String> {
        if self.stopped.load(Ordering::Acquire) {
            return Err("file picker GUI has stopped".into());
        }
        self.registration
            .snapshot(self.process.get().ok_or("file picker GUI is not bound")?)
    }

    fn request(
        self: &Arc<Self>,
        request: Request,
        client: &ClientIdentity,
        authority: &Decision,
    ) -> Result<Value, BrokerError> {
        let (session, id) = request.identity();
        let session = session.to_string();
        let id = id.to_string();
        if let Request::Open {
            mode,
            name: Some(name),
            ..
        } = &request
        {
            if *mode != Mode::Save
                || name.as_str().is_empty()
                || Path::new(name.as_str())
                    .file_name()
                    .and_then(|value| value.to_str())
                    != Some(name.as_str())
                || name.as_str().chars().any(|ch| matches!(ch, '/' | '\\'))
            {
                return Err(BrokerError::execution(
                    "file picker save name must be one literal filename",
                ));
            }
        }
        if session != self.registration.session_id
            || client.require_uid().map_err(BrokerError::authorization)?
                != self.registration.owner()
        {
            return Err(BrokerError::authorization(
                "file picker belongs to another GUI",
            ));
        }
        if id.len() != 32
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(BrokerError::execution("file picker correlation is invalid"));
        }
        let view = self.live().map_err(BrokerError::authorization)?;
        if authority.session_id() != Some(session.as_str())
            || authority.owner_uid() != self.registration.owner()
            || authority.app_id() != Some(self.registration.launch.app_id())
            || *authority.grant_ref() != view.id.audit_ref()
            || authority.generation() != view.generation
        {
            return Err(BrokerError::authorization(
                "file picker requires this GUI's exact current grant",
            ));
        }
        let selected = self
            .registration
            .launch
            .manifest()
            .gui_entry(self.registration.gui_entry.as_deref())
            .map_err(BrokerError::authorization)?;
        if selected.legacy_needs
            || !selected
                .launch_needs()
                .chain(
                    selected
                        .optional_needs
                        .iter()
                        .copied()
                        .filter(|need| selected.optional_need_is_bound(need)),
                )
                .any(|need| {
                    need.verb == Verb::UI_PROMPT
                        && match &need.scope {
                            crate::caps::manifest::ScopeBinding::Fixed { scope } => {
                                scope.covers(&Scope::Wild)
                            }
                            crate::caps::manifest::ScopeBinding::Wild => true,
                            _ => false,
                        }
                })
        {
            return Err(BrokerError::authorization(
                "this GUI must explicitly declare ui.prompt",
            ));
        }
        let _authorized = authority
            .require(Cap::unscoped(Verb::UI_PROMPT))
            .map_err(BrokerError::authorization)?;
        let mut entry = self
            .entry
            .lock()
            .map_err(|_| BrokerError::unavailable("file picker custody is poisoned"))?;
        if let Some(retained) = entry.as_ref() {
            if retained.id == id {
                if matches!(&request, Request::Open { title, modal, mode, name, filter, .. }
                    if title.as_str()!=retained.options.title.as_str() || *modal!=retained.options.modal
                        || *mode!=retained.options.mode || name.as_ref().map(Text::as_str)!=retained.options.name.as_ref().map(Text::as_str)
                        || *filter!=retained.options.filter)
                {
                    return Err(BrokerError::execution(
                        "file picker options cannot change during a request",
                    ));
                }
                if matches!(request, Request::Cancel { .. }) {
                    retained.cancelled.store(true, Ordering::Release);
                    self.cancel
                        .send_modify(|value| *value = value.wrapping_add(1));
                }
                return Ok(projection(retained));
            }
            if !retained.retired {
                return Err(BrokerError::unavailable(
                    "the previous file picker has not retired",
                ));
            }
        }
        let Request::Open {
            title,
            modal,
            mode,
            name,
            filter,
            ..
        } = request
        else {
            return Err(BrokerError::execution(
                "file picker correlation is not retained",
            ));
        };
        let slot = PICKERS
            .try_acquire()
            .map_err(|_| BrokerError::unavailable("file picker is at capacity"))?;
        let deadline = Instant::now()
            .checked_add(view.expires_in)
            .ok_or_else(|| BrokerError::unavailable("file picker deadline overflow"))?;
        let options = Options {
            title,
            modal,
            mode,
            name,
            filter,
        };
        let process = self
            .process
            .get()
            .ok_or_else(|| BrokerError::authorization("file picker is not bound"))?;
        let environment = DesktopEnvironment::for_user(
            self.registration.owner(),
            process.gid(),
            self.registration.home.clone(),
            self.registration.launcher.pid() as u32,
        )
        .map_err(BrokerError::execution)?;
        let (gate, peer) =
            UnixStream::pair().map_err(|error| BrokerError::execution(error.to_string()))?;
        gate.set_nonblocking(true)
            .map_err(|error| BrokerError::execution(error.to_string()))?;
        let mut dispatch = gate
            .try_clone()
            .map_err(|error| BrokerError::execution(error.to_string()))?;
        let gate = tokio::net::UnixStream::from_std(gate)
            .map_err(|error| BrokerError::execution(error.to_string()))?;
        let fd = peer.as_raw_fd();
        let mut command = protected_helper_command(
            &["--file-picker-helper".into(), fd.to_string()],
            &environment,
        )
        .map_err(BrokerError::execution)?;
        command.stdout(Stdio::null()).stderr(Stdio::null());
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
            .map_err(|error| BrokerError::execution(error.to_string()))?;
        drop(peer);
        let bytes = serde_json::to_vec(&options).expect("bounded picker options");
        let dispatched = dispatch
            .write_all(&[1])
            .and_then(|_| dispatch.write_all(&(bytes.len() as u32).to_be_bytes()))
            .and_then(|_| dispatch.write_all(&bytes))
            .is_ok();
        let cancelled = Arc::new(AtomicBool::new(false));
        *entry = Some(Entry {
            id: id.clone(),
            options,
            active: true,
            retired: false,
            report: None,
            cancelled: cancelled.clone(),
            slot: Some(slot),
        });
        let window = self.clone();
        let grant = view.id.audit_ref();
        let generation = view.generation;
        let context = crate::paths::RoutedPathContext::for_owner(
            self.registration.owner(),
            self.registration.home.clone(),
        );
        tokio::spawn(context.scope(async move {
            window
                .monitor(
                    child,
                    gate,
                    dispatched,
                    Binding {
                        grant,
                        generation,
                        deadline,
                        cancelled,
                    },
                )
                .await;
        }));
        Ok(projection(entry.as_ref().expect("retained picker request")))
    }

    async fn monitor(
        self: Arc<Self>,
        mut child: tokio::process::Child,
        gate: tokio::net::UnixStream,
        dispatched: bool,
        binding: Binding,
    ) {
        let (mut output, mut control) = gate.into_split();
        let read = async {
            let length = output.read_u32().await.map_err(|error| error.to_string())? as usize;
            if length == 0 || length > REPORT_BYTES {
                return Err("invalid picker report bound".to_string());
            }
            let mut bytes = vec![0; length];
            output
                .read_exact(&mut bytes)
                .await
                .map_err(|error| error.to_string())?;
            serde_json::from_slice::<Report>(&bytes).map_err(|error| error.to_string())
        };
        tokio::pin!(read);
        let mut cancelled = self.cancel.subscribe();
        let mut tick = tokio::time::interval(Duration::from_millis(200));
        let mut cancel_deadline = None;
        let report = loop {
            if !dispatched {
                break Report::failed("file picker was not dispatched", true);
            }
            if cancel_deadline.is_none()
                && (self.stopped.load(Ordering::Acquire)
                    || binding.cancelled.load(Ordering::Acquire)
                    || Instant::now() >= binding.deadline
                    || !self.live().is_ok_and(|view| {
                        view.id.audit_ref() == binding.grant
                            && view.generation == binding.generation
                    }))
            {
                cancel_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(8));
                if !tokio::time::timeout(Duration::from_secs(2), control.write_u8(2))
                    .await
                    .is_ok_and(|value| value.is_ok())
                {
                    break Report::failed("file picker cancellation is unconfirmed", false);
                }
            }
            tokio::select! {
                result = &mut read => break result.unwrap_or_else(|_|Report::failed("file picker helper ended without a portal report", false)),
                _ = tick.tick() => {},
                _ = cancelled.changed(), if cancel_deadline.is_none() => {},
                _ = async { tokio::time::sleep_until(cancel_deadline.expect("picker cancellation deadline")).await; },
                    if cancel_deadline.is_some() => break Report::failed("file picker cleanup is pending", false),
            }
        };
        drop(control);
        if tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .is_err()
        {
            let _ = child.start_kill();
        }
        loop {
            if child.try_wait().is_ok_and(|status| status.is_some()) {
                break;
            }
            let _ = child.start_kill();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if let Ok(mut entry) = self.entry.lock() {
            if let Some(entry) = entry.as_mut() {
                crate::provenance::audit(
                    "gui.file.picker.finished",
                    json!({
                        "session_id":self.registration.session_id,
                        "app_id":self.registration.launch.app_id(),
                        "request_id":entry.id,
                        "outcome":match &report.outcome {
                            Outcome::Selected { .. } => "selected",
                            Outcome::Cancelled => "cancelled",
                            Outcome::Failed { .. } => "failed",
                        },
                        "portal_retired":report.portal_retired,"helper_exited":true,
                    }),
                );
                entry.active = false;
                entry.retired = report.portal_retired;
                entry.report = Some(report);
                if entry.retired {
                    entry.slot.take();
                }
            }
        }
    }
}

fn projection(entry: &Entry) -> Value {
    let mut value = if !entry.active && entry.retired {
        serde_json::to_value(&entry.report.as_ref().expect("completed picker").outcome)
            .expect("picker outcome")
    } else {
        json!({"status":if entry.report.is_some() {"cleanup_pending"} else {"pending"}})
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
        BrokerError::authorization("file picker requires the exact private GUI endpoint")
    })?;
    let request = serde_json::from_value(params)
        .map_err(|_| BrokerError::execution("invalid closed file picker request"))?;
    window.request(request, client, authority)
}

pub fn helper(args: &[String]) -> Result<(), String> {
    if args.len() != 1 {
        return Err("file picker helper requires its Root control descriptor".into());
    }
    let fd = args[0].parse().map_err(|_| "invalid picker descriptor")?;
    let gate = authenticated_helper_gate(fd)?;
    if unsafe { libc::geteuid() } == 0
        || unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) } != 1
    {
        return Err("file picker helper requires the unprivileged no-new-privileges owner".into());
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?
        .block_on(async {
            let mut gate =
                tokio::net::UnixStream::from_std(gate).map_err(|error| error.to_string())?;
            let options = tokio::time::timeout(Duration::from_secs(10), async {
                if gate.read_u8().await? != 1 {
                    return Err(std::io::Error::other("picker dispatch was not admitted"));
                }
                let length = gate.read_u32().await? as usize;
                if length == 0 || length > 4096 {
                    return Err(std::io::Error::other("invalid picker options"));
                }
                let mut bytes = vec![0; length];
                gate.read_exact(&mut bytes).await?;
                serde_json::from_slice::<Options>(&bytes).map_err(std::io::Error::other)
            })
            .await
            .map_err(|_| "picker setup timed out")?
            .map_err(|error| error.to_string())?;
            let (mut control, mut output) = gate.into_split();
            let method = if options.mode == Mode::Save {
                "SaveFile"
            } else {
                "OpenFile"
            };
            let minimum_version = if options.mode == Mode::Folder { 3 } else { 1 };
            let result = owner_portal_request(
                &mut control,
                "org.freedesktop.portal.FileChooser",
                minimum_version,
                method,
                |token| {
                    let mut values = HashMap::from([
                        ("handle_token", PortalValue::from(token)),
                        ("modal", PortalValue::from(options.modal)),
                        ("multiple", PortalValue::from(false)),
                        ("directory", PortalValue::from(options.mode == Mode::Folder)),
                    ]);
                    if let Some(name) = &options.name {
                        values.insert("current_name", PortalValue::from(name.as_str().to_string()));
                    }
                    if options.filter == Some(Filter::Ron) {
                        values.insert(
                            "filters",
                            PortalValue::from(vec![(
                                options.title.as_str().to_string(),
                                vec![(0u32, "*.ron".to_string())],
                            )]),
                        );
                    }
                    (String::new(), options.title.as_str().to_string(), values)
                },
            )
            .await;
            let outcome = match result.result {
                PortalResult::Cancelled => Outcome::Cancelled,
                PortalResult::Failed(_) => {
                    Report::failed("OS file picker failed", result.portal_retired).outcome
                }
                PortalResult::Success(mut values) => {
                    let uris = values
                        .remove("uris")
                        .and_then(|value| Vec::<String>::try_from(value).ok());
                    match uris
                        .filter(|uris| uris.len() == 1)
                        .and_then(|mut uris| uris.pop())
                        .filter(|uri| {
                            uri.len() <= 8192
                                && url::Url::parse(uri)
                                    .ok()
                                    .and_then(|uri| uri.to_file_path().ok())
                                    .is_some_and(|path| {
                                        path.is_absolute()
                                            && path.to_str().is_some_and(|value| {
                                                value.len() <= 4096
                                                    && !value.chars().any(char::is_control)
                                            })
                                    })
                        })
                        .and_then(|uri| Text::parse(&uri).ok())
                    {
                        Some(uri) => Outcome::Selected { uri },
                        None => {
                            Report::failed(
                                "OS file picker returned an invalid local selection",
                                true,
                            )
                            .outcome
                        }
                    }
                }
            };
            let report = Report {
                outcome,
                portal_retired: result.portal_retired,
            };
            let bytes = serde_json::to_vec(&report).map_err(|error| error.to_string())?;
            if bytes.len() > REPORT_BYTES {
                return Err("file picker report exceeds its bound".into());
            }
            tokio::time::timeout(Duration::from_secs(2), async {
                output.write_u32(bytes.len() as u32).await?;
                output.write_all(&bytes).await?;
                output.shutdown().await
            })
            .await
            .map_err(|_| "file picker report timed out")?
            .map_err(|error| error.to_string())
        })
}

pub(crate) fn cli(
    action: &str,
    mut params: serde_json::Map<String, Value>,
) -> Result<Value, String> {
    if !matches!(action, "open" | "status" | "cancel")
        || params.contains_key("session")
        || params.contains_key("action")
    {
        return Err("file picker derives action and session from its GUI context".into());
    }
    params.insert(
        "session".into(),
        json!(std::env::var("COS_SESSION").map_err(|_| "file picker requires its GUI session")?),
    );
    params.insert("action".into(), json!(action));
    let request: Request = serde_json::from_value(Value::Object(params))
        .map_err(|_| "invalid closed file picker request")?;
    let response = crate::clawd::client::request_gui_resource_blocking(
        crate::clawd::protocol::Request::build(
            crate::clawd::routes::Command::GuiFilePicker,
            serde_json::to_value(request).map_err(|error| error.to_string())?,
        ),
    )
    .map_err(|error| format!("file picker outcome is unconfirmed: {error}; do not replay"))?;
    if response.ok {
        response
            .result
            .ok_or_else(|| "file picker result is missing".into())
    } else {
        Err(response
            .error
            .map(|error| error.message)
            .unwrap_or_else(|| "file picker refused".into()))
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/gui/file_picker.rs"
    ));
}
