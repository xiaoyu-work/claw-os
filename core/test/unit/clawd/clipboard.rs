use super::*;

#[test]
fn mime_validation_is_strict() {
    validate_action("read", Some("text/plain"), None, false, false).unwrap();
    assert!(validate_action("read", Some("--help"), None, false, false).is_err());
}

#[test]
fn wayland_display_rejects_paths() {
    assert!(valid_wayland_display("wayland-0"));
    assert!(!valid_wayland_display("../wayland-0"));
}

use crate::clawd::authority::{
    authority, Audience, AudienceSet, Binding, Issuance, Issuer, Presentation, Principal,
    Requirement, Subject, Uses,
};

fn decision(app: Option<&str>, caps: Vec<Cap>, uses: Uses) -> Decision {
    let uid = unsafe { libc::geteuid() };
    let pid = std::process::id();
    let session = format!("clipboard-{}", uuid::Uuid::new_v4().simple());
    let (_, view) = authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, pid).unwrap(),
            binding: Binding::ProcessTree,
            subject: Subject::session(&session).with_app(app.map(str::to_string)),
            audience: AudienceSet::one(Audience::SystemService),
            caps: crate::caps::CapSet::from_caps(caps),
            lifetime: Duration::from_secs(60),
            uses,
            index_session: true,
        })
        .unwrap();
    Decision::for_test(
        view,
        "system.clipboard.control",
        Audience::SystemService,
        Presentation {
            uid,
            pid,
            start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
            audience: Audience::SystemService,
            route: "system.clipboard.control",
            session_id: Some(session),
        },
        None,
        &Requirement::RouteDerived,
    )
}

fn request_value(
    decision: &Decision,
    action: &str,
    identity: Option<&str>,
    confirm: bool,
) -> Value {
    json!({"session":decision.session_id().unwrap(), "action":action,
        "identity":identity, "confirm":confirm, "deadline_unix_ms":u64::MAX})
}

#[test]
fn history_authority_is_exact_owner_session_bound_and_not_a_product_entitlement() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data"));
    let _caps =
        crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", root.path().join("caps"));
    let owner = unsafe { libc::geteuid() };
    let id = base64::engine::general_purpose::STANDARD.encode([1; 32]);
    for app in [
        Some("panel-clipboard"),
        Some("clipboard-manager"),
        Some("independent-client"),
        None,
    ] {
        for action in [Action::List, Action::Restore, Action::Remove, Action::Clear] {
            let needs = action.needs();
            let granted = decision(app, needs.clone(), Uses::Budget(1));
            let proof = authorize_session(&granted, owner, &needs).unwrap();
            assert_eq!(proof.spent(), needs.as_slice());
            assert!(authorize_session(&granted, owner, &needs).is_err());
        }
        let list = decision(app, Action::List.needs(), Uses::Budget(1));
        let params = request_value(&list, "history-list", None, false);
        let input = validate(&params).unwrap();
        assert!(check_owner(&list, owner + 1, &input).is_err());
        let mut other = params.clone();
        other["session"] = json!("another-session");
        assert!(check_owner(&list, owner, &validate(&other).unwrap()).is_err());
        check_owner(&list, owner, &input).unwrap();
        assert_eq!(
            authorize_session(&list, owner, &Action::List.needs())
                .unwrap()
                .spent(),
            &[Cap::new(Verb::CLIPBOARD_READ, Scope::name("history"))]
        );
        let history = decision(app, Action::Remove.needs(), Uses::Budget(1));
        let restore = validate(&request_value(
            &history,
            "history-restore",
            Some(&id),
            false,
        ))
        .unwrap();
        assert!(check_owner(&history, owner, &restore).is_err());
        let _proof = authorize_session(&history, owner, &Action::Remove.needs()).unwrap();
        let selection = decision(
            app,
            vec![Cap::new(Verb::CLIPBOARD_READ, Scope::name("selection"))],
            Uses::Budget(1),
        );
        assert!(check_owner(
            &selection,
            owner,
            &validate(&request_value(&selection, "history-list", None, false)).unwrap()
        )
        .is_err());
        let _proof = authorize_session(
            &selection,
            owner,
            &[Cap::new(Verb::CLIPBOARD_READ, Scope::name("selection"))],
        )
        .unwrap();
    }
    let revoked = decision(
        Some("independent-client"),
        Action::List.needs(),
        Uses::Unbounded,
    );
    let input = validate(&request_value(&revoked, "history-list", None, false)).unwrap();
    authority().revoke_session(revoked.session_id().unwrap());
    assert!(check_owner(&revoked, owner, &input).is_err());
}

#[test]
fn history_inputs_are_closed_and_clear_is_not_selection_write() {
    let id = base64::engine::general_purpose::STANDARD.encode([1; 32]);
    let valid = json!({"session":"fixture","action":"history-restore","identity":id,"deadline_unix_ms":u64::MAX});
    let route = crate::clawd::routes::Command::SystemClipboardControl.route();
    (route.decode)(valid.clone()).unwrap();
    validate(&valid).unwrap();
    for field in [
        "owner_uid",
        "app_id",
        "program",
        "socket",
        "bus",
        "path",
        "argv",
        "caps",
        "tab",
    ] {
        let mut forged = valid.clone();
        forged[field] = json!("caller-selected");
        assert!((route.decode)(forged).is_err(), "{field}");
    }
    for field in ["mime", "source", "primary"] {
        let mut forged = valid.clone();
        forged[field] = json!(null);
        assert!(validate(&forged).is_err(), "{field}");
    }
    for (field, value) in [
        ("identity", json!("'; select(0);")),
        ("action", json!("history-eval")),
        ("confirm", json!(true)),
        ("deadline_unix_ms", json!(0)),
    ] {
        let mut forged = valid.clone();
        forged[field] = value;
        assert!(validate(&forged).is_err(), "{field}");
    }
    assert!(validate(
        &json!({"session":"fixture","action":"history-clear","deadline_unix_ms":u64::MAX})
    )
    .is_err());
    validate(&json!({"session":"fixture","action":"history-clear","confirm":true,"deadline_unix_ms":u64::MAX})).unwrap();
    assert_eq!(
        Action::Clear.needs(),
        vec![Cap::new(Verb::CLIPBOARD_WRITE, Scope::name("history"))]
    );
    assert_eq!(
        Action::Restore.needs(),
        vec![
            Cap::new(Verb::CLIPBOARD_READ, Scope::name("history")),
            Cap::new(Verb::CLIPBOARD_WRITE, Scope::name("history")),
            Cap::new(Verb::CLIPBOARD_WRITE, Scope::name("selection")),
        ]
    );
}

#[test]
fn unavailable_history_reply_satisfies_mandatory_authority_without_a_second_spend() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data"));
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", root.path().join("caps"));
    let owner = unsafe { libc::geteuid() };
    let allowed = decision(Some("independent-client"), Action::List.needs(), Uses::Budget(1));
    let error = authorized_error(Err(crate::clawd::protocol::BrokerError::unavailable("synthetic unavailable")),
        &allowed, owner, Action::List).unwrap_err();
    assert_eq!(error.kind, crate::clawd::protocol::BrokerErrorKind::Unavailable);
    assert!(crate::clawd::authority::obligation_met(Some(&allowed)));
    let error = authorized_error(Err(error), &allowed, owner, Action::List).unwrap_err();
    assert_eq!(error.kind, crate::clawd::protocol::BrokerErrorKind::Unavailable);
    assert!(authorize_session(&allowed, owner, &Action::List.needs()).is_err());
}

#[test]
fn history_results_preserve_bounded_text_previews_and_stale_identity_failures() {
    let first = hex::encode([1; 32]);
    let second = hex::encode([2; 32]);
    let mut records = vec![
        json!({"identity":first,"text":"first"}),
        json!({"identity":first,"text":"duplicate"}),
        json!({"identity":second,"text":"bad\u{0}binary"}),
        json!({"identity":second,"text":"日 ".repeat(120),"copied_at":" 123 "}),
    ];
    for index in 3..30 {
        records.push(json!({"identity":hex::encode([index;32]),"text":"text"}));
    }
    let value = parse(Action::List, &serde_json::to_vec(&records).unwrap()).unwrap();
    let entries = value["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 20);
    assert_eq!(entries[0]["identity"], base64::engine::general_purpose::STANDARD.encode([1; 32]));
    assert_eq!(entries[0]["preview"], "first");
    assert_eq!(entries[1]["preview"].as_str().unwrap().chars().count(), 181);
    assert!(entries[1]["preview"].as_str().unwrap().ends_with('…'));
    assert_eq!(entries[1]["copied_at"], "123");
    assert!(parse(Action::List, &[0xff]).is_err());
    assert!(parse(Action::List, br#"[{"identity":"bad","text":"text"}]"#).is_err());
    for identity in [
        "01".repeat(31), "FF".repeat(32),
        base64::engine::general_purpose::STANDARD.encode([1; 32]),
    ] {
        assert!(parse(Action::List, &serde_json::to_vec(&json!([
            {"identity":identity,"text":"text"},
        ])).unwrap()).is_err());
    }
    assert!(parse(Action::List, &vec![b' '; MAX_OUTPUT + 1]).is_err());
    assert!(parse(Action::Remove, b"not-found")
        .unwrap_err()
        .contains("changed"));
    assert!(parse(Action::List, br#"{"error":"no-clipboard-tab"}"#)
        .unwrap_err()
        .contains("configured"));
    assert_eq!(
        parse(Action::Restore, b"ok\n").unwrap(),
        json!({"restored":true})
    );
    assert_eq!(
        parse(Action::Remove, b"ok\n").unwrap(),
        json!({"removed":true})
    );
    assert_eq!(
        parse(Action::Clear, b"ok\n").unwrap(),
        json!({"cleared":true})
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires root and a root-owned source tree for the isolated process fixture"]
async fn history_actual_typed_client_authority_and_pinned_process_relay_fixture() {
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "run this isolated fixture explicitly as root"
    );
    use crate::clawd::{client, protocol, routes::Command, transport::frame};
    use std::io::Read;
    let _lock = crate::test_env::lock_env();
    let root = tempfile::Builder::new()
        .prefix("ch-")
        .tempdir_in(std::env::current_dir().unwrap())
        .unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data"));
    let _caps =
        crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", root.path().join("caps"));
    let source = root.path().join("fixture.rs");
    let binary = root.path().join("copyq-fixture");
    fs::write(&source, r#"
use std::{env,fs,io::{Read,Write},net::Shutdown,os::unix::net::{UnixStream,UnixListener},path::PathBuf};
fn main() {
 let a:Vec<String>=env::args().collect();
 if a[1]=="--server" {
  let l=UnixListener::bind(&a[2]).unwrap(); fs::write(&a[3],b"ready").unwrap();
  let (mut s,_)=l.accept().unwrap(); let mut b=Vec::new(); s.read_to_end(&mut b).unwrap();
  fs::write(&a[4],b).unwrap(); s.write_all(&fs::read(&a[5]).unwrap()).unwrap();
 } else {
  assert_eq!(a[1],"eval"); assert_eq!(a.len(),3);
  assert_eq!(env::var("HOME").unwrap(),"/");
  assert!(env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none());
  assert!(env::var_os("WAYLAND_DISPLAY").is_none());
  assert!(env::var_os("COPYQ_BIN").is_none());
  let mut i=Vec::new(); std::io::stdin().read_to_end(&mut i).unwrap();
  let mut s=UnixStream::connect(PathBuf::from(env::var_os("XDG_CONFIG_HOME").unwrap()).join("copyq/.copyq_s")).unwrap();
  s.write_all(a[2].as_bytes()).unwrap(); s.write_all(&i).unwrap(); s.shutdown(Shutdown::Write).unwrap();
  let mut b=Vec::new(); s.read_to_end(&mut b).unwrap(); std::io::stdout().write_all(&b).unwrap();
 }
}
"#).unwrap();
    let compiler = std::process::Command::new("rustc")
        .args([
            "--edition=2021",
            "--crate-name",
            "clipboard_process_fixture",
        ])
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        compiler.status.success(),
        "{}",
        String::from_utf8_lossy(&compiler.stderr)
    );
    let executable = Executable::open(&binary).unwrap();
    let directory = root.path().join("session");
    fs::create_dir(&directory).unwrap();
    let directory = File::open(directory).unwrap();
    let socket = PathBuf::from(format!("/proc/self/fd/{}/.copyq_s", directory.as_raw_fd()));
    let real_socket = root.path().join("session/.copyq_s");
    let ready = root.path().join("ready");
    let received = root.path().join("received");
    let response = root.path().join("response");
    let expected = json!([{"identity":hex::encode([1;32]),"text":"synthetic private preview"}]);
    fs::write(&response, expected.to_string()).unwrap();
    struct Provider(std::process::Child);
    impl Drop for Provider {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut provider = Provider(
        std::process::Command::new(&binary)
            .arg("--server")
            .arg(&real_socket)
            .arg(&ready)
            .arg(&received)
            .arg(&response)
            .spawn()
            .unwrap(),
    );
    let until = std::time::Instant::now() + Duration::from_secs(5);
    while !ready.exists() && std::time::Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(ready.exists());
    let allowed = decision(
        Some("independent-history-client"),
        Action::List.needs(),
        Uses::Budget(1),
    );
    let params = request_value(&allowed, "history-list", None, false);
    let listener = UnixListener::bind(root.path().join("broker")).unwrap();
    crate::clawd::transport::peer::enable_credential_passing(listener.as_raw_fd()).unwrap();
    let broker = root.path().join("broker");
    let client = tokio::task::spawn_blocking(move || {
        client::request_blocking(
            &broker,
            protocol::Request::build(Command::SystemClipboardControl, params),
        )
        .unwrap()
    });
    let (stream, _) = listener.accept().await.unwrap();
    let mut stream = frame::PeerStream::new(stream).unwrap();
    let frame::ReadOutcome::Frame(frame) = stream.read_request(1024 * 1024).await.unwrap() else {
        panic!("missing typed request");
    };
    assert_eq!(frame.credentials.uid, 0);
    assert_eq!(frame.credentials.pid, std::process::id());
    let input: protocol::Request = serde_json::from_slice(&frame.body).unwrap();
    let params = (input.command.route().decode)(input.params).unwrap();
    let request = validate(&params).unwrap();
    let result = execute(
        &request,
        0,
        unsafe { libc::getegid() },
        &allowed,
        &executable,
        &directory,
        root.path(),
    )
    .await
    .unwrap();
    let answer = protocol::Response::ok(input.id, result);
    stream
        .write_response(&serde_json::to_vec(&answer).unwrap())
        .await
        .unwrap();
    let answer = client.await.unwrap();
    assert!(answer.ok);
    let result = answer.result.unwrap();
    assert_eq!(
        result["entries"][0]["preview"],
        "synthetic private preview"
    );
    assert_eq!(result["entries"][0]["identity"], base64::engine::general_purpose::STANDARD.encode([1; 32]));
    assert!(provider.0.wait().unwrap().success());
    let mut bytes = Vec::new();
    File::open(&received)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(
        bytes.starts_with(LIST.as_bytes()),
        "only the closed OS script reaches the provider"
    );
    let payload: Value = serde_json::from_slice(&bytes[LIST.len()..]).unwrap();
    assert_eq!(
        payload,
        json!({"identity":null, "deadline_unix_ms":request.deadline_unix_ms})
    );
    assert!(
        authorize_session(&allowed, 0, &Action::List.needs()).is_err(),
        "finite grant was spent once at actual dispatch"
    );
    assert!(
        executable.process(0, std::process::id()).is_err(),
        "an arbitrary process cannot impersonate the provider"
    );
    fs::remove_file(real_socket).unwrap();
    let unavailable = decision(None, Action::List.needs(), Uses::Unbounded);
    let missing = validate(&request_value(&unavailable, "history-list", None, false)).unwrap();
    assert_eq!(
        execute(
            &missing,
            0,
            0,
            &unavailable,
            &executable,
            &directory,
            root.path()
        )
        .await
        .unwrap_err()
        .kind,
        crate::clawd::protocol::BrokerErrorKind::Unavailable
    );
    assert!(!socket.exists());
}
