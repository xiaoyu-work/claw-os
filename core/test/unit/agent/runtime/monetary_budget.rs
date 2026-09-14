use super::*;

#[test]
fn serialized_utf8_bytes_are_a_conservative_token_upper_bound() {
    let request = ChatRequest {
        model: "m".into(),
        messages: vec![],
        system: Some("é".into()),
        tools: vec![],
        tool_choice: crate::agent::llm::ToolChoice::None,
        max_tokens: Some(1),
        temperature: None,
        top_p: None,
        stop_sequences: vec![],
        extra: serde_json::Value::Null,
    };
    let bound = serialized_input_upper_bound(&request).unwrap();
    assert!(bound >= 2);
}
