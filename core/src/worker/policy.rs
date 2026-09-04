//! Typed launch policy — the *definition* half of the worker sandbox
//! seam.
//!
//! A [`LaunchPolicy`] is the complete, self-contained description of
//! how one hostile worker process may run. It is produced only by
//! trusted derivation code (see [`super::derive`]) from authenticated
//! manifest / runtime / capability data, and consumed only by a
//! [`super::provider::WorkerSandbox`] implementation.
//!
//! Nothing in this type is taken from worker-controlled input:
//!
//! * the program and argv are chosen by the kernel's runtime
//!   selection, not by the worker;
//! * every mount is a canonical host path the authority already
//!   granted, paired with an explicit direction;
//! * the environment is a closed allowlist of `(name, value)` pairs,
//!   never the launcher's ambient environment;
//! * the network policy names exact endpoints, never a raw URL.
//!
//! The policy is also the audit record: [`LaunchPolicy::digest`] is a
//! stable content digest and [`LaunchPolicy::audit_facts`] projects
//! only counts, classes and enumerations — never a path, an argument
//! or a secret.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How much the kernel trusts the code about to be executed.
///
/// The tier is assigned by trusted code from the *installation* of the
/// worker, never from a manifest field a third party can set. It
/// selects the sandbox shape; it never relaxes capability enforcement.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrustTier {
    /// A third-party App operation worker: `main.py`, `main.js`, a
    /// shell entry or a packaged binary running one manifest
    /// operation. Fully hostile.
    AppOperation,
    /// A third-party MCP server or adapter speaking stdio JSON-RPC, or
    /// an App's own session server, which is the same shape. Fully
    /// hostile, and network-denied by default.
    McpServer,
    /// A model-authored command run through the agent's sandbox tool.
    /// Fully hostile, and never given App identity.
    AgentExec,
    /// A GUI App surface. Still third-party code, but it owns a window
    /// and therefore needs a display transport that a headless
    /// operation worker must never receive.
    DesktopSurface,
    /// A vendor-shipped App session server that cannot do its job
    /// without one exact desktop transport — the session bus for MPRIS,
    /// the screenshot portal, or `org.freedesktop.Notifications`.
    ///
    /// Sandboxed exactly like [`TrustTier::McpServer`]: private
    /// namespaces, the strict syscall filter, a resource governor, no
    /// egress and no host paths. The single difference is that it may
    /// hold the named transport sockets.
    ///
    /// Reaching this tier requires an authenticated, kernel-side
    /// allowlist entry checked against the package's vendor provenance
    /// and the root ownership of the artifact it executes. No manifest
    /// field, publisher signature, developer grant, path alias or App
    /// id alone selects it.
    TrustedDesktopSession,
    /// A built-in, root-owned native host that cannot run inside the
    /// sandbox (it drives privileged desktop/native transports).
    ///
    /// Reaching this tier requires an authenticated, kernel-side
    /// allowlist entry: no manifest field selects it.
    TrustedNativeHost,
}

impl TrustTier {
    pub const fn as_str(self) -> &'static str {
        match self {
            TrustTier::AppOperation => "app-operation",
            TrustTier::McpServer => "mcp-server",
            TrustTier::AgentExec => "agent-exec",
            TrustTier::DesktopSurface => "desktop-surface",
            TrustTier::TrustedDesktopSession => "trusted-desktop-session",
            TrustTier::TrustedNativeHost => "trusted-native-host",
        }
    }

    /// Does this tier execute under the hostile-worker sandbox?
    ///
    /// Only [`TrustTier::TrustedNativeHost`] is exempt, and only
    /// because it cannot function inside a mount/PID namespace. Every
    /// other tier fails closed when the sandbox is unavailable.
    pub const fn is_sandboxed(self) -> bool {
        !matches!(self, TrustTier::TrustedNativeHost)
    }

    /// May this tier receive a display / session transport?
    pub const fn allows_display(self) -> bool {
        matches!(
            self,
            TrustTier::DesktopSurface
                | TrustTier::TrustedDesktopSession
                | TrustTier::TrustedNativeHost
        )
    }
}

/// Direction of a bind mount. There is no "maybe writable": the
/// derivation decides once, from the capability that justified it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MountMode {
    ReadOnly,
    ReadWrite,
}

impl MountMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            MountMode::ReadOnly => "ro",
            MountMode::ReadWrite => "rw",
        }
    }
}

/// Why a mount exists. Audit records the class, never the path.
#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum MountClass {
    /// Interpreter, shared libraries, CA bundle — read-only system.
    System,
    /// The worker's own package directory, read-only.
    Package,
    /// Kernel-owned runtime assets (the `cos` binary, SDK trees).
    Runtime,
    /// The operation's private writable data directory.
    AppData,
    /// A host path the operation was explicitly granted for reading.
    Input,
    /// A host path the operation was explicitly granted for writing.
    Output,
    /// The narrow per-launch broker endpoint.
    BrokerIpc,
    /// A display / session transport (desktop tier only).
    Display,
    /// A device node the policy names explicitly (desktop tier only).
    Device,
}

impl MountClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            MountClass::System => "system",
            MountClass::Package => "package",
            MountClass::Runtime => "runtime",
            MountClass::AppData => "app-data",
            MountClass::Input => "input",
            MountClass::Output => "output",
            MountClass::BrokerIpc => "broker-ipc",
            MountClass::Display => "display",
            MountClass::Device => "device",
        }
    }

    /// Classes that only a tier with a display transport may hold.
    pub const fn requires_display_trust(self) -> bool {
        matches!(self, MountClass::Display | MountClass::Device)
    }
}

/// One capability-derived mount as it existed at daemon authorization.
///
/// A single-call worker accepts the mount only if path resolution and
/// inode identity still match this snapshot. The provider then pins the
/// same inode through `exec`, closing both halves of the authorization
/// to mount TOCTOU window.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedMount {
    pub source: PathBuf,
    pub target: PathBuf,
    pub mode: MountMode,
    pub class: MountClass,
    pub device: u64,
    pub inode: u64,
}

/// One canonical host path exposed inside the sandbox.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct Mount {
    /// Canonical host path. Derivation resolves symlinks before the
    /// policy is built so the provider never binds a moving target.
    pub source: PathBuf,
    /// Absolute path inside the sandbox.
    pub target: PathBuf,
    pub mode: MountMode,
    pub class: MountClass,
    /// `(st_dev, st_ino)` this source is required to have.
    ///
    /// Set for anything whose content has already been authenticated —
    /// a verified package directory, a signed entrypoint. The provider
    /// refuses to bind a different inode, so replacing the file between
    /// verification and `execve` fails the launch instead of silently
    /// running unverified bytes. `None` means "whatever the path
    /// resolves to", which is correct for system trees the package
    /// manager owns.
    #[serde(skip)]
    pub expect_identity: Option<(u64, u64)>,
}

impl Mount {
    /// Require this mount to resolve to one exact inode.
    pub fn expecting(mut self, identity: (u64, u64)) -> Self {
        self.expect_identity = Some(identity);
        self
    }

    pub fn read_only(
        source: impl Into<PathBuf>,
        target: impl Into<PathBuf>,
        class: MountClass,
    ) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            mode: MountMode::ReadOnly,
            class,
            expect_identity: None,
        }
    }

    pub fn read_write(
        source: impl Into<PathBuf>,
        target: impl Into<PathBuf>,
        class: MountClass,
    ) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            mode: MountMode::ReadWrite,
            class,
            expect_identity: None,
        }
    }
}

/// One exact egress endpoint the operation is allowed to reach.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, serde::Serialize)]
pub struct Endpoint {
    /// Lowercase DNS name or literal IP. Never a glob.
    pub host: String,
    pub port: u16,
}

impl Endpoint {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into().to_ascii_lowercase(),
            port,
        }
    }

    pub fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// Egress model. Direct networking is never granted to a hostile
/// worker: the sandbox always owns a private network namespace, and
/// approved traffic leaves through the brokered proxy instead.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum NetworkPolicy {
    /// Private network namespace, loopback only, no broker.
    Denied,
    /// Private network namespace plus a per-launch egress broker that
    /// admits exactly these endpoints.
    Brokered { endpoints: Vec<Endpoint> },
    /// The host network namespace. Reserved for
    /// [`TrustTier::TrustedNativeHost`]; a hostile tier that reaches
    /// this value is a bug and the provider rejects it.
    HostShared,
}

impl NetworkPolicy {
    pub const fn as_str(&self) -> &'static str {
        match self {
            NetworkPolicy::Denied => "denied",
            NetworkPolicy::Brokered { .. } => "brokered",
            NetworkPolicy::HostShared => "host-shared",
        }
    }

    pub fn endpoints(&self) -> &[Endpoint] {
        match self {
            NetworkPolicy::Brokered { endpoints } => endpoints,
            _ => &[],
        }
    }
}

/// Enforceable resource ceiling for one launch.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub struct Limits {
    pub memory_bytes: u64,
    pub cpu_percent: u32,
    pub pids_max: u32,
    pub open_files: u64,
    /// Largest file the worker may create, in bytes.
    pub file_size_bytes: u64,
    /// Wall-clock ceiling for the whole process group.
    #[serde(serialize_with = "serialize_secs")]
    pub runtime: Duration,
    /// Ceiling on captured stdout/stderr.
    pub output_bytes: u64,
    /// Ceiling on bytes written into the worker's stdin.
    pub input_bytes: u64,
}

fn serialize_secs<S: serde::Serializer>(value: &Duration, ser: S) -> Result<S::Ok, S::Error> {
    ser.serialize_u64(value.as_secs())
}

impl Limits {
    /// Default ceiling for a one-shot operation worker.
    pub fn operation() -> Self {
        Self {
            memory_bytes: 1024 * 1024 * 1024,
            cpu_percent: 100,
            pids_max: 128,
            open_files: 1024,
            file_size_bytes: 2 * 1024 * 1024 * 1024,
            runtime: Duration::from_secs(300),
            output_bytes: 32 * 1024 * 1024,
            input_bytes: 32 * 1024 * 1024,
        }
    }

    /// Long-lived servers get the same ceilings but no wall-clock
    /// deadline: they are torn down with their handle instead.
    pub fn server() -> Self {
        Self {
            runtime: Duration::ZERO,
            ..Self::operation()
        }
    }

    pub fn desktop() -> Self {
        Self {
            memory_bytes: 4 * 1024 * 1024 * 1024,
            pids_max: 512,
            open_files: 4096,
            runtime: Duration::ZERO,
            ..Self::operation()
        }
    }

    /// Wall-clock deadline, if any. `Duration::ZERO` means "no
    /// deadline"; a server is bounded by its handle, not by a timer.
    pub fn deadline(&self) -> Option<Duration> {
        (!self.runtime.is_zero()).then_some(self.runtime)
    }
}

/// Syscall filter strength. Both profiles deny the same
/// kernel-control syscall groups; the network profile exists only so
/// audit can distinguish a launch that was allowed to open sockets to
/// the broker relay from one that was not.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SeccompProfile {
    /// Deny namespace, mount, module, tracing, keyring, io_uring, BPF
    /// and time-control syscalls.
    Strict,
    /// `Strict` plus the socket syscalls the loopback broker relay
    /// needs. Still inside a private network namespace.
    StrictNetwork,
}

impl SeccompProfile {
    pub const fn as_str(self) -> &'static str {
        match self {
            SeccompProfile::Strict => "strict",
            SeccompProfile::StrictNetwork => "strict-network",
        }
    }
}

/// Which of the worker's standard streams the launcher owns.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StdioPlan {
    /// One-shot operation: stdin closed unless the operation declares
    /// it, stdout/stderr captured with a byte ceiling.
    Captured,
    /// Long-lived stdio JSON-RPC peer: stdin/stdout are the transport,
    /// stderr is captured line-by-line.
    Streamed,
    /// Desktop surface: the launcher's stdio is inherited so the
    /// window's own logging is visible.
    Inherited,
}

impl StdioPlan {
    pub const fn as_str(self) -> &'static str {
        match self {
            StdioPlan::Captured => "captured",
            StdioPlan::Streamed => "streamed",
            StdioPlan::Inherited => "inherited",
        }
    }
}

/// The complete description of one hostile-worker launch.
#[derive(Clone, Debug, serde::Serialize)]
pub struct LaunchPolicy {
    pub tier: TrustTier,
    /// Stable, non-sensitive identifier for logs: `app:fs/read`,
    /// `mcp:github`, `agent:exec`.
    pub label: String,
    /// Program executed inside the sandbox. Canonical host path.
    pub program: PathBuf,
    /// Arguments after the program. Chosen by trusted runtime
    /// selection and by the authority-bound effective arguments.
    pub argv: Vec<String>,
    /// Working directory *inside* the sandbox. Must be one of the
    /// mount targets.
    pub workdir: PathBuf,
    pub mounts: Vec<Mount>,
    pub network: NetworkPolicy,
    /// Closed environment allowlist. The provider clears the
    /// environment and sets exactly these.
    pub env: BTreeMap<String, String>,
    pub limits: Limits,
    pub seccomp: SeccompProfile,
    pub stdio: StdioPlan,
    /// Bind the per-launch broker endpoint at the standard broker
    /// socket path inside the sandbox. The real `clawd` socket is
    /// never visible.
    pub broker: bool,
    /// `umask` applied immediately before `exec`.
    pub umask: u32,
}

impl LaunchPolicy {
    /// Content digest over the canonical policy. Two launches with the
    /// same digest were isolated identically.
    pub fn digest(&self) -> String {
        use sha2::{Digest, Sha256};

        let canonical = serde_json::to_vec(self).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(&canonical);
        format!("sha256:{:x}", hasher.finalize())
    }

    /// Number of mounts in each class, for audit.
    pub fn mount_classes(&self) -> BTreeMap<&'static str, usize> {
        let mut counts = BTreeMap::new();
        for mount in &self.mounts {
            *counts.entry(mount.class.as_str()).or_insert(0) += 1;
        }
        counts
    }

    pub fn writable_mounts(&self) -> usize {
        self.mounts
            .iter()
            .filter(|mount| mount.mode == MountMode::ReadWrite)
            .count()
    }

    /// Typed, path-free and secret-free projection for the audit trail.
    pub fn audit_facts(&self) -> serde_json::Value {
        serde_json::json!({
            "policy": self.digest(),
            "tier": self.tier.as_str(),
            "label": self.label,
            "mounts": {
                "total": self.mounts.len(),
                "writable": self.writable_mounts(),
                "classes": self.mount_classes(),
            },
            "network": {
                "mode": self.network.as_str(),
                "endpoints": self.network.endpoints().len(),
            },
            "limits": {
                "memory_bytes": self.limits.memory_bytes,
                "cpu_percent": self.limits.cpu_percent,
                "pids_max": self.limits.pids_max,
                "open_files": self.limits.open_files,
                "runtime_secs": self.limits.runtime.as_secs(),
                "output_bytes": self.limits.output_bytes,
            },
            "seccomp": self.seccomp.as_str(),
            "stdio": self.stdio.as_str(),
            "env_names": self.env.keys().collect::<Vec<_>>(),
            "broker": self.broker,
        })
    }

    /// Structural self-check run by the provider before any untrusted
    /// code executes. Every failure here is a fail-closed error, not a
    /// downgrade.
    pub fn validate(&self) -> Result<(), String> {
        if self.label.is_empty() {
            return Err("worker launch policy has no label".to_string());
        }
        if !self.program.is_absolute() {
            return Err(format!(
                "worker launch program must be absolute: {}",
                self.program.display()
            ));
        }
        if !self.workdir.is_absolute() {
            return Err("worker launch workdir must be absolute".to_string());
        }
        if matches!(self.network, NetworkPolicy::HostShared) && self.tier.is_sandboxed() {
            return Err(format!(
                "tier `{}` may not share the host network namespace",
                self.tier.as_str()
            ));
        }
        for endpoint in self.network.endpoints() {
            validate_endpoint(endpoint)?;
        }
        let mut targets: Vec<&Path> = Vec::with_capacity(self.mounts.len());
        for mount in &self.mounts {
            if !mount.target.is_absolute() {
                return Err(format!(
                    "worker mount target must be absolute: {}",
                    mount.target.display()
                ));
            }
            if !mount.source.is_absolute() {
                return Err(format!(
                    "worker mount source must be absolute: {}",
                    mount.source.display()
                ));
            }
            if mount.class.requires_display_trust() && !self.tier.allows_display() {
                return Err(format!(
                    "tier `{}` may not receive a display transport",
                    self.tier.as_str()
                ));
            }
            if targets.contains(&mount.target.as_path()) {
                return Err(format!(
                    "duplicate worker mount target {}",
                    mount.target.display()
                ));
            }
            targets.push(mount.target.as_path());
        }
        for name in self.env.keys() {
            validate_env_name(name)?;
        }
        if self.limits.pids_max == 0 || self.limits.memory_bytes == 0 {
            return Err("worker resource limits must be non-zero".to_string());
        }
        if !matches!(self.limits.cpu_percent, 1..=100) {
            return Err("worker cpu ceiling must be between 1 and 100".to_string());
        }
        Ok(())
    }
}

/// Environment names are a closed allowlist by construction, but the
/// value still ends up on an `execve` boundary: reject anything that
/// could smuggle a second variable.
fn validate_env_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.contains('=') || name.contains('\0') {
        return Err(format!("invalid worker environment name `{name}`"));
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(format!("invalid worker environment name `{name}`"));
    }
    Ok(())
}

/// An endpoint is only ever produced from a manifest `Host` scope that
/// the authority already granted. It still has to be exact: a glob
/// would make the egress broker's identity check meaningless.
pub fn validate_endpoint(endpoint: &Endpoint) -> Result<(), String> {
    if endpoint.port == 0 {
        return Err("worker egress endpoint must name a port".to_string());
    }
    let host = &endpoint.host;
    if host.is_empty() || host.len() > 253 {
        return Err("worker egress endpoint host is out of range".to_string());
    }
    if host.contains('*') || host.contains('/') || host.contains('@') {
        return Err(format!("worker egress endpoint `{host}` is not exact"));
    }
    if host.parse::<std::net::IpAddr>().is_ok() {
        return Ok(());
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return Err(format!(
            "worker egress endpoint `{host}` is not a routable name"
        ));
    }
    for label in labels {
        if label.is_empty() || label.len() > 63 {
            return Err(format!(
                "worker egress endpoint `{host}` has an empty label"
            ));
        }
        if !label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(format!("worker egress endpoint `{host}` is not a DNS name"));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(format!(
                "worker egress endpoint `{host}` has a stray hyphen"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/worker/policy.rs"
    ));
}
