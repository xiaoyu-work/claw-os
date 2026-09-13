//! App directory validation and publication, independent of CLI and broker orchestration.
//!
//! The caller supplies permission review before publication and separately owns
//! developer trust, AI consent, audit reporting and desktop registration. This
//! backend neither grants permission nor executes App code.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{lint::app_lint_violations, App, AppManifest};

/// Source identity and destination selection, not a verified package or approval.
pub(crate) struct DirectoryInstall {
    source: PathBuf,
    dest: PathBuf,
    expected_id: String,
    same_path: bool,
}

impl DirectoryInstall {
    pub(crate) fn prepare(source: &Path, apps_root: &Path) -> Result<Self, String> {
        if !source.is_dir() {
            return Err(format!(
                "install source `{}` is not a directory",
                source.display()
            ));
        }
        let manifest_path = source.join("app.json");
        if !manifest_path.is_file() {
            return Err(format!(
                "install source `{}` has no app.json",
                source.display()
            ));
        }
        let body = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("read {}: {e}", manifest_path.display()))?;
        let manifest = AppManifest::from_json(&body)
            .map_err(|e| format!("parse {}: {e}", manifest_path.display()))?;
        let catalog = crate::ai::tools::list_names();
        manifest
            .validate_tools_against_catalog(&catalog)
            .map_err(|e| format!("manifest catalog check: {e}"))?;

        let dest = apps_root.join(&manifest.id);
        let same_path = source
            .canonicalize()
            .ok()
            .zip(dest.canonicalize().ok())
            .map(|(a, b)| a == b)
            .unwrap_or(false);
        Ok(Self {
            source: source.to_path_buf(),
            dest,
            expected_id: manifest.id,
            same_path,
        })
    }

    pub(crate) fn destination(&self) -> &Path {
        &self.dest
    }

    pub(crate) fn is_in_place(&self) -> bool {
        self.same_path
    }

    /// Authenticate the source for disclosure without staging or publishing it.
    pub(crate) fn preview(&self) -> Result<App, String> {
        let trust = if self.same_path {
            TreeTrust::Installed
        } else {
            TreeTrust::SignatureOnly
        };
        validate_install_tree(
            &self.source,
            &self.expected_id,
            "reviewed app",
            trust,
            false,
        )
    }

    /// Review and reverify before publication, or revalidate an in-place install.
    ///
    /// `allow_unsigned` retains a quarantine verdict for the caller's separate
    /// interactive developer-trust workflow; it does not authenticate the App.
    pub(crate) fn publish(
        &self,
        force: bool,
        allow_unsigned: bool,
        review: &mut dyn FnMut(&App) -> Result<(), String>,
    ) -> Result<AppManifest, String> {
        if self.same_path {
            let app = validate_install_tree(
                &self.source,
                &self.expected_id,
                "in-place app",
                TreeTrust::Installed,
                allow_unsigned,
            )?;
            review(&app)?;
            let current = validate_install_tree(
                &self.source,
                &self.expected_id,
                "reviewed in-place app",
                TreeTrust::Installed,
                allow_unsigned,
            )?;
            require_unchanged_review(&app, &current)?;
            Ok(app.manifest)
        } else {
            if path_entry_exists(&self.dest)
                .map_err(|e| format!("inspect destination {}: {e}", self.dest.display()))?
                && !force
            {
                return Err(format!(
                    "destination `{}` already exists. Re-run with --force to overwrite.",
                    self.dest.display()
                ));
            }
            stage_app_install(
                &self.source,
                &self.dest,
                force,
                &self.expected_id,
                allow_unsigned,
                review,
            )
        }
    }
}

pub(crate) fn verify_installed_app(
    dir: &Path,
    expected_id: &str,
) -> Result<std::sync::Arc<crate::provenance::VerifiedPackage>, String> {
    verify_app_tree(dir, expected_id, TreeTrust::Installed)
}

fn verify_app_tree(
    dir: &Path,
    expected_id: &str,
    mode: TreeTrust,
) -> Result<std::sync::Arc<crate::provenance::VerifiedPackage>, String> {
    use crate::provenance::{PackageKind, VerifyOptions};
    let trust = crate::provenance::trust_store();
    let options = match mode {
        // A staging directory is private scratch space: neither the
        // vendor package root nor a developer grant applies there, so
        // only a publisher signature can authenticate it.
        TreeTrust::SignatureOnly => VerifyOptions::new(PackageKind::App)
            .expect_id(expected_id)
            .signature_only(),
        TreeTrust::Installed => VerifyOptions::new(PackageKind::App).expect_id(expected_id),
    };
    crate::provenance::verify::verify_package(dir, &options, &trust)
        .map(std::sync::Arc::new)
        .map_err(|e| {
            crate::errors::error(
                e.code(),
                &crate::provenance::quarantine_reason(PackageKind::App, expected_id, &e),
            )
            .to_string()
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TreeTrust {
    /// Verify a private staging copy: signature required.
    SignatureOnly,
    /// Verify a tree in its installed location: vendor and developer
    /// trust may apply.
    Installed,
}

fn validate_install_tree(
    dir: &Path,
    expected_id: &str,
    description: &str,
    trust_mode: TreeTrust,
    allow_unsigned: bool,
) -> Result<App, String> {
    // Structural bounds run before anything is trusted: an untrusted
    // bundle must not be able to smuggle symlinks, hardlinks, special
    // files, traversal or case-colliding names into a live install.
    crate::provenance::install::assert_safe_tree(
        dir,
        &crate::provenance::install::Limits::default(),
    )
    .map_err(|e| format!("{description} bundle rejected: {e}"))?;

    let provenance = match verify_app_tree(dir, expected_id, trust_mode) {
        Ok(pkg) => Ok(pkg),
        Err(reason) if allow_unsigned => Err(reason),
        Err(reason) => return Err(reason),
    };

    // Capability-bearing manifest bytes come from the verified
    // snapshot whenever one exists.
    let body = match &provenance {
        Ok(pkg) => pkg
            .manifest_text()
            .map_err(|e| format!("read {description} manifest from verified snapshot: {e}"))?,
        Err(_) => {
            let manifest_path = dir.join("app.json");
            fs::read_to_string(&manifest_path).map_err(|e| {
                format!(
                    "read {description} manifest {}: {e}",
                    manifest_path.display()
                )
            })?
        }
    };
    let manifest = AppManifest::from_json(&body)
        .map_err(|e| format!("parse {description} manifest in {}: {e}", dir.display()))?;
    if manifest.id != expected_id {
        return Err(format!(
            "{description} manifest id changed during install: expected `{expected_id}`, got `{}`",
            manifest.id
        ));
    }
    manifest
        .validate_tools_against_catalog(&crate::ai::tools::list_names())
        .map_err(|e| format!("{description} manifest catalog check: {e}"))?;

    let app = App {
        manifest: manifest.clone(),
        dir: dir.to_path_buf(),
        provenance,
    };
    let violations = app_lint_violations(&app);
    if !violations.is_empty() {
        let details = serde_json::to_string(&violations)
            .unwrap_or_else(|_| "lint violations could not be rendered".to_string());
        return Err(format!(
            "{description} lint failed for `{expected_id}`: {details}"
        ));
    }
    Ok(app)
}

fn require_unchanged_review(reviewed: &App, current: &App) -> Result<(), String> {
    let reviewed_manifest = serde_json::to_value(&reviewed.manifest)
        .map_err(|error| format!("serialize reviewed App manifest: {error}"))?;
    let current_manifest = serde_json::to_value(&current.manifest)
        .map_err(|error| format!("serialize current App manifest: {error}"))?;
    if reviewed_manifest != current_manifest
        || reviewed.provenance_facts() != current.provenance_facts()
    {
        return Err(
            "App package changed during permission review; review the new package before installing"
                .into(),
        );
    }
    Ok(())
}

/// Copy, validate, and durably publish an App tree.
///
/// `staging` and any forced-install backup are siblings of `dest`, so
/// every rename stays on one filesystem. Linux uses `RENAME_EXCHANGE`
/// when the filesystem supports it; the portable fallback recovers any
/// orphaned backup before starting a later install.
fn stage_app_install(
    source: &Path,
    dest: &Path,
    force: bool,
    expected_id: &str,
    allow_unsigned: bool,
    review: &mut dyn FnMut(&App) -> Result<(), String>,
) -> Result<AppManifest, String> {
    stage_app_install_with_ops(
        source,
        dest,
        force,
        expected_id,
        allow_unsigned,
        review,
        (
            |from: &Path, to: &Path| fs::rename(from, to),
            atomic_exchange,
        ),
    )
}

fn stage_app_install_with_ops<R, E>(
    source: &Path,
    dest: &Path,
    force: bool,
    expected_id: &str,
    allow_unsigned: bool,
    review: &mut dyn FnMut(&App) -> Result<(), String>,
    operations: (R, E),
) -> Result<AppManifest, String>
where
    R: FnMut(&Path, &Path) -> io::Result<()>,
    E: FnMut(&Path, &Path) -> io::Result<bool>,
{
    let (mut rename, mut exchange) = operations;
    let parent = dest
        .parent()
        .ok_or_else(|| format!("install destination `{}` has no parent", dest.display()))?;
    fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    recover_interrupted_app_install(parent, dest, expected_id, &mut rename)?;

    let token = uuid::Uuid::new_v4();
    let staging = parent.join(format!(".{expected_id}.install-staging-{token}"));
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(&staging)
        .map_err(|e| format!("create staging {}: {e}", staging.display()))?;
    let _staging_guard = InstallStagingGuard(staging.clone());

    copy_dir_recursive(source, &staging)
        .map_err(|e| format!("copy {} -> {}: {e}", source.display(), staging.display()))?;
    let app = validate_install_tree(
        &staging,
        expected_id,
        "staged app",
        TreeTrust::SignatureOnly,
        allow_unsigned,
    )?;
    sync_install_tree(&staging)
        .map_err(|e| format!("fsync staged app {}: {e}", staging.display()))?;
    review(&app)?;
    let current = validate_install_tree(
        &staging,
        expected_id,
        "reviewed staged app",
        TreeTrust::SignatureOnly,
        allow_unsigned,
    )?;
    require_unchanged_review(&app, &current)?;
    let manifest = app.manifest;

    let destination_exists = path_entry_exists(dest)
        .map_err(|e| format!("inspect destination {}: {e}", dest.display()))?;
    if destination_exists && !force {
        return Err(format!(
            "destination `{}` already exists. Re-run with --force to overwrite.",
            dest.display()
        ));
    }

    if destination_exists {
        match exchange(&staging, dest) {
            Ok(true) => {
                sync_directory_best_effort(parent);
                if let Err(error) = remove_path(&staging) {
                    tracing::warn!(
                        path = %staging.display(),
                        %error,
                        "app install committed but exchanged old tree cleanup failed"
                    );
                }
                sync_directory_best_effort(parent);
                return Ok(manifest);
            }
            Ok(false) => {}
            Err(error) => {
                return Err(format!(
                    "atomically exchange staged app {} with {}: {error}",
                    staging.display(),
                    dest.display()
                ));
            }
        }
    }

    let backup = if destination_exists {
        let backup = parent.join(format!(".{expected_id}.install-backup-{token}"));
        rename(dest, &backup).map_err(|e| {
            format!(
                "move existing {} -> {}: {e}",
                dest.display(),
                backup.display()
            )
        })?;
        Some(backup)
    } else {
        None
    };

    if let Err(publish_error) = rename(&staging, dest) {
        let publish_message = format!(
            "publish staged app {} -> {}: {publish_error}",
            staging.display(),
            dest.display()
        );
        if let Some(backup) = backup.as_ref() {
            match rename(backup, dest) {
                Ok(()) => {
                    sync_directory_best_effort(parent);
                    return Err(format!("{publish_message}; previous install restored"));
                }
                Err(rollback_error) => {
                    sync_directory_best_effort(parent);
                    return Err(format!(
                        "{publish_message}; rollback {} -> {} failed: {rollback_error}; \
                         previous install retained at {}",
                        backup.display(),
                        dest.display(),
                        backup.display()
                    ));
                }
            }
        }
        return Err(publish_message);
    }

    sync_directory_best_effort(parent);
    if let Some(backup) = backup {
        if let Err(error) = remove_path(&backup) {
            tracing::warn!(
                path = %backup.display(),
                %error,
                "app install committed but old backup cleanup failed"
            );
        }
        sync_directory_best_effort(parent);
    }
    Ok(manifest)
}

fn recover_interrupted_app_install<R>(
    parent: &Path,
    dest: &Path,
    expected_id: &str,
    rename: &mut R,
) -> Result<(), String>
where
    R: FnMut(&Path, &Path) -> io::Result<()>,
{
    let mut backups = install_scratch_paths(parent, expected_id, "backup")
        .map_err(|e| format!("scan interrupted App install backups: {e}"))?;
    let staging = install_scratch_paths(parent, expected_id, "staging")
        .map_err(|e| format!("scan interrupted App install staging: {e}"))?;
    let destination_exists = path_entry_exists(dest)
        .map_err(|e| format!("inspect destination {}: {e}", dest.display()))?;

    if !destination_exists && backups.len() > 1 {
        let paths = backups
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "multiple interrupted App install backups found for `{expected_id}`; \
             refusing ambiguous recovery: {paths}"
        ));
    }

    if !destination_exists {
        if let Some(backup) = backups.pop() {
            validate_recovery_candidate(&backup, expected_id)?;
            rename(&backup, dest).map_err(|e| {
                format!(
                    "recover interrupted App install {} -> {}: {e}",
                    backup.display(),
                    dest.display()
                )
            })?;
            sync_directory_best_effort(parent);
        }
    }

    for path in backups.into_iter().chain(staging) {
        remove_path(&path)
            .map_err(|e| format!("clean interrupted App install {}: {e}", path.display()))?;
    }
    sync_directory_best_effort(parent);
    Ok(())
}

fn install_scratch_paths(parent: &Path, expected_id: &str, kind: &str) -> io::Result<Vec<PathBuf>> {
    let prefix = format!(".{expected_id}.install-{kind}-");
    let mut paths = Vec::new();
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(&prefix))
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

fn validate_recovery_candidate(path: &Path, expected_id: &str) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| format!("inspect interrupted App backup {}: {e}", path.display()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "interrupted App backup `{}` is not a directory",
            path.display()
        ));
    }
    let manifest_path = path.join("app.json");
    let body = fs::read_to_string(&manifest_path).map_err(|e| {
        format!(
            "read interrupted App backup {}: {e}",
            manifest_path.display()
        )
    })?;
    let manifest = AppManifest::from_json(&body).map_err(|e| {
        format!(
            "parse interrupted App backup {}: {e}",
            manifest_path.display()
        )
    })?;
    if manifest.id != expected_id {
        return Err(format!(
            "interrupted App backup `{}` declares id `{}`, expected `{expected_id}`",
            path.display(),
            manifest.id
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn atomic_exchange(staging: &Path, dest: &Path) -> io::Result<bool> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let staging = CString::new(staging.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "App install staging path contains a NUL byte",
        )
    })?;
    let dest = CString::new(dest.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "App install destination path contains a NUL byte",
        )
    })?;
    // Both pointers remain valid NUL-terminated strings for the syscall.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            staging.as_ptr(),
            libc::AT_FDCWD,
            dest.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if result == 0 {
        return Ok(true);
    }

    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ENOSYS | libc::EINVAL | libc::EOPNOTSUPP | libc::EPERM) => Ok(false),
        _ => Err(error),
    }
}

#[cfg(not(target_os = "linux"))]
fn atomic_exchange(_staging: &Path, _dest: &Path) -> io::Result<bool> {
    Ok(false)
}

struct InstallStagingGuard(PathBuf);

impl Drop for InstallStagingGuard {
    fn drop(&mut self) {
        let _ = remove_path(&self.0);
    }
}

fn path_entry_exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn remove_path(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path)
        }
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn sync_install_tree(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to fsync symlink at `{}`", path.display()),
        ));
    }
    if metadata.is_file() {
        return fs::File::open(path)?.sync_all();
    }
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unsupported app install entry at `{}`", path.display()),
        ));
    }

    for entry in fs::read_dir(path)? {
        sync_install_tree(&entry?.path())?;
    }
    sync_directory(path)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn sync_directory_best_effort(path: &Path) {
    let _ = sync_directory(path);
}

/// Plain recursive directory copy. **Symlinks are rejected** with an
/// error rather than followed.
///
/// `fs::copy` and `Path::is_dir` traverse symlinks, so a malicious
/// install source containing a link such as `passwd -> /etc/passwd`
/// (or `data -> /var/lib/cos/credentials`) used to either escape the
/// source tree or materialise privileged content as part of the
/// installed App. For Apps we want a verbatim copy of a developer tree:
/// rejecting symlinks is both safer and matches what every shipped
/// App actually needs (none use symlinks). Use `symlink_metadata` to
/// inspect entries without traversal, the same pattern checkpoint.rs
/// uses in `copy_dir_recursive`.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&from)?;
        let ft = metadata.file_type();
        if ft.is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "refusing to copy symlink at `{}` during app install: \
                     install sources must not contain symlinks",
                    from.display()
                ),
            ));
        } else if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/apps/installation.rs"
    ));
}
