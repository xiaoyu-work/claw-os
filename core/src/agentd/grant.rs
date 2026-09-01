//! Job grants — the only authority an `agentd` worker ever holds.
//!
//! A grant is minted by the broker *after* the worker has been forked,
//! so it can bind the exact process it is meant for: owner uid, worker
//! pid plus the kernel start-time that makes a recycled pid detectable,
//! the task and session it may report on, a lease deadline, and the
//! route allowlist the channel accepts. It is signed with an HMAC key
//! generated per broker process and never written to disk or handed to
//! a child, so a worker cannot mint one, edit one, or replay one
//! against a different worker.
//!
//! Nothing else grants authority. Executable path, controlling
//! terminal, `PR_SET_NO_NEW_PRIVS`, socket group membership and prompt
//! text are all irrelevant to [`GrantSigner::verify`]: a frame is
//! accepted only when the presented grant's own claims match what the
//! broker recorded when it spawned that worker.

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

/// Wire format version. Bumped whenever the claim set changes shape so
/// a mixed old/new install fails closed instead of mis-parsing.
pub const GRANT_VERSION: u32 = 7;

/// Intended recipient of the grant. A token issued for the worker
/// channel is meaningless anywhere else because every verifier requires
/// its own audience string.
pub const GRANT_AUDIENCE: &str = "cos.agentd.worker.v1";

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantError {
    Version { expected: u32, actual: u32 },
    Audience { expected: String, actual: String },
    Signature,
    Broker { expected: u32, actual: u32 },
    Task { expected: String, actual: String },
    Session,
    Client,
    Presence,
    CapabilityGeneration,
    ExecutionNonce,
    Extension,
    Owner { expected: u32, actual: u32 },
    OwnerGid { expected: u32, actual: u32 },
    WorkerPid { expected: u32, actual: u32 },
    WorkerIdentity,
    Expired { now_ms: u64, expires_at_ms: u64 },
    Route(String),
}

impl std::fmt::Display for GrantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GrantError::Version { expected, actual } => write!(
                f,
                "agentd grant version {actual} is not supported (expected {expected})"
            ),
            GrantError::Audience { expected, actual } => {
                write!(f, "agentd grant audience `{actual}` is not `{expected}`")
            }
            GrantError::Signature => f.write_str("agentd grant signature is invalid"),
            GrantError::Broker { expected, actual } => write!(
                f,
                "agentd grant was issued by broker pid {actual}, not {expected}"
            ),
            GrantError::Task { expected, actual } => {
                write!(f, "agentd grant is bound to task {actual}, not {expected}")
            }
            GrantError::Session => f.write_str("agentd grant is bound to a different session"),
            GrantError::Client => {
                f.write_str("agentd grant is bound to different session client metadata")
            }
            GrantError::Presence => {
                f.write_str("agentd grant is bound to a different presence lease")
            }
            GrantError::CapabilityGeneration => {
                f.write_str("agentd grant is bound to a different capability generation")
            }
            GrantError::ExecutionNonce => {
                f.write_str("agentd grant is bound to a different execution commit nonce")
            }
            GrantError::Extension => {
                f.write_str("agentd grant is bound to a different extension host")
            }
            GrantError::Owner { expected, actual } => write!(
                f,
                "agentd grant is bound to owner uid {actual}, not {expected}"
            ),
            GrantError::OwnerGid { expected, actual } => write!(
                f,
                "agentd grant is bound to isolated gid {actual}, not {expected}"
            ),
            GrantError::WorkerPid { expected, actual } => write!(
                f,
                "agentd grant is bound to worker pid {actual}, not {expected}"
            ),
            GrantError::WorkerIdentity => {
                f.write_str("agentd grant worker start-time does not match the running process")
            }
            GrantError::Expired {
                now_ms,
                expires_at_ms,
            } => write!(
                f,
                "agentd grant lease expired at {expires_at_ms}ms (now {now_ms}ms)"
            ),
            GrantError::Route(route) => {
                write!(f, "agentd grant does not allow route `{route}`")
            }
        }
    }
}

/// Everything a grant asserts. Serialized verbatim into the signing
/// input, so any change to a field invalidates the signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantClaims {
    pub v: u32,
    pub audience: String,
    /// pid of the `clawd` process that issued the grant. A grant minted
    /// by a previous daemon instance is rejected after a restart.
    pub broker_pid: u32,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub owner_uid: u32,
    pub owner_gid: u32,
    pub client: crate::session::SessionClient,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<crate::session::SessionPresence>,
    pub capability_generation: String,
    pub prepare_nonce: String,
    pub commit_nonce: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension: Option<crate::extension_host::protocol::ExtensionBinding>,
    pub worker_pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_start_time_ticks: Option<u64>,
    pub issued_at_ms: u64,
    /// Lease deadline. The supervisor extends it on heartbeat by
    /// re-issuing the grant; a worker that stops heartbeating loses the
    /// channel and its task is reconciled.
    pub expires_at_ms: u64,
    pub routes: Vec<String>,
}

impl GrantClaims {
    pub fn allows_route(&self, route: &str) -> bool {
        self.routes.iter().any(|allowed| allowed == route)
    }

    /// Deterministic, length-prefixed signing input. Explicit framing
    /// rather than JSON so no two distinct claim sets can ever collide
    /// through field reordering or string escaping.
    fn signing_input(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(256);
        push_u64(&mut buf, self.v as u64);
        push_bytes(&mut buf, self.audience.as_bytes());
        push_u64(&mut buf, self.broker_pid as u64);
        push_bytes(&mut buf, self.task_id.as_bytes());
        match self.session_id.as_deref() {
            Some(session_id) => {
                push_u64(&mut buf, 1);
                push_bytes(&mut buf, session_id.as_bytes());
            }
            None => push_u64(&mut buf, 0),
        }
        push_u64(&mut buf, self.owner_uid as u64);
        push_u64(&mut buf, self.owner_gid as u64);
        push_bytes(&mut buf, self.client.source.as_str().as_bytes());
        push_u64(&mut buf, u64::from(self.client.attended));
        push_u64(&mut buf, u64::from(self.client.local));
        match self.presence {
            Some(presence) => {
                push_u64(&mut buf, 1);
                push_u64(&mut buf, presence.owner_uid as u64);
                push_u64(&mut buf, presence.pid as u64);
                push_u64(&mut buf, presence.start_time_ticks);
                push_u64(&mut buf, presence.expires_at_ms);
            }
            None => push_u64(&mut buf, 0),
        }
        push_bytes(&mut buf, self.capability_generation.as_bytes());
        push_bytes(&mut buf, self.prepare_nonce.as_bytes());
        push_bytes(&mut buf, self.commit_nonce.as_bytes());
        match &self.extension {
            Some(extension) => {
                push_u64(&mut buf, 1);
                push_u64(&mut buf, extension.protocol as u64);
                push_bytes(&mut buf, extension.task_id.as_bytes());
                match extension.session_id.as_deref() {
                    Some(session_id) => {
                        push_u64(&mut buf, 1);
                        push_bytes(&mut buf, session_id.as_bytes());
                    }
                    None => push_u64(&mut buf, 0),
                }
                push_u64(&mut buf, extension.owner_uid as u64);
                push_u64(&mut buf, extension.extension_uid as u64);
                push_u64(&mut buf, extension.owner_gid as u64);
                push_u64(&mut buf, extension.worker_pid as u64);
                push_optional_u64(&mut buf, extension.worker_start_time_ticks);
                push_u64(&mut buf, extension.host_pid as u64);
                push_optional_u64(&mut buf, extension.host_start_time_ticks);
                push_bytes(&mut buf, extension.lease_nonce.as_bytes());
                push_u64(&mut buf, extension.expires_at_ms);
                push_bytes(&mut buf, extension.control_socket.as_bytes());
                push_bytes(&mut buf, extension.broker_socket.as_bytes());
            }
            None => push_u64(&mut buf, 0),
        }
        push_u64(&mut buf, self.worker_pid as u64);
        match self.worker_start_time_ticks {
            Some(ticks) => {
                push_u64(&mut buf, 1);
                push_u64(&mut buf, ticks);
            }
            None => push_u64(&mut buf, 0),
        }
        push_u64(&mut buf, self.issued_at_ms);
        push_u64(&mut buf, self.expires_at_ms);
        push_u64(&mut buf, self.routes.len() as u64);
        for route in &self.routes {
            push_bytes(&mut buf, route.as_bytes());
        }
        buf
    }
}

fn push_u64(buf: &mut Vec<u8>, value: u64) {
    buf.extend_from_slice(&value.to_be_bytes());
}

fn push_bytes(buf: &mut Vec<u8>, value: &[u8]) {
    push_u64(buf, value.len() as u64);
    buf.extend_from_slice(value);
}

fn push_optional_u64(buf: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(value) => {
            push_u64(buf, 1);
            push_u64(buf, value);
        }
        None => push_u64(buf, 0),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedGrant {
    pub claims: GrantClaims,
    pub mac: String,
}

/// What the broker requires of a grant presented on a worker channel.
#[derive(Debug, Clone)]
pub struct GrantExpectation {
    pub broker_pid: u32,
    pub task_id: String,
    pub session_id: Option<String>,
    pub owner_uid: u32,
    pub owner_gid: u32,
    pub client: crate::session::SessionClient,
    pub presence: Option<crate::session::SessionPresence>,
    pub capability_generation: String,
    pub prepare_nonce: String,
    pub commit_nonce: String,
    pub extension: Option<crate::extension_host::protocol::ExtensionBinding>,
    pub worker_pid: u32,
    pub worker_start_time_ticks: Option<u64>,
    pub route: String,
}

/// Per-process HMAC key. Held only in the broker's memory: it is never
/// serialized, exported through the channel, or inherited by a child,
/// so the signing capability cannot leave the daemon.
pub struct GrantSigner {
    secret: [u8; 32],
}

impl std::fmt::Debug for GrantSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GrantSigner(<redacted>)")
    }
}

impl GrantSigner {
    pub fn generate() -> Result<Self, String> {
        Ok(Self {
            secret: random_secret()?,
        })
    }

    pub fn from_secret(secret: [u8; 32]) -> Self {
        Self { secret }
    }

    pub fn issue(&self, claims: GrantClaims) -> SignedGrant {
        let mac = self.sign(&claims);
        SignedGrant { claims, mac }
    }

    fn sign(&self, claims: &GrantClaims) -> String {
        let mut mac =
            HmacSha256::new_from_slice(&self.secret).expect("HMAC accepts keys of any length");
        mac.update(&claims.signing_input());
        hex::encode(mac.finalize().into_bytes())
    }

    /// Authenticate a presented grant and check every binding the
    /// broker recorded when it spawned the worker. The signature is
    /// checked first so mismatched claims cannot be probed against an
    /// unauthenticated token.
    pub fn verify(
        &self,
        grant: &SignedGrant,
        expect: &GrantExpectation,
        now_ms: u64,
    ) -> Result<(), GrantError> {
        if grant.claims.v != GRANT_VERSION {
            return Err(GrantError::Version {
                expected: GRANT_VERSION,
                actual: grant.claims.v,
            });
        }
        let presented = hex::decode(&grant.mac).map_err(|_| GrantError::Signature)?;
        let mut mac =
            HmacSha256::new_from_slice(&self.secret).expect("HMAC accepts keys of any length");
        mac.update(&grant.claims.signing_input());
        mac.verify_slice(&presented)
            .map_err(|_| GrantError::Signature)?;

        if grant.claims.audience != GRANT_AUDIENCE {
            return Err(GrantError::Audience {
                expected: GRANT_AUDIENCE.to_string(),
                actual: grant.claims.audience.clone(),
            });
        }
        if grant.claims.broker_pid != expect.broker_pid {
            return Err(GrantError::Broker {
                expected: expect.broker_pid,
                actual: grant.claims.broker_pid,
            });
        }
        if grant.claims.task_id != expect.task_id {
            return Err(GrantError::Task {
                expected: expect.task_id.clone(),
                actual: grant.claims.task_id.clone(),
            });
        }
        if grant.claims.session_id != expect.session_id {
            return Err(GrantError::Session);
        }
        if grant.claims.owner_uid != expect.owner_uid {
            return Err(GrantError::Owner {
                expected: expect.owner_uid,
                actual: grant.claims.owner_uid,
            });
        }
        if grant.claims.owner_gid != expect.owner_gid {
            return Err(GrantError::OwnerGid {
                expected: expect.owner_gid,
                actual: grant.claims.owner_gid,
            });
        }
        if grant.claims.client != expect.client {
            return Err(GrantError::Client);
        }
        if grant.claims.presence != expect.presence {
            return Err(GrantError::Presence);
        }
        if grant.claims.capability_generation != expect.capability_generation {
            return Err(GrantError::CapabilityGeneration);
        }
        if grant.claims.prepare_nonce != expect.prepare_nonce
            || grant.claims.commit_nonce != expect.commit_nonce
        {
            return Err(GrantError::ExecutionNonce);
        }
        if grant.claims.extension != expect.extension {
            return Err(GrantError::Extension);
        }
        if grant.claims.worker_pid != expect.worker_pid {
            return Err(GrantError::WorkerPid {
                expected: expect.worker_pid,
                actual: grant.claims.worker_pid,
            });
        }
        if grant.claims.worker_start_time_ticks != expect.worker_start_time_ticks {
            return Err(GrantError::WorkerIdentity);
        }
        if now_ms > grant.claims.expires_at_ms {
            return Err(GrantError::Expired {
                now_ms,
                expires_at_ms: grant.claims.expires_at_ms,
            });
        }
        if !grant.claims.allows_route(&expect.route) {
            return Err(GrantError::Route(expect.route.clone()));
        }
        Ok(())
    }
}

impl SignedGrant {
    /// Checks a worker can make without the signing key: that the grant
    /// really describes *this* process, is still inside its lease, and
    /// permits the routes the worker intends to use. A worker that
    /// receives somebody else's grant refuses to start rather than
    /// running a job it cannot legitimately report on.
    pub fn validate_for_self(
        &self,
        now_ms: u64,
        uid: u32,
        gid: u32,
        pid: u32,
        start_time_ticks: Option<u64>,
    ) -> Result<(), GrantError> {
        if self.claims.v != GRANT_VERSION {
            return Err(GrantError::Version {
                expected: GRANT_VERSION,
                actual: self.claims.v,
            });
        }
        if self.claims.audience != GRANT_AUDIENCE {
            return Err(GrantError::Audience {
                expected: GRANT_AUDIENCE.to_string(),
                actual: self.claims.audience.clone(),
            });
        }
        if self.claims.owner_uid != uid {
            return Err(GrantError::Owner {
                expected: uid,
                actual: self.claims.owner_uid,
            });
        }
        if self.claims.owner_gid != gid {
            return Err(GrantError::OwnerGid {
                expected: gid,
                actual: self.claims.owner_gid,
            });
        }
        if self.claims.worker_pid != pid {
            return Err(GrantError::WorkerPid {
                expected: pid,
                actual: self.claims.worker_pid,
            });
        }
        // A missing broker-side reading is tolerated (the claim is then
        // `None` on both sides); a *mismatch* is not.
        if self.claims.worker_start_time_ticks.is_some()
            && start_time_ticks.is_some()
            && self.claims.worker_start_time_ticks != start_time_ticks
        {
            return Err(GrantError::WorkerIdentity);
        }
        if now_ms > self.claims.expires_at_ms {
            return Err(GrantError::Expired {
                now_ms,
                expires_at_ms: self.claims.expires_at_ms,
            });
        }
        Ok(())
    }
}

#[cfg(unix)]
fn random_secret() -> Result<[u8; 32], String> {
    use std::io::Read;

    let mut secret = [0u8; 32];
    let mut source = std::fs::File::open("/dev/urandom")
        .map_err(|error| format!("open /dev/urandom: {error}"))?;
    source
        .read_exact(&mut secret)
        .map_err(|error| format!("read /dev/urandom: {error}"))?;
    Ok(secret)
}

#[cfg(not(unix))]
fn random_secret() -> Result<[u8; 32], String> {
    let mut secret = [0u8; 32];
    secret[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    secret[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    Ok(secret)
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agentd/grant.rs"
    ));
}
