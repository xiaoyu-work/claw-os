use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use claw_display_control::identity::pidfd_exited;
use claw_display_control::wire::{
    monotonic_ms, AuthorityCommand, AuthorityReply, CompositorCommand, CompositorReply, PamCommand,
    PamReply, SessionEvent, SessionRequest, COMPOSITOR, SESSION_DIRECTORY,
};
use claw_display_control::{Connection, Epoch, Error, KernelLogin, Listener, ProcessIdentity};

use super::containment::{self, Containment};

const STARTUP: Duration = Duration::from_secs(20);
const STOP: Duration = Duration::from_secs(5);
const TICK: Duration = Duration::from_millis(10);
const MAX_SUBSCRIBERS: usize = 32;

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
                .receive::<AuthorityCommand>(&self.controllers.broker, Instant::now() + TICK)?;
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
                        Instant::now() + TICK,
                    )?;
                }
                AuthorityCommand::Shutdown { epoch } if epoch == self.controllers.epoch => {
                    return Err(Error::Closed);
                }
                AuthorityCommand::Gui { command } => {
                    let descriptors: Vec<_> = packet.descriptors.iter().map(AsFd::as_fd).collect();
                    self.control
                        .send(&command, &descriptors, Instant::now() + TICK)?;
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
                    .receive::<CompositorReply>(&self.compositor_identity, Instant::now() + TICK)?;
                if !matches!(
                    packet.message,
                    CompositorReply::CreatorReady { .. }
                        | CompositorReply::Refreshed { .. }
                        | CompositorReply::Retired { .. }
                        | CompositorReply::InstanceEnded { .. }
                        | CompositorReply::Rejected { .. }
                ) {
                    return Err(Error::Protocol("unexpected running compositor response"));
                }
                self.controllers.authority.send(
                    &AuthorityReply::Gui {
                        reply: packet.message,
                    },
                    &[],
                    Instant::now() + TICK,
                )?;
            }
            if self.controllers.pam.readable()? {
                match self
                    .controllers
                    .pam
                    .receive::<PamCommand>(&self.controllers.parent, Instant::now() + TICK)
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

    fn retire(&mut self, pam_closed: bool) -> Result<(), Error> {
        let deadline = Instant::now() + STOP;
        if let Some(listener) = self.listener.take() {
            listener.close()?;
        }
        let shutdown = self.control.send(
            &CompositorCommand::Shutdown {
                epoch: self.controllers.epoch,
            },
            &[],
            deadline,
        );
        if let Err(error) = shutdown {
            eprintln!("display control unavailable at retirement: {error}");
        } else if pam_closed {
            let graceful = (Instant::now() + Duration::from_secs(1)).min(deadline);
            while !pidfd_exited(self.compositor_pidfd.as_fd())? && Instant::now() < graceful {
                std::thread::sleep(TICK);
            }
        }
        self.containment.retire(deadline)?;
        if !pidfd_exited(self.compositor_pidfd.as_fd())? {
            return Err(Error::Protocol(
                "compositor remains alive after workload retirement",
            ));
        }
        self.compositor.wait()?;
        self.prune_subscribers();
        for subscriber in self.subscribers.drain(..) {
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
    let retirement = supervisor.retire(outcome.is_ok());
    match (outcome, retirement) {
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
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
        .env("XDG_CURRENT_DESKTOP", "COSMIC")
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
