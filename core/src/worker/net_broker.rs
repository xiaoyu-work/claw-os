//! Brokered egress for sandboxed workers.
//!
//! A hostile worker always runs in its own empty network namespace, so
//! it has no route to anything — not the internet, not the host's
//! loopback, not a neighbouring service. When an operation legitimately
//! holds `net.dial` / `net.http` for exact hosts, the launcher opens
//! this endpoint on a unix socket inside the launch directory and
//! bind-mounts it into the sandbox.
//!
//! The endpoint speaks HTTP `CONNECT`, which every mainstream client
//! already knows how to use, and enforces on every tunnel:
//!
//! * the requested `host:port` must match one of the exact endpoints
//!   the operation was granted — no globs, no suffix matching;
//! * the name is resolved *here*, and the connection is made to the
//!   address that resolution returned, so a name that changes answers
//!   between the check and the connect (DNS rebinding) cannot move the
//!   tunnel;
//! * every candidate address must be globally routable: loopback,
//!   link-local (including the `169.254.169.254` cloud metadata
//!   address), private, CGNAT, multicast, unspecified and unique-local
//!   addresses are refused;
//! * a redirect that a client follows arrives as a *new* `CONNECT`, so
//!   it is validated exactly like the first one — a redirect to a host
//!   outside the grant cannot be followed;
//! * the relay is bounded in bytes and time, and closes with the
//!   launch.
//!
//! Direct sockets stay unavailable in every case: the namespace has no
//! interfaces, and the strict seccomp profile does not even allow a
//! socket to be created unless egress was brokered.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::policy::Endpoint;

#[cfg(unix)]
mod lifetime;

const CONNECT_DEADLINE: Duration = Duration::from_secs(20);
const RELAY_IDLE_DEADLINE: Duration = Duration::from_secs(120);
/// Ceiling on one tunnel, in each direction.
const MAX_TUNNEL_BYTES: u64 = 512 * 1024 * 1024;
const MAX_REQUEST_HEAD: usize = 8 * 1024;
const MAX_TUNNELS: u64 = 16;

/// A live egress endpoint. Dropping it stops the listener and removes
/// the socket.
#[derive(Debug)]
pub struct EgressEndpoint {
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    stats: Arc<Stats>,
    #[cfg(unix)]
    lifetime: Arc<lifetime::Lifetime>,
    #[cfg(unix)]
    listener: Option<std::thread::JoinHandle<()>>,
}

#[derive(Clone)]
enum Peer {
    Owner(u32),
    #[cfg(target_os = "linux")]
    RootRelay(claw_display_control::ProcessIdentity),
}

impl Peer {
    #[cfg(unix)]
    fn accepts(&self, stream: &std::os::unix::net::UnixStream) -> bool {
        match self {
            Self::Owner(uid) => super::peer_uid_of(stream) == Some(*uid),
            #[cfg(target_os = "linux")]
            Self::RootRelay(expected) => {
                use std::os::fd::AsFd;
                claw_display_control::ProcessIdentity::unix_peer(stream.as_fd())
                    .is_ok_and(|actual| actual == *expected)
            }
        }
    }
}

#[derive(Debug, Default)]
struct Stats {
    admitted: AtomicU64,
    refused: AtomicU64,
    inflight: AtomicU64,
}

impl EgressEndpoint {
    pub fn start(
        socket: PathBuf,
        endpoints: Vec<Endpoint>,
        owner_uid: u32,
    ) -> Result<Self, String> {
        Self::start_for(socket, endpoints, Peer::Owner(owner_uid))
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn start_gui(socket: PathBuf, endpoints: Vec<Endpoint>) -> Result<Self, String> {
        let relay = claw_display_control::ProcessIdentity::current().map_err(|error| error.to_string())?;
        if relay.uid() != 0 {
            return Err("GUI egress requires its Root transport relay".to_string());
        }
        Self::start_for(socket, endpoints, Peer::RootRelay(relay))
    }

    fn start_for(socket: PathBuf, endpoints: Vec<Endpoint>, peer: Peer) -> Result<Self, String> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            use std::os::unix::net::UnixListener;

            for endpoint in &endpoints {
                super::policy::validate_endpoint(endpoint)?;
            }
            let listener = UnixListener::bind(&socket)
                .map_err(|error| format!("bind worker egress socket: {error}"))?;
            std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))
                .map_err(|error| format!("restrict worker egress socket: {error}"))?;

            let stop = Arc::new(AtomicBool::new(false));
            let stats = Arc::new(Stats::default());
            let lifetime = Arc::new(lifetime::Lifetime::default());
            let allowed = Arc::new(endpoints);
            listener.set_nonblocking(true).map_err(|error| format!("configure worker egress listener: {error}"))?;
            let thread = {
                let stop = Arc::clone(&stop);
                let stats = Arc::clone(&stats);
                let lifetime = Arc::clone(&lifetime);
                std::thread::Builder::new()
                    .name("cos-worker-egress".to_string())
                    .spawn(move || {
                        loop {
                            if stop.load(Ordering::Relaxed) {
                                return;
                            }
                            let stream = match listener.accept() {
                                Ok((stream, _)) => stream,
                                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                    std::thread::sleep(Duration::from_millis(5));
                                    continue;
                                }
                                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                                Err(error) => {
                                    tracing::error!(%error, "worker egress listener failed");
                                    return;
                                }
                            };
                            if stats.inflight.load(Ordering::Relaxed) >= MAX_TUNNELS {
                                stats.refused.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
                            stats.inflight.fetch_add(1, Ordering::Relaxed);
                            let allowed = Arc::clone(&allowed);
                            let stats_for_thread = Arc::clone(&stats);
                            let peer = peer.clone();
                            let tunnel = match lifetime.track(&stream) {
                                Ok(tunnel) => tunnel,
                                Err(error) => {
                                    stats.inflight.fetch_sub(1, Ordering::Relaxed);
                                    tracing::error!(%error, "worker egress lifecycle tracking failed");
                                    return;
                                }
                            };
                            let spawned = std::thread::Builder::new()
                                .name("cos-worker-egress-conn".to_string())
                                .spawn(move || {
                                    match serve(stream, &allowed, &peer, &tunnel) {
                                        true => stats_for_thread
                                            .admitted
                                            .fetch_add(1, Ordering::Relaxed),
                                        false => {
                                            stats_for_thread.refused.fetch_add(1, Ordering::Relaxed)
                                        }
                                    };
                                    drop(tunnel);
                                    stats_for_thread.inflight.fetch_sub(1, Ordering::Relaxed);
                                });
                            if let Err(error) = spawned {
                                stats.inflight.fetch_sub(1, Ordering::Relaxed);
                                tracing::error!(%error, "worker egress connection thread failed");
                            }
                        }
                    })
                    .map_err(|error| format!("start worker egress broker: {error}"))?
            };
            Ok(Self {
                socket,
                stop,
                stats,
                lifetime,
                listener: Some(thread),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (socket, endpoints, peer);
            Err("worker egress brokers require Unix".to_string())
        }
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    pub(crate) fn stop(&self) -> Result<(), String> {
        self.stop.store(true, Ordering::Release);
        #[cfg(unix)]
        self.lifetime.stop()?;
        Ok(())
    }

    pub(crate) fn retire(&mut self, deadline: std::time::Instant) -> Result<(), String> {
        self.stop()?;
        #[cfg(unix)]
        {
            while self.stats.inflight.load(Ordering::Acquire) != 0
                || self.listener.as_ref().is_some_and(|thread| !thread.is_finished())
            {
                if std::time::Instant::now() >= deadline {
                    return Err("worker egress connections have not retired".to_string());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            if let Some(thread) = self.listener.take() {
                thread.join().map_err(|_| "worker egress listener panicked")?;
            }
        }
        Ok(())
    }

    pub fn facts(&self) -> serde_json::Value {
        serde_json::json!({
            "admitted": self.stats.admitted.load(Ordering::Relaxed),
            "refused": self.stats.refused.load(Ordering::Relaxed),
        })
    }
}

impl Drop for EgressEndpoint {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            tracing::error!(%error, "worker egress cleanup failed");
        }
        if let Err(error) = std::fs::remove_file(&self.socket) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::error!(%error, "worker egress socket cleanup failed");
            }
        }
    }
}

#[cfg(unix)]
fn serve(
    mut stream: std::os::unix::net::UnixStream,
    allowed: &[Endpoint],
    peer: &Peer,
    tunnel: &lifetime::Tunnel,
) -> bool {
    if !peer.accepts(&stream) || tunnel.stopping() {
        return false;
    }
    let _ = stream.set_read_timeout(Some(CONNECT_DEADLINE));
    let _ = stream.set_write_timeout(Some(CONNECT_DEADLINE));

    let head = match read_head(&mut stream) {
        Ok(head) => head,
        Err(_) => {
            let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
            return false;
        }
    };
    let Some(target) = parse_connect(&head) else {
        let _ = stream.write_all(b"HTTP/1.1 405 Method Not Allowed\r\n\r\n");
        return false;
    };
    let Some(endpoint) = match_endpoint(&target, allowed) else {
        let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n");
        return false;
    };
    let address = match resolve_public(&endpoint) {
        Ok(address) => address,
        Err(_) => {
            let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n");
            return false;
        }
    };
    if tunnel.stopping() {
        return false;
    }
    let Ok(upstream) = TcpStream::connect_timeout(&address, CONNECT_DEADLINE) else {
        let _ = stream.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n");
        return false;
    };
    if let Err(error) = tunnel.upstream(&upstream) {
        tracing::warn!(%error, "worker egress was retired before upstream activation");
        return false;
    }
    if stream
        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
        .is_err()
    {
        return false;
    }
    relay(stream, upstream);
    true
}

/// Read the request head, bounded, stopping at the blank line.
#[cfg(unix)]
fn read_head(stream: &mut std::os::unix::net::UnixStream) -> Result<String, String> {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while head.len() < MAX_REQUEST_HEAD {
        match stream.read(&mut byte) {
            Ok(0) => return Err("closed".to_string()),
            Ok(_) => head.push(byte[0]),
            Err(error) => return Err(error.to_string()),
        }
        if head.ends_with(b"\r\n\r\n") {
            return String::from_utf8(head).map_err(|_| "non-utf8 head".to_string());
        }
    }
    Err("request head too large".to_string())
}

/// Extract the `host:port` from a `CONNECT` request line. Any other
/// method is refused: an absolute-form request would make the broker
/// an open forward proxy, and a worker that wants plain HTTP can use
/// TLS instead.
pub fn parse_connect(head: &str) -> Option<String> {
    let line = head.lines().next()?;
    let mut parts = line.split_whitespace();
    if !parts.next()?.eq_ignore_ascii_case("CONNECT") {
        return None;
    }
    let target = parts.next()?;
    if !parts.next()?.starts_with("HTTP/") {
        return None;
    }
    if target.len() > 300 || target.contains('/') || target.contains('@') {
        return None;
    }
    // A non-ASCII target has not been through IDNA and cannot be
    // compared byte-for-byte against a grant; refuse rather than guess
    // at an encoding.
    if !target.is_ascii() {
        return None;
    }
    Some(target.to_ascii_lowercase())
}

/// Exact match against the granted endpoints. No suffix rules, no
/// wildcards: the grant already named the host.
///
/// The comparison is over a normalized target — brackets stripped from
/// an IPv6 literal, one trailing root dot removed — because
/// `example.com.` and `example.com` resolve to the same name and a
/// grant naming one must not be bypassed by writing the other.
pub fn match_endpoint(target: &str, allowed: &[Endpoint]) -> Option<Endpoint> {
    let (host, port) = target.rsplit_once(':')?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    // Exactly one trailing dot is the absolute-name form; more than one
    // is malformed rather than equivalent.
    let host = match host.strip_suffix('.') {
        Some(stripped) if !stripped.ends_with('.') => stripped,
        Some(_) => return None,
        None => host,
    };
    if host.is_empty() || host.contains(':') {
        // A bare IPv6 literal without brackets cannot be split on `:`
        // unambiguously, so it is refused rather than parsed loosely.
        return None;
    }
    let port: u16 = port.parse().ok()?;
    allowed
        .iter()
        .find(|endpoint| endpoint.port == port && endpoint.host == host)
        .cloned()
}

/// Resolve the endpoint and pin one globally routable address.
fn resolve_public(endpoint: &Endpoint) -> Result<SocketAddr, String> {
    let addresses: Vec<SocketAddr> = (endpoint.host.as_str(), endpoint.port)
        .to_socket_addrs()
        .map_err(|error| format!("resolve {}: {error}", endpoint.host))?
        .collect();
    if addresses.is_empty() {
        return Err(format!("{} did not resolve", endpoint.host));
    }
    // Every answer must be routable, not just the one we pick: a name
    // that resolves to a mix of public and private addresses is exactly
    // the rebinding shape this check exists to refuse.
    for address in &addresses {
        if !is_globally_routable(address.ip()) {
            return Err(format!("{} resolves to a blocked address", endpoint.host));
        }
    }
    Ok(addresses[0])
}

/// Is this address safe for a sandboxed worker to reach?
pub fn is_globally_routable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_multicast()
                || v4.is_unspecified()
                // 100.64.0.0/10 carrier-grade NAT
                || (octets[0] == 100 && (64..128).contains(&octets[1]))
                // 0.0.0.0/8 "this network"
                || octets[0] == 0
                // 192.0.0.0/24 IETF protocol assignments
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
                // 198.18.0.0/15 benchmarking
                || (octets[0] == 198 && (18..20).contains(&octets[1]))
                // 240.0.0.0/4 reserved
                || octets[0] >= 240)
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // fe80::/10 link-local
                || (segments[0] & 0xffc0) == 0xfe80
                // fc00::/7 unique local
                || (segments[0] & 0xfe00) == 0xfc00
                // ::ffff:0:0/96 IPv4-mapped — judged on the mapped
                // address instead of being trusted as v6.
                || v6.to_ipv4_mapped().is_some_and(|v4| {
                    !is_globally_routable(IpAddr::V4(v4))
                }))
        }
    }
}

/// Bidirectional copy with per-direction byte ceilings.
#[cfg(unix)]
fn relay(client: std::os::unix::net::UnixStream, upstream: TcpStream) {
    let Ok(client_read) = client.try_clone() else {
        return;
    };
    let Ok(upstream_read) = upstream.try_clone() else {
        return;
    };
    let _ = client.set_read_timeout(Some(RELAY_IDLE_DEADLINE));
    let _ = upstream.set_read_timeout(Some(RELAY_IDLE_DEADLINE));

    let outbound = std::thread::spawn(move || {
        let mut source = client_read;
        let mut sink = upstream;
        copy_bounded(&mut source, &mut sink);
        let _ = sink.shutdown(std::net::Shutdown::Write);
    });
    let mut source = upstream_read;
    let mut sink = client;
    copy_bounded(&mut source, &mut sink);
    let _ = sink.shutdown(std::net::Shutdown::Write);
    let _ = outbound.join();
}

fn copy_bounded(source: &mut impl Read, sink: &mut impl Write) {
    let mut buffer = [0_u8; 32 * 1024];
    let mut total = 0_u64;
    loop {
        let read = match source.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        total = total.saturating_add(read as u64);
        if total > MAX_TUNNEL_BYTES || sink.write_all(&buffer[..read]).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/worker/net_broker.rs"
    ));
}
