use super::*;

#[test]
fn unknown_identity_has_no_uid_or_home() {
    let id = ClientIdentity::unknown();
    assert!(id.uid.is_none());
    assert!(id.home_dir().is_none());
}

#[cfg(unix)]
#[test]
fn resolve_home_for_current_uid_matches_passwd() {
    // The current process's uid must resolve to a real passwd
    // entry on any working unix system. Compare against the
    // `HOME` env var as a sanity check (they should normally
    // agree; if HOME has been overridden we just skip).
    let uid = unsafe { libc::getuid() } as u32;
    let resolved = resolve_home(uid);
    assert!(resolved.is_some(), "getpwuid_r returned None for self uid");
    if let (Some(env_home), Some(pwd_home)) =
        (std::env::var_os("HOME"), resolved.as_ref())
    {
        if env_home != pwd_home.as_os_str() {
            // Possible in containers where HOME is set to /root
            // but passwd points elsewhere — log and move on.
            eprintln!(
                "note: HOME ({:?}) differs from passwd entry ({:?})",
                env_home, pwd_home
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn resolve_home_for_bogus_uid_returns_none() {
    // uid 4_000_000_001 is well above any realistic system uid.
    assert!(resolve_home(4_000_000_001).is_none());
}
