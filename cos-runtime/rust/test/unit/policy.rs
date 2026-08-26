use super::*;

#[test]
fn scope_argv_shapes() {
    assert_eq!(Scope::Path("/etc".into()).argv(), &[OsString::from("--path"), OsString::from("/etc")]);
    assert_eq!(Scope::Wild.argv(), &[OsString::from("--wild")]);
    assert!(Scope::Unscoped.argv().is_empty());
}

#[test]
fn decision_is_allow_helper() {
    let d = Decision { decision: "allow".into(), verb: "fs.read".into(), scope: None, reason: None, hint: None, granted: Some(true) };
    assert!(d.is_allow());
    let d = Decision { decision: "deny".into(), verb: "fs.read".into(), scope: None, reason: None, hint: None, granted: Some(false) };
    assert!(!d.is_allow());
}
