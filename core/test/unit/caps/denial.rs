use super::*;

#[test]
fn verb_not_granted_has_no_granted_scopes() {
    let d = Denial::verb_not_granted(Verb::FS_DELETE, Scope::path("/etc"));
    assert!(d.granted_scopes.is_empty());
    assert!(matches!(d.reason, DenialReason::VerbNotGranted));
}

#[test]
fn scope_out_of_range_collects_granted_scopes_for_same_verb() {
    let granted = CapSet::from_caps([
        Cap::new(Verb::FS_READ, Scope::path("/home/jay/**")),
        Cap::new(Verb::FS_READ, Scope::path("/tmp/**")),
        Cap::new(Verb::NET_DIAL, Scope::host("*")), // unrelated verb
    ]);
    let d = Denial::scope_out_of_range(Verb::FS_READ, Scope::path("/etc/passwd"), &granted);
    assert_eq!(d.granted_scopes.len(), 2);
}

#[test]
fn display_includes_summary_and_hint() {
    let d = Denial::no_session(Verb::FS_READ, Scope::path("/etc"))
        .with_hint("run `cos session start` first");
    let s = d.to_string();
    assert!(s.contains("Permission denied"));
    assert!(s.contains("run `cos session start`"));
}

#[test]
fn json_envelope_shape_is_stable() {
    let d = Denial::pid_ancestry_mismatch(
        Verb::FS_DELETE,
        Scope::path("/etc/hosts"),
        42,
        7,
    );
    let v = d.to_json();
    assert_eq!(v["error"], "permission denied");
    assert_eq!(v["verb"], "fs.delete");
    assert_eq!(v["reason"]["pid-ancestry-mismatch"]["caller_pid"], 42);
}
