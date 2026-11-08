use super::*;
use crate::clawd::authority::{
    authority, Audience, AudienceSet, Binding, Issuance, Issuer, Presentation, Principal,
    Requirement, Subject, Uses,
};

const ACCOUNT: &str = "00000000-0000-4000-8000-000000000001:account1";
const OTHER: &str = "00000000-0000-4000-8000-000000000001:account2";

fn reference(account: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!([account, "native-folder", "public@example.invalid"])).unwrap(),
    )
}

fn request(action: &str, account: Option<&str>, args: Value) -> MailControl {
    serde_json::from_value(json!({
        "session":"mail-fixture", "action":action, "account":account,
        "args":args, "deadline_unix_ms":123456,
    }))
    .unwrap()
}

fn decision(app: Option<&str>, caps: Vec<Cap>) -> Decision {
    let uid = unsafe { libc::geteuid() };
    let session = format!("mail-{}", uuid::Uuid::new_v4().simple());
    let (_, view) = authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, std::process::id()).unwrap(),
            binding: Binding::ProcessTree,
            subject: Subject::session(session.clone()).with_app(app.map(str::to_string)),
            audience: AudienceSet::one(Audience::SystemService),
            caps: crate::caps::CapSet::from_caps(caps),
            lifetime: Duration::from_secs(60),
            uses: Uses::Budget(1),
            index_session: true,
        })
        .unwrap();
    Decision::for_test(
        view,
        "system.mail.control",
        Audience::SystemService,
        Presentation {
            uid,
            pid: std::process::id(),
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
            audience: Audience::SystemService,
            route: "system.mail.control",
            session_id: Some(session),
        },
        None,
        &Requirement::RouteDerived,
    )
}

#[test]
fn mail_authorization_is_owner_and_account_bound_not_app_name_bound() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data"));
    let request = request("read", Some(ACCOUNT), json!({"id":reference(ACCOUNT)}));
    let cap = Cap::new(Verb::MAIL_READ, Scope::name(ACCOUNT));
    let uid = unsafe { libc::geteuid() };
    for app in [
        Some("email"),
        Some("independent-mail-client"),
        Some("mail-ai"),
        None,
    ] {
        let allowed = decision(app, vec![cap.clone()]);
        assert!(authorize(&request, &allowed, uid + 1).is_err());
        assert!(!crate::clawd::authority::obligation_met(Some(&allowed)));
        assert_eq!(
            authorize(&request, &allowed, uid).unwrap().spent(),
            &[cap.clone()]
        );
        assert!(authorize(&request, &allowed, uid).is_err());
        let wrong = decision(app, vec![Cap::new(Verb::MAIL_READ, Scope::name(OTHER))]);
        assert!(authorize(&request, &wrong, uid).is_err());
        let ai_only = decision(app, vec![Cap::new(Verb::AI_CHAT_UNTRUSTED, Scope::Wild)]);
        assert!(authorize(&request, &ai_only, uid).is_err());
    }
}

#[test]
fn native_mail_references_and_draft_confirmation_cannot_cross_accounts() {
    assert!(prepare(&request(
        "read",
        Some(ACCOUNT),
        json!({"id":reference(OTHER)})
    ))
    .is_err());
    let valid = request(
        "draft_send",
        Some(ACCOUNT),
        json!({
            "id":reference(ACCOUNT), "sender_account":OTHER,
            "review":format!("sha256:{}", "a".repeat(64)), "confirm":true,
        }),
    );
    assert_eq!(
        prepare(&valid).unwrap(),
        vec![
            Cap::new(Verb::MAIL_READ, Scope::name(ACCOUNT)),
            Cap::new(Verb::MAIL_DRAFT, Scope::name(ACCOUNT)),
            Cap::new(Verb::MAIL_SEND, Scope::name(OTHER)),
        ]
    );
    let mut false_confirmation = valid.clone();
    false_confirmation
        .args
        .insert("confirm".into(), json!(false));
    assert!(prepare(&false_confirmation).is_err());
    let mut other_profile = valid;
    other_profile.args.insert(
        "sender_account".into(),
        json!("00000000-0000-4000-8000-000000000002:account2"),
    );
    assert!(prepare(&other_profile).is_err());
}

#[test]
fn mailbox_payloads_reject_provider_credentials_and_unbounded_attachment_requests() {
    assert!(prepare(&request("accounts", None, json!({"provider":"gmail"}))).is_err());
    assert!(prepare(&request(
        "send",
        Some(ACCOUNT),
        json!({
            "to":"public@example.invalid","subject":"hello\r\nBcc: other@example.invalid","body":"",
        })
    ))
    .is_err());
    let valid = request(
        "attachment_download",
        Some(ACCOUNT),
        json!({
            "id":reference(ACCOUNT), "attachment":"1.2", "output":"/tmp/mail-fixture.pdf",
            "max_bytes":MAX_ATTACHMENT,
        }),
    );
    assert_eq!(
        prepare(&valid).unwrap().last(),
        Some(&Cap::new(
            Verb::FS_WRITE,
            Scope::path("/tmp/mail-fixture.pdf")
        ))
    );
    let mut too_large = valid;
    too_large
        .args
        .insert("max_bytes".into(), json!(MAX_ATTACHMENT + 1));
    assert!(prepare(&too_large).is_err());
}

#[tokio::test]
async fn only_missing_or_stale_mailbox_sockets_trigger_startup() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let path = root.path().join("private-mail.sock");
    let owner = unsafe { libc::geteuid() };
    assert!(connect(&path, owner).await.unwrap().is_none());
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        connect(&path, owner).await.is_err(),
        "a same-UID impostor is not a native Mail host"
    );
    drop(listener);
    assert!(connect(&path, owner).await.unwrap().is_none());
    fs::remove_file(&path).unwrap();
    fs::write(&path, "not a socket").unwrap();
    assert!(connect(&path, owner).await.is_err());
}
