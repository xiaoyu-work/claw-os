use std::collections::HashMap;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use claw_display_control::identity::pidfd_exited;
use claw_display_control::wire::{
    monotonic_ms, AuthorityCommand, AuthorityReply, CompositorCommand, CompositorReply, LoginReply,
    LoginRequest, LOGIN_SOCKET,
};
use claw_display_control::workload::{SessionGroup, Workload};
use claw_display_control::{
    Connection, Epoch, Error, InstanceId, KernelLogin, Listener, ProcessIdentity,
};

const MAX_DISPLAYS: usize = 16;
const STARTUP_LIMIT: Duration = Duration::from_secs(20);
const LEASE_MS: u64 = 2000;
const RETIRE_RETRY_MIN: Duration = Duration::from_millis(250);
const RETIRE_RETRY_MAX: Duration = Duration::from_secs(2);
static REGISTRY: OnceLock<Arc<Registry>> = OnceLock::new();

pub struct Display {
    pub epoch: Epoch,
    pub login: KernelLogin,
    pub host: ProcessIdentity,
    pub wayland_display: Option<String>,
    pub workload: Option<Arc<Workload>>,
    control: SyncSender<GuiCall>,
    verified_retired: Arc<AtomicBool>,
}

struct GuiCall {
    command: CompositorCommand,
    descriptors: Vec<OwnedFd>,
    reply: SyncSender<Result<CompositorReply, Error>>,
    deadline: Instant,
}

#[derive(Clone)]
pub(crate) struct GuiControl {
    pub epoch: Epoch,
    pub owner_uid: u32,
    pub workload: Arc<Workload>,
    sender: SyncSender<GuiCall>,
    verified_retired: Arc<AtomicBool>,
}

impl GuiControl {
    pub fn of(display: &Mutex<Display>) -> Result<Self, Error> {
        let display = display.lock().map_err(|_| Error::Identity)?;
        if display.wayland_display.is_none() {
            return Err(Error::Protocol("GUI display is not active"));
        }
        Ok(Self {
            epoch: display.epoch,
            owner_uid: display.login.owner_uid,
            workload: display.workload.clone().ok_or(Error::Identity)?,
            sender: display.control.clone(),
            verified_retired: display.verified_retired.clone(),
        })
    }

    pub fn exchange(
        &self,
        command: CompositorCommand,
        descriptors: Vec<OwnedFd>,
        deadline: Instant,
    ) -> Result<CompositorReply, Error> {
        let (reply, result) = mpsc::sync_channel(1);
        self.sender
            .try_send(GuiCall {
                command,
                descriptors,
                reply,
                deadline,
            })
            .map_err(|_| Error::Protocol("GUI display control is busy or retired"))?;
        result
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| Error::Timeout)?
    }

    pub fn retire(&self, instance: InstanceId, deadline: Instant) -> Result<(), Error> {
        if self.verified_retired.load(Ordering::Acquire) {
            return Ok(());
        }
        let reply = self.exchange(
            CompositorCommand::Retire {
                epoch: self.epoch,
                instance,
            },
            Vec::new(),
            deadline,
        );
        if self.verified_retired.load(Ordering::Acquire) {
            return Ok(());
        }
        match reply? {
            CompositorReply::Retired {
                epoch,
                instance: received,
            } if epoch == self.epoch && received == instance => Ok(()),
            _ => Err(Error::Protocol("GUI retirement was not acknowledged")),
        }
    }
}

pub struct Registry {
    displays: Mutex<HashMap<u32, Arc<Mutex<Display>>>>,
}

impl Registry {
    pub fn get() -> Result<&'static Arc<Self>, Error> {
        REGISTRY.get().ok_or(Error::Protocol(
            "authenticated display service is unavailable",
        ))
    }

    pub fn start() -> Result<Arc<Self>, Error> {
        if ProcessIdentity::current()?.uid() != 0 {
            return Err(Error::Identity);
        }
        if REGISTRY.get().is_some() {
            return Err(Error::Protocol("display service already initialized"));
        }
        prepare_listener_path()?;
        let listener = Listener::bind(Path::new(LOGIN_SOCKET), 0)?;
        let registry = Arc::new(Self {
            displays: Mutex::new(HashMap::new()),
        });
        REGISTRY
            .set(registry.clone())
            .map_err(|_| Error::Protocol("display service initialized concurrently"))?;
        let service = registry.clone();
        std::thread::Builder::new()
            .name("display-login".to_string())
            .spawn(move || loop {
                match listener.accept() {
                    Ok(Some(connection)) => {
                        if let Err(error) = service.register(connection) {
                            tracing::warn!(%error, "Root display registration refused");
                        }
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                    Err(error) => {
                        tracing::error!(%error, "Root display listener failed");
                        break;
                    }
                }
            })?;
        Ok(registry)
    }

    fn register(self: &Arc<Self>, connection: Connection) -> Result<(), Error> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut packet = connection.receive_first::<LoginRequest>(0, deadline)?;
        let authenticator = packet.sender;
        let login = KernelLogin::of(&authenticator)?;
        let anchor = SessionGroup::of(&authenticator)?;
        let host_pidfd = packet.descriptors.pop().ok_or(Error::Identity)?;
        let host = ProcessIdentity::from_pidfd(host_pidfd.as_fd())?;
        let channel = Connection::from_owned(packet.descriptors.pop().ok_or(Error::Identity)?)?;
        if host.uid() != 0
            || channel.peer()? != authenticator
            || KernelLogin::of(&host)? != login
            || SessionGroup::of(&host)?.path() != anchor.path()
            || parent_pid(&host)? != authenticator.pid()
        {
            return Err(Error::Identity);
        }
        let epoch = Epoch::random()?;
        let (control, requests) = mpsc::sync_channel(16);
        let display = Arc::new(Mutex::new(Display {
            epoch,
            login,
            host: host.clone(),
            wayland_display: None,
            workload: None,
            control,
            verified_retired: Arc::new(AtomicBool::new(false)),
        }));
        self.insert_display(login.session_id, display.clone())?;
        let result = (|| {
            channel.send(
                &AuthorityCommand::Bind {
                    epoch,
                    owner_uid: login.owner_uid,
                    audit_session: login.session_id,
                },
                &[],
                deadline,
            )?;
            let broker_pidfd = ProcessIdentity::current()?.pidfd()?;
            connection.send(
                &LoginReply::Registered { epoch },
                &[broker_pidfd.as_fd()],
                deadline,
            )?;
            let registry = self.clone();
            let authenticator_pidfd = authenticator.pidfd()?;
            std::thread::Builder::new()
                .name("display-authority".to_string())
                .spawn(move || {
                    let result = monitor(
                        &display,
                        &channel,
                        &anchor,
                        &host,
                        &host_pidfd,
                        &authenticator_pidfd,
                        &requests,
                    );
                    let needs_retirement = result.is_err();
                    if let Err(error) = result {
                        tracing::error!(%error, "display authority retired after failure");
                    }
                    registry.finish_retirement(login.session_id, &display, || {
                        if needs_retirement {
                            retire_display(&display, &channel)
                        } else {
                            Ok(())
                        }
                    });
                })?;
            Ok(())
        })();
        if result.is_err() {
            self.displays
                .lock()
                .map_err(|_| Error::Identity)?
                .remove(&login.session_id);
        }
        result
    }

    fn insert_display(&self, session: u32, display: Arc<Mutex<Display>>) -> Result<(), Error> {
        let mut displays = self.displays.lock().map_err(|_| Error::Identity)?;
        if displays.len() >= MAX_DISPLAYS || displays.contains_key(&session) {
            return Err(Error::Protocol(
                "duplicate login or display capacity exceeded",
            ));
        }
        displays.insert(session, display);
        Ok(())
    }

    fn finish_retirement<F>(&self, session: u32, display: &Arc<Mutex<Display>>, mut retire: F)
    where
        F: FnMut() -> Result<(), Error>,
    {
        let mut delay = RETIRE_RETRY_MIN;
        loop {
            let result = retire().and_then(|()| {
                let mut entries = self.displays.lock().map_err(|_| Error::Identity)?;
                if entries
                    .get(&session)
                    .is_some_and(|entry| Arc::ptr_eq(entry, display))
                {
                    entries.remove(&session);
                }
                Ok(())
            });
            match result {
                Ok(()) => return,
                Err(error) => {
                    tracing::error!(%error, session,
                        retry_ms = delay.as_millis(),
                        "display retirement remains quarantined; retrying");
                    std::thread::sleep(delay);
                    delay = delay.saturating_mul(2).min(RETIRE_RETRY_MAX);
                }
            }
        }
    }

    pub fn for_process(&self, process: &ProcessIdentity) -> Result<Arc<Mutex<Display>>, Error> {
        let login = KernelLogin::of(process)?;
        if process.uid() != login.owner_uid {
            return Err(Error::Identity);
        }
        let display = self
            .displays
            .lock()
            .map_err(|_| Error::Identity)?
            .get(&login.session_id)
            .cloned()
            .ok_or(Error::Protocol("this login has no authenticated display"))?;
        {
            let state = display.lock().map_err(|_| Error::Identity)?;
            if state.login != login || state.wayland_display.is_none() {
                return Err(Error::Protocol("authenticated display is not ready"));
            }
            state.host.assert_current()?;
        }
        Ok(display)
    }
}

fn monitor(
    display: &Mutex<Display>,
    channel: &Connection,
    anchor: &SessionGroup,
    host: &ProcessIdentity,
    host_pidfd: &OwnedFd,
    authenticator_pidfd: &OwnedFd,
    requests: &Receiver<GuiCall>,
) -> Result<(), Error> {
    let started = Instant::now();
    let mut pending = HashMap::<InstanceId, (u8, GuiCall)>::new();
    let mut timed_out = std::collections::HashSet::new();
    loop {
        if pidfd_exited(host_pidfd.as_fd())? || pidfd_exited(authenticator_pidfd.as_fd())? {
            return Err(Error::Closed);
        }
        let (epoch, ready) = {
            let state = display.lock().map_err(|_| Error::Identity)?;
            (state.epoch, state.wayland_display.is_some())
        };
        if !ready && started.elapsed() >= STARTUP_LIMIT {
            return Err(Error::Timeout);
        }
        channel.send(
            &AuthorityCommand::Heartbeat {
                epoch,
                expires_monotonic_ms: monotonic_ms()?
                    .checked_add(LEASE_MS)
                    .ok_or(Error::Timeout)?,
            },
            &[],
            Instant::now() + Duration::from_millis(250),
        )?;
        while let Ok(call) = requests.try_recv() {
            let admission = control_target(&call.command, epoch).and_then(|(instance, kind)| {
                if !ready || pending.len() >= 16 || pending.contains_key(&instance) {
                    return Err(Error::Protocol(
                        "GUI control is not ready or already pending",
                    ));
                }
                if Instant::now() >= call.deadline {
                    return Err(Error::Timeout);
                }
                Ok((instance, kind))
            });
            match admission {
                Ok((instance, kind)) => {
                    let descriptors: Vec<_> = call.descriptors.iter().map(AsFd::as_fd).collect();
                    channel.send(
                        &AuthorityCommand::Gui {
                            command: call.command.clone(),
                        },
                        &descriptors,
                        call.deadline,
                    )?;
                    pending.insert(instance, (kind, call));
                }
                Err(error) => {
                    if call.reply.send(Err(error)).is_err() {
                        tracing::debug!("GUI control requester ended before admission");
                    }
                }
            }
        }
        let expired: Vec<_> = pending
            .iter()
            .filter_map(|(instance, (_, call))| {
                (Instant::now() >= call.deadline).then_some(*instance)
            })
            .collect();
        for instance in expired {
            let (_, call) = pending.remove(&instance).ok_or(Error::Identity)?;
            if call.reply.send(Err(Error::Timeout)).is_err() {
                tracing::debug!("expired GUI control requester already ended");
            }
            timed_out.insert(instance);
            if timed_out.len() > 4096 {
                return Err(Error::Protocol(
                    "GUI control retirement correlation limit reached",
                ));
            }
            channel.send(
                &AuthorityCommand::Gui {
                    command: CompositorCommand::Retire { epoch, instance },
                },
                &[],
                Instant::now() + Duration::from_millis(250),
            )?;
        }
        match channel.receive::<AuthorityReply>(host, Instant::now() + Duration::from_millis(100)) {
            Ok(mut packet) => match packet.message {
                AuthorityReply::Prepared {
                    epoch: received_epoch,
                } => {
                    let mut state = display.lock().map_err(|_| Error::Identity)?;
                    if received_epoch != epoch || state.workload.is_some() {
                        return Err(Error::Protocol("duplicate or stale display preparation"));
                    }
                    state.workload = Some(Arc::new(Workload::receive(
                        packet.descriptors.pop().ok_or(Error::Identity)?,
                        anchor,
                        epoch,
                    )?));
                    channel.send(
                        &AuthorityCommand::Prepared { epoch },
                        &[],
                        Instant::now() + Duration::from_millis(250),
                    )?;
                }
                AuthorityReply::Ready {
                    epoch: received_epoch,
                    wayland_display,
                } => {
                    let mut state = display.lock().map_err(|_| Error::Identity)?;
                    if received_epoch != epoch
                        || state.workload.is_none()
                        || state.wayland_display.is_some()
                    {
                        return Err(Error::Protocol(
                            "unprepared, duplicate or stale display readiness",
                        ));
                    }
                    state.wayland_display = Some(wayland_display);
                }
                AuthorityReply::Retired {
                    epoch: received_epoch,
                } if received_epoch == epoch => {
                    retire_display(display, channel)?;
                    channel.send(
                        &AuthorityCommand::Retired { epoch },
                        &[],
                        Instant::now() + Duration::from_millis(250),
                    )?;
                    return Ok(());
                }
                AuthorityReply::Gui { reply } => {
                    let (instance, kind) = reply_target(&reply, epoch)?;
                    if kind == 3 {
                        timed_out.remove(&instance);
                    }
                    if matches!(kind, 3 | 5) {
                        crate::clawd::gui::compositor_retired(epoch, instance)
                            .map_err(|_| Error::Protocol("GUI retirement notification failed"))?;
                    }
                    if kind == 5 {
                        tracing::debug!(instance = %Epoch(instance.0).label(),
                            "compositor ended an inactive GUI instance");
                        continue;
                    }
                    let Some((expected, call)) = pending.remove(&instance) else {
                        if kind == 3 {
                            tracing::info!(instance = %Epoch(instance.0).label(),
                                "compositor retired an expired GUI instance");
                            continue;
                        }
                        if timed_out.contains(&instance) {
                            tracing::warn!(instance = %Epoch(instance.0).label(),
                                "ignoring a late response for an already retiring GUI instance");
                            continue;
                        }
                        return Err(Error::Protocol("unsolicited GUI control response"));
                    };
                    if expected != kind && kind != 4 && kind != 3 {
                        return Err(Error::Protocol("GUI control response changed transition"));
                    }
                    if call.reply.send(Ok(reply)).is_err() {
                        tracing::warn!("GUI control requester disappeared; retiring its instance");
                        channel.send(
                            &AuthorityCommand::Gui {
                                command: CompositorCommand::Retire { epoch, instance },
                            },
                            &[],
                            Instant::now() + Duration::from_millis(250),
                        )?;
                    }
                }
                _ => return Err(Error::Protocol("stale display epoch")),
            },
            Err(Error::Timeout) => {}
            Err(error) => return Err(error),
        }
    }
}

fn control_target(command: &CompositorCommand, expected: Epoch) -> Result<(InstanceId, u8), Error> {
    let (epoch, instance, kind) = match command {
        CompositorCommand::Creator {
            epoch, instance, ..
        } => (*epoch, *instance, 1),
        CompositorCommand::Refresh {
            epoch, instance, ..
        } => (*epoch, *instance, 2),
        CompositorCommand::Retire { epoch, instance } => (*epoch, *instance, 3),
        _ => return Err(Error::Protocol("invalid GUI control transition")),
    };
    if epoch != expected {
        return Err(Error::Protocol("GUI control used a stale epoch"));
    }
    Ok((instance, kind))
}

fn reply_target(reply: &CompositorReply, expected: Epoch) -> Result<(InstanceId, u8), Error> {
    let (epoch, instance, kind) = match reply {
        CompositorReply::CreatorReady { epoch, instance } => (*epoch, *instance, 1),
        CompositorReply::Refreshed { epoch, instance } => (*epoch, *instance, 2),
        CompositorReply::Retired { epoch, instance } => (*epoch, *instance, 3),
        CompositorReply::InstanceEnded { epoch, instance } => (*epoch, *instance, 5),
        CompositorReply::Rejected {
            epoch, instance, ..
        } => (*epoch, *instance, 4),
        _ => return Err(Error::Protocol("invalid GUI control response")),
    };
    if epoch != expected {
        return Err(Error::Protocol("GUI response used a stale epoch"));
    }
    Ok((instance, kind))
}

fn retire_display(display: &Mutex<Display>, channel: &Connection) -> Result<(), Error> {
    let (epoch, workload, verified_retired) = {
        let mut state = display.lock().map_err(|_| Error::Identity)?;
        state.wayland_display = None;
        (state.epoch, state.workload.clone(), state.verified_retired.clone())
    };
    let send = channel.send(
        &AuthorityCommand::Shutdown { epoch },
        &[],
        Instant::now() + Duration::from_millis(250),
    );
    if !verified_retired.load(Ordering::Acquire) {
        if let Some(workload) = workload {
            workload.retire(Instant::now() + Duration::from_secs(5))?;
        }
        // PAM may remove this empty cgroup while GUI services finish retiring.
        verified_retired.store(true, Ordering::Release);
    }
    crate::clawd::gui::retire_epoch(epoch, Instant::now() + Duration::from_secs(5))
        .map_err(|error| Error::Io(std::io::Error::other(error)))?;
    if let Err(error) = send {
        tracing::debug!(%error, "display control already unavailable during retirement");
    }
    Ok(())
}

fn parent_pid(process: &ProcessIdentity) -> Result<i32, Error> {
    let stat = claw_display_control::identity::read_bounded(
        &format!("/proc/{}/stat", process.pid()),
        8192,
    )?;
    stat.rsplit_once(')')
        .ok_or(Error::Identity)?
        .1
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .ok_or(Error::Identity)
}

fn prepare_listener_path() -> Result<(), Error> {
    let path = Path::new(LOGIN_SOCKET);
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.uid() != 0 || !metadata.file_type().is_socket() || metadata.nlink() != 1 {
                return Err(Error::Protocol("unsafe existing display login socket"));
            }
            match Connection::connect(path) {
                Ok(_) => return Err(Error::Protocol("display login service is already running")),
                Err(Error::Io(error)) if error.raw_os_error() == Some(libc::ECONNREFUSED) => {
                    std::fs::remove_file(path)?;
                }
                Err(error) => return Err(error),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/display_session/registry.rs"
    ));
}
