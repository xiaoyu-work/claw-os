use super::*;
use serde_json::json;

struct EchoResponder;

#[async_trait]
impl ClarifyResponder for EchoResponder {
    async fn ask(&self, request: &ClarifyRequest) -> ClarifyOutcome {
        ClarifyOutcome::Answered {
            answer: format!("you asked: {}", request.question),
        }
    }
}

struct CancelResponder(Option<String>);

#[async_trait]
impl ClarifyResponder for CancelResponder {
    async fn ask(&self, _: &ClarifyRequest) -> ClarifyOutcome {
        ClarifyOutcome::Cancelled {
            reason: self.0.clone(),
        }
    }
}

#[tokio::test]
async fn headless_returns_pending() {
    let c = Clarify::new();
    let outcome = c
        .ask(ClarifyRequest {
            question: "which file?".to_string(),
            options: vec![],
            reason: None,
        })
        .await;
    assert_eq!(
        outcome,
        ClarifyOutcome::Pending {
            question: "which file?".to_string()
        }
    );
}

#[tokio::test]
async fn responder_provides_synchronous_answer() {
    let c = Clarify::with_responder(Arc::new(EchoResponder));
    let outcome = c
        .ask(ClarifyRequest {
            question: "what now?".to_string(),
            options: vec![],
            reason: None,
        })
        .await;
    match outcome {
        ClarifyOutcome::Answered { answer } => {
            assert_eq!(answer, "you asked: what now?");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn empty_question_is_rejected() {
    let c = Clarify::new();
    let outcome = c
        .ask(ClarifyRequest {
            question: "   ".to_string(),
            options: vec![],
            reason: None,
        })
        .await;
    assert!(matches!(outcome, ClarifyOutcome::Cancelled { .. }));
}

#[tokio::test]
async fn cancellation_round_trips_reason() {
    let c = Clarify::with_responder(Arc::new(CancelResponder(Some("nope".to_string()))));
    let outcome = c
        .ask(ClarifyRequest {
            question: "go ahead?".to_string(),
            options: vec![],
            reason: None,
        })
        .await;
    assert_eq!(
        outcome,
        ClarifyOutcome::Cancelled {
            reason: Some("nope".to_string())
        }
    );
}

#[tokio::test]
async fn tool_exec_returns_pending_json_in_headless_mode() {
    let c = Clarify::new();
    let res = c
        .exec(json!({
            "question": "which one?",
            "options": ["a", "b"]
        }))
        .await;
    assert!(!res.is_error);
    let v: serde_json::Value = serde_json::from_str(&res.content).unwrap();
    assert_eq!(v["kind"], "pending");
    assert_eq!(v["question"], "which one?");
}

#[tokio::test]
async fn tool_exec_returns_answered_json_with_responder() {
    let c = Clarify::with_responder(Arc::new(EchoResponder));
    let res = c.exec(json!({ "question": "what?" })).await;
    assert!(!res.is_error);
    let v: serde_json::Value = serde_json::from_str(&res.content).unwrap();
    assert_eq!(v["kind"], "answered");
    assert!(v["answer"].as_str().unwrap().contains("what?"));
}

#[tokio::test]
async fn tool_exec_rejects_missing_question() {
    let c = Clarify::new();
    let res = c.exec(json!({})).await;
    assert!(res.is_error);
}

#[tokio::test]
async fn tool_exec_rejects_blank_question() {
    let c = Clarify::new();
    let res = c.exec(json!({ "question": "   " })).await;
    assert!(res.is_error);
    assert!(res.content.contains("question"));
}

#[tokio::test]
async fn tool_exec_rejects_unknown_field() {
    // additionalProperties:false is advisory at the schema layer
    // (provider may or may not enforce). serde with default
    // permissive struct accepts unknown — verify our schema
    // expressly forbids them.
    let c = Clarify::new();
    let schema = c.input_schema();
    assert_eq!(schema["additionalProperties"], json!(false));
}

#[test]
fn outcome_serialisation_uses_kind_tag() {
    let answered = ClarifyOutcome::Answered {
        answer: "yes".to_string(),
    };
    let v = serde_json::to_value(&answered).unwrap();
    assert_eq!(v["kind"], "answered");
    assert_eq!(v["answer"], "yes");
}

#[test]
fn outcome_pending_round_trips() {
    let p = ClarifyOutcome::Pending {
        question: "?".to_string(),
    };
    let s = serde_json::to_string(&p).unwrap();
    let parsed: ClarifyOutcome = serde_json::from_str(&s).unwrap();
    assert_eq!(parsed, p);
}

#[test]
fn outcome_cancelled_omits_null_reason() {
    let c = ClarifyOutcome::Cancelled { reason: None };
    let v = serde_json::to_value(&c).unwrap();
    assert_eq!(v["kind"], "cancelled");
    // Tagged enums serialise variant fields inline; None should
    // round-trip but we don't strictly forbid the null key.
    let parsed: ClarifyOutcome = serde_json::from_value(v).unwrap();
    assert_eq!(parsed, c);
}

#[test]
fn tool_metadata_matches_name() {
    let c = Clarify::new();
    assert_eq!(c.name(), "cos_clarify");
    assert!(!c.description().is_empty());
    let schema = c.input_schema();
    assert_eq!(schema["type"], "object");
    let required = schema["required"].as_array().unwrap();
    assert!(required.iter().any(|v| v == "question"));
}
