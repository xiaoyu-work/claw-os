//! The compositor's authenticated parent connection, independent of Wayland policy.

use std::os::fd::{AsFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::identity::pidfd_exited;
use crate::wire::{monotonic_ms, CompositorCommand, CompositorReply};
use crate::{Connection, Epoch, Error, KernelLogin, ProcessIdentity, Received};

pub struct DisplayBinding {
    epoch: Epoch,
    login: KernelLogin,
    live: AtomicBool,
    expires_ms: AtomicU64,
}

impl DisplayBinding {
    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    pub fn owner_uid(&self) -> u32 {
        self.login.owner_uid
    }

    pub fn is_login_peer(&self, peer: &ProcessIdentity) -> Result<bool, Error> {
        if !self.active()? || peer.uid() != self.login.owner_uid || !peer.in_host_pid_namespace()? {
            return Ok(false);
        }
        match KernelLogin::of(peer) {
            Ok(login) => Ok(login == self.login),
            Err(Error::UnauthenticatedLogin) => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub fn active(&self) -> Result<bool, Error> {
        Ok(self.live.load(Ordering::Acquire)
            && monotonic_ms()? < self.expires_ms.load(Ordering::Acquire))
    }

    fn renew(&self, epoch: Epoch, expires: u64) -> Result<(), Error> {
        let now = monotonic_ms()?;
        if epoch != self.epoch || expires <= now || expires > now.saturating_add(2500) {
            return Err(Error::Protocol("invalid compositor authority lease"));
        }
        self.expires_ms.store(expires, Ordering::Release);
        Ok(())
    }
}

pub struct CompositorLink {
    connection: Arc<Connection>,
    binding: Arc<DisplayBinding>,
    inbox: mpsc::Receiver<Received<CompositorCommand>>,
    stopped: Arc<AtomicBool>,
    reader: Option<JoinHandle<Result<(), Error>>>,
}

impl CompositorLink {
    pub fn receive() -> Result<Self, Error> {
        if std::env::var_os("CLAW_DISPLAY_CONTROL_FD").as_deref() != Some("3".as_ref()) {
            return Err(Error::Protocol(
                "Root display activation descriptor is required",
            ));
        }
        let current = ProcessIdentity::current()?;
        let parent = ProcessIdentity::parent()?;
        let login = KernelLogin::of(&current)?;
        if current.uid() != login.owner_uid
            || parent.uid() != 0
            || KernelLogin::of(&parent)? != login
            || !current.in_host_pid_namespace()?
        {
            return Err(Error::Identity);
        }
        let connection = Arc::new(unsafe { Connection::inherit(3) }?);
        if connection.peer()? != parent {
            return Err(Error::Identity);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let packet = connection.receive::<CompositorCommand>(&parent, deadline)?;
        let CompositorCommand::Initialize {
            epoch,
            owner_uid,
            audit_session,
        } = packet.message
        else {
            return Err(Error::Protocol("compositor initialization is missing"));
        };
        if owner_uid != login.owner_uid || audit_session != login.session_id {
            return Err(Error::Identity);
        }
        if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let binding = Arc::new(DisplayBinding {
            epoch,
            login,
            live: AtomicBool::new(true),
            expires_ms: AtomicU64::new(0),
        });
        match connection
            .receive::<CompositorCommand>(&parent, deadline)?
            .message
        {
            CompositorCommand::Heartbeat {
                epoch,
                expires_monotonic_ms,
            } => {
                binding.renew(epoch, expires_monotonic_ms)?;
            }
            _ => return Err(Error::Protocol("initial compositor lease is missing")),
        }
        let parent_pidfd = parent.pidfd()?;
        let (sender, inbox) = mpsc::sync_channel(16);
        let stopped = Arc::new(AtomicBool::new(false));
        let reader_connection = connection.clone();
        let reader_binding = binding.clone();
        let reader_stopped = stopped.clone();
        let reader = std::thread::Builder::new()
            .name("display-control".to_string())
            .spawn(move || {
                let result = receive_commands(
                    reader_connection,
                    &reader_binding,
                    &reader_stopped,
                    parent,
                    parent_pidfd,
                    sender,
                );
                reader_binding.live.store(false, Ordering::Release);
                if let Err(error) = &result {
                    eprintln!("compositor Root control lost: {error}");
                }
                result
            })?;
        Ok(Self {
            connection,
            binding,
            inbox,
            stopped,
            reader: Some(reader),
        })
    }

    pub fn binding(&self) -> Arc<DisplayBinding> {
        self.binding.clone()
    }

    pub fn ready(&self, display: String) -> Result<(), Error> {
        if !self.binding.active()? {
            return Err(Error::Closed);
        }
        self.connection.send(
            &CompositorReply::Ready {
                epoch: self.binding.epoch,
                wayland_display: display,
            },
            &[],
            Instant::now() + Duration::from_secs(1),
        )
    }

    pub fn next(&self) -> Result<Option<Received<CompositorCommand>>, Error> {
        match self.inbox.try_recv() {
            Ok(packet) => Ok(Some(packet)),
            Err(mpsc::TryRecvError::Empty) => {
                if self.binding.active()? {
                    Ok(None)
                } else {
                    Err(Error::Closed)
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => Err(Error::Closed),
        }
    }

    pub fn reply(&self, reply: &CompositorReply) -> Result<(), Error> {
        self.connection
            .send(reply, &[], Instant::now() + Duration::from_secs(1))
    }

    pub fn close(&mut self) -> Result<(), Error> {
        self.binding.live.store(false, Ordering::Release);
        self.stopped.store(true, Ordering::Release);
        if let Some(reader) = self.reader.take() {
            reader
                .join()
                .map_err(|_| Error::Protocol("compositor control reader panicked"))??;
        }
        Ok(())
    }
}

impl Drop for CompositorLink {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            eprintln!("compositor control cleanup failed: {error}");
        }
    }
}

fn receive_commands(
    connection: Arc<Connection>,
    binding: &DisplayBinding,
    stopped: &AtomicBool,
    parent: ProcessIdentity,
    parent_pidfd: OwnedFd,
    sender: mpsc::SyncSender<Received<CompositorCommand>>,
) -> Result<(), Error> {
    while !stopped.load(Ordering::Acquire) {
        if !binding.active()? || pidfd_exited(parent_pidfd.as_fd())? {
            return Err(Error::Closed);
        }
        match connection
            .receive::<CompositorCommand>(&parent, Instant::now() + Duration::from_millis(100))
        {
            Ok(packet) => match packet.message {
                CompositorCommand::Heartbeat {
                    epoch,
                    expires_monotonic_ms,
                } => {
                    binding.renew(epoch, expires_monotonic_ms)?;
                }
                CompositorCommand::Initialize { .. } => {
                    return Err(Error::Protocol("compositor initialization was replayed"));
                }
                CompositorCommand::Shutdown { epoch } => {
                    if epoch != binding.epoch {
                        return Err(Error::Protocol("stale compositor shutdown"));
                    }
                    sender
                        .try_send(packet)
                        .map_err(|_| Error::Protocol("compositor control queue unavailable"))?;
                    return Ok(());
                }
                _ => sender
                    .try_send(packet)
                    .map_err(|_| Error::Protocol("compositor control queue unavailable"))?,
            },
            Err(Error::Timeout) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
