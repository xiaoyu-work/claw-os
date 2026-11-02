use super::*;
use clawd_client::system_review::PermissionChoice;

fn caller(token: &str) -> AuthenticatedToken {
    AuthenticatedToken {
        uid: 1000,
        token_id: token.into(),
        expires_at: u64::MAX,
    }
}

fn session() -> Arc<Session> {
    let (cancel, _) = watch::channel(false);
    Arc::new(Session {
        owner_uid: 1000,
        token_id: "browser-one".into(),
        origin: "http://127.0.0.1:7878".into(),
        presentation: Mutex::new(Presentation {
            snapshot: Snapshot {
                id: nonce().unwrap(),
                review_id: "approval-one".into(),
                revision: "7".into(),
                label: "Read system status".into(),
                target: "environment".into(),
                choice: PermissionChoice::AllowSession,
                phase: Phase::Starting,
                prompt: None,
                message: None,
            },
            response: None,
        }),
        cancel,
    })
}

#[test]
fn authorization_is_bound_to_the_owner_and_browser_token() {
    let session = session();
    let id = session.snapshot().unwrap().id;
    let controller = Controller {
        current: Mutex::new(Some(session)),
    };
    assert!(controller.status(&id, &caller("browser-one")).is_ok());
    assert!(controller.status(&id, &caller("browser-two")).is_err());
    let mut foreign = caller("browser-one");
    foreign.uid = 1001;
    assert!(controller.status(&id, &foreign).is_err());
    assert!(controller
        .status("different", &caller("browser-one"))
        .is_err());
    assert!(controller
        .cancel(&id, &caller("browser-one"), "https://different.example")
        .is_err());
}

#[tokio::test]
async fn os_responses_are_single_use_and_never_part_of_a_snapshot() {
    let session = session();
    let prompt_session = session.clone();
    let pending = tokio::spawn(async move { prompt_session.prompt("Password:".into()).await });
    tokio::task::yield_now().await;
    let prompt_id = session.snapshot().unwrap().prompt.unwrap().id;
    assert!(session
        .respond("stale", Zeroizing::new("not-sent".into()))
        .is_err());
    let secret = "synthetic-pam-response";
    session
        .respond(&prompt_id, Zeroizing::new(secret.into()))
        .unwrap();
    assert!(session
        .respond(&prompt_id, Zeroizing::new("replay".into()))
        .is_err());
    assert_eq!(pending.await.unwrap().unwrap().as_str(), secret);
    let snapshot = serde_json::to_string(&session.snapshot().unwrap()).unwrap();
    assert!(!snapshot.contains(secret));
    assert!(!snapshot.contains("response"));
    assert!(session.snapshot().unwrap().prompt.is_none());
}

#[tokio::test]
async fn cancellation_and_invalid_input_cannot_answer_a_prompt() {
    let session = session();
    let prompt_session = session.clone();
    let pending = tokio::spawn(async move { prompt_session.prompt("Password:".into()).await });
    tokio::task::yield_now().await;
    let prompt_id = session.snapshot().unwrap().prompt.unwrap().id;
    assert!(session
        .respond(&prompt_id, Zeroizing::new("first\nsecond".into()))
        .is_err());
    assert!(session
        .respond(
            &prompt_id,
            Zeroizing::new("x".repeat(MAX_RESPONSE_BYTES + 1))
        )
        .is_err());
    session.cancel.send_replace(true);
    assert!(pending.await.unwrap().is_err());
    assert!(session
        .respond(&prompt_id, Zeroizing::new("late".into()))
        .is_err());
}
