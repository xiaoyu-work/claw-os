//! Admission control for the broker socket.
//!
//! Four independent ceilings, all fixed at startup and none of them
//! derived from anything a caller sends:
//!
//! * open connections, globally and per authenticated principal;
//! * requests in flight, globally and per authenticated principal;
//! * requests in flight per route, from that route's declared budget;
//! * a bounded, fixed-capacity record of recent mutations, so a
//!   replayed frame cannot repeat a non-idempotent privileged call.
//!
//! Root gets a separately justified — but still finite — allowance:
//! `clawd`'s own internal clients (package and unit rollback, the
//! approval helper, the heartbeat) run as root, and starving them would
//! break rollback while a user floods the socket. Nothing gets an
//! unbounded allowance.
//!
//! Connection accounting is keyed by the uid `SO_PEERCRED` reports at
//! accept. That is *not* used as authority anywhere — authority comes
//! from the credentials the kernel attaches to the request message —
//! it is only a bucket to spread a fixed budget across principals
//! before any message has been read.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::super::routes::{Command, Route, ROUTES};
use super::super::wire::Fault;

/// Fixed ceilings for one daemon.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_connections: usize,
    pub max_connections_per_user: usize,
    pub max_connections_for_root: usize,
    pub max_in_flight: usize,
    pub max_in_flight_per_user: usize,
    pub max_in_flight_for_root: usize,
    /// How long a peer has to deliver a complete request frame once it
    /// is connected. This is the slowloris bound: an idle or dribbling
    /// connection is closed rather than held.
    pub read_deadline: Duration,
    /// How long the daemon will spend pushing one response.
    pub write_deadline: Duration,
    /// How long a repeated mutation id stays recognisable.
    pub duplicate_window: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_connections: 512,
            max_connections_per_user: 64,
            max_connections_for_root: 128,
            max_in_flight: 128,
            max_in_flight_per_user: 16,
            max_in_flight_for_root: 32,
            read_deadline: Duration::from_secs(10),
            write_deadline: Duration::from_secs(60),
            duplicate_window: Duration::from_secs(120),
        }
    }
}

/// Recent mutations, in a ring that never grows.
///
/// Capacity is fixed and entries are overwritten oldest-first, so this
/// is a bounded duplicate detector and not a replay cache: a flood of
/// distinct ids evicts history rather than consuming memory, and the
/// worst outcome of eviction is that a very old replay is served again
/// — which one-request-per-connection plus the peer's own liveness
/// already makes uninteresting.
const DUPLICATE_CAPACITY: usize = 1024;

struct Duplicates {
    entries: Vec<Option<(u64, Instant)>>,
    next: usize,
}

impl Duplicates {
    fn new() -> Self {
        Self {
            entries: vec![None; DUPLICATE_CAPACITY],
            next: 0,
        }
    }

    fn admit(&mut self, key: u64, now: Instant, window: Duration) -> bool {
        for slot in self.entries.iter().flatten() {
            if slot.0 == key && now.duration_since(slot.1) < window {
                return false;
            }
        }
        self.entries[self.next] = Some((key, now));
        self.next = (self.next + 1) % self.entries.len();
        true
    }
}

#[derive(Default)]
struct Counters {
    total: usize,
    per_uid: HashMap<u32, usize>,
}

impl Counters {
    fn admit(&mut self, uid: u32, total_max: usize, per_uid_max: usize) -> bool {
        if self.total >= total_max {
            return false;
        }
        let slot = self.per_uid.entry(uid).or_insert(0);
        if *slot >= per_uid_max {
            return false;
        }
        *slot += 1;
        self.total += 1;
        true
    }

    fn release(&mut self, uid: u32) {
        self.total = self.total.saturating_sub(1);
        if let Some(slot) = self.per_uid.get_mut(&uid) {
            *slot = slot.saturating_sub(1);
            if *slot == 0 {
                self.per_uid.remove(&uid);
            }
        }
    }
}

pub struct Admission {
    limits: Limits,
    connections: Mutex<Counters>,
    in_flight: Mutex<Counters>,
    routes: Vec<AtomicU32>,
    duplicates: Mutex<Duplicates>,
}

impl Admission {
    pub fn new(limits: Limits) -> Arc<Self> {
        Arc::new(Self {
            limits,
            connections: Mutex::new(Counters::default()),
            in_flight: Mutex::new(Counters::default()),
            routes: ROUTES.iter().map(|_| AtomicU32::new(0)).collect(),
            duplicates: Mutex::new(Duplicates::new()),
        })
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    fn connection_ceiling(&self, uid: u32) -> usize {
        if uid == 0 {
            self.limits.max_connections_for_root
        } else {
            self.limits.max_connections_per_user
        }
    }

    fn in_flight_ceiling(&self, uid: u32) -> usize {
        if uid == 0 {
            self.limits.max_in_flight_for_root
        } else {
            self.limits.max_in_flight_per_user
        }
    }

    pub fn accept_connection(self: &Arc<Self>, uid: u32) -> Option<ConnectionPermit> {
        let admitted = self.connections.lock().ok()?.admit(
            uid,
            self.limits.max_connections,
            self.connection_ceiling(uid),
        );
        admitted.then(|| ConnectionPermit {
            admission: Arc::clone(self),
            uid,
        })
    }

    pub fn accept_request(self: &Arc<Self>, uid: u32) -> Result<RequestPermit, Fault> {
        let mut guard = self.in_flight.lock().map_err(|_| Fault::TooManyRequests)?;
        if guard.admit(uid, self.limits.max_in_flight, self.in_flight_ceiling(uid)) {
            drop(guard);
            Ok(RequestPermit {
                admission: Arc::clone(self),
                uid,
            })
        } else {
            Err(Fault::TooManyRequests)
        }
    }

    pub fn accept_route(self: &Arc<Self>, route: &'static Route) -> Result<RoutePermit, Fault> {
        let index = route.command as usize;
        let slot = &self.routes[index];
        let mut current = slot.load(Ordering::Acquire);
        loop {
            if current >= route.budget.max_in_flight {
                return Err(Fault::RouteBusy);
            }
            match slot.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(RoutePermit {
                        admission: Arc::clone(self),
                        command: route.command,
                    })
                }
                Err(seen) => current = seen,
            }
        }
    }

    /// Refuse a mutation whose correlation id this principal already
    /// spent inside the duplicate window.
    ///
    /// The key mixes the authenticated principal — uid, pid and the
    /// process start time the daemon re-read from `/proc` — with the
    /// route and the id, so one caller's id can never collide with
    /// another's, and a recycled pid is a different principal.
    pub fn admit_mutation(&self, key: u64) -> Result<(), Fault> {
        let mut guard = self
            .duplicates
            .lock()
            .map_err(|_| Fault::DuplicateRequest)?;
        if guard.admit(key, Instant::now(), self.limits.duplicate_window) {
            Ok(())
        } else {
            Err(Fault::DuplicateRequest)
        }
    }
}

pub struct ConnectionPermit {
    admission: Arc<Admission>,
    uid: u32,
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.admission.connections.lock() {
            guard.release(self.uid);
        }
    }
}

pub struct RequestPermit {
    admission: Arc<Admission>,
    uid: u32,
}

impl Drop for RequestPermit {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.admission.in_flight.lock() {
            guard.release(self.uid);
        }
    }
}

pub struct RoutePermit {
    admission: Arc<Admission>,
    command: Command,
}

impl Drop for RoutePermit {
    fn drop(&mut self) {
        self.admission.routes[self.command as usize].fetch_sub(1, Ordering::AcqRel);
    }
}

/// Stable key for the duplicate detector.
pub fn mutation_key(
    uid: u32,
    pid: u32,
    start_time_ticks: u64,
    command: Command,
    request_id: &str,
) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    uid.hash(&mut hasher);
    pid.hash(&mut hasher);
    start_time_ticks.hash(&mut hasher);
    (command as usize).hash(&mut hasher);
    request_id.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/transport/limits.rs"
    ));
}
