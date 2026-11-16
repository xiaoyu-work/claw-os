//! Output-only PCM relay. Apps never receive a PipeWire socket or capture descriptor.

use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use base64::Engine;
use claw_display_control::ProcessIdentity;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

use crate::caps::{Cap, Scope, Verb};
use crate::clawd::app_sessions::gui::{kernel_peer, Registration};
use crate::clawd::authority::{Decision, GrantRef};
use crate::clawd::client_identity::ClientIdentity;
use crate::clawd::desktop::{configured_user_command, DesktopEnvironment};
use crate::clawd::protocol::BrokerError;
use crate::clawd::wire::bounded::{Text, Token};

const PCM_BYTES: usize = 16 * 1024;
static STREAMS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Request {
    Open {
        session: Token,
        stream_id: Token<32>,
    },
    Write {
        session: Token,
        stream_id: Token<32>,
        sequence: u64,
        data: Text<21848>,
    },
    Close {
        session: Token,
        stream_id: Token<32>,
        sequence: u64,
    },
}

impl Request {
    fn identity(&self) -> (&str, &str) {
        match self {
            Self::Open { session, stream_id }
            | Self::Write {
                session, stream_id, ..
            }
            | Self::Close {
                session, stream_id, ..
            } => (session.as_str(), stream_id.as_str()),
        }
    }
}

fn decode_pcm(data: &str) -> Result<Vec<u8>, BrokerError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| BrokerError::execution("audio data is not base64 PCM"))?;
    if bytes.is_empty() || bytes.len() > PCM_BYTES || bytes.len() % 4 != 0 {
        return Err(BrokerError::execution(
            "audio data must be bounded stereo S16LE frames",
        ));
    }
    Ok(bytes)
}

struct Stream {
    id: String,
    sequence: u64,
    active: bool,
    sender: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
    cancel: tokio::sync::watch::Sender<bool>,
}

struct Binding {
    grant: GrantRef,
    generation: u64,
    producer: ProcessIdentity,
}

pub(crate) struct Window {
    registration: Arc<Registration>,
    process: OnceLock<ProcessIdentity>,
    stopped: AtomicBool,
    serial: tokio::sync::Mutex<()>,
    stream: Mutex<Option<Stream>>,
}

tokio::task_local! { static WINDOW: Arc<Window>; }

pub(crate) async fn within<F: std::future::Future>(window: Arc<Window>, future: F) -> F::Output {
    WINDOW.scope(window, future).await
}

impl Window {
    pub(crate) fn new(registration: Arc<Registration>) -> Arc<Self> {
        Arc::new(Self {
            registration,
            process: OnceLock::new(),
            stopped: AtomicBool::new(false),
            serial: tokio::sync::Mutex::new(()),
            stream: Mutex::new(None),
        })
    }

    pub(crate) fn bind(&self, process: ProcessIdentity) -> Result<(), String> {
        self.registration.snapshot(&process)?;
        self.process
            .set(process)
            .map_err(|_| "audio window is already bound".into())
    }

    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        if let Ok(mut stream) = self.stream.lock() {
            if let Some(stream) = stream.as_mut() {
                stream.sender.take();
                stream.cancel.send_replace(true);
            }
        }
    }

    pub(crate) fn quiescent(&self) -> bool {
        self.stream
            .lock()
            .is_ok_and(|stream| stream.as_ref().is_none_or(|stream| !stream.active))
    }

    fn live(&self) -> Result<crate::clawd::authority::GrantView, String> {
        if self.stopped.load(Ordering::Acquire) {
            return Err("audio window has stopped".into());
        }
        self.registration
            .snapshot(self.process.get().ok_or("audio window is not bound")?)
    }

    async fn request(
        self: &Arc<Self>,
        request: Request,
        client: &ClientIdentity,
        authority: &Decision,
    ) -> Result<Value, BrokerError> {
        let (session, id) = request.identity();
        if id.len() != 32
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(BrokerError::execution(
                "audio stream correlation is invalid",
            ));
        }
        if session != self.registration.session_id
            || authority.session_id() != Some(session)
            || authority.owner_uid() != self.registration.owner()
            || authority.app_id() != Some(self.registration.launch.app_id())
            || client.require_uid().map_err(BrokerError::authorization)?
                != self.registration.owner()
        {
            return Err(BrokerError::authorization(
                "audio stream belongs to another GUI",
            ));
        }
        let pcm = match &request {
            Request::Write { data, .. } => Some(decode_pcm(data.as_str())?),
            _ => None,
        };
        let _serial = tokio::time::timeout(Duration::from_secs(2), self.serial.lock())
            .await
            .map_err(|_| BrokerError::unavailable("audio relay is busy"))?;
        let view = self.live().map_err(BrokerError::authorization)?;
        if view.id.audit_ref() != *authority.grant_ref()
            || view.generation != authority.generation()
        {
            return Err(BrokerError::authorization("audio GUI grant changed"));
        }
        let _authorized = authority
            .require(Cap::new(Verb::DEVICE_AUDIO, Scope::name("output")))
            .map_err(BrokerError::authorization)?;
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
                    need.verb == Verb::DEVICE_AUDIO
                        && match &need.scope {
                            crate::caps::manifest::ScopeBinding::Fixed { scope } => {
                                scope.covers(&Scope::name("output"))
                            }
                            crate::caps::manifest::ScopeBinding::Wild => true,
                            _ => false,
                        }
                })
        {
            return Err(BrokerError::authorization(
                "audio output requires this GUI's own explicit declaration",
            ));
        }
        if matches!(request, Request::Open { .. }) {
            let mut retained = self
                .stream
                .lock()
                .map_err(|_| BrokerError::unavailable("audio custody is poisoned"))?;
            if let Some(stream) = retained.as_ref() {
                if stream.id == id {
                    return Ok(
                        json!({"version":1,"stream_id":id,"state":if stream.active {"open"} else {"closed"}}),
                    );
                }
                if stream.active {
                    return Err(BrokerError::unavailable(
                        "this GUI already owns an audio stream",
                    ));
                }
            }
            let slot = STREAMS
                .try_acquire()
                .map_err(|_| BrokerError::unavailable("audio output is at capacity"))?;
            let producer = kernel_peer(client).map_err(BrokerError::authorization)?;
            let process = self
                .process
                .get()
                .ok_or_else(|| BrokerError::authorization("audio GUI is not bound"))?;
            let environment = DesktopEnvironment::for_user(
                self.registration.owner(),
                process.gid(),
                self.registration.home.clone(),
                self.registration.launcher.pid() as u32,
            )
            .map_err(BrokerError::execution)?;
            let program = Path::new("/usr/bin/pw-cat");
            crate::provenance::fsec::require_secure_location(program, &[0]).map_err(|_| {
                BrokerError::unavailable("install the OS pipewire-bin audio provider")
            })?;
            let mut command = configured_user_command(
                program,
                &[
                    "--playback".into(),
                    "--raw".into(),
                    "--format".into(),
                    "s16".into(),
                    "--rate".into(),
                    "48000".into(),
                    "--channels".into(),
                    "2".into(),
                    "-".into(),
                ],
                &environment,
            );
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let mut command = tokio::process::Command::from(command);
            command.kill_on_drop(true);
            let mut child = command.spawn().map_err(|error| {
                BrokerError::execution(format!("start OS audio output: {error}"))
            })?;
            let input = child.stdin.take().expect("piped OS audio input");
            let (sender, receiver) = tokio::sync::mpsc::channel(4);
            let (cancel, cancelled) = tokio::sync::watch::channel(false);
            *retained = Some(Stream {
                id: id.into(),
                sequence: 0,
                active: true,
                sender: Some(sender),
                cancel,
            });
            let window = self.clone();
            let id = id.to_string();
            let binding = Binding {
                grant: view.id.audit_ref(),
                generation: view.generation,
                producer,
            };
            let context = crate::paths::RoutedPathContext::for_owner(
                self.registration.owner(),
                self.registration.home.clone(),
            );
            tokio::spawn(context.scope(async move {
                let _slot = slot;
                window
                    .monitor(id, child, input, receiver, cancelled, binding)
                    .await;
            }));
            return Ok(json!({"version":1,"stream_id":request.identity().1,"state":"open"}));
        }
        let (sender, sequence) = {
            let mut retained = self
                .stream
                .lock()
                .map_err(|_| BrokerError::unavailable("audio custody is poisoned"))?;
            let stream = retained
                .as_mut()
                .filter(|stream| stream.id == id && stream.active)
                .ok_or_else(|| BrokerError::execution("audio stream is not active in this GUI"))?;
            let sequence = match &request {
                Request::Write { sequence, .. } | Request::Close { sequence, .. } => *sequence,
                Request::Open { .. } => unreachable!(),
            };
            if sequence != stream.sequence {
                return Err(BrokerError::execution(
                    "audio sequence is not current; do not replay PCM",
                ));
            }
            if pcm.is_none() {
                stream.sender.take();
                return Ok(json!({"version":1,"stream_id":id,"state":"closing"}));
            }
            (
                stream
                    .sender
                    .as_ref()
                    .ok_or_else(|| BrokerError::execution("audio stream is closing"))?
                    .clone(),
                sequence,
            )
        };
        tokio::time::timeout(
            Duration::from_secs(2),
            sender.send(pcm.expect("validated PCM")),
        )
        .await
        .map_err(|_| BrokerError::unavailable("audio output backpressure timed out"))?
        .map_err(|_| BrokerError::execution("OS audio output ended"))?;
        let mut retained = self
            .stream
            .lock()
            .map_err(|_| BrokerError::unavailable("audio custody is poisoned"))?;
        let stream = retained
            .as_mut()
            .filter(|stream| stream.id == id && stream.active)
            .ok_or_else(|| BrokerError::execution("audio output ended after accepting PCM"))?;
        stream.sequence = sequence
            .checked_add(1)
            .ok_or_else(|| BrokerError::execution("audio sequence exhausted"))?;
        Ok(json!({"version":1,"stream_id":id,"sequence":stream.sequence,"state":"open"}))
    }

    async fn monitor(
        self: Arc<Self>,
        id: String,
        mut child: tokio::process::Child,
        mut input: tokio::process::ChildStdin,
        mut receiver: tokio::sync::mpsc::Receiver<Vec<u8>>,
        mut cancelled: tokio::sync::watch::Receiver<bool>,
        binding: Binding,
    ) {
        let mut tick = tokio::time::interval(Duration::from_millis(200));
        let producer = binding.producer.pidfd();
        loop {
            tokio::select! {
                _ = cancelled.changed() => break,
                _ = tick.tick() => {
                    if binding.producer.assert_current().is_err()
                        || !producer.as_ref().is_ok_and(|fd|
                            claw_display_control::identity::pidfd_exited(fd.as_fd()).is_ok_and(|exited|!exited))
                        || !self.live().is_ok_and(|view|
                            view.id.audit_ref() == binding.grant && view.generation == binding.generation)
                        || !child.try_wait().is_ok_and(|status| status.is_none())
                    { break; }
                }
                block = receiver.recv() => {
                    let Some(block) = block else { break; };
                    if !tokio::time::timeout(Duration::from_secs(2), input.write_all(&block)).await.is_ok_and(|result|result.is_ok()) { break; }
                }
            }
        }
        drop(input);
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
        if let Ok(mut retained) = self.stream.lock() {
            if let Some(stream) = retained.as_mut().filter(|stream| stream.id == id) {
                stream.active = false;
                stream.sender.take();
            }
        }
    }
}

pub(crate) async fn control(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, BrokerError> {
    let window = WINDOW.try_with(Arc::clone).map_err(|_| {
        BrokerError::authorization("audio output requires the exact private GUI endpoint")
    })?;
    let request = serde_json::from_value(params)
        .map_err(|_| BrokerError::execution("invalid closed audio request"))?;
    window.request(request, client, authority).await
}

pub(crate) fn cli() -> Result<(), String> {
    let session = std::env::var("COS_SESSION").map_err(|_| "audio requires its GUI session")?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    let call = |params: Value| -> Result<Value, String> {
        let request: Request =
            serde_json::from_value(params).map_err(|_| "invalid GUI audio request")?;
        let response = crate::clawd::client::request_gui_resource_blocking(
            crate::clawd::protocol::Request::build(
                crate::clawd::routes::Command::GuiAudioControl,
                serde_json::to_value(request).map_err(|error| error.to_string())?,
            ),
        )
        .map_err(|error| format!("audio output is unconfirmed: {error}"))?;
        if response.ok {
            response.result.ok_or_else(|| "audio result missing".into())
        } else {
            Err(response
                .error
                .map(|error| error.message)
                .unwrap_or_else(|| "audio output refused".into()))
        }
    };
    let opened = call(json!({"action":"open","session":session,"stream_id":id}))?;
    if opened["state"] != "open" {
        return Err("audio stream was not opened".into());
    }
    println!("{{\"version\":1,\"ready\":true}}");
    std::io::stdout()
        .flush()
        .map_err(|error| error.to_string())?;
    let mut sequence = 0u64;
    let result: Result<(), String> = (|| {
        let mut input = std::io::stdin().lock();
        let mut pending = Vec::new();
        let mut buffer = [0; PCM_BYTES];
        loop {
            let bytes = input.read(&mut buffer).map_err(|error| error.to_string())?;
            if bytes == 0 {
                break;
            }
            pending.extend_from_slice(&buffer[..bytes]);
            let count = (pending.len().min(PCM_BYTES) / 4) * 4;
            if count == 0 {
                continue;
            }
            let data = base64::engine::general_purpose::STANDARD.encode(&pending[..count]);
            call(
                json!({"action":"write","session":session,"stream_id":id,"sequence":sequence,"data":data}),
            )?;
            pending.drain(..count);
            sequence = sequence.checked_add(1).ok_or("audio sequence exhausted")?;
        }

        if !pending.is_empty() {
            return Err("PCM ended with a partial stereo frame".into());
        }
        Ok(())
    })();
    let closed =
        call(json!({"action":"close","session":session,"stream_id":id,"sequence":sequence}));
    result?;
    closed?;
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/gui/audio.rs"
    ));
}
