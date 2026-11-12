//! Fresh verification of one registration's exact package, not App discovery.

use crate::provenance::runtime::PackageRef;
use crate::provenance::trust::TrustStore;
use crate::provenance::verify::{verify_package, VerifyOptions};
use crate::provenance::{PackageKind, VerifiedPackage};

pub(super) fn reverify(
    retained: &VerifiedPackage,
    trust: &TrustStore,
) -> Result<VerifiedPackage, String> {
    if retained.kind() != PackageKind::App {
        return Err("selected file authority requires its retained App package".into());
    }
    retained.assert_current(trust).map_err(|error| {
        crate::provenance::quarantine_reason(PackageKind::App, retained.id(), &error)
    })?;
    // A pinned descriptor is not immutable. Keep the complete signature/tree
    // verification, including content, modes and unsigned additions, on every use.
    let current = verify_package(
        retained.dir(),
        &VerifyOptions::new(PackageKind::App).expect_id(retained.id()),
        trust,
    )
    .map_err(|error| {
        crate::provenance::quarantine_reason(PackageKind::App, retained.id(), &error)
    })?;
    if current.identity() != retained.identity()
        || PackageRef::of(&current) != PackageRef::of(retained)
        || current.manifest_path() != retained.manifest_path()
        || current.entrypoints() != retained.entrypoints()
        || current.resources() != retained.resources()
    {
        return Err("selected file package differs from the Root-held registration".into());
    }
    retained
        .assert_current(trust)
        .map_err(|error| error.to_string())?;
    Ok(current)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/gui/selected_source.rs"
    ));
}
