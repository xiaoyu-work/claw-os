use super::*;
use crate::clawd::authority::{authority, Audience, GrantView, Presentation};
use crate::operations::invocation::PreparedSessionCall;

async fn open(fixture: &Fixture) -> (PreparedInvocation, Value) {
    let response = fixture
        .call(AppHostCall::BeginSession(AppSessionInvocation {
            app_id: fixture.original.app_id.clone(),
            package_digest: fixture.original.package_digest.clone(),
        }))
        .await;
    assert!(response.ok, "{response:?}");
    let prepared: PreparedInvocation = serde_json::from_value(response.result.unwrap()).unwrap();
    assert!(prepared.args.is_empty());
    let response = fixture
        .call(
            serde_json::from_value(json!({
                "method":"register","params":{
                    "invocation_id":prepared.id, "request":{"app_id":"demo","kind":"mcp"},
                },
            }))
            .unwrap(),
        )
        .await;
    assert!(response.ok, "{response:?}");
    let mut registration = response.result.unwrap();
    let response = fixture.bind(&registration).await;
    assert!(response.ok, "{response:?}");
    registration["relay_handle"] = response.result.unwrap()["relay_handle"].clone();
    (prepared, registration)
}

async fn start(fixture: &Fixture, registration: &Value, path: &str) -> Response {
    fixture
        .call(
            serde_json::from_value(json!({
                "method":"start_call","params":{
                    "session_id":registration["session_id"],"handle":registration["handle"],
                    "call":{"tool":"demo.read","args":{"path":path}},
                },
            }))
            .unwrap(),
        )
        .await
}

async fn end(fixture: &Fixture, registration: &Value, call_id: &str) -> Response {
    fixture.call(serde_json::from_value(json!({
        "method":"end_call","params":{
            "call_id":call_id,"request":{
                "session_id":registration["session_id"],"handle":registration["handle"],"call":null,
            },
        },
    })).unwrap()).await
}

fn grant(fixture: &Fixture, registration: &Value) -> GrantView {
    authority()
        .resolve_session(
            registration["session_id"].as_str().unwrap(),
            &Presentation::new(
                fixture.child.uid,
                fixture.child.pid,
                fixture.child.start_time_ticks,
                Audience::SystemService,
                "test.stateful",
            ),
        )
        .unwrap()
}

async fn checked_caps(fixture: &Fixture, registration: &Value) -> CapSet {
    let response = fixture
        .call(
            serde_json::from_value(json!({
                "method":"check","params":{
                    "session_id":registration["session_id"],
                    "package_digest":fixture.original.package_digest,
                },
            }))
            .unwrap(),
        )
        .await;
    assert!(response.ok, "{response:?}");
    serde_json::from_value(response.result.unwrap()["caps"].clone()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires root with an isolated /run tmpfs; see App host MODULE.md"]
async fn stable_aliases_rotate_exact_call_grants_and_delayed_end_cannot_clear_a_later_call() {
    require_private_root();
    let fixture = Fixture::new();
    let before = authority().len();
    let (prepared, registration) = open(&fixture).await;
    let resting = grant(&fixture, &registration);
    assert_eq!(resting.caps.len(), 1);
    assert_eq!(checked_caps(&fixture, &registration).await, resting.caps);
    let alias = registration["handle"].as_str().unwrap();
    assert!(
        authority()
            .resolve(
                alias,
                &Presentation::new(
                    fixture.child.uid,
                    fixture.process.id(),
                    fixture.host.client.start_time_ticks,
                    Audience::AppLaunch,
                    "test.alias",
                )
            )
            .is_err(),
        "task aliases must never be real grant handles"
    );

    let response = start(&fixture, &registration, "input.txt").await;
    assert!(response.ok, "{response:?}");
    let first: PreparedSessionCall = serde_json::from_value(response.result.unwrap()).unwrap();
    let input = fixture.harness.data_dir().join("workspace/input.txt");
    assert_eq!(first.args["path"], input.to_str().unwrap());
    let first_grant = grant(&fixture, &registration);
    assert_ne!(first_grant.id, resting.id);
    assert_eq!(first_grant.caps.len(), 2);
    assert!(first_grant.caps.covers(&Cap::new(
        Verb::FS_READ,
        Scope::path(input.to_string_lossy())
    )));
    assert!(first_grant.expires_in <= Duration::from_secs(75));
    assert_eq!(
        checked_caps(&fixture, &registration).await,
        first_grant.caps
    );
    assert!(end(&fixture, &registration, &first.id).await.ok);
    assert!(end(&fixture, &registration, &first.id).await.ok);
    let cleared = grant(&fixture, &registration);
    assert_eq!(cleared.caps.len(), 1);
    assert!(
        cleared.expires_in >= resting.expires_in.saturating_sub(Duration::from_secs(75)),
        "clearing a short call must retain the original idle-session lifetime"
    );
    let idle_relay = fixture
        .call(
            serde_json::from_value(json!({
                "method":"relay","params":{
                    "session_id":registration["session_id"],"handle":registration["relay_handle"],
                    "command":"system.file.replace","params":{},
                },
            }))
            .unwrap(),
        )
        .await;
    assert!(!idle_relay.ok);

    let response = start(&fixture, &registration, "second.txt").await;
    assert!(response.ok, "{response:?}");
    let second: PreparedSessionCall = serde_json::from_value(response.result.unwrap()).unwrap();
    assert_ne!(first.id, second.id);
    let active = grant(&fixture, &registration);
    assert!(!active.caps.covers(&Cap::new(
        Verb::FS_READ,
        Scope::path(input.to_string_lossy())
    )));
    assert!(!end(&fixture, &registration, &first.id).await.ok);
    assert_eq!(grant(&fixture, &registration).id, active.id);
    assert!(!start(&fixture, &registration, "input.txt").await.ok);
    assert_eq!(grant(&fixture, &registration).id, active.id);
    assert_eq!(checked_caps(&fixture, &registration).await, active.caps);
    assert!(end(&fixture, &registration, &second.id).await.ok);

    let response = fixture
        .call(
            serde_json::from_value(json!({
                "method":"end","params":{"invocation_id":prepared.id},
            }))
            .unwrap(),
        )
        .await;
    assert!(response.ok, "{response:?}");
    assert!(!fixture.child.still_matches());
    assert!(fixture.host.sessions.lock().await.active.is_empty());
    assert_eq!(
        authority().len(),
        before,
        "retirement must revoke launch parents as well as App children"
    );
    fixture.host.close().unwrap().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires root with an isolated /run tmpfs; see App host MODULE.md"]
async fn task_and_alias_mismatches_do_not_touch_the_owned_active_call() {
    require_private_root();
    let fixture = Fixture::new();
    let (_, registration) = open(&fixture).await;
    let started = start(&fixture, &registration, "input.txt").await;
    assert!(started.ok, "{started:?}");
    let call: PreparedSessionCall = serde_json::from_value(started.result.unwrap()).unwrap();
    let active = grant(&fixture, &registration);
    let mut foreign = registration.clone();
    foreign["handle"] = json!(uuid::Uuid::new_v4().simple().to_string());
    assert!(!end(&fixture, &foreign, &call.id).await.ok);
    let response = fixture
        .host
        .handle(AppHostRequest {
            task_id: "another-task".into(),
            correlation_id: 99,
            request_id: RequestId::generate(),
            call: serde_json::from_value(json!({
                "method":"end_call","params":{
                    "call_id":call.id,"request":{
                        "session_id":registration["session_id"],"handle":registration["handle"],
                    },
                },
            }))
            .unwrap(),
        })
        .await;
    assert!(!response.ok);
    assert_eq!(grant(&fixture, &registration).id, active.id);
    assert!(end(&fixture, &registration, &call.id).await.ok);
    fixture.host.close().unwrap().await.unwrap();
}
