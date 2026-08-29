use super::*;
use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
use crate::config::AgentConfig;

fn parent_cfg() -> AgentConfig {
    AgentConfig {
        provider: "mock".into(),
        model: "mock-model".into(),
        max_turns: 5,
        max_tokens: 1024,
        temperature: 0.0,
        system_prompt_path: None,
        ..Default::default()
    }
}

fn user_msg(text: &str) -> Message {
    Message::user_text(text)
}

fn assistant_msg(text: &str) -> Message {
    Message::assistant_text(text)
}

fn tool_use_msg(id: &str, name: &str, input: serde_json::Value) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: id.to_string(),
            name: name.to_string(),
            input,
        }],
    }
}

fn tool_result_msg(tool_use_id: &str, content: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: tool_use_id.to_string(),
            is_error: false,
            content: content.to_string(),
        }],
    }
}

fn make_compressor(cfg: CompressorConfig) -> LlmCompressor {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new("mock-model", &parent_cfg()));
    LlmCompressor::new(provider, "mock-model").with_config(cfg)
}

#[test]
fn estimate_text_tokens_rounds_up() {
    assert_eq!(estimate_text_tokens(""), 0);
    assert_eq!(estimate_text_tokens("a"), 1);
    assert_eq!(estimate_text_tokens("abcd"), 1);
    assert_eq!(estimate_text_tokens("abcde"), 2);
}

#[test]
fn estimate_message_includes_overhead() {
    let m = user_msg("hi");
    // text "hi" -> 1 token + 4 overhead == 5.
    assert_eq!(estimate_message_tokens(&m), 5);
}

#[test]
fn estimate_total_sums_system_and_messages() {
    let messages = vec![user_msg("hello"), assistant_msg("world")];
    // "hello" -> 2, "world" -> 2, each with 4 overhead = 12 messages
    // + system "sys" -> 1 == 13.
    let total = estimate_total_tokens(Some("sys"), &messages);
    assert_eq!(total, 13);
}

#[test]
fn estimate_tools_counts_name_description_schema_and_framing() {
    let tools = vec![LlmTool {
        name: "lookup".into(),
        description: "find a record".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"query": {"type": "string"}}
        }),
    }];

    let estimated = estimate_tools_tokens(&tools);

    assert!(estimated > 8);
    assert!(estimated >= estimate_text_tokens("lookup"));
    assert!(estimated >= estimate_text_tokens("find a record"));
}

#[test]
fn should_compress_false_when_below_trigger() {
    let mut cfg = CompressorConfig::default();
    cfg.trigger_tokens = 10_000;
    let c = make_compressor(cfg);
    let msgs = vec![
        user_msg("short"),
        assistant_msg("short"),
        user_msg("short"),
        assistant_msg("short"),
    ];
    assert!(!c.should_compress(None, &msgs));
}

#[test]
fn should_compress_false_when_too_few_messages() {
    let cfg = CompressorConfig {
        trigger_tokens: 1, // any non-zero estimated total triggers
        ..Default::default()
    };
    let c = make_compressor(cfg);
    // Only 2 messages — must not trigger even with a trivially low
    // threshold.
    let msgs = vec![user_msg("hello"), assistant_msg("world")];
    assert!(!c.should_compress(None, &msgs));
}

#[test]
fn should_compress_true_above_trigger_with_enough_messages() {
    let cfg = CompressorConfig {
        trigger_tokens: 10,
        ..Default::default()
    };
    let c = make_compressor(cfg);
    let msgs = vec![
        user_msg("hello"),
        assistant_msg("world"),
        user_msg("hello"),
        assistant_msg("world"),
    ];
    assert!(c.should_compress(None, &msgs));
}

#[test]
fn raw_boundary_keeps_tail_within_budget() {
    let cfg = CompressorConfig {
        keep_tail_tokens: 10,
        ..Default::default()
    };
    let c = make_compressor(cfg);
    // Each message ~5 tokens. Tail budget = 10 → fits two messages
    // (10 tokens), boundary at index 4.
    let msgs: Vec<Message> = (0..6).map(|i| user_msg(&format!("m{i}"))).collect();
    let b = c.raw_boundary(&msgs);
    assert_eq!(b, 4);
    assert_eq!(msgs.len() - b, 2);
}

#[test]
fn raw_boundary_returns_zero_when_nothing_fits_in_budget() {
    // Budget so tiny that even one message exceeds it. Boundary
    // walks back to len()-1 to keep at least the last message.
    let cfg = CompressorConfig {
        keep_tail_tokens: 0,
        ..Default::default()
    };
    let c = make_compressor(cfg);
    let msgs = vec![user_msg("a"), assistant_msg("b"), user_msg("c")];
    let b = c.raw_boundary(&msgs);
    // First iteration always accepts the last message regardless of
    // budget (so tail is never empty).
    assert_eq!(b, 2);
}

#[test]
fn adjust_for_tool_pairs_keeps_pair_together_in_tail() {
    // Sequence: user, assistant(tool_use:t1), tool_result(t1), user.
    // Raw boundary lands at index 2 (tool_result alone in tail).
    // Adjust must move boundary back to 1 so the matching tool_use
    // also lives in the tail.
    let msgs = vec![
        user_msg("intro"),
        tool_use_msg("t1", "echo", serde_json::json!({"text": "hi"})),
        tool_result_msg("t1", "hi"),
        user_msg("ok"),
    ];
    let adjusted = LlmCompressor::adjust_for_tool_pairs(&msgs, 2);
    assert_eq!(adjusted, 1);
}

#[test]
fn adjust_for_tool_pairs_no_change_when_pairs_already_aligned() {
    // user, assistant(tool_use:t1), tool_result(t1), user.
    // Raw boundary at 3 — tail is just [user]. Nothing to adjust.
    let msgs = vec![
        user_msg("intro"),
        tool_use_msg("t1", "echo", serde_json::json!({"text": "hi"})),
        tool_result_msg("t1", "hi"),
        user_msg("ok"),
    ];
    let adjusted = LlmCompressor::adjust_for_tool_pairs(&msgs, 3);
    assert_eq!(adjusted, 3);
}

#[test]
fn adjust_for_tool_pairs_orphan_result_at_boundary_pulls_use_in() {
    // boundary lands ON a tool_result whose use is in the head.
    let msgs = vec![
        user_msg("intro"),
        tool_use_msg("t1", "echo", serde_json::json!({"text": "hi"})),
        tool_result_msg("t1", "hi"),
    ];
    // raw boundary index 2 == the tool_result. Tail is just the
    // tool_result with no matching tool_use in the tail. Adjust
    // walks back to include the tool_use at index 1.
    let adjusted = LlmCompressor::adjust_for_tool_pairs(&msgs, 2);
    assert_eq!(adjusted, 1);
}

#[test]
fn adjust_for_flattened_tool_pairs_pulls_call_into_tail() {
    let msgs = vec![
        user_msg("intro"),
        assistant_msg("[tool: lookup]"),
        user_msg("[tool result]\nlarge output"),
        user_msg("continue"),
    ];
    let adjusted = LlmCompressor::adjust_for_tool_pairs(&msgs, 2);
    assert_eq!(adjusted, 1);
}

#[test]
fn protected_tail_keeps_a_real_user_anchor() {
    let msgs = vec![
        user_msg("original goal"),
        assistant_msg("working"),
        assistant_msg("more work"),
        assistant_msg("latest status"),
    ];
    let (boundary, user_index) = LlmCompressor::protect_user_anchor(&msgs, 3);
    assert_eq!(boundary, 0);
    assert_eq!(user_index, 0);
}

#[test]
fn render_transcript_marks_roles_and_tool_calls() {
    let msgs = vec![
        user_msg("hello"),
        tool_use_msg("t1", "echo", serde_json::json!({"text": "hi"})),
        tool_result_msg("t1", "hi"),
    ];
    let s = LlmCompressor::render_transcript(&msgs);
    assert!(s.contains("(user)"));
    assert!(s.contains("(assistant)"));
    assert!(s.contains("<tool_call:echo"));
    assert!(s.contains("<tool_result"));
}

#[test]
fn make_summary_message_contains_marker_and_count() {
    let m = LlmCompressor::make_summary_message("the gist", 7);
    match &m.content[0] {
        ContentBlock::Text { text } => {
            assert!(text.contains(SUMMARY_MARKER));
            assert!(text.contains("7 prior messages"));
            assert!(text.contains("the gist"));
        }
        _ => panic!("expected text block"),
    }
}

#[tokio::test]
async fn compress_below_trigger_returns_unchanged() {
    let cfg = CompressorConfig {
        trigger_tokens: 1_000_000,
        ..Default::default()
    };
    let c = make_compressor(cfg);
    let msgs = vec![
        user_msg("a"),
        assistant_msg("b"),
        user_msg("c"),
        assistant_msg("d"),
    ];
    let after = c.compress(None, msgs.clone()).await;
    assert_eq!(after.len(), msgs.len());
}

#[tokio::test]
async fn compress_with_mock_provider_inserts_summary_before_tail() {
    let cfg = CompressorConfig {
        trigger_tokens: 5,
        keep_tail_tokens: 12,
        summary_max_tokens: 256,
        ..Default::default()
    };
    let provider_arc: Arc<MockProvider> =
        Arc::new(MockProvider::new("mock-model", &parent_cfg()));
    provider_arc.push_response(MockResponse::Text("compressed history goes here".into()));
    let provider: Arc<dyn Provider> = provider_arc.clone();
    let c = LlmCompressor::new(provider, "mock-model").with_config(cfg);
    let msgs: Vec<Message> = (0..8).map(|i| user_msg(&format!("msg-{i}"))).collect();
    let original_len = msgs.len();
    let after = c.compress(None, msgs).await;
    assert!(after.len() < original_len);
    // First message should be the synthesised summary.
    match &after[0].content[0] {
        ContentBlock::Text { text } => {
            assert!(text.contains(SUMMARY_MARKER));
            assert!(text.contains("compressed history"));
        }
        _ => panic!("expected text block"),
    }
    // Tail messages remain.
    let tail_text = match &after[1].content[0] {
        ContentBlock::Text { text } => text.clone(),
        _ => panic!(),
    };
    assert!(tail_text.starts_with("msg-"));
}

#[tokio::test]
async fn compress_with_empty_provider_summary_preserves_history() {
    // Mock provider has no responses queued; chat() returns a
    // configurable empty error response — but our mock's default
    // behaviour is to echo. We force a failure by pushing an
    // error-shaped Text response that's blank (extract_text
    // returns "") — which the compressor treats as "empty
    // summary, fall back to truncate-only".
    let cfg = CompressorConfig {
        trigger_tokens: 5,
        keep_tail_tokens: 12,
        ..Default::default()
    };
    let provider_arc: Arc<MockProvider> =
        Arc::new(MockProvider::new("mock-model", &parent_cfg()));
    provider_arc.push_response(MockResponse::Text(String::new()));
    let provider: Arc<dyn Provider> = provider_arc.clone();
    let c = LlmCompressor::new(provider, "mock-model").with_config(cfg);
    let msgs: Vec<Message> = (0..8).map(|i| user_msg(&format!("msg-{i}"))).collect();
    let after = c.compress(None, msgs.clone()).await;
    assert_eq!(
        after.len(),
        msgs.len(),
        "failed summarization must preserve the original history"
    );
    assert!(after.iter().all(|message| message.content.iter().all(
        |block| !matches!(block, ContentBlock::Text { text } if text.contains(SUMMARY_MARKER))
    )));
}

#[tokio::test]
async fn deterministic_tool_pruning_can_avoid_provider_summary() {
    let cfg = CompressorConfig {
        trigger_tokens: 500,
        keep_tail_tokens: 10,
        ..Default::default()
    };
    let provider = Arc::new(MockProvider::new("mock-model", &parent_cfg()));
    let c = LlmCompressor::new(provider.clone(), "mock-model").with_config(cfg);
    let msgs = vec![
        user_msg("inspect the service"),
        tool_use_msg("t1", "logs", serde_json::json!({})),
        tool_result_msg("t1", &"x".repeat(5000)),
        assistant_msg("the command completed"),
        user_msg("what happened?"),
    ];

    let plan = c.prepare(None, msgs).expect("compression plan");
    assert_eq!(plan.algorithm(), DETERMINISTIC_PRUNE_ALGORITHM);
    assert_eq!(plan.pruned_tool_results(), 1);
    let execution = c.execute(plan).await;

    assert!(execution.failure.is_none());
    assert!(provider.last_request().is_none(), "no LLM call is needed");
    let projection = execution.projection.expect("durable projection");
    assert_eq!(projection.algorithm, DETERMINISTIC_PRUNE_ALGORITHM);
    assert!(projection.summary_text.contains("sha256="));
}

#[tokio::test]
async fn compress_preserves_tool_use_result_pair_in_tail() {
    // Construct a long history where a tool_use lands right at the
    // raw boundary. After compression the surviving tail must
    // start with the tool_use, not the tool_result.
    let cfg = CompressorConfig {
        trigger_tokens: 5,
        keep_tail_tokens: 30,
        summary_max_tokens: 256,
        ..Default::default()
    };
    let provider_arc: Arc<MockProvider> =
        Arc::new(MockProvider::new("mock-model", &parent_cfg()));
    provider_arc.push_response(MockResponse::Text("summary".into()));
    let provider: Arc<dyn Provider> = provider_arc;
    let c = LlmCompressor::new(provider, "mock-model").with_config(cfg);

    // 6 user msgs, then tool_use + tool_result + final user.
    let mut msgs: Vec<Message> = (0..6).map(|i| user_msg(&format!("m{i}"))).collect();
    msgs.push(tool_use_msg(
        "tx",
        "echo",
        serde_json::json!({"text": "ping"}),
    ));
    msgs.push(tool_result_msg("tx", "ping"));
    msgs.push(user_msg("done"));

    let after = c.compress(None, msgs).await;
    // Verify: there is no tool_result whose tool_use isn't in the
    // post-summary list (i.e., no orphaned tool_result).
    let all_use_ids: std::collections::HashSet<&str> = after
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    for m in &after {
        for b in &m.content {
            if let ContentBlock::ToolResult { tool_use_id, .. } = b {
                assert!(
                    all_use_ids.contains(tool_use_id.as_str()),
                    "found tool_result for {tool_use_id} without matching tool_use in compressed list"
                );
            }
        }
    }
}

#[tokio::test]
async fn noop_compressor_never_compresses() {
    let c = NoopCompressor;
    let msgs = vec![user_msg("a"); 100];
    assert!(!c.should_compress(None, &msgs));
    let after = c.compress(None, msgs.clone()).await;
    assert_eq!(after.len(), msgs.len());
}

#[test]
fn config_defaults_are_consistent() {
    let cfg = CompressorConfig::default();
    assert!(cfg.trigger_tokens < cfg.target_tokens);
    assert!(cfg.keep_tail_tokens < cfg.target_tokens);
}

#[tokio::test]
async fn compress_does_not_crash_on_empty_messages() {
    let cfg = CompressorConfig::default();
    let c = make_compressor(cfg);
    let after = c.compress(None, Vec::new()).await;
    assert!(after.is_empty());
}

#[test]
fn estimate_image_block_charges_baseline_overhead() {
    let m = Message {
        role: Role::User,
        content: vec![ContentBlock::Image {
            media_type: "image/png".into(),
            data: "x".repeat(1000),
        }],
    };
    // Should be at least the 256 baseline + 4 message overhead.
    let t = estimate_message_tokens(&m);
    assert!(t >= 260);
}
