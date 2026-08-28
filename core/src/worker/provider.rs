//! Provider seam for hostile-worker isolation.
//!
//! Consumers (the App bridge, the MCP attach path, the agent sandbox
//! tool) never build a `Command` for untrusted code themselves. They
//! derive a [`LaunchPolicy`], hand it to [`prepare`], and spawn the
//! command the provider gives back.
//!
//! There is exactly one provider today ([`super::linux`]). Every other
//! platform resolves to [`Unsupported`], which refuses to prepare
//! anything: a host that cannot enforce the policy does not get to run
//! the worker.

use std::process::Command;

use super::policy::{LaunchPolicy, TrustTier};
use crate::caps::CapSet;

/// Authority the per-launch broker endpoint answers with.
///
/// This is the *only* channel through which a sandboxed worker can
/// reach kernel authority, and it is deliberately not a capability
/// snapshot. `base_caps` is the set the launcher derived at
/// registration and is used only as a fallback; every check reads the
/// live routed registry row instead, so a transient capability set for
/// one MCP tool call is honoured while it is set and gone the moment it
/// is cleared.
///
/// `relay` is the opaque grant `clawd` issues to *this launcher
/// process* when the session is bound. The endpoint has to exist before
/// the worker is spawned and the grant only exists after the spawn, so
/// the cell is shared with the session object and filled in on bind. It
/// never enters the sandbox.
#[derive(Clone, Debug)]
pub struct BrokerAuthority {
    pub session_id: String,
    pub app_id: Option<String>,
    pub base_caps: CapSet,
    relay: RelayHandle,
}

/// Shared slot holding the launcher-bound relay grant.
pub type RelayHandle = std::sync::Arc<std::sync::Mutex<Option<String>>>;

/// A fresh, empty relay slot.
pub fn relay_slot() -> RelayHandle {
    std::sync::Arc::new(std::sync::Mutex::new(None))
}

/// Install the grant `clawd` issued at bind time.
pub fn install_relay(slot: &RelayHandle, handle: Option<String>) {
    if let Ok(mut guard) = slot.lock() {
        *guard = handle;
    }
}

impl BrokerAuthority {
    pub fn new(
        session_id: impl Into<String>,
        app_id: Option<String>,
        base_caps: CapSet,
        relay: RelayHandle,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            app_id,
            base_caps,
            relay,
        }
    }

    /// The launcher's relay grant, if the session has been bound.
    /// Without one the endpoint answers policy questions and refuses
    /// every broker route.
    pub fn relay_handle(&self) -> Option<String> {
        self.relay.lock().ok().and_then(|guard| guard.clone())
    }

    /// The session's capabilities right now, base plus transient.
    pub fn live_caps(&self) -> CapSet {
        super::broker::live_session_caps(&self.session_id, &self.base_caps)
    }
}

/// A policy plus the runtime authority it is launched with.
pub struct WorkerLaunch {
    pub policy: LaunchPolicy,
    /// `None` means the worker gets no broker endpoint at all — the
    /// sandbox contains no path to kernel authority.
    pub authority: Option<BrokerAuthority>,
}

impl WorkerLaunch {
    pub fn new(policy: LaunchPolicy) -> Self {
        Self {
            policy,
            authority: None,
        }
    }

    pub fn with_authority(mut self, authority: BrokerAuthority) -> Self {
        self.authority = Some(authority);
        self
    }
}

/// How resource ceilings are enforced for a launch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Governor {
    /// A dedicated cgroup v2 directory created under a delegated
    /// subtree. Memory, CPU and task ceilings are kernel-enforced and
    /// the whole tree can be killed atomically.
    Cgroup,
    /// POSIX resource limits installed before `exec`, plus a
    /// launcher-owned wall-clock deadline and process-group kill.
    /// Used when no delegated cgroup subtree is available.
    Rlimit,
}

impl Governor {
    pub const fn as_str(self) -> &'static str {
        match self {
            Governor::Cgroup => "cgroup-v2",
            Governor::Rlimit => "rlimit",
        }
    }
}

/// Whether this host can enforce the hostile-worker policy at all.
#[derive(Clone, Debug)]
pub struct Availability {
    pub provider: &'static str,
    pub governor: Option<Governor>,
    /// Empty when the provider can enforce the full policy.
    pub missing: Vec<String>,
}

impl Availability {
    pub fn is_available(&self) -> bool {
        self.missing.is_empty() && self.governor.is_some()
    }

    /// Fail-closed error text naming every missing facility.
    pub fn refusal(&self) -> String {
        if self.governor.is_none() && self.missing.is_empty() {
            return "worker isolation unavailable: no enforceable resource governor".to_string();
        }
        format!("worker isolation unavailable: {}", self.missing.join("; "))
    }
}

/// Everything the launcher must keep alive while the worker runs.
///
/// Dropping it closes the broker endpoints and releases the sandbox
/// runtime directory, so a worker cannot outlive the authority that
/// launched it.
pub struct LaunchResources {
    #[allow(dead_code)]
    pub(crate) broker: Option<super::broker::BrokerEndpoint>,
    #[allow(dead_code)]
    pub(crate) egress: Option<super::net_broker::EgressEndpoint>,
    #[allow(dead_code)]
    pub(crate) runtime_dir: Option<super::runtime::LaunchDir>,
    #[allow(dead_code)]
    pub(crate) seccomp: Option<std::fs::File>,
    #[cfg(target_os = "linux")]
    #[allow(dead_code)]
    pub(crate) pinned: Option<super::linux::PinnedSources>,
    pub(crate) cgroup: Option<super::cgroup::Scope>,
}

impl LaunchResources {
    pub fn empty() -> Self {
        Self {
            broker: None,
            egress: None,
            runtime_dir: None,
            seccomp: None,
            #[cfg(target_os = "linux")]
            pinned: None,
            cgroup: None,
        }
    }

    /// Kill everything still running under this launch. Safe to call
    /// more than once.
    pub fn kill_all(&self, child_pid: Option<u32>) {
        if let Some(scope) = &self.cgroup {
            scope.kill();
        }
        #[cfg(unix)]
        if let Some(pid) = child_pid {
            // The child is its own process-group leader (`setsid` in
            // `pre_exec`), so this reaches every descendant that
            // survived the cgroup kill or that ran without one.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        #[cfg(not(unix))]
        let _ = child_pid;
    }
}

/// A launch the provider has fully prepared but not yet started.
pub struct PreparedLaunch {
    /// Ready to spawn. Consumers may only set stdio and spawn it;
    /// mutating the environment or argv would defeat the policy.
    pub command: Command,
    /// Typed, secret-free audit projection of the enforced policy.
    pub facts: serde_json::Value,
    pub governor: Governor,
    pub resources: LaunchResources,
}

pub trait WorkerSandbox: Send + Sync {
    fn name(&self) -> &'static str;
    fn availability(&self) -> Availability;
    fn prepare(&self, launch: &WorkerLaunch) -> Result<PreparedLaunch, String>;
}

/// Provider used on hosts with no enforceable isolation. It never
/// prepares a command; the launch fails closed instead.
pub struct Unsupported;

impl WorkerSandbox for Unsupported {
    fn name(&self) -> &'static str {
        "unsupported"
    }

    fn availability(&self) -> Availability {
        Availability {
            provider: "unsupported",
            governor: None,
            missing: vec![
                "hostile-worker isolation requires Linux namespaces, seccomp and bubblewrap"
                    .to_string(),
            ],
        }
    }

    fn prepare(&self, _launch: &WorkerLaunch) -> Result<PreparedLaunch, String> {
        Err(self.availability().refusal())
    }
}

#[cfg(target_os = "linux")]
static PROVIDER: super::linux::LinuxSandbox = super::linux::LinuxSandbox;
#[cfg(not(target_os = "linux"))]
static PROVIDER: Unsupported = Unsupported;

pub fn provider() -> &'static dyn WorkerSandbox {
    &PROVIDER
}

pub fn availability() -> Availability {
    provider().availability()
}

/// Prepare `launch` for execution.
///
/// Fails closed on every path: an unusable provider, a policy that
/// does not validate, a tier that is not allowed to be sandboxed here,
/// or any missing isolation facility.
pub fn prepare(launch: &WorkerLaunch) -> Result<PreparedLaunch, String> {
    launch.policy.validate()?;
    if !launch.policy.tier.is_sandboxed() {
        return Err(format!(
            "tier `{}` is not launched through the hostile-worker sandbox",
            launch.policy.tier.as_str()
        ));
    }
    let provider = provider();
    let availability = provider.availability();
    if !availability.is_available() {
        return Err(availability.refusal());
    }
    provider.prepare(launch)
}

/// Human-readable reason a tier is exempt from the sandbox, for audit.
pub fn exemption_reason(tier: TrustTier) -> Option<&'static str> {
    match tier {
        TrustTier::TrustedNativeHost => Some(
            "root-owned native host: drives privileged desktop transports that cannot \
             be reconstructed inside a mount and PID namespace",
        ),
        _ => None,
    }
}
