use std::io::Read;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

use claw_display_control::{Error, InstanceId, wire::monotonic_ms};

use super::containment;

const EXECUTABLE: &str = "/usr/libexec/claw-hyprland/claw-lockscreen";
pub(super) const LOCK_START_MS: u64 = 8000;

pub(super) enum Status {
    Waiting,
    Locked,
    Unlocked,
}

pub(super) struct Locker {
    pub instance: InstanceId,
    child: Child,
    readiness: Option<UnixStream>,
    expires: u64,
    admitted: bool,
}

impl Locker {
    pub fn spawn(
        instance: InstanceId,
        expires: u64,
        uid: u32,
        membership: BorrowedFd<'_>,
    ) -> Result<(Self, UnixStream), Error> {
        containment::protected_executable(EXECUTABLE)?;
        let (gid, name, _) = containment::account(uid)?;
        let (server, client) = UnixStream::pair()?;
        let (readiness, signal) = UnixStream::pair()?;
        readiness.set_nonblocking(true)?;
        let client_fd = duplicate(&client)?;
        let signal_fd = duplicate(&signal)?;
        let group = membership.as_raw_fd();
        let (wayland, ready) = (client_fd.as_raw_fd(), signal_fd.as_raw_fd());
        let parent = unsafe { libc::getpid() };
        let mut command = Command::new(EXECUTABLE);
        command
            .args([
                "--config",
                "/dev/null",
                "--ready-fd",
                "4",
                "--color",
                "18151f",
                "--ring-color",
                "f7b7cb",
                "--indicator-idle-visible",
                "--ignore-empty-password",
            ])
            .env_clear()
            .env("WAYLAND_SOCKET", "3")
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", "/")
            .env("LC_ALL", "C.UTF-8")
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        unsafe {
            command.pre_exec(move || {
                if libc::getppid() != parent {
                    return Err(std::io::Error::from_raw_os_error(libc::EPERM));
                }
                containment::enter(group)?;
                if libc::dup2(wayland, 3) < 0
                    || libc::dup2(ready, 4) < 0
                    || libc::syscall(
                        libc::SYS_close_range,
                        5_u32,
                        u32::MAX,
                        libc::CLOSE_RANGE_CLOEXEC,
                    ) != 0
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
                Ok(())
            });
        }
        Ok((
            Self {
                instance,
                child: command.spawn()?,
                readiness: Some(readiness),
                expires,
                admitted: false,
            },
            server,
        ))
    }

    pub fn admit(&mut self) -> Result<(), Error> {
        if self.admitted || monotonic_ms()? >= self.expires {
            return Err(Error::Protocol("invalid native locker admission"));
        }
        self.admitted = true;
        Ok(())
    }

    pub fn poll(&mut self) -> Result<Status, Error> {
        if let Some(status) = self.child.try_wait()? {
            return if self.readiness.is_none() && status.success() {
                Ok(Status::Unlocked)
            } else {
                Err(Error::Protocol(
                    "native locker exited without checked PAM unlock",
                ))
            };
        }
        let Some(readiness) = self.readiness.as_mut() else {
            return Ok(Status::Waiting);
        };
        if monotonic_ms()? >= self.expires {
            return Err(Error::Timeout);
        }
        if self.admitted {
            let mut marker = [0];
            match readiness.read(&mut marker) {
                Ok(1) if marker == [b'\n'] => {
                    self.readiness = None;
                    return Ok(Status::Locked);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
                _ => return Err(Error::Protocol("invalid native locker readiness")),
            }
        }
        Ok(Status::Waiting)
    }

    pub fn reap(mut self) -> Result<(), Error> {
        self.child.wait()?;
        Ok(())
    }
}

fn duplicate(stream: &UnixStream) -> Result<std::os::fd::OwnedFd, Error> {
    use std::os::fd::FromRawFd;
    let fd = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 64) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
}
