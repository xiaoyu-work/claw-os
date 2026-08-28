//! Linux session-keyring cache for the credential master key.
//!
//! The keyring is a cache, not an independent persistence source: an
//! unavailable keyring falls through to machine-id or root-key persistence.
//! Failures are returned to the resolver, which applies that fallback policy
//! explicitly and records it rather than silently discarding syscall errors.

use super::{CredentialError, CredentialResult};

pub(super) const MASTER_KEY_LABEL: &[u8] = b"cos-credential-key";

#[cfg(target_os = "linux")]
pub(super) fn read_master_key() -> CredentialResult<Option<[u8; 32]>> {
    let Some(payload) = read(MASTER_KEY_LABEL)? else {
        return Ok(None);
    };
    payload.try_into().map(Some).map_err(|payload: Vec<u8>| {
        CredentialError::corrupt(
            "keyring.read",
            format!(
                "credential keyring entry has invalid length {} (expected 32)",
                payload.len()
            ),
        )
    })
}

#[cfg(not(target_os = "linux"))]
pub(super) fn read_master_key() -> CredentialResult<Option<[u8; 32]>> {
    Ok(None)
}

#[cfg(target_os = "linux")]
pub(super) fn cache_master_key(key: &[u8; 32]) -> CredentialResult<()> {
    store(MASTER_KEY_LABEL, key)
}

#[cfg(not(target_os = "linux"))]
pub(super) fn cache_master_key(_key: &[u8; 32]) -> CredentialResult<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn store(description: &[u8], payload: &[u8]) -> CredentialResult<()> {
    use std::ffi::CString;

    const KEY_SPEC_SESSION_KEYRING: i32 = -3;
    let description = CString::new(description).map_err(|error| {
        CredentialError::with_source(
            super::CredentialErrorKind::InvalidInput,
            "keyring.store",
            "credential keyring label contains a NUL byte",
            error,
        )
    })?;
    let key_type = c"user";

    let key_id = unsafe {
        libc::syscall(
            libc::SYS_add_key,
            key_type.as_ptr(),
            description.as_ptr(),
            payload.as_ptr(),
            payload.len(),
            KEY_SPEC_SESSION_KEYRING,
        )
    };
    if key_id < 0 {
        return Err(keyring_failure(
            "keyring.store",
            "cache credential master key in session keyring",
            std::io::Error::last_os_error(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn read(description: &[u8]) -> CredentialResult<Option<Vec<u8>>> {
    use std::ffi::CString;

    const KEY_SPEC_SESSION_KEYRING: i32 = -3;
    const KEYCTL_READ: libc::c_int = 11;
    let description = CString::new(description).map_err(|error| {
        CredentialError::with_source(
            super::CredentialErrorKind::InvalidInput,
            "keyring.read",
            "credential keyring label contains a NUL byte",
            error,
        )
    })?;
    let key_type = c"user";

    unsafe {
        let key_id = libc::syscall(
            libc::SYS_request_key,
            key_type.as_ptr(),
            description.as_ptr(),
            std::ptr::null::<libc::c_char>(),
            KEY_SPEC_SESSION_KEYRING,
        );
        if key_id < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOKEY) {
                return Ok(None);
            }
            return Err(keyring_failure(
                "keyring.read",
                "read credential master key from session keyring",
                error,
            ));
        }

        let mut payload = vec![0u8; 64];
        let len = libc::syscall(
            libc::SYS_keyctl,
            KEYCTL_READ as libc::c_long,
            key_id,
            payload.as_mut_ptr(),
            payload.len(),
        );
        if len < 0 {
            return Err(keyring_failure(
                "keyring.read",
                "read credential master key payload from session keyring",
                std::io::Error::last_os_error(),
            ));
        }
        payload.truncate(len as usize);
        Ok(Some(payload))
    }
}

#[cfg(target_os = "linux")]
fn keyring_failure(
    operation: &'static str,
    context: &'static str,
    source: std::io::Error,
) -> CredentialError {
    CredentialError::io(operation, context, source)
}

#[cfg(all(test, target_os = "linux"))]
pub(super) fn inject_keyring_failure() -> CredentialError {
    keyring_failure(
        "keyring.read",
        "read credential master key from session keyring",
        std::io::Error::other("injected keyring failure"),
    )
}
