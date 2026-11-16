use super::super::portal::Outcome;
use super::*;

fn request(action: &str, id: &str, modal: Option<bool>) -> Value {
    let mut request = json!({"session":"window", "action":action, "request_id":id});
    if let Some(modal) = modal {
        request["modal"] = json!(modal);
    }
    request
}

#[test]
fn gui_capture_correlation_is_single_window_immutable_and_closed() {
    let id = "a".repeat(32);
    let entry = Entry {
        id: id.clone(),
        modal: true,
        active: true,
        retired: false,
        report: None,
        slot: None,
    };
    for action in ["request", "status", "cancel"] {
        let value = request(action, &id, (action == "request").then_some(true));
        assert!(entry
            .check(&serde_json::from_value(value.clone()).unwrap())
            .is_ok());
        for field in ["owner_uid", "path", "window", "interactive", "executable"] {
            let mut invalid = value.clone();
            invalid[field] = json!("caller-selected");
            assert!(serde_json::from_value::<Request>(invalid).is_err());
        }
    }
    let changed = request("request", &id, Some(false));
    assert!(entry
        .check(&serde_json::from_value(changed).unwrap())
        .is_err());
    let foreign = request("status", &"b".repeat(32), None);
    assert!(entry
        .check(&serde_json::from_value(foreign).unwrap())
        .is_err());
    for invalid in ["", "not-a-correlation", &"c".repeat(33), &"Z".repeat(32)] {
        let value = request("request", invalid, Some(true));
        assert!(serde_json::from_value::<Request>(value)
            .map_or(true, |request| request.identity().is_err()));
    }
    assert!(serde_json::from_value::<Request>(request("status", &id, Some(true))).is_err());
}

#[test]
fn gui_capture_never_reports_outcome_before_portal_and_helper_retirement() {
    let mut entry = Entry {
        id: "a".repeat(32),
        modal: false,
        active: true,
        retired: false,
        report: None,
        slot: None,
    };
    assert_eq!(projection(&entry, false)["status"], "pending");
    assert_eq!(projection(&entry, true)["status"], "cancelling");
    entry.report = Some(Report {
        outcome: Outcome::Clipboard,
        portal_retired: true,
    });
    assert_eq!(projection(&entry, false)["status"], "cleanup_pending");
    entry.active = false;
    assert_eq!(projection(&entry, false)["status"], "cleanup_pending");
    entry.retired = true;
    assert_eq!(projection(&entry, false)["status"], "clipboard");
    assert!(projection(&entry, false).get("path").is_none());
    entry.report = Some(Report {
        outcome: Outcome::Cancelled,
        portal_retired: true,
    });
    assert_eq!(projection(&entry, false)["status"], "cancelled");
}

#[tokio::test]
async fn gui_capture_is_not_a_system_service_or_a_public_endpoint() {
    use crate::clawd::authority::{
        self, Audience, AudienceSet, Issuance, Issuer, Presentation, Principal, Requirement,
        Subject, Uses,
    };
    use crate::clawd::routes::Command;
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data"));
    let route = Command::GuiCaptureControl.route();
    assert!(crate::clawd::app_sessions::gui::provider_route(route.command).is_ok());
    let uid = unsafe { libc::geteuid() };
    let session = format!("gui-capture-{}", uuid::Uuid::new_v4().simple());
    let (_, view) = authority::authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, std::process::id()).unwrap(),
            binding: authority::Binding::ProcessTree,
            subject: Subject::session(&session).with_app(Some("independent-capture-client".into())),
            audience: AudienceSet::one(Audience::GuiResource),
            caps: crate::caps::CapSet::from_caps([Cap::new(
                Verb::DESKTOP_CAPTURE,
                Scope::name("screen"),
            )]),
            lifetime: Duration::from_secs(60),
            uses: Uses::Unbounded,
            index_session: true,
        })
        .unwrap();
    let decision = Decision::for_test(
        view,
        route.name,
        Audience::GuiResource,
        Presentation {
            uid,
            pid: std::process::id(),
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
            audience: Audience::GuiResource,
            route: route.name,
            session_id: Some(session.clone()),
        },
        None,
        &Requirement::RouteDerived,
    );
    let mut params = request("request", &"a".repeat(32), Some(true));
    params["session"] = json!(session);
    let error = control(params, &ClientIdentity::unknown(), &decision)
        .await
        .unwrap_err();
    assert!(error.message.contains("private GUI endpoint"));
}
