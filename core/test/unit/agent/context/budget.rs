use super::*;

#[test]
fn unknown_model_uses_the_configured_budget_without_inventing_a_window() {
    let config = AgentConfig {
        model: "unlisted-local-model".into(),
        compress_target_tokens: 12_000,
        ..Default::default()
    };
    let budget = ContextBudget::from_config(&config).unwrap();
    assert_eq!(budget.input_tokens, 12_000);
    assert_eq!(budget.model_window_tokens, None);
}

#[test]
fn smaller_fallback_window_and_response_reserve_bound_the_request() {
    let config = AgentConfig {
        model: "claude-sonnet-4-5".into(),
        compress_target_tokens: 200_000,
        max_tokens: 2048,
        provider_fallbacks: vec![serde_json::from_value(serde_json::json!({
            "provider": "deepseek", "model": "deepseek-chat"
        }))
        .unwrap()],
        ..Default::default()
    };
    let budget = ContextBudget::from_config(&config).unwrap();
    assert_eq!(budget.model_window_tokens, Some(64_000));
    assert_eq!(budget.input_tokens, 64_000 - 2048 - 1024);
}

#[test]
fn zero_budget_and_exhausted_model_window_are_errors() {
    let mut config = AgentConfig {
        compress_target_tokens: 0,
        ..Default::default()
    };
    assert!(ContextBudget::from_config(&config).is_err());
    config.compress_target_tokens = 80_000;
    config.model = "deepseek-chat".into();
    config.max_tokens = 64_000;
    assert!(ContextBudget::from_config(&config).is_err());
}

#[test]
fn multilingual_estimate_does_not_apply_the_english_discount_to_cjk() {
    assert_eq!(estimate_text_tokens("abcd"), 1);
    assert_eq!(estimate_text_tokens("你好"), 6);
    assert_eq!(estimate_text_tokens("abcd你好"), 7);
    assert!(estimate_text_tokens("🙂") >= 1);
}

#[test]
fn excerpts_include_the_marker_in_the_exact_estimated_budget() {
    let text = "Mixed 中文🙂 and ASCII text. ".repeat(100);
    for limit in [0, 1, 12, 20, 50, 100, 500] {
        if let Some(result) = excerpt(&text, limit) {
            assert!(estimate_text_tokens(&result) <= limit);
            assert!(result.contains("read the source"));
            assert!(text.starts_with(result.split("\n[...").next().unwrap()));
        }
    }
    assert_eq!(excerpt("small", 2).as_deref(), Some("small"));
}

#[test]
fn tool_definitions_consume_the_same_input_budget_as_messages() {
    let budget = ContextBudget {
        input_tokens: 100,
        memory_tokens: 10,
        model_window_tokens: None,
        output_tokens: 100,
    };
    let messages = vec![Message::user_text("hello")];
    assert!(budget.check("test", "system", &messages, &[]).is_ok());
    let tools = vec![Tool {
        name: "read".into(),
        description: "d".repeat(1000),
        input_schema: serde_json::json!({"type": "object"}),
    }];
    assert!(matches!(
        budget.check("test", "system", &messages, &tools),
        Err(ContextError::BudgetExceeded { .. })
    ));
}
