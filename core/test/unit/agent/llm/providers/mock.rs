use super::*;
use crate::agent::llm::Message;

fn make() -> MockProvider {
    MockProvider::new("mock-model", &AgentConfig::default())
}

fn req(text: &str) -> ChatRequest {
    ChatRequest {
        model: "mock-model".into(),
        messages: vec![Message::user_text(text)],
        system: None,
        tools: vec![],
        tool_choice: Default::default(),
        max_tokens: None,
        temperature: None,
        top_p: None,
        stop_sequences: vec![],
        extra: serde_json::Value::Null,
    }
}

#[tokio::test]
async fn echoes_last_user_message_by_default() {
    let p = make();
    let resp = p.chat(req("hello world")).await.unwrap();
    let text = match &resp.content[0] {
        ContentBlock::Text { text } => text.clone(),
        _ => panic!("expected text block"),
    };
    assert!(text.contains("hello world"));
    assert_eq!(resp.finish_reason, FinishReason::Stop);
}

#[tokio::test]
async fn scripted_tool_use_then_text() {
    let p = make();
    p.push_response(MockResponse::ToolUse(vec![ToolCall {
        id: "call_1".into(),
        name: "echo".into(),
        input: serde_json::json!({"text": "hi"}),
    }]));
    p.push_response(MockResponse::Text("done".into()));

    let r1 = p.chat(req("call a tool")).await.unwrap();
    assert_eq!(r1.finish_reason, FinishReason::ToolUse);
    assert_eq!(r1.tool_calls.len(), 1);

    let r2 = p.chat(req("after tool")).await.unwrap();
    assert_eq!(r2.finish_reason, FinishReason::Stop);
}
