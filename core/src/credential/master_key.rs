use super::*;

// ===========================================================================
// Key derivation and nonce generation
// ===========================================================================

/// Path to the on-disk persistent root key. Used as a last-resort source of
/// keying material when neither the kernel keyring (Linux) nor
/// `/etc/machine-id` are available — e.g. inside chroots, minimal containers,
/// non-Linux dev boxes, or test harnesses. Override the file location with
/// `COS_CREDENTIAL_ROOT_KEY_PATH` (used by tests).
///
/// Lives next to the rest of the per-install state under `$COS_STATE_DIR`
/// (aliased to `$COS_DATA_DIR` in this codebase via [`crate::paths::data_dir`]).
fn credential_root_key_path() -> PathBuf {
    if let Some(v) = std::env::var_os("COS_CREDENTIAL_ROOT_KEY_PATH") {
        return PathBuf::from(v);
    }
    crate::paths::data_dir().join("credential-root.key")
}

/// Path the code consults for the machine identity. Tests override this with
/// `COS_MACHINE_ID_PATH` to simulate "no machine-id" environments without
/// touching `/etc/machine-id`.
#[cfg(target_os = "linux")]
fn machine_id_path() -> PathBuf {
    if let Some(v) = std::env::var_os("COS_MACHINE_ID_PATH") {
        return PathBuf::from(v);
    }
    PathBuf::from("/etc/machine-id")
}

/// Fill `buf` with cryptographically secure random bytes from the OS CSPRNG.
/// Returns the underlying syscall error on failure.
///
///   * Linux:   `getrandom(2)`
///   * macOS / BSD: `getentropy(3)` (limited to 256 bytes per call)
///   * Other Unix: `/dev/urandom` blocking read
pub(crate) fn os_random_bytes(buf: &mut [u8]) -> Result<(), std::io::Error> {
    #[cfg(target_os = "linux")]
    {
        let mut filled = 0;
        while filled < buf.len() {
            let ret = unsafe {
                libc::getrandom(
                    buf[filled..].as_mut_ptr() as *mut libc::c_void,
                    buf.len() - filled,
                    0,
                )
            };
            if ret > 0 {
                filled += ret as usize;
                continue;
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        Ok(())
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    ))]
    {
        for chunk in buf.chunks_mut(256) {
            let ret =
                unsafe { libc::getentropy(chunk.as_mut_ptr() as *mut libc::c_void, chunk.len()) };
            if ret != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        return Ok(());
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    )))]
    {
        use std::io::Read;
        let mut f = std::fs::File::open("/dev/urandom")?;
        f.read_exact(buf)?;
        Ok(())
    }
}

/// Read the persistent on-disk root key, returning its bytes if present and
/// well-formed (exactly 32 bytes). Only absence returns `None`; malformed keys
/// and filesystem failures propagate so callers never generate over damage.
fn load_persistent_root_key() -> CredentialResult<Option<[u8; 32]>> {
    load_persistent_root_key_at(&credential_root_key_path())
}

/// Inner of [`load_persistent_root_key`] — same logic but reads from a
/// caller-supplied path. Exists so unit tests can exercise the persistence
/// helpers against a per-test scratch path without mutating process-global
/// env vars (which races other tests).
pub(super) fn load_persistent_root_key_at(path: &Path) -> CredentialResult<Option<[u8; 32]>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(CredentialError::io_at(
                "root_key.load",
                "failed to read credential root key",
                path,
                error,
            ))
        }
    };
    if bytes.len() != 32 {
        return Err(CredentialError::corrupt(
            "root_key.load",
            format!(
                "credential root key {} has invalid length {} (expected 32)",
                path.display(),
                bytes.len()
            ),
        ));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(Some(out))
}

/// Generate a fresh 32-byte root key from the OS CSPRNG and persist it to
/// `credential_root_key_path()` with mode `0600`, fsync the file, then fsync
/// the parent directory.
///
/// Atomicity / TOCTOU: opens with `O_CREAT|O_EXCL` and `mode=0o600` so the
/// file exists with restrictive permissions from the very first byte written
/// (no post-write `chmod` race). If a sibling process raced us to create the
/// file, we honor whatever they wrote and return that instead.
fn generate_and_persist_root_key() -> CredentialResult<[u8; 32]> {
    generate_and_persist_root_key_at(&credential_root_key_path())
}

/// Inner of [`generate_and_persist_root_key`] — writes to a caller-supplied
/// path. Exists so unit tests can exercise the generator without mutating
/// process-global env vars.
pub(super) fn generate_and_persist_root_key_at(path: &Path) -> CredentialResult<[u8; 32]> {
    generate_and_persist_root_key_at_with(path, os_random_bytes)
}

fn generate_and_persist_root_key_at_with(
    path: &Path,
    random: impl FnOnce(&mut [u8]) -> Result<(), std::io::Error>,
) -> CredentialResult<[u8; 32]> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            CredentialError::io_at("root_key.persist", "failed to create", parent, error)
        })?;
    }

    let mut key = [0u8; 32];
    random(&mut key).map_err(|error| {
        CredentialError::io(
            "root_key.random",
            "OS CSPRNG failed; refusing predictable key material",
            error,
        )
    })?;

    // Atomic: O_CREAT|O_EXCL with mode 0o600 *at create time*.
    #[cfg(unix)]
    let open_result = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    };
    #[cfg(not(unix))]
    let open_result = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path);

    match open_result {
        Ok(mut f) => {
            use std::io::Write;
            f.write_all(&key).map_err(|error| {
                CredentialError::io(
                    "root_key.persist",
                    "failed to write credential root key",
                    error,
                )
            })?;
            f.sync_all().map_err(|error| {
                CredentialError::io(
                    "root_key.persist",
                    "failed to fsync credential root key",
                    error,
                )
            })?;
            if let Some(parent) = path.parent() {
                std::fs::File::open(parent)
                    .and_then(|directory| directory.sync_all())
                    .map_err(|error| {
                        CredentialError::io_at(
                            "root_key.persist",
                            "failed to fsync root key directory",
                            parent,
                            error,
                        )
                    })?;
            }
            Ok(key)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Race: another process wrote the key. Read what they wrote.
            load_persistent_root_key_at(path)?.ok_or_else(|| {
                CredentialError::unavailable(
                    "root_key.persist",
                    "credential root key exists but disappeared before it could be read",
                )
            })
        }
        Err(error) => Err(CredentialError::io_at(
            "root_key.persist",
            "failed to create credential root key at",
            path,
            error,
        )),
    }
}

#[cfg(test)]
pub(super) fn inject_root_key_random_failure(path: &Path) -> CredentialError {
    generate_and_persist_root_key_at_with(path, |_| {
        Err(std::io::Error::other("injected random failure"))
    })
    .expect_err("injected random source must fail")
}

/// Derive a 256-bit encryption key.
///
/// Resolution order:
///   1. Kernel session keyring (Linux only — fast in-memory cache).
///   2. `/etc/machine-id` (Linux only — stable per-install identifier).
///   3. Persistent on-disk root key at `${COS_STATE_DIR}/credential-root.key`,
///      generated from the OS CSPRNG on first use, mode `0600`.
///
/// The previous behaviour of falling back to `sha256("claw-os-credential-store-key-v1")`
/// when `/etc/machine-id` was unreadable has been removed — that constant was a
/// universally known key that decrypted every credential store offline. We
/// either find / derive a per-install secret or we generate a fresh random one
/// and persist it. Recoverable key-source failures are returned to the caller.
pub(super) fn derive_key() -> CredentialResult<[u8; 32]> {
    #[cfg(target_os = "linux")]
    {
        // 1. Kernel keyring cache (zero-cost when populated).
        match read_master_key() {
            Ok(Some(key)) => return Ok(key),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    operation = error.operation(),
                    error = %error,
                    "credential keyring cache unavailable; trying durable key sources"
                );
            }
        }

        // 2. Machine-id (per-install identifier).
        if let Ok(id) = fs::read_to_string(machine_id_path()) {
            let trimmed = id.trim();
            if !trimmed.is_empty() {
                let derived = sha256::hash(trimmed.as_bytes());
                if let Err(error) = cache_master_key(&derived) {
                    tracing::warn!(
                        operation = error.operation(),
                        error = %error,
                        "credential key derived but could not be cached in the session keyring"
                    );
                }
                return Ok(derived);
            }
        }
    }

    // 3. Persistent on-disk root key (any platform).
    let key = match load_persistent_root_key()? {
        Some(key) => key,
        None => generate_and_persist_root_key()?,
    };

    if let Err(error) = cache_master_key(&key) {
        tracing::warn!(
            operation = error.operation(),
            error = %error,
            "credential key loaded but could not be cached in the session keyring"
        );
    }

    Ok(key)
}

/// Generate a random 12-byte nonce using the OS CSPRNG.
///
/// Returns an error if the CSPRNG syscall fails — there is no safe fallback. AES-GCM
/// catastrophically loses confidentiality and authenticity if a (key, nonce)
/// pair is reused, and the legacy fallback path (`now_nanos || counter`)
/// trivially collided across process restarts and across cooperating
/// processes. Failing loudly is the correct behaviour.
pub(super) fn generate_nonce() -> CredentialResult<[u8; 12]> {
    let mut nonce = [0u8; 12];
    os_random_bytes(&mut nonce).map_err(|error| {
        CredentialError::io(
            "nonce.random",
            "OS CSPRNG failed; refusing predictable nonce",
            error,
        )
    })?;
    Ok(nonce)
}

// ===========================================================================
// Legacy XOR obfuscation (backward compatibility only)
// ===========================================================================

/// Key used by the legacy XOR obfuscation scheme.
///
/// Historically this fell back to the literal string
/// `"claw-os-credential-store-key-v1"` when `/etc/machine-id` was unreadable,
/// which meant any attacker could trivially decrypt legacy XOR credentials on
/// any host without a machine-id (containers, chroots, non-Linux). That
/// hard-coded fallback has been removed: callers now derive the same per-
/// install secret used by AES-GCM, so the legacy XOR scheme is at least no
/// weaker than `derive_key()` itself.
pub(super) fn legacy_obfuscation_key() -> CredentialResult<Vec<u8>> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(id) = fs::read_to_string(machine_id_path()) {
            let trimmed = id.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.as_bytes().to_vec());
            }
        }
    }
    // No machine-id: fall through to the per-install random root key. Note
    // that legacy-XOR credentials predating this codebase were created with
    // the machine-id key, so on machines without machine-id (which never had
    // a working legacy key to begin with) decryption will simply fail loudly
    // rather than succeed with the universal hard-coded literal.
    Ok(derive_key()?.to_vec())
}

/// XOR-based deobfuscation (symmetric — same function encrypts and decrypts).
pub(super) fn legacy_xor(data: &[u8]) -> CredentialResult<Vec<u8>> {
    let key = legacy_obfuscation_key()?;
    Ok(data
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ key[i % key.len()])
        .collect())
}
