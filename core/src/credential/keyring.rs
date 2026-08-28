//! Best-effort Linux session-keyring cache for the credential master key.
//!
//! The keyring is a cache, not an independent persistence source: an
//! unavailable keyring falls through to machine-id or root-key persistence.

pub(super) const MASTER_KEY_LABEL: &[u8] = b"cos-credential-key";

#[cfg(target_os = "linux")]
pub(super) fn read_master_key() -> Option<[u8; 32]> {
    let payload = read(MASTER_KEY_LABEL)?;
    payload.try_into().ok()
}

#[cfg(not(target_os = "linux"))]
pub(super) fn read_master_key() -> Option<[u8; 32]> {
    None
}

#[cfg(target_os = "linux")]
pub(super) fn cache_master_key(key: &[u8; 32]) {
    store(MASTER_KEY_LABEL, key);
}

#[cfg(not(target_os = "linux"))]
pub(super) fn cache_master_key(_key: &[u8; 32]) {}

#[cfg(target_os = "linux")]
fn store(description: &[u8], payload: &[u8]) {
    use std::ffi::CString;

    const KEY_SPEC_SESSION_KEYRING: i32 = -3;
    let Ok(description) = CString::new(description) else {
        return;
    };
    let key_type = c"user";

    unsafe {
        libc::syscall(
            libc::SYS_add_key,
            key_type.as_ptr(),
            description.as_ptr(),
            payload.as_ptr(),
            payload.len(),
            KEY_SPEC_SESSION_KEYRING,
        );
    }
}

#[cfg(target_os = "linux")]
fn read(description: &[u8]) -> Option<Vec<u8>> {
    use std::ffi::CString;

    const KEY_SPEC_SESSION_KEYRING: i32 = -3;
    const KEYCTL_READ: libc::c_int = 11;
    let description = CString::new(description).ok()?;
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
            return None;
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
            return None;
        }
        payload.truncate(len as usize);
        Some(payload)
    }
}
