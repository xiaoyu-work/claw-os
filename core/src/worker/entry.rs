//! Generic package-local App entry admission. This grants no runtime privilege
//! and never consults the transitional absolute-entry/native MCP table.

use std::path::PathBuf;

/// Validate a manifest-selected entry from the authenticated package.
///
/// The caller still holds `AppLaunch::bind` across authorization, preparation
/// and spawn. This path alone is not an execution or permission binding.
pub fn verified_entrypoint(
    app_id: &str,
    package: &crate::provenance::VerifiedPackage,
    entry: &str,
    runtime: crate::caps::manifest::Runtime,
) -> Result<PathBuf, String> {
    if package.kind() != crate::provenance::PackageKind::App || package.id() != app_id {
        return Err(format!(
            "App `{app_id}` does not match its verified package identity"
        ));
    }
    crate::provenance::envelope::validate_tree_path(entry).map_err(|error| {
        format!("App `{app_id}` entry `{entry}` must be package-relative: {error}")
    })?;
    if !package
        .entrypoints()
        .iter()
        .any(|declared| declared == entry)
    {
        return Err(format!(
            "App `{app_id}` entry `{entry}` is not a declared, signed entrypoint"
        ));
    }
    let signed = package
        .files()
        .find(|file| file.path == entry)
        .ok_or_else(|| format!("App `{app_id}` entry `{entry}` is absent from the signed tree"))?;
    if signed.kind != crate::provenance::envelope::NodeKind::File
        || (matches!(runtime, crate::caps::manifest::Runtime::Binary) && signed.mode & 0o111 == 0)
    {
        return Err(format!(
            "App `{app_id}` entry `{entry}` is not executable for its runtime"
        ));
    }
    package
        .assert_current(&crate::provenance::trust_store())
        .map_err(|error| {
            format!("App `{app_id}` entry provenance is no longer current: {error}")
        })?;
    #[cfg(unix)]
    {
        let fd = package
            .open_entrypoint(entry)
            .map_err(|error| format!("App `{app_id}` entry verification failed: {error}"))?;
        let metadata = fd.meta();
        if metadata.nlink != 1 || metadata.mode & 0o7777 != signed.mode {
            return Err(format!(
                "App `{app_id}` entry `{entry}` links or permissions changed after verification"
            ));
        }
        Ok(package.dir().join(entry))
    }
    #[cfg(not(unix))]
    {
        Err("App entry admission requires Unix descriptor binding".to_string())
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/worker/entry.rs"
    ));
}
