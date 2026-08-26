use super::*;

#[test]
fn list_handles_missing_root_dir() {
    // Best-effort: should not panic if /var/lib/cos/models doesn't exist.
    let _ = list();
}
