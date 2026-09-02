//! Hostile-worker isolation.
//!
//! Everything Claw OS executes that it did not write — Python and
//! polyglot App operations, GUI App surfaces, MCP servers, adapters,
//! and model-authored commands — runs through this module. There is
//! one definition of what such a process may do ([`policy`]), one
//! trusted place that derives it ([`derive`]), and one provider that
//! enforces it ([`provider`], implemented for Linux by [`linux`]).
//!
//! ## Trust tiers
//!
//! | Tier | Example | Sandbox | Display | Egress |
//! | --- | --- | --- | --- | --- |
//! | `AppOperation` | `cos app fs read` | yes | no | brokered, exact hosts |
//! | `DesktopSurface` | `cos app notes --gui` | yes | yes | brokered, exact hosts |
//! | `McpServer` | a configured stdio MCP server, an adapter, an App session server | yes | no | denied |
//! | `AgentExec` | `cos_sandbox exec` | yes | no | brokered, exact hosts |
//! | `TrustedDesktopSession` | a vendor App session server that needs the session bus | yes | one exact socket | denied |
//! | `TrustedNativeHost` | the root-owned `mail-ai` native host | no | yes | host |
//!
//! Only the last tier is exempt, it is selected by a kernel-side
//! allowlist rather than by anything in a manifest, and it is the only
//! place [`provider::exemption_reason`] returns a value.
//!
//! ## Failing closed
//!
//! A launch is refused — never downgraded — when bubblewrap is
//! missing, unprivileged user namespaces are disabled, seccomp is
//! unavailable, no resource governor can be established, the policy
//! does not validate, or a granted path resolves into a kernel-owned
//! root. `cos` on a non-Linux host cannot isolate anything and refuses
//! every hostile-worker launch outright.

pub mod audit;
pub mod broker;
#[cfg(target_os = "linux")]
pub mod cgroup;
pub mod derive;
pub mod exec;
#[cfg(target_os = "linux")]
pub mod linux;
pub(crate) mod migrate;
pub mod net_broker;
pub mod policy;
pub mod provider;
pub mod runtime;
#[cfg(target_os = "linux")]
pub mod seccomp;
pub mod trusted_desktop;

pub use exec::{run_captured, WorkerOutput};
pub use policy::{
    Endpoint, LaunchPolicy, Limits, Mount, MountClass, MountMode, NetworkPolicy, SeccompProfile,
    StdioPlan, TrustTier,
};
pub use provider::{
    availability, exemption_reason, install_relay, prepare, relay_slot, Availability,
    BrokerAuthority, Governor, LaunchResources, PreparedLaunch, RelayHandle, WorkerLaunch,
    WorkerSandbox,
};

/// Set inside every sandboxed worker. Its presence is what tells the
/// in-sandbox `cos` to ask the launch broker for capability decisions
/// instead of reading a session registry that is not mounted.
///
/// It is not a security boundary: a worker can unset it, which only
/// makes its own advisory checks fail closed. The real boundary is the
/// mount, network and syscall policy around it.
pub const SANDBOX_MARKER_ENV: &str = "COS_WORKER_SANDBOX";

/// Path the brokered egress endpoint appears at inside the sandbox.
pub fn linux_egress_socket() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        linux::SANDBOX_EGRESS_SOCKET
    }
    #[cfg(not(target_os = "linux"))]
    {
        "/run/cos/worker-egress.sock"
    }
}

/// Is the current process running inside a worker sandbox?
pub fn in_sandbox() -> bool {
    std::env::var_os(SANDBOX_MARKER_ENV).is_some()
}

/// Authenticated uid of a unix-socket peer.
///
/// `SO_PEERCRED` is the kernel's answer, not the peer's: it cannot be
/// forged by anything the worker sends. A platform without it returns
/// `None`, and every caller treats that as a refusal.
#[cfg(unix)]
pub fn peer_uid_of(stream: &std::os::unix::net::UnixStream) -> Option<u32> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;

        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let code = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                std::ptr::addr_of_mut!(credentials).cast::<libc::c_void>(),
                &mut length,
            )
        };
        (code == 0).then_some(credentials.uid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = stream;
        None
    }
}

/// One-line explanation of why a launch was refused, for callers that
/// surface the failure to a user or to the model.
pub fn refusal_hint() -> String {
    let availability = availability();
    if availability.is_available() {
        return String::new();
    }
    availability.refusal()
}
