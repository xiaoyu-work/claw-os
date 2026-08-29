//! The refusal policy every enforcement point shares.
//!
//! `preinst`, `postinst`, `prerm`, the APT pre-install hook, the
//! `clawd` startup gate and the worker spawn gate all call into this
//! module rather than re-implementing comparisons in shell. There is
//! one ordering rule, one revocation rule and one recovery rule, and
//! they are tested here rather than five times over in maintainer
//! scripts.
//!
//! Ordering, in the order it is applied:
//!
//! 1. the release manifest must parse, be canonical, name this
//!    package, and not have expired;
//! 2. it must be signed by a key the floor already trusts — unless the
//!    floor has never seen a trusted key, which is the developer /
//!    unsigned-build case and is recorded as such;
//! 3. its manifest digest and component digests must not be revoked;
//! 4. a **lower security epoch is always refused**, whatever the
//!    Debian version says;
//! 5. at the same epoch, a lower Debian version is refused
//!    (`dpkg --compare-versions` ordering, epochs and revisions
//!    included);
//! 6. at the same epoch and the same version, a *different* artifact
//!    is refused: reinstalling the identical release is fine,
//!    substituting different bytes for it is not;
//! 7. a **higher security epoch supersedes semantic version
//!    ordering**, so an emergency release can be lower-versioned;
//! 8. the ABI generation and the installed sibling packages must stay
//!    inside the candidate's declared compatibility window;
//! 9. only then, an exactly matching one-use recovery authorization
//!    can override 4-6 — nothing else can.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use super::debver;
use super::floor::{Floor, FloorState};
use super::manifest::Manifest;
use super::recovery::{Authorization, RecoveryStore};
use super::signature::Signature;

/// Stable decision classes. Used in journal records, maintainer-script
/// output and tests, so they do not change casually.
pub mod class {
    pub const ALLOWED: &str = "allowed";
    pub const ALLOWED_BOOTSTRAP: &str = "allowed_bootstrap";
    pub const ALLOWED_SAME_RELEASE: &str = "allowed_same_release";
    pub const ALLOWED_RECOVERY: &str = "allowed_recovery";
    pub const MANIFEST_INVALID: &str = "manifest_invalid";
    pub const MANIFEST_EXPIRED: &str = "manifest_expired";
    pub const MANIFEST_UNSIGNED: &str = "manifest_unsigned";
    pub const MANIFEST_UNTRUSTED: &str = "manifest_untrusted";
    pub const DIGEST_REVOKED: &str = "digest_revoked";
    pub const EPOCH_REGRESSION: &str = "security_epoch_regression";
    pub const VERSION_REGRESSION: &str = "version_regression";
    pub const ARTIFACT_MISMATCH: &str = "artifact_mismatch";
    pub const ABI_INCOMPATIBLE: &str = "abi_incompatible";
    pub const SET_INCOMPATIBLE: &str = "incompatible_installed_set";
    pub const SUITE_MISMATCH: &str = "repository_suite_mismatch";
    pub const FLOOR_UNAVAILABLE: &str = "floor_unavailable";
}

/// What the caller is about to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// No version of the package is installed.
    Install,
    /// A version is installed and is being replaced.
    Upgrade,
    /// The package is being configured after unpack.
    Configure,
    /// A pre-transaction check with no unpack yet (APT hook, `prerm`).
    Plan,
}

impl Operation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Upgrade => "upgrade",
            Self::Configure => "configure",
            Self::Plan => "plan",
        }
    }
}

/// The release a caller wants to install, activate or run.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub package: String,
    pub version: String,
    pub manifest: Manifest,
    pub signature: Signature,
    pub operation: Operation,
    /// Versions of the other Claw OS packages currently installed.
    pub installed: BTreeMap<String, String>,
}

/// The outcome, with the stable class an audit record carries.
#[derive(Debug, Clone)]
pub struct Decision {
    pub allowed: bool,
    pub class: &'static str,
    pub message: String,
    /// Set when the decision depended on a recovery authorization that
    /// the caller must consume before committing.
    pub recovery: Option<Authorization>,
    /// Whether the manifest signature was actually verified.
    pub signature_verified: bool,
}

impl Decision {
    fn allow(class: &'static str, message: impl Into<String>, verified: bool) -> Self {
        Self {
            allowed: true,
            class,
            message: message.into(),
            recovery: None,
            signature_verified: verified,
        }
    }

    fn refuse(class: &'static str, message: impl Into<String>) -> Self {
        Self {
            allowed: false,
            class,
            message: message.into(),
            recovery: None,
            signature_verified: false,
        }
    }
}

/// Decide whether `candidate` may be installed or activated.
pub fn evaluate(
    candidate: &Candidate,
    state: &FloorState,
    recovery: Option<&RecoveryStore>,
    now: DateTime<Utc>,
) -> Decision {
    let manifest = &candidate.manifest;
    if manifest.package != candidate.package {
        return Decision::refuse(
            class::MANIFEST_INVALID,
            format!(
                "release manifest describes `{}`, not `{}`",
                manifest.package, candidate.package
            ),
        );
    }
    if manifest.version != candidate.version {
        return Decision::refuse(
            class::MANIFEST_INVALID,
            format!(
                "release manifest describes version {}, but the candidate is {}",
                manifest.version, candidate.version
            ),
        );
    }
    if manifest.is_expired(now) {
        return Decision::refuse(
            class::MANIFEST_EXPIRED,
            format!(
                "release manifest for {} {} expired at {}; refresh the package index from a current mirror",
                manifest.package,
                manifest.version,
                manifest.valid_until.to_rfc3339()
            ),
        );
    }

    let floor = match state {
        FloorState::Uninitialized => None,
        FloorState::Present { floor, .. } => Some(floor.as_ref()),
    };

    if let Err(decision) = check_signature(candidate, floor) {
        return *decision;
    }
    let signature_verified = candidate.signature.is_verified();

    if let Some(floor) = floor {
        if floor.revoked_digests.contains(&manifest.digest) {
            return Decision::refuse(
                class::DIGEST_REVOKED,
                format!(
                    "release {} {} has been revoked",
                    manifest.package, manifest.version
                ),
            );
        }
        for component in &manifest.components {
            if floor.revoked_digests.contains(&component.sha256) {
                return Decision::refuse(
                    class::DIGEST_REVOKED,
                    format!(
                        "component `{}` in this release has been revoked",
                        component.name
                    ),
                );
            }
        }
        for revoked in &manifest.revoked_digests {
            if *revoked == manifest.digest {
                return Decision::refuse(
                    class::DIGEST_REVOKED,
                    "this release revokes its own artifact".to_string(),
                );
            }
        }
    }

    let Some(floor) = floor else {
        return Decision::allow(
            class::ALLOWED_BOOTSTRAP,
            format!(
                "no security floor recorded yet; {} {} will seed it",
                manifest.package, manifest.version
            ),
            signature_verified,
        );
    };

    if !floor.suite.is_empty() && floor.suite != manifest.suite {
        return Decision::refuse(
            class::SUITE_MISMATCH,
            format!(
                "release manifest is published for suite `{}`, but this system tracks `{}`",
                manifest.suite, floor.suite
            ),
        );
    }

    let ordering = order_against_floor(candidate, floor);
    let ordering = match ordering {
        Ok(ordering) => ordering,
        Err(message) => return Decision::refuse(class::VERSION_REGRESSION, message),
    };

    if let Some(refusal) = compatibility_refusal(candidate, floor) {
        return refusal;
    }

    match ordering {
        Ordering::Forward => Decision::allow(
            class::ALLOWED,
            format!(
                "{} {} is at or above the recorded security floor",
                manifest.package, manifest.version
            ),
            signature_verified,
        ),
        Ordering::SameRelease => Decision::allow(
            class::ALLOWED_SAME_RELEASE,
            format!(
                "{} {} is the release already recorded by the floor",
                manifest.package, manifest.version
            ),
            signature_verified,
        ),
        Ordering::Backward {
            class: refusal,
            message,
        } => match authorized_recovery(candidate, floor, recovery, now) {
            Some(authorization) => Decision {
                allowed: true,
                class: class::ALLOWED_RECOVERY,
                message: format!(
                    "{message}; permitted by recovery authorization {} ({})",
                    authorization.id, authorization.reason
                ),
                recovery: Some(authorization),
                signature_verified,
            },
            None => Decision::refuse(refusal, message),
        },
    }
}

enum Ordering {
    Forward,
    SameRelease,
    Backward {
        class: &'static str,
        message: String,
    },
}

fn order_against_floor(candidate: &Candidate, floor: &Floor) -> Result<Ordering, String> {
    let manifest = &candidate.manifest;
    if manifest.security_epoch < floor.security_epoch {
        return Ok(Ordering::Backward {
            class: class::EPOCH_REGRESSION,
            message: format!(
                "{} {} belongs to security epoch {}, below this system's floor of {}",
                manifest.package, manifest.version, manifest.security_epoch, floor.security_epoch
            ),
        });
    }
    let Some(recorded) = floor.packages.get(&manifest.package) else {
        // A package this floor has never seen: the epoch check above
        // is the whole ordering constraint.
        return Ok(Ordering::Forward);
    };
    if manifest.security_epoch > recorded.security_epoch {
        // A higher epoch supersedes version ordering outright.
        return Ok(Ordering::Forward);
    }
    if manifest.security_epoch < recorded.security_epoch {
        return Ok(Ordering::Backward {
            class: class::EPOCH_REGRESSION,
            message: format!(
                "{} {} belongs to security epoch {}, below the {} recorded for the installed release",
                manifest.package, manifest.version, manifest.security_epoch, recorded.security_epoch
            ),
        });
    }
    match debver::compare(&manifest.version, &recorded.version)? {
        std::cmp::Ordering::Greater => Ok(Ordering::Forward),
        std::cmp::Ordering::Less => Ok(Ordering::Backward {
            class: class::VERSION_REGRESSION,
            message: format!(
                "{} {} is older than the recorded floor {}",
                manifest.package, manifest.version, recorded.version
            ),
        }),
        std::cmp::Ordering::Equal => {
            if manifest.digest == recorded.manifest_sha256 {
                Ok(Ordering::SameRelease)
            } else {
                Ok(Ordering::Backward {
                    class: class::ARTIFACT_MISMATCH,
                    message: format!(
                        "{} {} does not match the artifact recorded for that version",
                        manifest.package, manifest.version
                    ),
                })
            }
        }
    }
}

fn compatibility_refusal(candidate: &Candidate, floor: &Floor) -> Option<Decision> {
    let manifest = &candidate.manifest;
    if manifest.abi < floor.abi {
        return Some(Decision::refuse(
            class::ABI_INCOMPATIBLE,
            format!(
                "{} {} implements ABI generation {}, below this system's {}",
                manifest.package, manifest.version, manifest.abi, floor.abi
            ),
        ));
    }
    for (name, epoch) in &floor.protocols {
        let Some(candidate_epoch) = manifest.protocols.get(name) else {
            continue;
        };
        if candidate_epoch < epoch {
            return Some(Decision::refuse(
                class::ABI_INCOMPATIBLE,
                format!(
                    "{} {} speaks {name} protocol v{candidate_epoch}, below this system's v{epoch}",
                    manifest.package, manifest.version
                ),
            ));
        }
    }
    for (package, installed_version) in &candidate.installed {
        if *package == manifest.package {
            continue;
        }
        let Some(minimum) = manifest.minimum_compatible.get(package) else {
            continue;
        };
        match debver::compare(installed_version, minimum) {
            Ok(std::cmp::Ordering::Less) => {
                return Some(Decision::refuse(
                    class::SET_INCOMPATIBLE,
                    format!(
                        "{} {} requires {package} {minimum} or newer, but {installed_version} is installed; \
                         upgrade the whole set with `apt full-upgrade`",
                        manifest.package, manifest.version
                    ),
                ));
            }
            Ok(_) => {}
            Err(error) => {
                return Some(Decision::refuse(class::SET_INCOMPATIBLE, error));
            }
        }
    }
    None
}

fn check_signature(candidate: &Candidate, floor: Option<&Floor>) -> Result<(), Box<Decision>> {
    let requires_signature = floor.is_some_and(|floor| !floor.trusted_keys.is_empty());
    match &candidate.signature {
        Signature::Verified { key_id, .. } => {
            if candidate.manifest.revoked_keys.contains(key_id) {
                return Err(Box::new(Decision::refuse(
                    class::MANIFEST_UNTRUSTED,
                    format!("release manifest is signed by revoked key {key_id}"),
                )));
            }
            if let Some(floor) = floor {
                if !floor.trusted_keys.is_empty() && !floor.trusted_keys.contains(key_id) {
                    return Err(Box::new(Decision::refuse(
                        class::MANIFEST_UNTRUSTED,
                        format!(
                            "release manifest is signed by {key_id}, which this system does not trust"
                        ),
                    )));
                }
            }
            Ok(())
        }
        Signature::Absent if requires_signature => Err(Box::new(Decision::refuse(
            class::MANIFEST_UNSIGNED,
            format!(
                "{} {} carries no signed release manifest, but this system was installed from a signed release",
                candidate.package, candidate.version
            ),
        ))),
        Signature::Unverifiable { reason } if requires_signature => Err(Box::new(Decision::refuse(
            class::MANIFEST_UNTRUSTED,
            format!(
                "release manifest signature for {} {} could not be verified: {reason}",
                candidate.package, candidate.version
            ),
        ))),
        // No trusted key has ever been recorded: an unsigned developer
        // or private build. Ordering still applies; the decision is
        // journaled with `signature_verified = false` so the weaker
        // trust is visible rather than implied.
        Signature::Absent | Signature::Unverifiable { .. } => Ok(()),
    }
}

fn authorized_recovery(
    candidate: &Candidate,
    floor: &Floor,
    recovery: Option<&RecoveryStore>,
    now: DateTime<Utc>,
) -> Option<Authorization> {
    let store = recovery?;
    store
        .find(
            &candidate.package,
            &candidate.version,
            candidate.manifest.security_epoch,
            &candidate.manifest.digest,
            floor.generation,
            Some(floor.digest.as_str()),
            now,
        )
        .ok()
        .flatten()
        .map(|(_, authorization)| authorization)
}

/// Coarse pre-unpack gate for a package that is *already installed*
/// and is about to be replaced.
///
/// `prerm upgrade <new-version>` runs before the incoming package's
/// own `preinst`, and it is the only hook the currently installed —
/// and therefore protected — release controls. It cannot see the
/// incoming manifest, so it decides on version ordering alone; the
/// epoch-supersedes-version case is decided later by `preinst`, which
/// does have the manifest. An intentional downgrade below the floor
/// therefore needs a recovery authorization naming that version.
pub fn evaluate_incoming_version(
    package: &str,
    incoming_version: &str,
    state: &FloorState,
    recovery: Option<&RecoveryStore>,
    now: DateTime<Utc>,
) -> Decision {
    let FloorState::Present { floor, .. } = state else {
        return Decision::allow(
            class::ALLOWED_BOOTSTRAP,
            "no security floor recorded yet",
            false,
        );
    };
    let Some(recorded) = floor.packages.get(package) else {
        return Decision::allow(class::ALLOWED, "package is not tracked by the floor", false);
    };
    match debver::compare(incoming_version, &recorded.version) {
        Err(error) => Decision::refuse(class::VERSION_REGRESSION, error),
        Ok(std::cmp::Ordering::Less) => {
            let authorized = recovery.and_then(|store| {
                store
                    .pending()
                    .ok()?
                    .into_iter()
                    .map(|(_, authorization)| authorization)
                    .find(|authorization| {
                        authorization.package == package
                            && authorization.version == incoming_version
                            && authorization.floor_generation == floor.generation
                            && now <= authorization.expires_at
                    })
            });
            match authorized {
                Some(authorization) => Decision {
                    allowed: true,
                    class: class::ALLOWED_RECOVERY,
                    message: format!(
                        "downgrade of {package} to {incoming_version} is covered by recovery authorization {}",
                        authorization.id
                    ),
                    recovery: Some(authorization),
                    signature_verified: false,
                },
                None => Decision::refuse(
                    class::VERSION_REGRESSION,
                    format!(
                        "refusing to replace {package} {} with the older {incoming_version}; \
                         the security floor only moves forward. Record an explicit recovery \
                         authorization with `claw-security-floor recover authorize` if this \
                         downgrade is intended.",
                        recorded.version
                    ),
                ),
            }
        }
        Ok(_) => Decision::allow(
            class::ALLOWED,
            format!("{package} {incoming_version} is at or above the floor"),
            false,
        ),
    }
}

/// Is the currently installed set mutually compatible?
///
/// Used by `postinst` to decide whether a service may be (re)started
/// yet. During an ordered multi-package transaction the answer is
/// legitimately "not yet": the package configured first must not
/// restart a daemon into a half-replaced set, and the package
/// configured last completes the restart.
pub fn installed_set_is_compatible(
    manifest: &Manifest,
    installed: &BTreeMap<String, String>,
) -> Result<(), String> {
    for (package, version) in installed {
        if *package == manifest.package {
            continue;
        }
        let Some(minimum) = manifest.minimum_compatible.get(package) else {
            continue;
        };
        if debver::compare(version, minimum)? == std::cmp::Ordering::Less {
            return Err(format!(
                "{package} {version} is older than the {minimum} this release requires"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/update/decide.rs"
    ));
}
