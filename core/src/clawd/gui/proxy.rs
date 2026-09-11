use std::io;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use super::transport::Binding;
use crate::worker::gui_transport::kernel;

const MAX_CONNECTIONS: usize = 16;
const MAX_BYTES: usize = 65536;
const MAX_DESCRIPTORS: usize = 32;

struct State {
    binding: Arc<Binding>,
    stopped: AtomicBool,
    active: AtomicUsize,
    failure: Mutex<Option<String>>,
}

pub(super) struct Proxy {
    state: Arc<State>,
    listener: Option<JoinHandle<()>>,
}

impl Proxy {
    pub fn start(
        listener: UnixListener,
        target: PathBuf,
        binding: Arc<Binding>,
        descriptors: bool,
    ) -> Result<Self, String> {
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        crate::clawd::transport::peer::enable_credential_passing(listener.as_raw_fd())
            .map_err(|error| error.to_string())?;
        let state = Arc::new(State {
            binding,
            stopped: AtomicBool::new(false),
            active: AtomicUsize::new(0),
            failure: Mutex::new(None),
        });
        let endpoint = state.clone();
        let thread = std::thread::Builder::new().name("gui-transport-listen".to_string()).spawn(move || {
            while !stopped(&endpoint) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if endpoint.active.load(Ordering::Acquire) >= MAX_CONNECTIONS {
                            tracing::warn!("GUI transport connection ceiling reached");
                            continue;
                        }
                        endpoint.active.fetch_add(1, Ordering::AcqRel);
                        let scope = endpoint.clone();
                        let target = target.clone();
                        let spawned = std::thread::Builder::new().name("gui-transport".to_string()).spawn(move || {
                            let _active = Active(scope.clone());
                            if let Err(error) = relay(stream, &target, &scope, descriptors) {
                                tracing::warn!(%error, "GUI transport connection refused or closed");
                            }
                        });
                        if let Err(error) = spawned {
                            endpoint.active.fetch_sub(1, Ordering::AcqRel);
                            fail(&endpoint, format!("start GUI transport connection: {error}"));
                            return;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        fail(&endpoint, format!("accept GUI transport: {error}"));
                        return;
                    }
                }
            }
        }).map_err(|error| format!("start GUI transport listener: {error}"))?;
        Ok(Self {
            state,
            listener: Some(thread),
        })
    }

    pub fn health(&self) -> Result<(), String> {
        let failure = self
            .state
            .failure
            .lock()
            .map_err(|_| "GUI transport status lock poisoned")?;
        match failure.as_ref() {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    pub fn stop(&self) {
        self.state.stopped.store(true, Ordering::Release);
    }

    pub fn finish(&mut self, deadline: Instant) -> Result<(), String> {
        self.stop();
        while self.state.active.load(Ordering::Acquire) != 0
            || self
                .listener
                .as_ref()
                .is_some_and(|listener| !listener.is_finished())
        {
            if Instant::now() >= deadline {
                return Err("GUI transport connection retirement is pending".to_string());
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        if let Some(listener) = self.listener.take() {
            listener
                .join()
                .map_err(|_| "GUI transport listener panicked")?;
        }
        Ok(())
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop();
        if self.listener.is_some() {
            tracing::error!("GUI transport proxy dropped before checked retirement");
        }
    }
}

struct Active(Arc<State>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

fn stopped(state: &State) -> bool {
    state.stopped.load(Ordering::Acquire) || state.binding.stopping()
}

fn fail(state: &State, error: String) {
    tracing::error!(%error, "GUI transport failed");
    if let Ok(mut failure) = state.failure.lock() {
        *failure = Some(error);
    }
    state.stopped.store(true, Ordering::Release);
}

struct Packet {
    bytes: Vec<u8>,
    descriptors: Vec<OwnedFd>,
    credentials: Option<libc::ucred>,
    written: usize,
}

#[derive(Default)]
struct Direction {
    pending: Option<Packet>,
    eof: bool,
    write_closed: bool,
}

fn relay(front: UnixStream, target: &std::path::Path, state: &State, fds: bool) -> io::Result<()> {
    let back = UnixStream::connect(target)?;
    relay_connected(front, back, state, fds)
}

fn relay_connected(front: UnixStream, back: UnixStream, state: &State, fds: bool) -> io::Result<()> {
    front.set_nonblocking(true)?;
    back.set_nonblocking(true)?;
    let mut downstream = Direction::default();
    let mut upstream = Direction::default();
    let sockets = [&front, &back];
    loop {
        if stopped(state) {
            front.shutdown(std::net::Shutdown::Both)?;
            back.shutdown(std::net::Shutdown::Both)?;
            return Ok(());
        }
        let mut polls = [
            (front.as_raw_fd(), &upstream, &downstream),
            (back.as_raw_fd(), &downstream, &upstream),
        ]
        .map(|(fd, reading, writing)| {
            let events = if !reading.eof && reading.pending.is_none() {
                libc::POLLIN
            } else {
                0
            } | if writing.pending.is_some() {
                libc::POLLOUT
            } else {
                0
            };
            libc::pollfd {
                // HUP is reported even with events=0; omit blocked sources
                // until their pending packet has reached the other socket.
                fd: if events == 0 { -1 } else { fd },
                events,
                revents: 0,
            }
        });
        let ready = unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, 50) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        for (index, poll) in polls.iter().enumerate() {
            if poll.revents & libc::POLLNVAL != 0 {
                return Err(io::Error::from_raw_os_error(libc::EBADF));
            }
            if poll.revents & libc::POLLERR != 0 {
                return Err(sockets[index]
                    .take_error()?
                    .unwrap_or_else(|| io::Error::other("GUI transport socket poll failed")));
            }
        }
        for (index, direction) in [(0, &mut upstream), (1, &mut downstream)] {
            if !direction.eof
                && direction.pending.is_none()
                && polls[index].revents & (libc::POLLIN | libc::POLLHUP) != 0
            {
                let packet = match receive(sockets[index].as_fd(), fds) {
                    Ok(Some(packet)) => packet,
                    Ok(None) => {
                        direction.eof = true;
                        continue;
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) =>
                    {
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                if index == 0 {
                    let peer = kernel::sender(packet.credentials.ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "GUI message has no kernel writer",
                        )
                    })?)?;
                    if !state.binding.accepts(&peer).map_err(io::Error::other)? {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "GUI transport writer is outside its instance",
                        ));
                    }
                }
                direction.pending = Some(packet);
            }
        }
        for (index, direction) in [(0, &mut downstream), (1, &mut upstream)] {
            if polls[index].revents & (libc::POLLOUT | libc::POLLHUP) != 0 {
                if let Some(packet) = &mut direction.pending {
                    if send(sockets[index].as_fd(), packet)? {
                        direction.pending = None;
                    }
                }
            }
            if direction.eof && direction.pending.is_none() && !direction.write_closed {
                sockets[index].shutdown(std::net::Shutdown::Write)?;
                direction.write_closed = true;
            }
        }
        if upstream.write_closed && downstream.write_closed {
            return Ok(());
        }
    }
}

fn receive(socket: std::os::fd::BorrowedFd<'_>, fds: bool) -> io::Result<Option<Packet>> {
    let mut bytes = vec![0_u8; MAX_BYTES];
    let mut control = [0_usize; 64];
    let mut vector = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    let mut message: libc::msghdr = unsafe { zeroed() };
    message.msg_iov = &mut vector;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = size_of_val(&control);
    let count = unsafe {
        libc::recvmsg(
            socket.as_raw_fd(),
            &mut message,
            libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC,
        )
    };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut descriptors = Vec::new();
    let mut credentials = None;
    let mut invalid = message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0;
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(&message);
        while !header.is_null() {
            let length = (*header)
                .cmsg_len
                .saturating_sub(libc::CMSG_LEN(0) as usize);
            let data = libc::CMSG_DATA(header);
            match ((*header).cmsg_level, (*header).cmsg_type) {
                (libc::SOL_SOCKET, libc::SCM_CREDENTIALS) => {
                    if length == size_of::<libc::ucred>() && credentials.is_none() {
                        credentials = Some(std::ptr::read_unaligned(data.cast::<libc::ucred>()));
                    } else {
                        invalid = true;
                    }
                }
                (libc::SOL_SOCKET, libc::SCM_RIGHTS) => {
                    invalid |= !length.is_multiple_of(size_of::<i32>());
                    for index in 0..length / size_of::<i32>() {
                        descriptors.push(OwnedFd::from_raw_fd(std::ptr::read_unaligned(
                            data.add(index * size_of::<i32>()).cast::<i32>(),
                        )));
                    }
                }
                _ => invalid = true,
            }
            header = libc::CMSG_NXTHDR(&message, header);
        }
    }
    if invalid || descriptors.len() > MAX_DESCRIPTORS || (!fds && !descriptors.is_empty()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "GUI transport descriptor/control bounds exceeded",
        ));
    }
    for descriptor in &descriptors {
        validate_descriptor(descriptor.as_fd())?;
    }
    if count == 0 {
        return Ok(None);
    }
    bytes.truncate(count as usize);
    Ok(Some(Packet {
        bytes,
        descriptors,
        credentials,
        written: 0,
    }))
}

fn validate_descriptor(descriptor: std::os::fd::BorrowedFd<'_>) -> io::Result<()> {
    let mut status: libc::stat = unsafe { zeroed() };
    if unsafe { libc::fstat(descriptor.as_raw_fd(), &mut status) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if matches!(
        status.st_mode & libc::S_IFMT,
        libc::S_IFSOCK | libc::S_IFDIR
    ) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Wayland cannot transfer socket or directory authority",
        ));
    }
    let target = std::fs::read_link(format!("/proc/self/fd/{}", descriptor.as_raw_fd()))?;
    if target.as_os_str() == "anon_inode:[pidfd]" {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Wayland cannot transfer process authority",
        ));
    }
    Ok(())
}

fn send(socket: std::os::fd::BorrowedFd<'_>, packet: &mut Packet) -> io::Result<bool> {
    let bytes = &packet.bytes[packet.written..];
    let mut vector = libc::iovec {
        iov_base: bytes.as_ptr().cast_mut().cast(),
        iov_len: bytes.len(),
    };
    let mut control = [0_usize; 32];
    let mut message: libc::msghdr = unsafe { zeroed() };
    message.msg_iov = &mut vector;
    message.msg_iovlen = 1;
    if packet.written == 0 && !packet.descriptors.is_empty() {
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen =
            unsafe { libc::CMSG_SPACE((packet.descriptors.len() * size_of::<i32>()) as u32) }
                as usize;
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&message);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len =
                libc::CMSG_LEN((packet.descriptors.len() * size_of::<i32>()) as u32) as usize;
            for (index, descriptor) in packet.descriptors.iter().enumerate() {
                std::ptr::write_unaligned(
                    libc::CMSG_DATA(header).cast::<i32>().add(index),
                    descriptor.as_raw_fd(),
                );
            }
        }
    }
    let count = unsafe {
        libc::sendmsg(
            socket.as_raw_fd(),
            &message,
            libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
        )
    };
    if count < 0 {
        let error = io::Error::last_os_error();
        if matches!(
            error.kind(),
            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
        ) {
            return Ok(false);
        }
        return Err(error);
    }
    if count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "GUI transport write made no progress",
        ));
    }
    packet.written += count as usize;
    packet.descriptors.clear();
    Ok(packet.written == packet.bytes.len())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/gui/proxy.rs"
    ));
}
