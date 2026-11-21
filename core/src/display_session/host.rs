use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use claw_display_control::identity::pidfd_exited;
use claw_display_control::wire::{
    monotonic_ms, AuthorityCommand, AuthorityReply, CompositorCommand, CompositorReply, PamCommand,
    PamReply, SessionEvent, SessionRequest, LaunchTarget, COMPOSITOR, HOST_RETIRE_TIMEOUT,
    GuiRefusal, MENU_LAUNCH_DIGEST, MENU_LAUNCH_PUBLISHER, SESSION_DIRECTORY,
};
use claw_display_control::{Connection, Epoch, Error, KernelLogin, Listener, ProcessIdentity};

use super::containment::{self, Containment};
use super::locker::{Locker, Status as LockStatus, LOCK_START_MS};
use super::{registry::parent_pid, SHELL_APP};

const STARTUP: Duration = Duration::from_secs(20);
const STOP: Duration = Duration::from_secs(5);
const TICK: Duration = Duration::from_millis(10);
const CONTROL_IO: Duration = Duration::from_millis(250);
const MAX_SUBSCRIBERS: usize = 32;
const MAX_APP_LAUNCHERS: usize = 16;

struct Controllers {
    parent: ProcessIdentity,
    parent_pidfd: OwnedFd,
    pam: Connection,
    broker: ProcessIdentity,
    broker_pidfd: OwnedFd,
    authority: Connection,
    epoch: Epoch,
    login: KernelLogin,
    expires_ms: u64,
    locale: claw_display_control::locale::LocaleEnvironment,
}

impl Controllers {
    fn receive() -> Result<Self, Error> {
        let current = ProcessIdentity::current()?;
        let parent = ProcessIdentity::parent()?;
        if current.uid() != 0
            || parent.uid() != 0
            || std::env::args_os().skip(1).collect::<Vec<_>>() != ["--supervisor"]
            || std::env::vars_os().next().is_some()
        {
            return Err(Error::Identity);
        }
        let login = KernelLogin::of(&current)?;
        if KernelLogin::of(&parent)? != login {
            return Err(Error::UnauthenticatedLogin);
        }
        // Only the Root PAM spawner supplies these inherited, otherwise closed descriptors.
        let pam = unsafe { Connection::inherit(3) }?;
        let authority = unsafe { Connection::inherit(4) }?;
        if pam.peer()? != parent || authority.peer()? != parent {
            return Err(Error::Identity);
        }
        let deadline = Instant::now() + STARTUP;
        let mut start = pam.receive::<PamCommand>(&parent, deadline)?;
        let PamCommand::Start { epoch, locale } = start.message else {
            return Err(Error::Protocol("display supervisor has no PAM start"));
        };
        let broker_pidfd = start.descriptors.pop().ok_or(Error::Identity)?;
        let broker = ProcessIdentity::from_pidfd(broker_pidfd.as_fd())?;
        if broker.uid() != 0 || broker == parent || broker == current {
            return Err(Error::Identity);
        }
        let binding = authority.receive::<AuthorityCommand>(&broker, deadline)?;
        if !matches!(binding.message, AuthorityCommand::Bind {
            epoch: bound, owner_uid, audit_session,
        } if bound == epoch && owner_uid == login.owner_uid && audit_session == login.session_id)
        {
            return Err(Error::Protocol("PAM and broker display bindings differ"));
        }
        let parent_pidfd = parent.pidfd()?;
        let mut controllers = Self {
            parent,
            parent_pidfd,
            pam,
            broker,
            broker_pidfd,
            authority,
            epoch,
            login,
            expires_ms: 0,
            locale,
        };
        controllers.heartbeat(deadline)?;
        Ok(controllers)
    }

    fn heartbeat(&mut self, deadline: Instant) -> Result<(), Error> {
        match self
            .authority
            .receive::<AuthorityCommand>(&self.broker, deadline)?
            .message
        {
            AuthorityCommand::Heartbeat {
                epoch,
                expires_monotonic_ms,
            } => self.renew(epoch, expires_monotonic_ms),
            _ => Err(Error::Protocol("expected authenticated display heartbeat")),
        }
    }

    fn renew(&mut self, epoch: Epoch, expires: u64) -> Result<(), Error> {
        let now = monotonic_ms()?;
        if epoch != self.epoch || expires <= now || expires > now.saturating_add(2500) {
            return Err(Error::Protocol("invalid display authority lease"));
        }
        self.expires_ms = expires;
        Ok(())
    }

    fn assert_live(&self) -> Result<(), Error> {
        if monotonic_ms()? >= self.expires_ms
            || pidfd_exited(self.parent_pidfd.as_fd())?
            || pidfd_exited(self.broker_pidfd.as_fd())?
        {
            return Err(Error::Closed);
        }
        Ok(())
    }

    fn live_io_deadline(&self) -> Result<Instant, Error> {
        let now = Instant::now();
        let remaining = self.expires_ms.saturating_sub(monotonic_ms()?);
        Ok(now + CONTROL_IO.min(Duration::from_millis(remaining)))
    }

    fn wait_authority(&mut self, retired: bool, deadline: Instant) -> Result<(), Error> {
        loop {
            let packet = self
                .authority
                .receive::<AuthorityCommand>(&self.broker, deadline)?;
            match packet.message {
                AuthorityCommand::Prepared { epoch } if !retired && epoch == self.epoch => {
                    return Ok(());
                }
                AuthorityCommand::Retired { epoch } if retired && epoch == self.epoch => {
                    return Ok(());
                }
                AuthorityCommand::Heartbeat { epoch, .. } if retired && epoch == self.epoch => {}
                AuthorityCommand::Gui { .. } if retired => {
                    // This workload is already dead. Queued requests cannot run or renew it.
                }
                AuthorityCommand::Launch { epoch, .. } | AuthorityCommand::LaunchNative { epoch, .. }
                    | AuthorityCommand::ShowAgent { epoch, .. }
                    if retired && epoch == self.epoch => {}
                AuthorityCommand::Heartbeat {
                    epoch,
                    expires_monotonic_ms,
                } => {
                    self.renew(epoch, expires_monotonic_ms)?;
                }
                AuthorityCommand::Shutdown { epoch } if retired && epoch == self.epoch => {}
                _ => return Err(Error::Protocol("unexpected display authority transition")),
            }
        }
    }
}

struct Supervisor {
    controllers: Controllers,
    containment: Containment,
    compositor: Child,
    compositor_identity: ProcessIdentity,
    compositor_pidfd: OwnedFd,
    control: Connection,
    display: String,
    listener: Option<Listener>,
    subscribers: Vec<Subscriber>,
    launchers: Vec<(Option<String>, Child)>,
    shell_started: bool,
    shell: Option<(ProcessIdentity, Child)>,
    locker: Option<Locker>,
}

struct Subscriber {
    connection: Connection,
    process: ProcessIdentity,
    pidfd: OwnedFd,
}

impl Supervisor {
    fn start(mut controllers: Controllers) -> Result<Self, Error> {
        let containment = Containment::prepare(&controllers.parent, controllers.epoch)?;
        let descriptor = containment.workload.duplicate()?;
        let deadline = Instant::now() + STARTUP;
        controllers.pam.send(
            &PamReply::Prepared {
                epoch: controllers.epoch,
            },
            &[descriptor.as_fd()],
            deadline,
        )?;
        controllers.authority.send(
            &AuthorityReply::Prepared {
                epoch: controllers.epoch,
            },
            &[descriptor.as_fd()],
            deadline,
        )?;
        let prepared = controllers
            .pam
            .receive::<PamCommand>(&controllers.parent, deadline)?;
        if !matches!(prepared.message, PamCommand::Prepared { epoch } if epoch == controllers.epoch)
        {
            return Err(Error::Protocol("PAM did not retain display containment"));
        }
        controllers.wait_authority(false, deadline)?;
        controllers.assert_live()?;
        let (control, child_control) = Connection::pair()?;
        let mut compositor = spawn_compositor(&controllers, &containment, child_control)?;
        let identity = match ProcessIdentity::child(&compositor) {
            Ok(identity) => identity,
            Err(error) => {
                containment.workload.retire(Instant::now() + STOP)?;
                compositor.wait()?;
                return Err(error);
            }
        };
        let compositor_pidfd = identity.pidfd()?;
        let mut supervisor = Self {
            controllers,
            containment,
            compositor,
            compositor_identity: identity,
            compositor_pidfd,
            control,
            display: String::new(),
            listener: None,
            subscribers: Vec::new(),
            launchers: Vec::new(),
            shell_started: false,
            shell: None,
            locker: None,
        };
        supervisor.control.send(
            &CompositorCommand::Initialize {
                epoch: supervisor.controllers.epoch,
                owner_uid: supervisor.controllers.login.owner_uid,
                audit_session: supervisor.controllers.login.session_id,
            },
            &[],
            deadline,
        )?;
        supervisor.control.send(
            &CompositorCommand::Heartbeat {
                epoch: supervisor.controllers.epoch,
                expires_monotonic_ms: supervisor.controllers.expires_ms,
            },
            &[],
            deadline,
        )?;
        let ready = loop {
            supervisor.poll_authority()?;
            supervisor.controllers.assert_live()?;
            if pidfd_exited(supervisor.compositor_pidfd.as_fd())? {
                return Err(Error::Protocol("compositor exited before activation"));
            }
            if supervisor.control.readable()? {
                break supervisor
                    .control
                    .receive::<CompositorReply>(&supervisor.compositor_identity, deadline)?;
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout);
            }
            std::thread::sleep(TICK);
        };
        match ready.message {
            CompositorReply::Ready {
                epoch,
                wayland_display,
            } if epoch == supervisor.controllers.epoch => supervisor.display = wayland_display,
            _ => {
                return Err(Error::Protocol(
                    "actual compositor did not acknowledge activation",
                ))
            }
        }
        ensure_session_directory()?;
        let endpoint = Path::new(SESSION_DIRECTORY).join(format!(
            "login-{}.sock",
            supervisor.controllers.login.session_id
        ));
        supervisor.listener = Some(Listener::bind(
            &endpoint,
            supervisor.controllers.login.owner_uid,
        )?);
        supervisor.controllers.authority.send(
            &AuthorityReply::Ready {
                epoch: supervisor.controllers.epoch,
                wayland_display: supervisor.display.clone(),
            },
            &[],
            deadline,
        )?;
        supervisor.controllers.pam.send(
            &PamReply::Ready {
                epoch: supervisor.controllers.epoch,
            },
            &[],
            deadline,
        )?;
        Ok(supervisor)
    }

    fn poll_authority(&mut self) -> Result<(), Error> {
        for _ in 0..8 {
            if !self.controllers.authority.readable()? {
                break;
            }
            let packet = self
                .controllers
                .authority
                .receive::<AuthorityCommand>(
                    &self.controllers.broker,
                    self.controllers.live_io_deadline()?,
                )
                .map_err(|error| io_timeout(error, "timed out receiving display authority"))?;
            self.controllers.assert_live()?;
            match packet.message {
                AuthorityCommand::Heartbeat {
                    epoch,
                    expires_monotonic_ms,
                } => {
                    self.controllers.renew(epoch, expires_monotonic_ms)?;
                    self.control.send(
                        &CompositorCommand::Heartbeat {
                            epoch,
                            expires_monotonic_ms,
                        },
                        &[],
                        self.controllers.live_io_deadline()?,
                    )
                    .map_err(|error| io_timeout(error, "timed out forwarding display heartbeat"))?;
                }
                AuthorityCommand::Shutdown { epoch } if epoch == self.controllers.epoch => {
                    return Err(Error::Closed);
                }
                AuthorityCommand::Gui { command: CompositorCommand::Lock { epoch, instance, expires_monotonic_ms } }
                    if epoch == self.controllers.epoch =>
                {
                    self.lock(instance, expires_monotonic_ms)?;
                }
                AuthorityCommand::Gui { command: CompositorCommand::Lock { .. } } => return Err(Error::Identity),
                AuthorityCommand::Gui { command } => {
                    let descriptors: Vec<_> = packet.descriptors.iter().map(AsFd::as_fd).collect();
                    self.control
                        .send(&command, &descriptors, self.controllers.live_io_deadline()?)
                        .map_err(|error| io_timeout(error, "timed out forwarding compositor command"))?;
                }
                AuthorityCommand::Launch { epoch, request, expires_monotonic_ms, target, from_dock }
                    if epoch == self.controllers.epoch =>
                {
                    self.launch(request, expires_monotonic_ms, DesktopProcess::App(target), from_dock)?;
                }
                AuthorityCommand::LaunchNative { epoch, request, expires_monotonic_ms, desktop_id }
                    if epoch == self.controllers.epoch =>
                {
                    self.launch(request, expires_monotonic_ms, DesktopProcess::Native(desktop_id), false)?;
                }
                AuthorityCommand::ShowAgent { epoch, request, expires_monotonic_ms }
                    if epoch == self.controllers.epoch =>
                {
                    self.launch(request, expires_monotonic_ms, DesktopProcess::Agent, false)?;
                }
                _ => {
                    return Err(Error::Protocol(
                        "unexpected running display authority message",
                    ))
                }
            }
        }
        Ok(())
    }

    fn launch(&mut self, request: claw_display_control::InstanceId, expires: u64, process: DesktopProcess, from_dock: bool) -> Result<(), Error> {
        let now = monotonic_ms()?;
        let app_id = match &process {
            DesktopProcess::App(target) => Some(target.app_id.clone()),
            _ => None,
        };
        let submitted = if !self.display.is_empty()
            && !matches!(&process, DesktopProcess::App(target) if target.app_id == SHELL_APP)
            && !(self.locker.is_some() && matches!(&process, DesktopProcess::Native(_)))
            && expires > now && expires <= now.saturating_add(2000)
        {
            if from_dock && app_id.is_some() && self.launchers.iter().any(|(id, _)| id == &app_id) {
                true
            } else if self.launchers.len() < MAX_APP_LAUNCHERS {
                match spawn_desktop_process(&self.controllers, &self.containment, &self.display, process) {
                    Ok(child) => {
                        self.launchers.push((app_id, child));
                        true
                    }
                    Err(error) => {
                        log_failure(c"desktop launcher refused: %s", &error);
                        false
                    }
                }
            } else {
                false
            }
        } else {
            false
        };
        self.controllers.authority.send(
            &AuthorityReply::Launch { epoch: self.controllers.epoch, request, submitted },
            &[],
            self.controllers.live_io_deadline()?,
        )
    }

    fn lock(&mut self, instance: claw_display_control::InstanceId, expires: u64) -> Result<(), Error> {
        let now = monotonic_ms()?;
        let epoch = self.controllers.epoch;
        let failure = if expires <= now || expires > now.saturating_add(LOCK_START_MS) {
            GuiRefusal::Expired
        } else if self.display.is_empty() || self.locker.is_some() {
            GuiRefusal::Unavailable
        } else {
            match Locker::spawn(
                instance, expires, self.controllers.login.owner_uid, self.containment.compositor_membership().as_fd(),
            ) {
                Ok((locker, compositor_fd)) => {
                    self.locker = Some(locker);
                    return self.control.send(
                        &CompositorCommand::Locker { epoch, instance, expires_monotonic_ms: expires },
                        &[compositor_fd.as_fd()],
                        self.controllers.live_io_deadline()?,
                    );
                }
                Err(error) => {
                    log_failure(c"native locker refused before display admission: %s", &error);
                    GuiRefusal::Unavailable
                }
            }
        };
        self.controllers.authority.send(
            &AuthorityReply::Gui { reply: CompositorReply::Rejected { epoch, instance, reason: failure } },
            &[],
            self.controllers.live_io_deadline()?,
        )
    }

    fn watch(&mut self) -> Result<(), Error> {
        loop {
            if pidfd_exited(self.compositor_pidfd.as_fd())? {
                return Err(Error::Protocol("compositor exited during the login"));
            }
            self.poll_authority()?;
            self.controllers.assert_live()?;
            if self.control.readable()? {
                let packet = self
                    .control
                    .receive::<CompositorReply>(
                        &self.compositor_identity,
                        self.controllers.live_io_deadline()?,
                    )
                    .map_err(|error| io_timeout(error, "timed out receiving compositor reply"))?;
                match packet.message {
                    CompositorReply::LockerReady { epoch, instance } => {
                        let locker = self.locker.as_mut()
                            .filter(|locker| locker.instance == instance && epoch == self.controllers.epoch)
                            .ok_or(Error::Identity)?;
                        locker.admit()?;
                        continue;
                    }
                    CompositorReply::Rejected { instance, .. }
                        if self.locker.as_ref().is_some_and(|locker| locker.instance == instance) =>
                            return Err(Error::Protocol("compositor refused the native locker")),
                    _ => {}
                }
                if !matches!(
                    packet.message,
                    CompositorReply::CreatorReady { .. }
                        | CompositorReply::Refreshed { .. }
                        | CompositorReply::SelectionSet { .. }
                        | CompositorReply::DesktopResult { .. }
                        | CompositorReply::DesktopChanged { .. }
                        | CompositorReply::Retired { .. }
                        | CompositorReply::InstanceEnded { .. }
                        | CompositorReply::Rejected { .. }
                ) {
                    return Err(Error::Protocol("unexpected running compositor response"));
                }
                if let Some(locker) = &mut self.locker {
                    match locker.poll()? {
                        LockStatus::Waiting => {}
                        LockStatus::Locked => self.controllers.authority.send(
                            &AuthorityReply::Gui { reply: CompositorReply::Locked {
                                epoch: self.controllers.epoch, instance: locker.instance,
                            } },
                            &[],
                            self.controllers.live_io_deadline()?,
                        )?,
                        LockStatus::Unlocked => { self.locker.take(); }
                    }
                }
                let descriptors: Vec<_> = packet.descriptors.iter().map(AsFd::as_fd).collect();
                self.controllers.authority.send(
                    &AuthorityReply::Gui {
                        reply: packet.message,
                    },
                    &descriptors,
                    self.controllers.live_io_deadline()?,
                )
                .map_err(|error| io_timeout(error, "timed out forwarding compositor reply"))?;
            }
            if self.controllers.pam.readable()? {
                match self
                    .controllers
                    .pam
                    .receive::<PamCommand>(
                        &self.controllers.parent,
                        self.controllers.live_io_deadline()?,
                    )
                {
                    Ok(packet) => match packet.message {
                        PamCommand::Close { epoch } if epoch == self.controllers.epoch => {
                            return Ok(())
                        }
                        _ => {
                            return Err(Error::Protocol("unexpected PAM display lifecycle message"))
                        }
                    },
                    Err(Error::Timeout) => {}
                    Err(error) => return Err(error),
                }
            }
            self.prune_subscribers();
            self.accept_subscriber()?;
            if let Some(status) = self.shell.as_mut()
                .map(|(_, child)| child.try_wait()).transpose()?.flatten()
            {
                let (subscriber, _) = self.shell.take().ok_or(Error::Identity)?;
                self.shell_result(&subscriber, status.success());
            }
            let mut index = 0;
            while index < self.launchers.len() {
                if self.launchers[index].1.try_wait()?.is_some() {
                    self.launchers.swap_remove(index).1.wait()?;
                } else {
                    index += 1;
                }
            }
            std::thread::sleep(TICK);
        }
    }

    fn accept_subscriber(&mut self) -> Result<(), Error> {
        let Some(connection) = self.listener.as_ref().ok_or(Error::Closed)?.accept()? else {
            return Ok(());
        };
        let admission = (|| {
            let packet = connection.receive_first::<SessionRequest>(
                self.controllers.login.owner_uid,
                Instant::now() + Duration::from_millis(100),
            )?;
            if KernelLogin::of(&packet.sender)? != self.controllers.login {
                return Err(Error::Identity);
            }
            if !matches!(packet.message, SessionRequest::Subscribe {}) {
                return Err(Error::Protocol("expected display subscription"));
            }
            if self.subscribers.len() >= MAX_SUBSCRIBERS {
                return Err(Error::Protocol("display subscriber capacity exceeded"));
            }
            let pidfd = packet.sender.pidfd()?;
            connection.send(
                &SessionEvent::Ready {
                    epoch: self.controllers.epoch,
                    wayland_display: self.display.clone(),
                },
                &[],
                Instant::now() + TICK,
            )?;
            Ok((packet.sender, pidfd))
        })();
        match admission {
            Ok((process, pidfd)) => self.subscribers.push(Subscriber {
                connection,
                process,
                pidfd,
            }),
            Err(error) => eprintln!("display session subscription refused: {error}"),
        }
        Ok(())
    }

    fn prune_subscribers(&mut self) {
        let subscribers = std::mem::take(&mut self.subscribers);
        for subscriber in subscribers {
            let live = (|| {
                if pidfd_exited(subscriber.pidfd.as_fd())? {
                    return Ok(false);
                }
                if !subscriber.connection.readable()? {
                    return Ok(true);
                }
                match subscriber
                    .connection
                    .receive::<SessionRequest>(&subscriber.process, Instant::now() + TICK)
                {
                    Err(Error::Closed) => Ok(false),
                    Err(Error::Timeout) => Ok(true),
                    Ok(packet) if matches!(packet.message, SessionRequest::StartShell {}) => {
                        self.start_shell(&subscriber)?;
                        Ok(true)
                    }
                    Ok(_) => Err(Error::Protocol("duplicate display subscription")),
                    Err(error) => Err(error),
                }
            })();
            match live {
                Ok(true) => self.subscribers.push(subscriber),
                Ok(false) => {}
                Err(error) => eprintln!("display session subscription retired: {error}"),
            }
        }
    }

    fn start_shell(&mut self, subscriber: &Subscriber) -> Result<(), Error> {
        const CLIENT: &str = "/usr/local/bin/claw-display-session";
        self.controllers.assert_live()?;
        if self.shell_started
            || parent_pid(&subscriber.process)? != self.controllers.parent.pid()
            || std::fs::read_link(format!("/proc/{}/exe", subscriber.process.pid()))?
                != Path::new(CLIENT)
        {
            return Err(Error::Protocol("system desktop shell startup is reserved for the login client"));
        }
        containment::protected_executable(CLIENT)?;
        self.shell_started = true;
        match spawn_desktop_process(&self.controllers, &self.containment, &self.display, DesktopProcess::Shell) {
            Ok(child) => self.shell = Some((subscriber.process.clone(), child)),
            Err(error) => {
                log_failure(c"system desktop shell launcher refused: %s", &error);
                subscriber.connection.send(
                    &SessionEvent::ShellExited { epoch: self.controllers.epoch, success: false },
                    &[],
                    Instant::now() + CONTROL_IO,
                )?;
            }
        }
        Ok(())
    }

    fn shell_result(&self, process: &ProcessIdentity, success: bool) {
        if let Some(subscriber) = self.subscribers.iter().find(|peer| &peer.process == process) {
            if let Err(error) = subscriber.connection.send(
                &SessionEvent::ShellExited { epoch: self.controllers.epoch, success },
                &[],
                Instant::now() + CONTROL_IO,
            ) {
                eprintln!("desktop shell exit notification failed: {error}");
            }
        }
    }

    fn retire(&mut self, pam_closed: bool) -> Result<(), Error> {
        let started = Instant::now();
        let work_deadline = started + STOP;
        let deadline = started + HOST_RETIRE_TIMEOUT;
        if let Some(listener) = self.listener.take() {
            listener.close()?;
        }
        let shutdown = self.control.send(
            &CompositorCommand::Shutdown {
                epoch: self.controllers.epoch,
            },
            &[],
            work_deadline,
        );
        if let Err(error) = shutdown {
            eprintln!("display control unavailable at retirement: {error}");
        } else if pam_closed {
            let graceful = (Instant::now() + Duration::from_secs(1)).min(work_deadline);
            while !pidfd_exited(self.compositor_pidfd.as_fd())? && Instant::now() < graceful {
                std::thread::sleep(TICK);
            }
        }
        self.containment.retire(work_deadline)?;
        if !pidfd_exited(self.compositor_pidfd.as_fd())? {
            return Err(Error::Protocol(
                "compositor remains alive after workload retirement",
            ));
        }
        self.compositor.wait()?;
        for (_, mut launcher) in self.launchers.drain(..) {
            launcher.wait()?;
        }
        if let Some((_, mut shell)) = self.shell.take() {
            shell.wait()?;
        }
        if let Some(locker) = self.locker.take() {
            locker.reap()?;
        }
        for subscriber in self.subscribers.drain(..) {
            if pidfd_exited(subscriber.pidfd.as_fd())? {
                continue;
            }
            if let Err(error) = subscriber.connection.send(
                &SessionEvent::Ended {
                    epoch: self.controllers.epoch,
                },
                &[],
                Instant::now() + TICK,
            ) {
                eprintln!("display session end notification failed: {error}");
            }
        }
        self.controllers.authority.send(
            &AuthorityReply::Retired {
                epoch: self.controllers.epoch,
            },
            &[],
            deadline,
        )?;
        self.controllers.wait_authority(true, deadline)?;
        if pam_closed {
            self.controllers.pam.send(
                &PamReply::Retired {
                    epoch: self.controllers.epoch,
                },
                &[],
                deadline,
            )?;
            let finished = self
                .controllers
                .pam
                .receive::<PamCommand>(&self.controllers.parent, deadline)?;
            if !matches!(finished.message, PamCommand::Finished { epoch }
                if epoch == self.controllers.epoch)
            {
                return Err(Error::Protocol(
                    "PAM did not acknowledge checked retirement",
                ));
            }
            self.containment.remove_empty_workload()?;
            if let Err(error) = self.containment.restore_empty_login() {
                eprintln!("display login leaf remains logind-owned: {error}");
            }
        }
        Ok(())
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        if let Err(error) = self.containment.retire(Instant::now() + STOP) {
            eprintln!("display supervisor emergency containment failed: {error}");
        }
        if let Ok(true) = pidfd_exited(self.compositor_pidfd.as_fd()) {
            if let Err(error) = self.compositor.wait() {
                eprintln!("display compositor reap failed: {error}");
            }
        }
    }
}

pub fn run() -> Result<(), Error> {
    let controllers = Controllers::receive()?;
    let mut supervisor = Supervisor::start(controllers)?;
    let outcome = supervisor.watch();
    if let Err(error) = &outcome {
        log_failure(c"authenticated display lost: %s", error);
    }
    let retirement = supervisor.retire(outcome.is_ok());
    if let Err(error) = &retirement {
        log_failure(c"authenticated display retirement failed: %s", error);
    }
    match (outcome, retirement) {
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn log_failure(format: &std::ffi::CStr, error: &Error) {
    if let Ok(reason) = std::ffi::CString::new(error.to_string()) {
        // The compositor owns the VT, so terminal diagnostics can be lost at logout.
        unsafe {
            libc::syslog(libc::LOG_DAEMON | libc::LOG_ERR, format.as_ptr(), reason.as_ptr());
        }
    }
}

fn io_timeout(error: Error, phase: &'static str) -> Error {
    match error {
        Error::Timeout => Error::Protocol(phase),
        other => other,
    }
}

enum DesktopProcess {
    Shell,
    App(LaunchTarget),
    Native(String),
    Agent,
}

fn spawn_desktop_process(
    controllers: &Controllers,
    containment: &Containment,
    display: &str,
    process: DesktopProcess,
) -> Result<Child, Error> {
    const COS: &str = "/usr/local/bin/yu";
    const AGENT: &str = "/usr/local/bin/cos-agent-ui";
    let executable = match &process {
        DesktopProcess::Native(_) => "/usr/bin/gtk4-launch",
        DesktopProcess::Agent => AGENT,
        _ => COS,
    };
    containment::protected_executable(executable)?;
    let uid = controllers.login.owner_uid;
    let (gid, name, home) = containment::account(uid)?;
    let username = name.to_str().map_err(|_| Error::Identity)?;
    let runtime = format!("/run/user/{uid}");
    let membership = containment.compositor_membership().as_raw_fd();
    let parent = unsafe { libc::getpid() };
    let mut command = Command::new(executable);
    command
        .env_clear()
        .envs(controllers.locale.iter())
        .env("HOME", &home)
        .env("USER", username)
        .env("LOGNAME", username)
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("COS_APPS_DIR", "/usr/lib/cos/apps")
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("WAYLAND_DISPLAY", display)
        .env("XDG_SESSION_TYPE", "wayland")
        .env("XDG_CURRENT_DESKTOP", "Hyprland")
        .current_dir(&home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    match process {
        DesktopProcess::App(target) => {
            command.args(["app", &target.app_id, "--gui"])
                .env(MENU_LAUNCH_DIGEST, target.digest)
                .env(MENU_LAUNCH_PUBLISHER, target.publisher);
        }
        DesktopProcess::Shell => {
            command.args(["app", SHELL_APP, "--gui"])
                .stdout(std::io::stderr().as_fd().try_clone_to_owned()?);
        }
        DesktopProcess::Native(desktop_id) => {
            command.args(["--", &desktop_id])
                .env_remove("COS_APPS_DIR")
                .env("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={runtime}/bus"))
                .env("XDG_DATA_DIRS", std::env::join_paths([
                    home.join(".local/share/flatpak/exports/share"),
                    "/usr/local/share".into(),
                    "/usr/share".into(),
                    "/var/lib/flatpak/exports/share".into(),
                ]).map_err(|_| Error::Protocol("invalid desktop data path"))?);
        }
        DesktopProcess::Agent => {
            command.env("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={runtime}/bus"));
        }
    }
    unsafe {
        command.pre_exec(move || {
            if libc::getppid() != parent {
                return Err(std::io::Error::from_raw_os_error(libc::EPERM));
            }
            containment::enter(membership)?;
            if libc::syscall(libc::SYS_close_range, 3_u32, u32::MAX, libc::CLOSE_RANGE_CLOEXEC) != 0
                || libc::initgroups(name.as_ptr(), gid) != 0
                || libc::setresgid(gid, gid, gid) != 0
                || libc::setresuid(uid, uid, uid) != 0
                || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(std::io::Error::from_raw_os_error(libc::EPERM));
            }
            libc::umask(0o077);
            Ok(())
        });
    }
    Ok(command.spawn()?)
}

fn spawn_compositor(
    controllers: &Controllers,
    containment: &Containment,
    descriptor: Connection,
) -> Result<Child, Error> {
    containment::protected_executable(COMPOSITOR)?;
    let uid = controllers.login.owner_uid;
    let (gid, name, home) = containment::account(uid)?;
    let runtime = format!("/run/user/{uid}");
    let metadata = std::fs::symlink_metadata(&runtime)?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(Error::Protocol(
            "login runtime directory is not owner-private",
        ));
    }
    let inherited =
        unsafe { libc::fcntl(descriptor.as_fd().as_raw_fd(), libc::F_DUPFD_CLOEXEC, 64) };
    if inherited < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let inherited = unsafe { OwnedFd::from_raw_fd(inherited) };
    let raw = inherited.as_raw_fd();
    let membership = containment.compositor_membership().as_raw_fd();
    let parent_pid = unsafe { libc::getpid() };
    let mut command = Command::new(COMPOSITOR);
    command
        .env_clear()
        .envs(controllers.locale.iter())
        .env("HOME", &home)
        .env(
            "USER",
            std::ffi::OsStr::new(name.to_str().map_err(|_| Error::Identity)?),
        )
        .env(
            "LOGNAME",
            std::ffi::OsStr::new(name.to_str().map_err(|_| Error::Identity)?),
        )
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("XDG_RUNTIME_DIR", &runtime)
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={runtime}/bus"),
        )
        .env("XDG_SESSION_TYPE", "wayland")
        .env("XDG_CURRENT_DESKTOP", "Hyprland")
        .env("XDG_SESSION_DESKTOP", "Hyprland")
        .env("LIBINPUT_QUIRKS_DIR", "/usr/share/claw-hyprland/libinput")
        .env("CLAW_DISPLAY_CONTROL_FD", "3")
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    unsafe {
        command.pre_exec(move || {
            if libc::getppid() != parent_pid {
                return Err(std::io::Error::from_raw_os_error(libc::EPERM));
            }
            containment::enter(membership)?;
            if libc::dup2(raw, 3) < 0
                || libc::syscall(
                    libc::SYS_close_range,
                    4_u32,
                    u32::MAX,
                    libc::CLOSE_RANGE_CLOEXEC,
                ) != 0
                || libc::initgroups(name.as_ptr(), gid) != 0
                || libc::setresgid(gid, gid, gid) != 0
                || libc::setresuid(uid, uid, uid) != 0
                || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0
                || libc::getppid() != parent_pid
            {
                return Err(std::io::Error::last_os_error());
            }
            libc::umask(0o077);
            Ok(())
        });
    }
    Ok(command.spawn()?)
}

fn ensure_session_directory() -> Result<(), Error> {
    match std::fs::create_dir(SESSION_DIRECTORY) {
        Ok(()) => {
            std::fs::set_permissions(SESSION_DIRECTORY, std::fs::Permissions::from_mode(0o755))?
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = std::fs::symlink_metadata(SESSION_DIRECTORY)?;
    if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(Error::Protocol(
            "display session directory is not protected",
        ));
    }
    Ok(())
}
