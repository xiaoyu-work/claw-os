use super::*;

pub(super) struct FileCredentialStore;

pub(super) const FILE_STORE: FileCredentialStore = FileCredentialStore;

impl FileCredentialStore {
    fn path(&self, id: &CredentialId) -> PathBuf {
        namespace_dir(id.namespace()).join(format!("{}.json", id.name()))
    }

    pub(super) fn read_record(
        &self,
        id: &CredentialId,
    ) -> CredentialResult<Option<StoredCredential>> {
        let path = self.path(id);
        let Some(data) = crate::filelock::read_locked(&path).map_err(|message| {
            CredentialError::unavailable(
                "credential.read",
                format!("failed to read credential {}: {message}", path.display()),
            )
        })?
        else {
            return Ok(None);
        };
        serde_json::from_str(&data).map(Some).map_err(|source| {
            CredentialError::with_source(
                CredentialErrorKind::Corrupt,
                "credential.parse",
                format!("failed to parse credential {}", path.display()),
                source,
            )
        })
    }

    pub(super) fn write_record(&self, record: &StoredCredential) -> CredentialResult<()> {
        let id = CredentialId::parse(&record.namespace, &record.name)?;
        let data = serde_json::to_string_pretty(record).map_err(|source| {
            CredentialError::with_source(
                CredentialErrorKind::Corrupt,
                "credential.serialize",
                "failed to serialize credential record",
                source,
            )
        })?;
        write_credential_atomic_typed(&self.path(&id), &data)
    }

    pub(super) fn with_refresh<T>(
        &self,
        id: &CredentialId,
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        with_refresh_lock(&self.path(id), operation)
    }

    pub(super) fn write_bundle(
        &self,
        namespace: &NamespaceId,
        name: &str,
        keys: Vec<String>,
    ) -> CredentialResult<BundleManifest> {
        let dir = bundles_dir(namespace.as_str());
        fs::create_dir_all(&dir).map_err(|source| {
            CredentialError::io_at(
                "bundle.write",
                "failed to create bundles directory",
                &dir,
                source,
            )
        })?;
        let manifest = BundleManifest {
            name: name.to_string(),
            namespace: namespace.as_str().to_string(),
            keys,
            created_at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        };
        let data = serde_json::to_string_pretty(&manifest).map_err(|source| {
            CredentialError::with_source(
                CredentialErrorKind::Corrupt,
                "bundle.serialize",
                "failed to serialize credential bundle",
                source,
            )
        })?;
        let path = dir.join(format!("{name}.json"));
        crate::filelock::write_locked(&path, &data).map_err(|message| {
            CredentialError::unavailable(
                "bundle.write",
                format!(
                    "failed to write credential bundle {}: {message}",
                    path.display()
                ),
            )
        })?;
        Ok(manifest)
    }

    pub(super) fn read_bundle(
        &self,
        namespace: &NamespaceId,
        name: &str,
    ) -> CredentialResult<Option<BundleManifest>> {
        let path = bundles_dir(namespace.as_str()).join(format!("{name}.json"));
        let Some(data) = crate::filelock::read_locked(&path).map_err(|message| {
            CredentialError::unavailable(
                "bundle.read",
                format!(
                    "failed to read credential bundle {}: {message}",
                    path.display()
                ),
            )
        })?
        else {
            return Ok(None);
        };
        serde_json::from_str(&data).map(Some).map_err(|source| {
            CredentialError::with_source(
                CredentialErrorKind::Corrupt,
                "bundle.parse",
                format!("failed to parse credential bundle {}", path.display()),
                source,
            )
        })
    }
}

impl CredentialStore for FileCredentialStore {
    fn contains(&self, id: &CredentialId) -> CredentialResult<bool> {
        let path = self.path(id);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => Ok(metadata.file_type().is_file()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(CredentialError::io_at(
                "credential.inspect",
                "failed to inspect",
                &path,
                source,
            )),
        }
    }

    fn load(&self, id: &CredentialId, enforce_tier: bool) -> CredentialResult<String> {
        read_credential_value(id.name(), id.namespace(), enforce_tier)
            .map_err(|message| CredentialError::external("credential.load", message))
    }

    fn minimum_tier(&self, id: &CredentialId) -> CredentialResult<Option<u8>> {
        credential_min_tier_if_present(id.name(), id.namespace())
            .map_err(|message| CredentialError::external("credential.metadata", message))
    }

    fn store(&self, request: StoreRequest<'_>) -> CredentialResult<StoreResult> {
        store_credential_record(
            request.id.name(),
            request.value,
            request.id.namespace(),
            request.min_tier,
            request.ttl,
            request.refresh_cmd,
        )
        .map_err(|message| CredentialError::external("credential.store", message))
    }
}

// ===========================================================================
// Path helpers
// ===========================================================================

/// Root credentials directory: `~/.local/share/cos/credentials`
/// (overridable via `COS_CREDENTIALS_DIR`). Per-user so non-root
/// callers can store API keys without touching `/var/lib/cos`.
pub(super) fn credentials_dir() -> PathBuf {
    crate::paths::user_credentials_dir()
}

/// Namespace directory: `<credentials_dir>/<namespace>`.
pub(super) fn namespace_dir(namespace: &str) -> PathBuf {
    credentials_dir().join(namespace)
}

/// Bundle directory: `<credentials_dir>/<namespace>/bundles`.
pub(super) fn bundles_dir(namespace: &str) -> PathBuf {
    namespace_dir(namespace).join("bundles")
}

// ===========================================================================
// Atomic, 0600-from-the-start credential file writes
// ===========================================================================

/// Path of the per-credential atomic-write lock sentinel:
/// `<path>.lock`. Held briefly by [`write_credential_atomic`] to serialize
/// concurrent tmp+rename writers against the same data file.
pub(super) fn lock_sentinel_path(path: &Path) -> PathBuf {
    let mut s: std::ffi::OsString = path.as_os_str().to_os_string();
    s.push(".lock");
    PathBuf::from(s)
}

/// Path of the per-credential **refresh** lock sentinel:
/// `<path>.refresh.lock`. Held for the duration of an auto-refresh attempt
/// (executing the OAuth round-trip, re-checking expiry, writing the rotated
/// token). Distinct from [`lock_sentinel_path`] so that the OAuth refresh
/// command — which itself shells out to `cos credential store` in a child
/// process and therefore needs the *write* lock — cannot deadlock against the
/// parent's *refresh* lock. See the HIGH "refresh-token cannibalisation race"
/// audit finding.
pub(super) fn refresh_sentinel_path(path: &Path) -> PathBuf {
    let mut s: std::ffi::OsString = path.as_os_str().to_os_string();
    s.push(".refresh.lock");
    PathBuf::from(s)
}

/// Run `f` while holding an exclusive `flock(2)` on the per-credential
/// refresh sentinel (`<path>.refresh.lock`). Cleans up the lock on success or
/// failure (the OS releases it automatically when `lock_file` is dropped).
///
/// Used to serialize auto-refresh attempts for a credential, ensuring only
/// one OAuth round-trip runs at a time per credential id.
pub(super) fn with_refresh_lock<F, T>(path: &Path, f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String>,
{
    use std::fs::OpenOptions;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }

    let lock_path = refresh_sentinel_path(path);
    let lock_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| format!("open credential refresh lock {}: {e}", lock_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let ret = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) };
        if ret != 0 {
            return Err(format!(
                "flock LOCK_EX {}: {}",
                lock_path.display(),
                std::io::Error::last_os_error()
            ));
        }
    }

    let result = f();

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        if unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_UN) } != 0 {
            let error = std::io::Error::last_os_error();
            if result.is_ok() {
                return Err(format!(
                    "flock LOCK_UN {}: {error}",
                    lock_path.display()
                ));
            }
            tracing::error!(
                path = %lock_path.display(),
                error = %error,
                "credential refresh operation and lock release both failed"
            );
        }
    }
    drop(lock_file);
    result
}

/// Run `f` while holding an exclusive `flock(2)` on the per-credential
/// atomic-write sentinel (`<path>.lock`). Brief; used only by
/// [`write_credential_atomic`] to serialize tmp+rename writers.
pub(super) fn with_write_lock<F, T>(path: &Path, f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String>,
{
    use std::fs::OpenOptions;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }

    let lock_path = lock_sentinel_path(path);
    let lock_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| format!("open credential write lock {}: {e}", lock_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let ret = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) };
        if ret != 0 {
            return Err(format!(
                "flock LOCK_EX {}: {}",
                lock_path.display(),
                std::io::Error::last_os_error()
            ));
        }
    }

    let result = f();

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        if unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_UN) } != 0 {
            let error = std::io::Error::last_os_error();
            if result.is_ok() {
                return Err(format!(
                    "flock LOCK_UN {}: {error}",
                    lock_path.display()
                ));
            }
            tracing::error!(
                path = %lock_path.display(),
                error = %error,
                "credential write operation and lock release both failed"
            );
        }
    }
    drop(lock_file);
    result
}

fn with_refresh_lock_typed<F, T>(path: &Path, f: F) -> CredentialResult<T>
where
    F: FnOnce() -> CredentialResult<T>,
{
    with_lock_typed(
        path,
        refresh_sentinel_path(path),
        "credential.refresh_lock",
        f,
    )
}

fn with_write_lock_typed<F, T>(path: &Path, f: F) -> CredentialResult<T>
where
    F: FnOnce() -> CredentialResult<T>,
{
    with_lock_typed(path, lock_sentinel_path(path), "credential.write_lock", f)
}

fn with_lock_typed<F, T>(
    path: &Path,
    lock_path: PathBuf,
    operation: &'static str,
    f: F,
) -> CredentialResult<T>
where
    F: FnOnce() -> CredentialResult<T>,
{
    use std::fs::OpenOptions;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| {
            CredentialError::io_at(operation, "failed to create", parent, source)
        })?;
    }
    let lock_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|source| {
            CredentialError::io_at(operation, "failed to open", &lock_path, source)
        })?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        if unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(CredentialError::io_at(
                operation,
                "failed to lock",
                &lock_path,
                std::io::Error::last_os_error(),
            ));
        }
    }

    let result = f();

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        if unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_UN) } != 0 {
            let source = std::io::Error::last_os_error();
            if result.is_ok() {
                return Err(CredentialError::io_at(
                    operation,
                    "failed to unlock",
                    &lock_path,
                    source,
                ));
            }
            tracing::error!(
                path = %lock_path.display(),
                error = %source,
                "typed credential operation and lock release both failed"
            );
        }
    }
    result
}

/// Atomically write a credential JSON file with mode `0600` set *at creation
/// time* — there is no post-write `chmod` window during which a same-uid
/// reader could open the file with default umask permissions (the MEDIUM
/// "credential file perms applied AFTER write" finding).
///
/// Sequence (Unix):
///   1. Acquire `flock(LOCK_EX)` on the sibling `.lock` sentinel.
///   2. Remove any stale `<path>.tmp` left by a previous crash.
///   3. `open(O_WRONLY|O_CREAT|O_EXCL, 0600)` the tmp file.
///   4. Write payload, then `fsync` the tmp file.
///   5. `rename(tmp, path)` — atomic on same filesystem.
///   6. `fsync` the parent directory so the rename hits disk.
pub(super) fn write_credential_atomic(path: &Path, data: &str) -> Result<(), String> {
    write_credential_atomic_typed(path, data).map_err(|error| error.to_string())
}

fn write_credential_atomic_typed(path: &Path, data: &str) -> CredentialResult<()> {
    with_write_lock_typed(path, || write_credential_atomic_unlocked_typed(path, data))
}

/// Remove a credential while excluding both refreshers and atomic writers.
/// Lock order deliberately matches refresh (`refresh -> write`) so revoke
/// cannot deadlock with a refresh command that persists a rotated token.
pub(super) fn remove_credential_atomic(path: &Path) -> Result<bool, String> {
    with_refresh_lock(path, || {
        with_write_lock(path, || match fs::remove_file(path) {
            Ok(()) => {
                if let Some(parent) = path.parent() {
                    std::fs::File::open(parent)
                        .and_then(|directory| directory.sync_all())
                        .map_err(|error| {
                            format!("fsync credential directory {}: {error}", parent.display())
                        })?;
                }
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(format!("remove {}: {error}", path.display())),
        })
    })
}

/// Inner of [`write_credential_atomic`] — does the tmp+rename+fsync dance but
/// does NOT acquire the per-credential write lock. Caller is responsible for
/// synchronization.
fn write_credential_atomic_unlocked(path: &Path, data: &str) -> Result<(), String> {
    write_credential_atomic_unlocked_typed(path, data).map_err(|error| error.to_string())
}

fn write_credential_atomic_unlocked_typed(path: &Path, data: &str) -> CredentialResult<()> {
    use std::fs::OpenOptions;
    use std::io::Write;

    let parent = path.parent().ok_or_else(|| {
        CredentialError::invalid(
            "credential.persist",
            format!("credential path has no parent: {}", path.display()),
        )
    })?;
    fs::create_dir_all(parent).map_err(|source| {
        CredentialError::io_at("credential.persist", "failed to create", parent, source)
    })?;

    let tmp_path = path.with_extension("tmp");
    match fs::remove_file(&tmp_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(CredentialError::io_at(
                "credential.persist",
                "failed to remove stale temporary credential",
                &tmp_path,
                source,
            ))
        }
    }

    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut tmp_file = opts.open(&tmp_path).map_err(|source| {
        CredentialError::io_at(
            "credential.persist",
            "failed to open temporary credential",
            &tmp_path,
            source,
        )
    })?;

    tmp_file.write_all(data.as_bytes()).map_err(|source| {
        CredentialError::io_at(
            "credential.persist",
            "failed to write temporary credential",
            &tmp_path,
            source,
        )
    })?;
    tmp_file.sync_all().map_err(|source| {
        CredentialError::io_at(
            "credential.persist",
            "failed to fsync temporary credential",
            &tmp_path,
            source,
        )
    })?;
    drop(tmp_file);

    fs::rename(&tmp_path, path).map_err(|source| {
        CredentialError::io(
            "credential.persist",
            format!(
                "failed to rename temporary credential {} to {}",
                tmp_path.display(),
                path.display()
            ),
            source,
        )
    })?;

    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| {
            CredentialError::io_at(
                "credential.persist",
                "failed to fsync credential directory",
                parent,
                source,
            )
        })?;
    Ok(())
}

pub(super) fn store_credential_record(
    name: &str,
    value: &str,
    namespace: &str,
    min_tier: u8,
    ttl: Option<u64>,
    refresh_cmd: Option<String>,
) -> Result<StoreResult, String> {
    let dir = namespace_dir(namespace);
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create credentials dir: {e}"))?;

    // Encrypt with AES-256-GCM
    let (value_b64, nonce_b64) =
        encrypt_value(value.as_bytes()).map_err(|error| error.to_string())?;

    let session = crate::proc::current_session_id();
    let now = chrono::Utc::now();
    let stored_at = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let expires_at = ttl.map(|secs| {
        let exp = now + chrono::Duration::seconds(secs as i64);
        exp.format("%Y-%m-%dT%H:%M:%SZ").to_string()
    });

    let cred = StoredCredential {
        name: name.to_string(),
        namespace: namespace.to_string(),
        value_b64,
        nonce_b64: Some(nonce_b64),
        min_tier,
        stored_at: stored_at.clone(),
        stored_by: session,
        expires_at: expires_at.clone(),
        refresh_cmd,
    };

    let path = dir.join(format!("{name}.json"));
    let data =
        serde_json::to_string_pretty(&cred).map_err(|e| format!("failed to serialize: {e}"))?;
    // Atomic write with mode 0600 from creation time + fsync of tmp & parent.
    write_credential_atomic(&path, &data)
        .map_err(|e| format!("failed to write credential: {e}"))?;

    Ok(StoreResult {
        stored_at,
        expires_at,
    })
}

/// Re-store a credential value as part of a session rollback (the undo
/// of a `credential.revoke`, or of a `credential.store` that overwrote a
/// prior value). Reuses the normal AES-256-GCM at-rest encryption and
/// the atomic 0600 write so the restored entry is indistinguishable from
/// one written by `cos credential store`.
///
/// Tier / TTL / refresh metadata is not captured in the mutation log, so
/// the restored entry uses the default tier (0) and no expiry.
///
/// Security note: the value being restored already lived in this
/// session's own (session-private) mutation log, so restoring it grants
/// no access the session did not already have — hence no extra caps gate.
pub fn rollback_restore(namespace: &str, name: &str, value: &str) -> Result<(), String> {
    let dir = namespace_dir(namespace);
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create credentials dir: {e}"))?;
    let (value_b64, nonce_b64) =
        encrypt_value(value.as_bytes()).map_err(|error| error.to_string())?;
    let stored_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let cred = StoredCredential {
        name: name.to_string(),
        namespace: namespace.to_string(),
        value_b64,
        nonce_b64: Some(nonce_b64),
        min_tier: 0,
        stored_at,
        stored_by: Some("session-rollback".to_string()),
        expires_at: None,
        refresh_cmd: None,
    };
    let path = dir.join(format!("{name}.json"));
    let data =
        serde_json::to_string_pretty(&cred).map_err(|e| format!("failed to serialize: {e}"))?;
    write_credential_atomic(&path, &data)
}

/// Delete a credential entry as part of a session rollback (the undo of a
/// `credential.store` that created a brand-new key). No-op if the entry
/// is already gone.
pub fn rollback_delete(namespace: &str, name: &str) -> Result<(), String> {
    let path = namespace_dir(namespace).join(format!("{name}.json"));
    remove_credential_atomic(&path)
        .map(|_| ())
        .map_err(|error| format!("failed to delete credential {namespace}/{name}: {error}"))
}

pub(super) fn revoke(id: &CredentialId) -> Result<bool, String> {
    let path = namespace_dir(id.namespace()).join(format!("{}.json", id.name()));
    remove_credential_atomic(&path)
}

/// List credentials within a single namespace.
pub(super) fn list_namespace(namespace: &NamespaceId) -> Result<Vec<CredentialMetadata>, String> {
    let dir = namespace_dir(namespace.as_str());
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut credentials = Vec::new();
    let entries = fs::read_dir(&dir).map_err(|e| format!("failed to read credentials dir: {e}"))?;

    for entry in entries.flatten() {
        let fname = entry.file_name().to_string_lossy().to_string();
        if !fname.ends_with(".json") {
            continue;
        }
        // Skip the bundles subdirectory
        if entry.path().is_dir() {
            continue;
        }
        if let Ok(Some(data)) = crate::filelock::read_locked(&entry.path()) {
            if let Ok(cred) = serde_json::from_str::<StoredCredential>(&data) {
                let expired = is_expired(&cred.expires_at);
                credentials.push(CredentialMetadata {
                    name: cred.name,
                    min_tier: cred.min_tier,
                    stored_at: cred.stored_at,
                    stored_by: cred.stored_by,
                    expires_at: cred.expires_at,
                    refresh_cmd: cred.refresh_cmd,
                    expired,
                });
            }
        }
    }

    credentials.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(credentials)
}

/// List all namespaces and their credential counts.
pub(super) fn list_all_namespaces() -> Result<Vec<NamespaceSummary>, String> {
    let dir = credentials_dir();
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut namespaces = Vec::new();

    let entries = fs::read_dir(&dir).map_err(|e| format!("failed to read credentials dir: {e}"))?;

    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let ns_name = entry.file_name().to_string_lossy().to_string();
        let mut count: usize = 0;
        if let Ok(ns_entries) = fs::read_dir(entry.path()) {
            for ns_entry in ns_entries.flatten() {
                let fname = ns_entry.file_name().to_string_lossy().to_string();
                if fname.ends_with(".json") && ns_entry.path().is_file() {
                    count += 1;
                }
            }
        }
        namespaces.push(NamespaceSummary {
            namespace: ns_name,
            count,
        });
    }

    namespaces.sort_by(|a, b| a.namespace.cmp(&b.namespace));
    Ok(namespaces)
}

pub(super) fn credential_min_tier(name: &str, namespace: &str) -> Result<u8, String> {
    credential_min_tier_if_present(name, namespace)?
        .ok_or_else(|| format!("credential not found: {name} (namespace: {namespace})"))
}

pub(super) fn credential_min_tier_if_present(
    name: &str,
    namespace: &str,
) -> Result<Option<u8>, String> {
    credential_scope(namespace, name)?;
    let path = namespace_dir(namespace).join(format!("{name}.json"));
    let Some(data) = crate::filelock::read_locked(&path)
        .map_err(|error| format!("failed to read credential metadata: {error}"))?
    else {
        return Ok(None);
    };
    let credential: StoredCredential = serde_json::from_str(&data)
        .map_err(|error| format!("failed to parse credential metadata: {error}"))?;
    Ok(Some(credential.min_tier))
}

pub(super) fn read_credential_value(
    name: &str,
    namespace: &str,
    enforce_tier: bool,
) -> Result<String, String> {
    let path = namespace_dir(namespace).join(format!("{name}.json"));
    if !path.is_file() {
        return Err(format!(
            "credential not found: {name} (namespace: {namespace}). Store it with: cos credential store {name} <value> --namespace {namespace}"
        ));
    }
    let data = crate::filelock::read_locked(&path)
        .map_err(|e| format!("failed to read: {e}"))?
        .ok_or_else(|| format!("credential not found: {name} (namespace: {namespace})"))?;
    let cred: StoredCredential =
        serde_json::from_str(&data).map_err(|e| format!("failed to parse: {e}"))?;
    if enforce_tier {
        let current_tier = effective_session_tier();
        if !tier_grants_access(current_tier, cred.min_tier) {
            return Err(format!(
                "insufficient tier: credential '{name}' requires {}, current session has {current_tier}",
                cred.min_tier
            ));
        }
    }
    if is_expired(&cred.expires_at) {
        return Err(format!("credential '{name}' has expired"));
    }

    match decrypt_value(&cred) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|e| format!("not valid UTF-8: {e}")),
        Err(error) => Err(error.to_string()),
    }
}

/// Public read-only accessor for credential values.
///
/// Use this from kernel subsystems that need a stored secret (LLM
/// provider API keys, OAuth tokens, …) instead of going through the CLI
/// dispatcher. Returns the plaintext value or a human-readable error.
///
/// Returns `Ok(None)` if the credential is not present so callers can
/// fall back to environment variables or other lookup paths without
/// converting a not-found into a hard error.
pub fn try_load(name: &str, namespace: &str) -> Result<Option<String>, String> {
    try_load_typed(name, namespace).map_err(|error| error.to_string())
}

pub fn try_load_typed(name: &str, namespace: &str) -> CredentialResult<Option<String>> {
    let id = CredentialId::parse(namespace, name)?;
    credential_scope(id.namespace(), id.name())
        .map_err(|message| CredentialError::unauthorized("credential.load", message))?;
    if !FILE_STORE.contains(&id)? {
        return Ok(None);
    }
    // Trusted kernel accessor used to construct providers on behalf of a
    // session. User/App-facing reads must go through `cmd_load`.
    FILE_STORE.load(&id, false).map(Some)
}

/// Return whether a credential record exists without decrypting its value.
///
/// Trusted launch planning uses this to select an exact provider capability
/// before an App starts. User/App-facing reads still go through `cmd_load`.
pub fn is_configured(name: &str, namespace: &str) -> Result<bool, String> {
    credential_scope(namespace, name)?;
    Ok(namespace_dir(namespace)
        .join(format!("{name}.json"))
        .is_file())
}

pub(crate) fn load_for_broker(
    name: &str,
    namespace: &str,
    current_tier: u8,
) -> Result<String, String> {
    credential_scope(namespace, name)?;
    let required_tier = credential_min_tier(name, namespace)?;
    if !tier_grants_access(current_tier, required_tier) {
        return Err(format!(
            "insufficient tier: credential '{name}' requires {required_tier}, current session has {current_tier}"
        ));
    }
    read_credential_value(name, namespace, false)
}

pub fn load_for_scheduler(
    name: &str,
    namespace: &str,
    home: &Path,
    owner_uid: u32,
    session_tier: u8,
) -> Result<String, String> {
    credential_scope(namespace, name)?;
    let home = home
        .canonicalize()
        .map_err(|error| format!("canonicalize scheduled credential home: {error}"))?;
    let path = home
        .join(".local")
        .join("share")
        .join("cos")
        .join("credentials")
        .join(namespace)
        .join(format!("{name}.json"));
    let data = read_owner_credential(&path, &home, owner_uid)?;
    let credential: StoredCredential = serde_json::from_str(&data)
        .map_err(|error| format!("failed to parse scheduled credential: {error}"))?;
    if !tier_grants_access(session_tier, credential.min_tier) {
        return Err(format!(
            "insufficient tier for scheduled credential {namespace}/{name}"
        ));
    }
    if is_expired(&credential.expires_at) {
        return Err(format!("credential '{name}' has expired"));
    }
    String::from_utf8(decrypt_value(&credential).map_err(|error| error.to_string())?)
        .map_err(|error| format!("credential is not valid UTF-8: {error}"))
}

pub fn load_optional_for_scheduler(
    name: &str,
    namespace: &str,
    home: &Path,
    owner_uid: u32,
    session_tier: u8,
) -> Result<Option<String>, String> {
    credential_scope(namespace, name)?;
    let home = home
        .canonicalize()
        .map_err(|error| format!("canonicalize scheduled credential home: {error}"))?;
    let path = home
        .join(".local")
        .join("share")
        .join("cos")
        .join("credentials")
        .join(namespace)
        .join(format!("{name}.json"));
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(format!(
                "scheduled credential is not a regular file: {name}"
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("inspect scheduled credential: {error}")),
    }
    load_for_scheduler(name, namespace, &home, owner_uid, session_tier).map(Some)
}

#[cfg(target_os = "linux")]
fn read_owner_credential(path: &Path, home: &Path, owner_uid: u32) -> Result<String, String> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::os::unix::io::AsRawFd;

    const MAX_CREDENTIAL_FILE_BYTES: u64 = 1024 * 1024;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| format!("failed to open scheduled credential: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect scheduled credential: {error}"))?;
    if !metadata.is_file() || metadata.uid() != owner_uid {
        return Err("scheduled credential is not a regular owner-controlled file".to_string());
    }
    let target = fs::canonicalize(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .map_err(|error| format!("resolve scheduled credential: {error}"))?;
    if !target.starts_with(home) {
        return Err("scheduled credential escapes the owner home".to_string());
    }
    let mut data = String::new();
    file.take(MAX_CREDENTIAL_FILE_BYTES + 1)
        .read_to_string(&mut data)
        .map_err(|error| format!("failed to read scheduled credential: {error}"))?;
    if data.len() as u64 > MAX_CREDENTIAL_FILE_BYTES {
        return Err("scheduled credential file exceeds 1 MiB".to_string());
    }
    Ok(data)
}

#[cfg(not(target_os = "linux"))]
fn read_owner_credential(_path: &Path, _home: &Path, _owner_uid: u32) -> Result<String, String> {
    Err("scheduled credential loading requires Linux".to_string())
}
