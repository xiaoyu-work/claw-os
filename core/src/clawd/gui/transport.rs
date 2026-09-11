use std::io;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use claw_display_control::{workload::InstanceProcess, ProcessIdentity};

use crate::worker::gui_transport::{bootstrap, kernel};
use crate::worker::linux::{GUI_WAYLAND_SOCKET, SANDBOX_BROKER_SOCKET, SANDBOX_EGRESS_SOCKET};

pub(super) struct Binding {
    process: InstanceProcess,
    stopped: AtomicBool,
}

impl Binding {
    pub fn new(
        process: &ProcessIdentity,
        resources: &crate::worker::LaunchResources,
    ) -> Result<Arc<Self>, String> {
        Self::receive(
            process.pidfd().map_err(|error| error.to_string())?,
            resources.gui_cgroup()?,
        )
    }

    pub(super) fn receive(pidfd: OwnedFd, cgroup: OwnedFd) -> Result<Arc<Self>, String> {
        Ok(Arc::new(Self {
            process: InstanceProcess::receive(pidfd, cgroup).map_err(|error| error.to_string())?,
            stopped: AtomicBool::new(false),
        }))
    }

    pub fn accepts(&self, peer: &ProcessIdentity) -> Result<bool, String> {
        Ok(!self.stopped.load(Ordering::Acquire)
            && self
                .process
                .is_peer(peer)
                .map_err(|error| error.to_string())?)
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
    }

    pub fn stopping(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
}

pub(super) struct Targets {
    pub wayland: PathBuf,
    pub broker: PathBuf,
    pub egress: Option<PathBuf>,
}

impl Targets {
    fn resolve(&self, address: &[u8]) -> Result<&std::path::Path, i32> {
        let name = address.get(2..).ok_or(libc::EINVAL)?;
        if name.first() == Some(&0) {
            return Err(libc::EACCES);
        }
        let end = name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(name.len());
        if name[end..].iter().any(|byte| *byte != 0) {
            return Err(libc::EINVAL);
        }
        match &name[..end] {
            name if name == GUI_WAYLAND_SOCKET.as_bytes() => Ok(&self.wayland),
            name if name == SANDBOX_BROKER_SOCKET.as_bytes() => Ok(&self.broker),
            name if name == SANDBOX_EGRESS_SOCKET.as_bytes() => {
                self.egress.as_deref().ok_or(libc::EACCES)
            }
            _ => Err(libc::EACCES),
        }
    }
}

pub(super) struct Bootstrap {
    channel: OwnedFd,
    cookie: u64,
}

impl Bootstrap {
    pub fn new() -> Result<(Self, OwnedFd), String> {
        let (channel, child) = bootstrap::pair().map_err(|error| error.to_string())?;
        let cookie = kernel::socket_cookie(child.as_fd()).map_err(|error| error.to_string())?;
        Ok((Self { channel, cookie }, child))
    }

    pub fn accept(
        self,
        binding: Arc<Binding>,
        targets: Targets,
        deadline: Instant,
    ) -> Result<(Guard, std::fs::File), String> {
        let packet = bootstrap::receive(self.channel.as_fd(), bootstrap::READY, deadline)
            .map_err(|error| format!("receive gated GUI transport: {error}"))?;
        let sender = kernel::sender(packet.credentials).map_err(|error| error.to_string())?;
        if !binding.accepts(&sender)? {
            return Err("GUI transport handoff came from another worker instance".to_string());
        }
        let actual_input = kernel::descriptor_of(&sender, libc::STDIN_FILENO).map_err(|error| {
            format!("GUI transport requires checked pidfd descriptor custody: {error}")
        })?;
        if kernel::socket_cookie(actual_input.as_fd()).map_err(|error| error.to_string())?
            != self.cookie
        {
            return Err(
                "GUI transport handoff did not come from the Root-staged execution gate"
                    .to_string(),
            );
        }
        let listener = kernel::Listener::receive(packet.descriptor)
            .map_err(|error| format!("validate GUI syscall listener: {error}"))?;
        let mut pipes = [-1_i32; 2];
        if unsafe { libc::pipe2(pipes.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(format!(
                "create GUI execution gate: {}",
                io::Error::last_os_error()
            ));
        }
        let read = unsafe { OwnedFd::from_raw_fd(pipes[0]) };
        let write = unsafe { OwnedFd::from_raw_fd(pipes[1]) };
        bootstrap::send(
            self.channel.as_fd(),
            bootstrap::INPUT,
            read.as_fd(),
            deadline,
        )
        .map_err(|error| format!("replace GUI bootstrap with execution pipe: {error}"))?;
        let guard = Guard::start(listener, binding, targets)?;
        Ok((guard, std::fs::File::from(write)))
    }
}

pub(super) struct Guard {
    binding: Arc<Binding>,
    failure: Arc<Mutex<Option<String>>>,
    worker: Option<JoinHandle<()>>,
}

impl Guard {
    fn start(
        listener: kernel::Listener,
        binding: Arc<Binding>,
        targets: Targets,
    ) -> Result<Self, String> {
        let failure = Arc::new(Mutex::new(None));
        let observed = failure.clone();
        let scope = binding.clone();
        let worker = std::thread::Builder::new()
            .name("gui-syscalls".to_string())
            .spawn(move || {
                if let Err(error) = serve(listener, &scope, &targets) {
                    tracing::error!(%error, "GUI syscall guard failed");
                    if let Ok(mut failure) = observed.lock() {
                        *failure = Some(error);
                    }
                    scope.stop();
                }
            })
            .map_err(|error| format!("start GUI syscall guard: {error}"))?;
        Ok(Self {
            binding,
            failure,
            worker: Some(worker),
        })
    }

    pub fn health(&self) -> Result<(), String> {
        let failure = self
            .failure
            .lock()
            .map_err(|_| "GUI syscall status lock poisoned")?;
        match failure.as_ref() {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    pub fn stop(&self) {
        self.binding.stop();
    }

    pub fn finish(&mut self, deadline: Instant) -> Result<(), String> {
        self.stop();
        if let Some(worker) = &self.worker {
            while !worker.is_finished() {
                if Instant::now() >= deadline {
                    return Err("GUI syscall retirement is pending".to_string());
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| "GUI syscall guard panicked")?;
        }
        Ok(())
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.stop();
        if self.worker.is_some() {
            tracing::error!("GUI syscall guard dropped before checked retirement");
        }
    }
}

fn serve(listener: kernel::Listener, binding: &Binding, targets: &Targets) -> Result<(), String> {
    while !binding.stopping() {
        let notice = match listener.next() {
            Ok(kernel::Notification::Request(notice)) => notice,
            Ok(kernel::Notification::Idle) => continue,
            Ok(kernel::Notification::Ended) => {
                binding.stop();
                break;
            }
            Err(error) if matches!(error.raw_os_error(), Some(libc::EINTR | libc::ENOENT)) => {
                continue
            }
            Err(error) => return Err(error.to_string()),
        };
        let result = operation(&listener, &notice, binding, targets);
        if let Err(errno) = result {
            tracing::warn!(
                syscall = notice.data.number,
                errno,
                "GUI transport operation refused"
            );
        }
        if let Err(error) = listener.reply(notice.id, result) {
            if error.raw_os_error() != Some(libc::ENOENT) {
                return Err(format!("reply to GUI syscall: {error}"));
            }
        }
    }
    Ok(())
}

fn operation(
    listener: &kernel::Listener,
    notice: &kernel::Notice,
    binding: &Binding,
    targets: &Targets,
) -> Result<(), i32> {
    if notice.data.architecture != crate::worker::seccomp::gui::architecture() {
        return Err(libc::EPERM);
    }
    let number = i64::from(notice.data.number);
    if ![libc::SYS_connect, libc::SYS_pidfd_send_signal].contains(&number) {
        return Err(libc::ENOSYS);
    }
    if number == libc::SYS_pidfd_send_signal
        && (notice.data.args[1] > 64 || notice.data.args[2] != 0 || notice.data.args[3] != 0)
    {
        return Err(libc::EINVAL);
    }
    let subject = listener.subject(notice).map_err(errno)?;
    let address = if number == libc::SYS_connect {
        Some(
            subject
                .address(notice.data.args[1], notice.data.args[2])
                .map_err(errno)?,
        )
    } else {
        None
    };
    let destination = address
        .as_deref()
        .map(|value| targets.resolve(value))
        .transpose()?;
    if !binding
        .accepts(&subject.identity)
        .map_err(|_| libc::EPERM)?
    {
        return Err(libc::EPERM);
    }
    let descriptor = subject.descriptor(notice.data.args[0]).map_err(errno)?;
    listener.valid(notice.id).map_err(errno)?;
    if let Some(destination) = destination {
        let socket = claw_display_control::identity::unix_stream(descriptor)
            .map_err(|_| libc::EPROTOTYPE)?;
        if binding.stopping() {
            return Err(libc::EPERM);
        }
        kernel::connect(socket.as_fd(), destination).map_err(errno)
    } else {
        let target = ProcessIdentity::from_pidfd(descriptor.as_fd()).map_err(|_| libc::ESRCH)?;
        if !binding.accepts(&target).map_err(|_| libc::EPERM)? {
            return Err(libc::EPERM);
        }
        listener.valid(notice.id).map_err(errno)?;
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                descriptor.as_raw_fd(),
                notice.data.args[1] as i32,
                std::ptr::null::<libc::siginfo_t>(),
                0_u32,
            )
        };
        if result != 0 {
            return Err(errno(io::Error::last_os_error()));
        }
        Ok(())
    }
}

fn errno(error: io::Error) -> i32 {
    error
        .raw_os_error()
        .filter(|value| *value > 0)
        .unwrap_or(libc::EIO)
}
