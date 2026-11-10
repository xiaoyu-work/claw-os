use super::*;

#[test]
fn status_requires_only_identity_observation() {
    assert_eq!(
        requested_caps("status", None),
        [Cap::new(Verb::SYS_OBSERVE, Scope::name("identities"))]
    );
}

#[test]
fn mutations_retain_identity_management_and_exact_password_scope() {
    for action in [
        "create-user",
        "delete-user",
        "lock-user",
        "unlock-user",
        "set-shell",
        "create-group",
        "delete-group",
        "add-to-group",
        "remove-from-group",
        "restore",
    ] {
        assert_eq!(
            requested_caps(action, None),
            [Cap::new(Verb::SYS_IDENTITY, Scope::name("manage"))],
            "{action}"
        );
    }
    let credential = ("default".to_string(), "alice-password".to_string());
    assert_eq!(
        requested_caps("set-password", Some(&credential)),
        [
            Cap::new(Verb::SYS_IDENTITY, Scope::name("manage")),
            Cap::new(Verb::SECRET_READ, Scope::name("default/alice-password")),
        ]
    );
}

#[test]
fn account_names_are_strict() {
    validate_account_name("user", "alice").unwrap();
    assert!(validate_account_name("user", "Alice").is_err());
    assert!(validate_account_name("user", "../root").is_err());
}

#[test]
fn passwords_are_never_allowed_to_span_lines() {
    validate_password("correct horse battery staple").unwrap();
    assert!(validate_password("line1\nline2").is_err());
}

fn account_request(action: &str, field: Option<(&str, Value)>) -> Value {
    let mut value = json!({
        "session":format!("identity-{}", uuid::Uuid::new_v4().simple()),
        "action":action,"user":"fixture_user","uid":1001,
    });
    if let Some((field, content)) = field {
        value[field] = content;
    }
    value
}

fn account_decision(
    params: &Value,
    caps: Vec<Cap>,
    uses: super::super::authority::Uses,
) -> Decision {
    use super::super::authority::{
        authority, AudienceSet, Binding, Issuance, Issuer, Presentation, Principal, Requirement,
        Subject,
    };
    let owner = unsafe { libc::geteuid() };
    let session = params["session"].as_str().unwrap();
    let (_, view) = authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(owner, std::process::id()).unwrap(),
            binding: Binding::ProcessTree,
            subject: Subject::session(session)
                .with_app(Some("independently-authorized-client".into())),
            audience: AudienceSet::one(Audience::SystemService),
            caps: crate::caps::CapSet::from_caps(caps),
            lifetime: Duration::from_secs(60),
            uses,
            index_session: true,
        })
        .unwrap();
    Decision::for_test(
        view,
        "system.users.control",
        Audience::SystemService,
        Presentation {
            uid: owner,
            pid: std::process::id(),
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
            audience: Audience::SystemService,
            route: "system.users.control",
            session_id: Some(session.into()),
        },
        None,
        &Requirement::RouteDerived,
    )
}

#[test]
fn accounts_actions_have_closed_arguments_and_exact_consumer_needs() {
    let cases = [
        (
            "rename-user",
            Some(("new_name", json!("renamed_user"))),
            AccountValue::Name("renamed_user".into()),
        ),
        (
            "set-full-name",
            Some(("full_name", json!("Fixture User"))),
            AccountValue::FullName("Fixture User".into()),
        ),
        (
            "set-icon",
            Some(("icon", json!("/home/fixture/profile.png"))),
            AccountValue::Icon("/home/fixture/profile.png".into()),
        ),
        (
            "set-account-type",
            Some(("account_type", json!("administrator"))),
            AccountValue::Type(1),
        ),
        ("show-user", None, AccountValue::Show),
    ];
    for (action, field, expected) in cases {
        let value = account_request(action, field);
        let wire = (crate::clawd::routes::Command::SystemUsersControl
            .route()
            .decode)(value.clone())
        .unwrap();
        assert_eq!(wire["uid"], 1001);
        let change = AccountChange::decode(&value).unwrap().unwrap();
        assert_eq!(change.value, expected);
        let mut caps = if action == "show-user" {
            vec![Cap::new(Verb::SYS_OBSERVE, Scope::name("identities"))]
        } else {
            vec![Cap::new(Verb::SYS_IDENTITY, Scope::name("manage"))]
        };
        if action == "set-icon" {
            caps.push(Cap::new(
                Verb::FS_READ,
                Scope::path("/home/fixture/profile.png"),
            ));
        }

        assert_eq!(change.caps(), caps);
        for (key, extra) in [
            ("owner_uid", json!(0)),
            ("bus", json!("caller-bus")),
            ("home", json!("/new-home")),
            ("command", json!("usermod")),
            ("credential", json!("default/password")),
            ("password", json!("not-accepted")),
            ("confirm", json!(true)),
            ("unknown", Value::Null),
        ] {
            let mut bad = value.clone();
            bad[key] = extra;
            assert!(AccountChange::decode(&bad).is_err(), "{action}: {key}");
        }
        for uid in [
            Value::Null,
            json!(0),
            json!(-1),
            json!(1001.5),
            json!(u64::MAX),
        ] {
            let mut bad = value.clone();
            bad["uid"] = uid;
            assert!(AccountChange::decode(&bad).is_err(), "{action}");
        }
    }
    assert!(AccountChange::decode(&account_request(
        "rename-user",
        Some(("new_name", json!("../bad")))
    ))
    .is_err());
    assert!(AccountChange::decode(&account_request(
        "set-full-name",
        Some(("full_name", json!("Name,Other GECOS")))
    ))
    .is_err());
    assert!(AccountChange::decode(&account_request(
        "set-account-type",
        Some(("account_type", json!("sudo")))
    ))
    .is_err());
    for path in [
        "/home/fixture/../secret",
        "/home/fixture/./profile.png",
        "/home//fixture/profile.png",
        "profile.png",
        "/proc/self/fd/1\n",
    ] {
        assert!(
            AccountChange::decode(&account_request("set-icon", Some(("icon", json!(path)))))
                .is_err()
        );
    }
    AccountChange::decode(&account_request(
        "set-full-name",
        Some(("full_name", json!(""))),
    ))
    .unwrap()
    .unwrap();
}

#[test]
fn identity_wire_preserves_old_clients_and_cli_rejects_duplicate_or_ambiguous_new_flags() {
    let decode = crate::clawd::routes::Command::SystemUsersControl
        .route()
        .decode;
    decode(json!({"session":"fixture","action":"set-password","user":"fixture_user","credential":"default/password"})).unwrap();
    for (field, value) in [
        ("owner_uid", json!(0)),
        ("bus", json!("caller-bus")),
        ("uid", json!(-1)),
        ("uid", json!(1001.1)),
    ] {
        let mut params = account_request("show-user", None);
        params[field] = value;
        assert!(decode(params).is_err(), "{field}");
    }
    let _lock = crate::test_env::lock_env();
    let _session = crate::test_env::TestEnvVarGuard::set("COS_SESSION", "fixture-session");
    for (flag, value) in [
        ("--uid", "1001"),
        ("--new-name", "renamed"),
        ("--icon", "/home/fixture/icon.png"),
        ("--account-type", "standard"),
    ] {
        let error = crate::router::dispatch(&[
            "__users".into(),
            "rename-user".into(),
            flag.into(),
            value.into(),
            flag.into(),
            value.into(),
        ])
        .unwrap_err();
        assert!(error.contains("duplicate internal user"), "{error}");
    }
    for uid in ["-1", "+1001", "1001.5", "4294967296"] {
        let error = crate::router::dispatch(&[
            "__users".into(),
            "show-user".into(),
            "--uid".into(),
            uid.into(),
        ])
        .unwrap_err();
        assert!(error.contains("unsigned 32-bit target identity"), "{error}");
    }
    let facts = crate::audit_policy::request_facts(
        "system.users.control",
        &json!({
            "session":"fixture","action":"rename-user","user":"fixture_user","uid":1001,
            "new_name":"renamed_user","account_type":"administrator","owner_uid":0,
            "credential":"default/not-audit-data","full_name":"Private Name",
        }),
    );
    assert_eq!(facts.params["uid"], 1001);
    assert_eq!(facts.params["new_name"], "renamed_user");
    assert_eq!(facts.params["account_type"], "administrator");
    for private in ["owner_uid", "credential", "full_name"] {
        assert!(facts.params.get(private).is_none());
    }
}

#[test]
fn accounts_require_authenticated_owner_session_exact_icon_read_and_live_authority() {
    use super::super::authority::{authority, Uses};
    let _lock = crate::test_env::lock_env();
    let temporary = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data =
        crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temporary.path().join("data"));
    let params = account_request(
        "set-icon",
        Some(("icon", json!("/home/fixture/profile.png"))),
    );
    let change = AccountChange::decode(&params).unwrap().unwrap();
    let owner = unsafe { libc::geteuid() };
    let grant = account_decision(&params, change.caps(), Uses::Budget(1));
    let mut client = ClientIdentity::unknown();
    client.uid = Some(owner);
    assert_eq!(validate_owner(&params, &client, &grant).unwrap(), owner);
    account_preflight(&change, owner, &grant).unwrap();
    let mut wrong = params.clone();
    wrong["session"] = json!("another-session");
    assert!(validate_owner(&wrong, &client, &grant).is_err());
    assert!(account_preflight(&change, owner + 1, &grant).is_err());
    assert!(validate_owner(&params, &ClientIdentity::unknown(), &grant).is_err());
    let mut missing_params = params.clone();
    missing_params["session"] = json!(format!("identity-{}", uuid::Uuid::new_v4().simple()));
    let missing = account_decision(
        &missing_params,
        vec![Cap::new(Verb::SYS_IDENTITY, Scope::name("manage"))],
        Uses::Unbounded,
    );
    validate_owner(&missing_params, &client, &missing).unwrap();
    assert!(account_preflight(&change, owner, &missing).is_err());
    let mut unrelated_params = params.clone();
    unrelated_params["session"] = json!(format!("identity-{}", uuid::Uuid::new_v4().simple()));
    let unrelated = account_decision(
        &unrelated_params,
        vec![
            Cap::new(Verb::SYS_IDENTITY, Scope::name("manage")),
            Cap::new(Verb::FS_READ, Scope::path("/home/fixture/other.png")),
        ],
        Uses::Unbounded,
    );
    validate_owner(&unrelated_params, &client, &unrelated).unwrap();
    assert!(account_preflight(&change, owner, &unrelated).is_err());
    authority().revoke_session(params["session"].as_str().unwrap());
    assert!(account_preflight(&change, owner, &grant).is_err());
}

#[test]
fn account_targets_bind_login_to_uid_without_changing_the_home() {
    let record = PasswdRecord {
        name: "fixture_user".into(),
        uid: 1001,
        gid: 1001,
        gecos: "Fixture,room,phone".into(),
        home: "/home/original-home".into(),
        shell: "/bin/bash".into(),
    };
    validate_target(&record, "fixture_user", 1001).unwrap();
    assert!(validate_target(&record, "other_user", 1001).is_err());
    assert!(validate_target(&record, "fixture_user", 1002).is_err());
    assert!(validate_target(&record, "fixture_user", 0).is_err());
    let request = account_request("rename-user", Some(("new_name", json!("renamed_user"))));
    let change = AccountChange::decode(&request).unwrap().unwrap();
    assert_eq!(change.value, AccountValue::Name("renamed_user".into()));
    assert_eq!(record.home, "/home/original-home");
    let mut forbidden = request;
    forbidden["home"] = json!("/home/renamed_user");
    assert!(AccountChange::decode(&forbidden).is_err());
}

#[test]
fn icon_handoff_pins_owner_readable_regular_bytes_and_seals_the_authorized_snapshot() {
    use super::super::authority::Uses;
    let _lock = crate::test_env::lock_env();
    let temporary = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data =
        crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temporary.path().join("data"));
    let path = temporary.path().join("icon.png");
    fs::write(&path, b"synthetic-image").unwrap();
    let params = account_request("set-icon", Some(("icon", json!(path))));
    let change = AccountChange::decode(&params).unwrap().unwrap();
    let grant = account_decision(&params, change.caps(), Uses::Budget(1));
    let owner = unsafe { libc::geteuid() };
    let authorize_path = |path: &Path| {
        let params = account_request("set-icon", Some(("icon", json!(path))));
        let change = AccountChange::decode(&params).unwrap().unwrap();
        account_decision(&params, change.caps(), Uses::Budget(1))
            .require_all(&change.caps())
            .unwrap()
    };
    let authorized = grant.require_all(&change.caps()).unwrap();
    let source = icon_source(&path, owner, temporary.path(), &authorized).unwrap();
    let sealed = seal_icon(source, &authorized).unwrap();
    fs::write(&path, b"changed-after-snapshot").unwrap();
    let pinned = format!("/proc/self/fd/{}", sealed.as_raw_fd());
    assert_eq!(fs::read(&pinned).unwrap(), b"synthetic-image");
    assert!(sealed.try_clone().unwrap().write_all(b"overwrite").is_err());
    let link = temporary.path().join("link.png");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(icon_source(&link, owner, temporary.path(), &authorize_path(&link)).is_err());
    let hardlink = temporary.path().join("hardlink.png");
    fs::hard_link(&path, &hardlink).unwrap();
    assert!(icon_source(
        &hardlink,
        owner,
        temporary.path(),
        &authorize_path(&hardlink)
    )
    .is_err());
    fs::remove_file(hardlink).unwrap();
    fs::write(&path, vec![0; 1_048_577]).unwrap();
    assert!(icon_source(&path, owner, temporary.path(), &authorized).is_err());
}
