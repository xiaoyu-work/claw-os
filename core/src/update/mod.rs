//! Update freshness: the local security floor that keeps a validly
//! signed *older* Claw OS release from being installed, activated or
//! executed.
//!
//! # What this is, and what it is not
//!
//! APT signatures, `Release`/`InRelease` and the extension-provenance
//! envelopes all answer **authenticity**: "did the publisher produce
//! these bytes?". None of them answer **freshness**: an artifact that
//! was validly signed two years ago is still validly signed today, so
//! a stale mirror, a preserved repository snapshot, or a plain
//! `apt install claw-os-agent=<old>` can put a known-vulnerable
//! `clawd` back on disk without forging anything.
//!
//! The floor is the freshness authority. It is a root-owned, monotonic
//! record of the highest security epoch, package version and component
//! digests this machine has ever accepted, plus a hash-chained
//! generation history so a single-file rollback is detectable. Every
//! install, activation and daemon start is compared against it, and a
//! candidate that would move the machine backwards is refused.
//!
//! # Threat boundary
//!
//! This is a software-only control on the machine's own filesystem.
//!
//! * **In scope.** An unprivileged local attacker; a stale or hostile
//!   mirror; a preserved old repository snapshot; an operator or tool
//!   running `apt install <pkg>=<old-version>`; a partially completed
//!   or reordered multi-package transaction; a component binary
//!   replaced on disk behind the package manager's back.
//! * **Out of scope.** Local root, or physical replacement of the
//!   complete filesystem *and* state, is not defeated by software on
//!   that same filesystem: root can rewrite the floor together with
//!   the binaries. Detecting that requires a TPM measurement or a
//!   remote attestation anchor, neither of which Claw OS has. We do
//!   not claim hardware anti-rollback.
//!
//! # Layout
//!
//! | Module | Responsibility |
//! | --- | --- |
//! | [`canonical`] | Deterministic JSON encoding used for signing and hashing |
//! | [`debver`] | Debian version ordering (`dpkg --compare-versions` semantics) |
//! | [`manifest`] | The signed `claw.release-security/v1` release manifest |
//! | [`signature`] | OpenPGP verification against the release/APT trust already on the machine |
//! | [`floor`] | The durable, root-owned monotonic floor state |
//! | [`projection`] | The unprivileged, root-owned runtime view of that floor |
//! | [`recovery`] | One-use, narrowly scoped operator authorizations |
//! | [`decide`] | The refusal policy every enforcement point shares |
//! | [`journal`] | Auditable record of every decision |
//! | [`runtime`] | Startup/spawn gates for `clawd`, `claw-agentd` and the CLI |
//! | [`cli`] | `claw-security-floor`, the helper maintainer scripts call |

pub mod canonical;
pub mod cli;
pub mod debver;
pub mod decide;
pub mod floor;
pub mod journal;
pub mod manifest;
pub mod projection;
pub mod recovery;
pub mod runtime;
pub mod signature;

/// The release-security epoch this build belongs to.
///
/// Monotonic and **independent of the Debian version**: raising it
/// supersedes semantic version ordering, so an emergency release can
/// invalidate everything published before it even when its own
/// upstream version is lower. It is never lowered.
pub const SECURITY_EPOCH: u64 = 1;

/// Cross-package ABI generation.
///
/// `claw-os-agent` advertises `Provides: claw-os-abi-<ABI>` and the
/// dependent packages `Depends:` on that virtual name, so APT's own
/// solver refuses to combine an agent with a base or desktop outside
/// the supported protocol range — without pinning exact versions,
/// which would make phased publication impossible.
pub const ABI: u32 = 1;

/// Authoritative floor state. A compiled-in absolute path: never
/// derived from the environment, so no caller can point enforcement at
/// a state directory it controls.
///
/// `0700 root:root` — it holds the generation history and the recovery
/// authorizations. Unprivileged processes read
/// [`RUNTIME_STATE_DIR`] instead.
pub const SYSTEM_STATE_DIR: &str = "/var/lib/cos/security";

/// The unprivileged runtime view of the floor: a separate root-owned,
/// world-readable directory holding a minimal projection with no
/// recovery or history data. Deliberately *outside* `/var/lib/cos`, so
/// nothing in the private tree has to be widened.
pub const RUNTIME_STATE_DIR: &str = "/var/lib/cos-security";

/// File name of the projection inside [`RUNTIME_STATE_DIR`].
pub const RUNTIME_FLOOR_FILE: &str = "runtime-floor.json";

/// Where each package drops its signed release manifest.
///
/// Every package owns its **own** subdirectory, so two packages can
/// never write the same regular file and a maintainer script always
/// reads its own release rather than whichever package was unpacked
/// last.
pub const RELEASE_SECURITY_DIR: &str = "/usr/lib/cos/release-security";

/// Installed manifest path for one package.
pub fn release_manifest_path(package: &str) -> String {
    format!("{RELEASE_SECURITY_DIR}/{package}/manifest.json")
}

/// Path of that manifest inside a `.deb` payload tar.
pub fn release_manifest_member(package: &str) -> String {
    format!(".{}", release_manifest_path(package))
}

/// The keyring APT already pins with `signed-by=`. The release
/// manifest is signed by the same publisher identity, so a machine
/// that can verify its package index can verify its release manifest
/// without a second trust root to distribute.
pub const APT_KEYRING: &str = "/usr/share/keyrings/claw-os-archive-keyring.gpg";

/// Operator-managed additional release keyrings, for a key rotation or
/// a private rebuild. Never shipped by a package.
pub const OPERATOR_KEYRING_DIR: &str = "/etc/cos/trust/release.d";

/// A binary whose version and content digest the floor tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Component {
    pub name: &'static str,
    /// Installed absolute path.
    pub path: &'static str,
    /// Package that owns the file.
    pub package: &'static str,
    /// Security-critical: `clawd` refuses to start when one of these
    /// no longer matches the floor.
    pub critical: bool,
}

/// Every component under floor control. Mirrors
/// `packaging/release-security/policy.json`, which the packaging and
/// publication scripts read; the pairing is asserted by a unit test.
pub const COMPONENTS: &[Component] = &[
    Component {
        name: "clawd",
        path: "/usr/local/bin/clawd",
        package: "claw-os-agent",
        critical: true,
    },
    Component {
        name: "claw-agentd",
        path: "/usr/local/bin/claw-agentd",
        package: "claw-os-agent",
        critical: true,
    },
    Component {
        name: "claw-approval-helper",
        path: "/usr/local/bin/claw-approval-helper",
        package: "claw-os-agent",
        critical: true,
    },
    Component {
        name: "claw-app-runner",
        path: "/usr/local/bin/claw-app-runner",
        package: "claw-os-agent",
        critical: true,
    },
    Component {
        name: "claw-security-floor",
        path: "/usr/lib/cos/bin/claw-security-floor",
        package: "claw-os-agent",
        critical: true,
    },
    Component {
        name: "cos",
        path: "/usr/local/bin/cos",
        package: "claw-os-agent",
        critical: true,
    },
    Component {
        name: "cos-init",
        path: "/usr/local/bin/cos-init",
        package: "claw-os-base",
        critical: false,
    },
    Component {
        name: "cos-agent-bridge",
        path: "/usr/local/bin/cos-agent-bridge",
        package: "claw-os-desktop",
        critical: false,
    },
    Component {
        name: "cos-agent-ui",
        path: "/usr/local/bin/cos-agent-ui",
        package: "claw-os-desktop",
        critical: false,
    },
    Component {
        name: "cos-ask-claw-launcher",
        path: "/usr/local/bin/cos-ask-claw-launcher",
        package: "claw-os-desktop",
        critical: false,
    },
];

/// The packages whose lifecycle is gated. Anything owning a security
/// policy, protocol or privileged component belongs here.
pub const GATED_PACKAGES: &[&str] = &["claw-os-agent", "claw-os-base", "claw-os-desktop"];

/// Look up a tracked component by name.
pub fn component(name: &str) -> Option<&'static Component> {
    COMPONENTS.iter().find(|entry| entry.name == name)
}

/// Components owned by one package.
pub fn components_of(package: &str) -> Vec<&'static Component> {
    COMPONENTS
        .iter()
        .filter(|entry| entry.package == package)
        .collect()
}

/// Protocol/ABI epochs this build speaks. Recorded in the manifest and
/// in the floor so a mixed install is refused before it can run.
pub fn compiled_protocols() -> std::collections::BTreeMap<String, u32> {
    let mut map = std::collections::BTreeMap::new();
    map.insert(
        "agentd_worker".to_string(),
        crate::agentd::protocol::PROTOCOL_VERSION,
    );
    map.insert(
        "broker_envelope".to_string(),
        crate::clawd::wire::PROTOCOL_VERSION,
    );
    map
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/update/mod.rs"
    ));
}
