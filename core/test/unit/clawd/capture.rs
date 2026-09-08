use super::*;
use crate::clawd::authority::{
    authority, Audience, AudienceSet, Binding, Issuance, Issuer, Presentation, Principal,
    Requirement, Subject, Uses,
};

fn decision(app: &str, caps: Vec<Cap>) -> Decision {
    let uid = unsafe { libc::geteuid() };
    let session = format!("capture-{}", uuid::Uuid::new_v4().simple());
    let (_, view) = authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, std::process::id()).unwrap(),
            binding: Binding::ProcessTree,
            subject: Subject::session(session.clone()).with_app(Some(app.into())),
            audience: AudienceSet::one(Audience::SystemService),
            caps: crate::caps::CapSet::from_caps(caps),
            lifetime: Duration::from_secs(60),
            uses: Uses::Unbounded,
            index_session: true,
        })
        .unwrap();
    Decision::for_test(
        view,
        "system.screenshot.capture",
        Audience::SystemService,
        Presentation {
            uid,
            pid: std::process::id(),
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
            audience: Audience::SystemService,
            route: "system.screenshot.capture",
            session_id: Some(session),
        },
        None,
        &Requirement::RouteDerived,
    )
}

struct Fixture {
    _data: crate::test_env::TestEnvVarGuard,
    _root: tempfile::TempDir,
    directory: PathBuf,
}

impl Fixture {
    fn path(&self) -> &Path {
        &self.directory
    }
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let directory = root.path().join("shots");
    fs::create_dir(&directory).unwrap();
    Fixture {
        _data: crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data")),
        _root: root,
        directory,
    }
}

fn command(script: &str) -> std::process::Command {
    let mut command = std::process::Command::new("/bin/sh");
    command.args(["-c", script]);
    command
}

const PNG: &str = "printf '\\211PNG\\r\\n\\032\\nfixture'";

#[tokio::test]
async fn revoked_capture_cannot_start_the_native_process() {
    let _lock = crate::test_env::lock_env();
    let dir = fixture();
    let caps = requested_caps(dir.path());
    let allowed = decision("cosmic-screenshot", caps.clone());
    crate::approvals::app_policy::revoke(
        unsafe { libc::geteuid() },
        "cosmic-screenshot",
        caps[0].clone(),
    )
    .unwrap();
    let marker = dir.path().join("ran");
    let mut fake = command("touch \"$MARKER\"");
    fake.env("MARKER", &marker);
    let error = capture_to(dir.path(), &allowed, fake, Duration::from_secs(1))
        .await
        .unwrap_err();
    assert!(error.contains("revoked"), "{error}");
    assert!(!marker.exists());
}

#[tokio::test]
async fn capture_requires_own_identity_screen_and_exact_output_before_execution() {
    let _lock = crate::test_env::lock_env();
    let dir = fixture();
    let marker = dir.path().join("ran");
    let valid = requested_caps(dir.path());
    for denied in [
        decision("cosmic-screenshot", vec![]),
        decision("cosmic-screenshot", vec![valid[0].clone()]),
        decision("cosmic-screenshot", vec![valid[1].clone()]),
        decision(
            "cosmic-screenshot",
            requested_caps(&dir.path().join("other")),
        ),
        decision(
            "cosmic-screenshot",
            vec![
                Cap::new(Verb::DESKTOP_CAPTURE, Scope::name("clipboard")),
                valid[1].clone(),
            ],
        ),
        decision("cosmic-player", valid.clone()),
    ] {
        let mut fake = command("touch \"$MARKER\"");
        fake.env("MARKER", &marker);
        assert!(
            capture_to(dir.path(), &denied, fake, Duration::from_secs(1))
                .await
                .is_err()
        );
        assert!(!marker.exists());
    }
    assert_eq!(
        native_args(false),
        ["--portal-capture-stdout", "--modal=false"]
    );
    assert_eq!(
        native_args(true),
        ["--portal-capture-stdout", "--modal=true"]
    );
    assert_eq!(valid[0].scope, Scope::name("screen"));
    assert_ne!(valid[1].scope, Scope::Wild);
}

#[tokio::test]
async fn capture_persists_private_png_and_never_clobbers_existing_output() {
    let _lock = crate::test_env::lock_env();
    use std::os::unix::fs::PermissionsExt;
    let dir = fixture();
    let allowed = decision("cosmic-screenshot", requested_caps(dir.path()));
    let result = capture_to(dir.path(), &allowed, command(PNG), Duration::from_secs(1))
        .await
        .unwrap();
    let path = Path::new(result["path"].as_str().unwrap());
    assert_eq!(result["cancelled"], false);
    assert_eq!(path.parent(), Some(dir.path()));
    assert!(path
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("Screenshot_"));
    assert_eq!(fs::read(path).unwrap(), b"\x89PNG\r\n\x1a\nfixture");
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::metadata(path).unwrap().uid(), unsafe {
        libc::geteuid()
    });
    let target = Target::open(path, unsafe { libc::geteuid() }).unwrap();
    assert!(target.write_new(b"replacement", &allowed).is_err());
    assert_eq!(fs::read(path).unwrap(), b"\x89PNG\r\n\x1a\nfixture");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn cancelled_failed_invalid_and_timed_out_captures_leave_no_output() {
    let _lock = crate::test_env::lock_env();
    let dir = fixture();
    let allowed = decision("cosmic-screenshot", requested_caps(dir.path()));
    assert_eq!(
        capture_to(
            dir.path(),
            &allowed,
            command("exit 0"),
            Duration::from_secs(1)
        )
        .await
        .unwrap(),
        json!({"cancelled":true,"path":null}),
    );
    for (script, limit, message) in [
        (
            "printf 'portal unavailable' >&2; exit 1",
            1000,
            "portal unavailable",
        ),
        ("printf 'not an image'", 1000, "invalid PNG"),
        ("sleep 5", 30, "timed out"),
        (
            "head -c 9000 /dev/zero >&2; sleep 5",
            1000,
            "diagnostics exceed",
        ),
    ] {
        let error = capture_to(
            dir.path(),
            &allowed,
            command(script),
            Duration::from_millis(limit),
        )
        .await
        .unwrap_err();
        assert!(error.contains(message), "{error}");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}

#[tokio::test]
async fn capture_refuses_directory_replacement_while_portal_runs() {
    let _lock = crate::test_env::lock_env();
    let root = fixture();
    let dir = root.path().join("shots");
    let moved = root.path().join("moved");
    fs::create_dir(&dir).unwrap();
    let allowed = decision("cosmic-screenshot", requested_caps(&dir));
    let mut fake = command(&format!("mv \"$DIR\" \"$MOVED\"; mkdir \"$DIR\"; {PNG}"));
    fake.env("DIR", &dir).env("MOVED", &moved);
    let error = capture_to(&dir, &allowed, fake, Duration::from_secs(1))
        .await
        .unwrap_err();
    assert!(error.contains("directory changed"), "{error}");
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    assert_eq!(fs::read_dir(&moved).unwrap().count(), 0);
}

#[test]
fn capture_wire_and_paths_reject_interactive_or_forged_authority() {
    let _lock = crate::test_env::lock_env();
    use crate::clawd::routes::Command;
    let valid = json!({"session":"s","request":{"directory":"/work/shots","modal":true}});
    (Command::SystemScreenshotCapture.route().decode)(valid.clone()).unwrap();
    for (field, value) in [
        ("interactive", json!(true)),
        ("owner_uid", json!(0)),
        ("app_id", json!("cosmic-player")),
        ("program", json!("/bin/sh")),
        ("source", json!("/work/private")),
        ("modal", json!("true")),
        ("directory", json!("x".repeat(4097))),
    ] {
        let mut invalid = valid.clone();
        invalid["request"][field] = value;
        assert!(
            (Command::SystemScreenshotCapture.route().decode)(invalid).is_err(),
            "{field}"
        );
    }
    for path in ["", "relative", "/invalid\0path", "/proc", "/"] {
        assert!(
            resolve_directory(path, unsafe { libc::geteuid() }).is_err(),
            "{path:?}"
        );
    }
    let dir = fixture();
    fs::write(dir.path().join("file"), "not a directory").unwrap();
    assert!(
        resolve_directory(dir.path().join("file").to_str().unwrap(), unsafe {
            libc::geteuid()
        })
        .is_err()
    );
}
