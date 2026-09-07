use super::*;
use crate::clawd::authority::{
    authority, Audience, AudienceSet, Binding, Decision, Issuance, Issuer, Presentation, Principal,
    Requirement, Subject, Uses,
};

fn decision(caps: Vec<Cap>) -> Decision {
    let uid = unsafe { libc::geteuid() };
    let session = format!("file-{}", uuid::Uuid::new_v4().simple());
    let (_, view) = authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, std::process::id()).unwrap(),
            binding: Binding::ProcessTree,
            subject: Subject::session(session.clone()).with_app(Some("cosmic-edit".into())),
            audience: AudienceSet::one(Audience::SystemService),
            caps: crate::caps::CapSet::from_caps(caps),
            lifetime: std::time::Duration::from_secs(60),
            uses: Uses::Unbounded,
            index_session: true,
        })
        .unwrap();
    let presentation = Presentation {
        uid,
        pid: std::process::id(),
        start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
        audience: Audience::SystemService,
        route: "system.filesystem.write",
        session_id: Some(session),
    };
    Decision::for_test(
        view,
        "system.filesystem.write",
        Audience::SystemService,
        presentation,
        None,
        &Requirement::RouteDerived,
    )
}

fn caps(path: &Path) -> Vec<Cap> {
    vec![
        Cap::new(Verb::FS_READ, Scope::path(path.to_str().unwrap())),
        Cap::new(Verb::FS_WRITE, Scope::path(path.to_str().unwrap())),
    ]
}

async fn access(params: Value, authority: &Decision, mutation: bool) -> Result<Value, String> {
    super::access(params, authority, mutation).await
}

#[tokio::test]
async fn full_text_roundtrips_and_failed_replacements_preserve_file() {
    let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let path = dir.path().join("new é.txt");
    let authority = decision(caps(&path));
    let write = |content: &str| json!({"request":{"action":"write","path":path,"content":content}});
    access(write(""), &authority, true).await.unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"");
    access(write("one\né\0three"), &authority, true)
        .await
        .unwrap();
    let read = access(
        json!({"request":{"action":"read","path":path}}),
        &authority,
        false,
    )
    .await
    .unwrap();
    assert_eq!(read["content"], "one\né\0three");
    access(
        json!({"request":{"action":"replace","path":path,"find":"é","replace":"two"}}),
        &authority,
        true,
    )
    .await
    .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "one\ntwo\0three");
    for find in ["", "absent", "o"] {
        let before = fs::read(&path).unwrap();
        assert!(access(
            json!({"request":{"action":"replace","path":path,"find":find,"replace":"X"}}),
            &authority,
            true
        )
        .await
        .is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    fs::write(&path, [0xff, 0xfe]).unwrap();
    assert!(access(
        json!({"request":{"action":"read","path":path}}),
        &authority,
        false
    )
    .await
    .is_err());
    assert!(access(
        json!({"request":{"action":"replace","path":path,"find":"x","replace":"y"}}),
        &authority,
        true
    )
    .await
    .is_err());
    assert_eq!(fs::read(&path).unwrap(), [0xff, 0xfe]);
    fs::write(&path, vec![b'x'; MAX_TEXT_BYTES + 1]).unwrap();
    assert!(access(
        json!({"request":{"action":"replace","path":path,"find":"x","replace":"y"}}),
        &authority,
        true
    )
    .await
    .is_err());
    assert_eq!(
        fs::metadata(&path).unwrap().len(),
        (MAX_TEXT_BYTES + 1) as u64
    );
}

#[tokio::test]
async fn denied_and_symlink_scopes_never_write_outside_authority() {
    let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let path = dir.path().join("document");
    let other = dir.path().join("other");
    fs::write(&path, "original").unwrap();
    fs::write(&other, "secret").unwrap();
    let denied = decision(vec![]);
    assert!(access(
        json!({"request":{"action":"write","path":path,"content":"bad"}}),
        &denied,
        true
    )
    .await
    .is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "original");
    let allowed = decision(caps(&path));
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&other, &link).unwrap();
    assert!(access(
        json!({"request":{"action":"write","path":link,"content":"bad"}}),
        &allowed,
        true
    )
    .await
    .is_err());
    assert_eq!(fs::read_to_string(&other).unwrap(), "secret");
    fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&path, &link).unwrap();
    access(
        json!({"request":{"action":"write","path":link,"content":"good"}}),
        &allowed,
        true,
    )
    .await
    .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "good");
    let target = Target::open(&path, unsafe { libc::geteuid() }).unwrap();
    fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&other, &path).unwrap();
    assert!(target.read().is_err());
}

#[test]
fn wire_rejects_unknown_outer_fields_and_bounds_request() {
    use crate::clawd::wire::requests::FilesystemAccess;
    assert!(serde_json::from_value::<FilesystemAccess>(json!({
        "session":"s", "request":{"action":"read","path":"/x"}, "owner":0,
    }))
    .is_err());
    assert!(serde_json::from_value::<FilesystemAccess>(json!({
        "session":"s", "request":{"content":"x".repeat(crate::clawd::wire::bounded::APP_ARGS_STDIN_MAX_BYTES)},
    })).is_err());
    assert!(serde_json::from_value::<Request>(
        json!({"action":"write","path":"/x","content":"","session":"forged"})
    )
    .is_err());
    let large = json!({"session":"s","request":{"action":"write","path":"/work/a","content":"x".repeat(256 * 1024)}});
    let relay = json!({"session_id":"s","handle":"opaque","command":"system.filesystem.write","params":large});
    serde_json::from_value::<crate::clawd::wire::requests::AppSessionRelay>(relay).unwrap();
    (crate::clawd::routes::Command::SystemFilesystemWrite
        .route()
        .decode)(large)
    .unwrap();
    let invalid = json!({"session":"s","request":{"action":"write","path":"/work/a","content":"","owner_uid":0}});
    assert!((crate::clawd::routes::Command::SystemFilesystemWrite
        .route()
        .decode)(invalid)
    .is_err());
    assert!(
        serde_json::from_value::<super::super::wire::bounded::FileText>(json!(
            "x".repeat(MAX_TEXT_BYTES + 1)
        ))
        .is_err()
    );
    assert!(replace_unique("aaa", "aa", "b").is_err());
}

#[test]
fn filesystem_preserves_owner_dac_and_thread_identity() {
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let path = directory.path().join("private");
    fs::write(&path, "private bytes").unwrap();
    let uid = unsafe { libc::geteuid() };
    let gid = unsafe { libc::getegid() };
    let target = Target::open(&path, uid).unwrap();
    if uid == 0 {
        assert!(Target::open(&path, 65534).is_err());
    } else {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        assert!(target.read().is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(FsIdentityGuard::enter(0).is_err());
    }
    assert_eq!(unsafe { libc::setfsuid(!0) }, uid as libc::c_int);
    assert_eq!(unsafe { libc::setfsgid(!0) }, gid as libc::c_int);
    assert_eq!(target.read().unwrap().unwrap().0, b"private bytes");
}
