//! Runtime gates: defense in depth for the case where the package
//! manager was bypassed entirely.
//!
//! Maintainer scripts only run when `dpkg` runs. Someone who copies an
//! old `clawd` over `/usr/local/bin/clawd`, restores a component from
//! a backup, or reinstalls an old package after `apt remove` never
//! passes through them — but the floor survives removal, so the
//! binaries themselves can still refuse.
//!
//! # Two views, two privileges
//!
//! The authoritative floor is `0700 root:root`, so an ordinary `cos`
//! process cannot read it and must not need to. Enforcement therefore
//! comes in two shapes:
//!
//! * [`enforce_startup`] — every unprivileged Claw OS binary compares
//!   its compiled epoch and protocol epochs against the **runtime
//!   projection** in `/var/lib/cos-security`, which is root-owned,
//!   world-readable and carries no recovery or history data.
//! * [`enforce_broker_startup`] — `clawd` runs as root, so it reads the
//!   authoritative floor, re-measures the critical component set on
//!   disk, and cross-checks the projection against the authority,
//!   repairing a stale or missing projection before it serves anything.
//!
//! Plus [`enforce_worker_binary`]: `clawd` measures the `claw-agentd`
//! executable before spawning it, so a replaced worker is refused
//! before it exists as a process rather than being asked politely over
//! its own channel.
//!
//! A peer's *self-reported* version is never authority: the worker
//! handshake carries its compiled epoch only so a mismatch produces a
//! named error, and the value is compared against the broker's own
//! compiled constant — an honest worker that is too old is refused,
//! and a lying one still fails the digest measurement.

use std::path::{Path, PathBuf};

use crate::provenance::fsec;

use super::floor::{FloorState, FloorStore};
use super::projection::ProjectionStore;

/// Why a process refused to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub class: &'static str,
    pub message: String,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

fn refuse(class: &'static str, message: impl Into<String>) -> Refusal {
    Refusal {
        class,
        message: message.into(),
    }
}

/// How much of the installed set a caller wants verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Compare the compiled epoch and protocol epochs only. Used by
    /// short-lived CLI processes where hashing binaries would be a
    /// visible cost.
    CompiledEpoch,
    /// Also re-measure every security-critical component on disk. Used
    /// by `clawd`, which starts once and supervises everything else.
    CriticalComponents,
}

/// Gate an **unprivileged** process against the runtime projection.
///
/// Returns `Ok(())` when this machine has never been protected (no
/// `/var/lib/cos-security`, i.e. a source build or an unsealed image),
/// or when the projection is satisfied. Returns a [`Refusal`] when the
/// projection is missing, unreadable, insecure or ahead of this build.
pub fn enforce_startup(scope: Scope) -> Result<(), Refusal> {
    enforce_startup_in(&ProjectionStore::system(), Path::new("/"), scope)
}

/// [`enforce_startup`] against a specific projection store.
pub fn enforce_startup_in(
    store: &ProjectionStore,
    root: &Path,
    scope: Scope,
) -> Result<(), Refusal> {
    let projection = match store.load() {
        Ok(Some(projection)) => projection,
        // Never protected: nothing to be older than.
        Ok(None) => return Ok(()),
        Err(error) => {
            return Err(refuse(
                super::decide::class::FLOOR_UNAVAILABLE,
                format!(
                    "{error}. Claw OS refuses to run against unreadable update-security state; \
                     reinstall claw-os-agent from the signed repository, or recover the state \
                     described in docs/updating.md."
                ),
            ))
        }
    };

    if super::SECURITY_EPOCH < projection.security_epoch {
        return Err(refuse(
            super::decide::class::EPOCH_REGRESSION,
            format!(
                "this binary belongs to Claw OS security epoch {}, but this system has already \
                 accepted epoch {}. Refusing to run a superseded build; reinstall the current \
                 claw-os-agent package.",
                super::SECURITY_EPOCH,
                projection.security_epoch
            ),
        ));
    }
    let compiled = super::compiled_protocols();
    for (name, epoch) in &projection.protocols {
        let Some(mine) = compiled.get(name) else {
            continue;
        };
        if mine < epoch {
            return Err(refuse(
                super::decide::class::ABI_INCOMPATIBLE,
                format!(
                    "this binary speaks {name} protocol v{mine}, but this system requires \
                     v{epoch}. Finish the upgrade so every Claw OS binary comes from the same \
                     release."
                ),
            ));
        }
    }
    if scope == Scope::CompiledEpoch {
        return Ok(());
    }
    verify_components(root, |name| {
        projection
            .components
            .get(name)
            .map(|entry| entry.sha256.clone())
    })
}

/// Gate the broker, which is root and therefore holds the authority.
///
/// Reads the private floor, re-measures the critical component set,
/// and then makes the unprivileged projection agree with it — a stale
/// or missing projection is repaired here, by the one process that can
/// see both sides. A projection that cannot be made to agree is fatal:
/// the rest of the system would otherwise enforce against a view that
/// no longer describes this machine.
pub fn enforce_broker_startup() -> Result<(), Refusal> {
    enforce_broker_startup_in(
        &FloorStore::system(),
        &ProjectionStore::system(),
        Path::new("/"),
    )
}

/// [`enforce_broker_startup`] against specific stores.
pub fn enforce_broker_startup_in(
    store: &FloorStore,
    projection: &ProjectionStore,
    root: &Path,
) -> Result<(), Refusal> {
    let state = store.load().map_err(|error| {
        refuse(
            super::decide::class::FLOOR_UNAVAILABLE,
            format!(
                "{error}. Claw OS refuses to run against unreadable update-security state; \
                 reinstall claw-os-agent from the signed repository, or recover the state \
                 described in docs/updating.md."
            ),
        )
    })?;
    let FloorState::Present { floor, .. } = &state else {
        // No authoritative floor. If a projection nevertheless exists,
        // something removed the authority underneath it: fail closed.
        if projection.is_established() {
            return Err(refuse(
                super::decide::class::FLOOR_UNAVAILABLE,
                "this system publishes an update-security runtime view but has no authoritative \
                 floor state; refusing to start. Reinstall claw-os-agent from the signed \
                 repository."
                    .to_string(),
            ));
        }
        return Ok(());
    };

    if super::SECURITY_EPOCH < floor.security_epoch {
        return Err(refuse(
            super::decide::class::EPOCH_REGRESSION,
            format!(
                "this binary belongs to Claw OS security epoch {}, but this system has already \
                 accepted epoch {}. Refusing to run a superseded build; reinstall the current \
                 claw-os-agent package.",
                super::SECURITY_EPOCH,
                floor.security_epoch
            ),
        ));
    }
    let compiled = super::compiled_protocols();
    for (name, epoch) in &floor.protocols {
        let Some(mine) = compiled.get(name) else {
            continue;
        };
        if mine < epoch {
            return Err(refuse(
                super::decide::class::ABI_INCOMPATIBLE,
                format!(
                    "this binary speaks {name} protocol v{mine}, but this system requires \
                     v{epoch}. Finish the upgrade so every Claw OS binary comes from the same \
                     release."
                ),
            ));
        }
    }
    verify_components(root, |name| {
        floor.components.get(name).map(|entry| entry.sha256.clone())
    })?;

    match projection.load() {
        Ok(Some(published)) if published.matches(floor) => Ok(()),
        // Anything else — missing, stale, corrupt, tampered — is
        // *derived* data whose authority is right here. Republish it,
        // and only refuse if that cannot be done, because every
        // unprivileged binary enforces against this view.
        _ => projection.publish(floor).map_err(|error| {
            refuse(
                super::decide::class::FLOOR_UNAVAILABLE,
                format!(
                    "the unprivileged update-security view does not match the authoritative \
                     floor and could not be repaired: {error}"
                ),
            )
        }),
    }
}

/// Re-measure every security-critical component against `recorded`.
fn verify_components(
    root: &Path,
    recorded: impl Fn(&str) -> Option<String>,
) -> Result<(), Refusal> {
    for component in super::COMPONENTS.iter().filter(|entry| entry.critical) {
        let Some(expected) = recorded(component.name) else {
            continue;
        };
        let path = super::signature::joined(root, component.path);
        let measured = match super::floor::measure_component(component.name, &path) {
            Ok(measured) => measured,
            // A component the current package set does not install (a
            // desktop binary on a headless system, say) is not
            // evidence of a downgrade.
            Err(_) if missing(&path) => continue,
            Err(error) => {
                return Err(refuse(
                    super::decide::class::FLOOR_UNAVAILABLE,
                    format!("cannot verify `{}`: {error}", component.name),
                ))
            }
        };
        if measured.sha256 != expected {
            return Err(refuse(
                super::decide::class::ARTIFACT_MISMATCH,
                format!(
                    "`{}` does not match the content this system recorded for the installed \
                     release. Refusing to start against a replaced security component; \
                     reinstall claw-os-agent from the signed repository.",
                    component.path
                ),
            ));
        }
    }
    Ok(())
}

/// Measure the agent worker before it is spawned.
///
/// `clawd` is the authority here: the worker never gets to vouch for
/// itself, because the check happens before `execve`.
pub fn enforce_worker_binary(binary: &Path) -> Result<(), Refusal> {
    enforce_worker_binary_in(&FloorStore::system(), binary)
}

/// [`enforce_worker_binary`] against a specific store.
pub fn enforce_worker_binary_in(store: &FloorStore, binary: &Path) -> Result<(), Refusal> {
    let Ok(FloorState::Present { floor, .. }) = store.load() else {
        // An unreadable floor is already fatal at daemon startup; the
        // spawn path does not need to duplicate that refusal, and a
        // system with no floor at all has nothing to compare against.
        return Ok(());
    };
    let Some(recorded) = floor.components.get("claw-agentd") else {
        return Ok(());
    };
    // Only the installed worker is measured. A development tree points
    // `COS_AGENTD_BIN` somewhere else entirely, and the floor has
    // nothing to say about a binary it never recorded.
    let installed = PathBuf::from(
        super::component("claw-agentd")
            .map(|component| component.path)
            .unwrap_or("/usr/local/bin/claw-agentd"),
    );
    if binary != installed {
        return Ok(());
    }
    let measured = super::floor::measure_component("claw-agentd", binary).map_err(|error| {
        refuse(
            super::decide::class::FLOOR_UNAVAILABLE,
            format!("cannot verify the agent worker binary: {error}"),
        )
    })?;
    if measured.sha256 != recorded.sha256 {
        return Err(refuse(
            super::decide::class::ARTIFACT_MISMATCH,
            "the installed claw-agentd binary does not match the content recorded for this \
             release; refusing to spawn a replaced agent worker"
                .to_string(),
        ));
    }
    Ok(())
}

/// Compare a worker's declared epoch against the broker's own.
///
/// The worker's number is corroboration, not authority: the binary it
/// came from was already measured by [`enforce_worker_binary`].
pub fn check_peer_epoch(peer: &str, declared: u64) -> Result<(), Refusal> {
    if declared == super::SECURITY_EPOCH {
        return Ok(());
    }
    Err(refuse(
        super::decide::class::EPOCH_REGRESSION,
        format!(
            "{peer} reports Claw OS security epoch {declared} but this build is epoch {}; \
             reinstall claw-os-agent so every binary comes from the same release",
            super::SECURITY_EPOCH
        ),
    ))
}

fn missing(path: &Path) -> bool {
    matches!(fsec::lstat(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/update/runtime.rs"
    ));
}
