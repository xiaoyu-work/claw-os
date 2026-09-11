use std::collections::HashMap;
use std::net::{Shutdown, TcpStream};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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

    pub fn upstream(&self, stream: &TcpStream) -> Result<(), String> {
        let mut upstream = self
            .sockets
            .upstream
            .lock()
            .map_err(|_| "egress upstream lock poisoned")?;
        if self.stopping() {
            return Err("egress authority retired while connecting".to_string());
        }
        *upstream = Some(stream.try_clone().map_err(|error| error.to_string())?);
        Ok(())
    }
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
