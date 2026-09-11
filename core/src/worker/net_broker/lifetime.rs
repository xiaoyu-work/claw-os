use std::collections::HashMap;
use std::io;
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol, Socket, Type};

#[derive(Debug, Default)]
pub(super) struct Lifetime {
    stopped: AtomicBool,
    next: AtomicU64,
    tunnels: Mutex<HashMap<u64, Arc<Sockets>>>,
}

#[derive(Debug)]
struct Sockets {
    downstream: UnixStream,
    upstream: Mutex<Option<TcpStream>>,
}

pub(super) struct Tunnel {
    id: u64,
    lifetime: Arc<Lifetime>,
    sockets: Arc<Sockets>,
}

impl Lifetime {
    pub fn track(self: &Arc<Self>, downstream: &UnixStream) -> Result<Tunnel, String> {
        let mut tunnels = self
            .tunnels
            .lock()
            .map_err(|_| "egress lifecycle lock poisoned")?;
        if self.stopped.load(Ordering::Acquire) {
            return Err("egress endpoint is retiring".to_string());
        }
        let id = self.next.fetch_add(1, Ordering::AcqRel);
        let sockets = Arc::new(Sockets {
            downstream: downstream.try_clone().map_err(|error| error.to_string())?,
            upstream: Mutex::new(None),
        });
        if tunnels.insert(id, sockets.clone()).is_some() {
            return Err("egress tunnel identity exhausted".to_string());
        }
        Ok(Tunnel {
            id,
            lifetime: self.clone(),
            sockets,
        })
    }

    pub fn stop(&self) -> Result<(), String> {
        self.stopped.store(true, Ordering::Release);
        let tunnels = self
            .tunnels
            .lock()
            .map_err(|_| "egress lifecycle lock poisoned")?;
        let mut errors = Vec::new();
        for sockets in tunnels.values() {
            shutdown(&sockets.downstream, &mut errors);
            let upstream = sockets
                .upstream
                .lock()
                .map_err(|_| "egress upstream lock poisoned")?;
            if let Some(upstream) = upstream.as_ref() {
                if let Err(error) = upstream.shutdown(Shutdown::Both) {
                    if error.kind() != std::io::ErrorKind::NotConnected {
                        errors.push(error.to_string());
                    }
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "egress socket shutdown failed: {}",
                errors.join("; ")
            ))
        }
    }
}

impl Tunnel {
    pub fn stopping(&self) -> bool {
        self.lifetime.stopped.load(Ordering::Acquire)
    }

    pub fn connect(&self, address: &SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
        if timeout.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "zero TCP connect timeout",
            ));
        }
        let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "TCP connect timeout overflow")
        })?;
        let socket = Socket::new(
            Domain::for_address(*address),
            Type::STREAM,
            Some(Protocol::TCP),
        )?;
        socket.set_nonblocking(true)?;
        let connected = {
            let mut upstream = self
                .sockets
                .upstream
                .lock()
                .map_err(|_| io::Error::other("egress upstream lock poisoned"))?;
            if self.stopping() {
                return Err(connect_cancelled());
            }
            if upstream.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "egress upstream already exists",
                ));
            }
            // Stop must own this socket before the first nonblocking connect.
            *upstream = Some(socket.try_clone()?.into());
            match socket.connect(&(*address).into()) {
                Ok(()) => true,
                Err(error)
                    if error.raw_os_error() == Some(libc::EINPROGRESS)
                        || error.kind() == io::ErrorKind::WouldBlock =>
                {
                    false
                }
                Err(error) => {
                    upstream.take();
                    return Err(error);
                }
            }
        };
        let stream: TcpStream = socket.into();
        let result = self
            .wait_connected(&stream, connected, deadline)
            .and_then(|()| stream.set_nonblocking(false));
        if let Err(error) = result {
            self.sockets
                .upstream
                .lock()
                .map_err(|_| io::Error::other("egress upstream lock poisoned during cleanup"))?
                .take();
            return Err(error);
        }
        Ok(stream)
    }

    fn wait_connected(
        &self,
        stream: &TcpStream,
        mut connected: bool,
        deadline: Instant,
    ) -> io::Result<()> {
        loop {
            if self.stopping() {
                return Err(connect_cancelled());
            }
            if connected {
                return Ok(());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "egress TCP connection timed out",
                ));
            }
            let wait_ms = remaining.as_millis().clamp(1, 50) as libc::c_int;
            let mut ready = libc::pollfd {
                fd: stream.as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            };
            let result = unsafe { libc::poll(&mut ready, 1, wait_ms) };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if result == 0 {
                continue;
            }
            if self.stopping() {
                return Err(connect_cancelled());
            }
            if ready.revents & libc::POLLNVAL != 0 {
                return Err(io::Error::from_raw_os_error(libc::EBADF));
            }
            if let Some(error) = stream.take_error()? {
                return Err(error);
            }
            if ready.revents & libc::POLLOUT == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "egress TCP connection closed before completion",
                ));
            }
            stream.peer_addr()?;
            connected = true;
        }
    }
}

fn connect_cancelled() -> io::Error {
    io::Error::new(
        io::ErrorKind::Interrupted,
        "egress authority retired while connecting",
    )
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        match self.lifetime.tunnels.lock() {
            Ok(mut tunnels) => {
                tunnels.remove(&self.id);
            }
            Err(_) => tracing::error!("egress lifecycle lock poisoned during cleanup"),
        }
    }
}

fn shutdown(stream: &UnixStream, errors: &mut Vec<String>) {
    if let Err(error) = stream.shutdown(Shutdown::Both) {
        if error.kind() != std::io::ErrorKind::NotConnected {
            errors.push(error.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/worker/net_broker/lifetime.rs"
    ));
}
