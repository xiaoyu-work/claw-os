use super::*;
use crate::agent::run;

#[test]
fn interactive_chat_hides_evidence_warnings() {
    use crate::agent::runtime::evidence::EvidenceStatus;

    assert!(!should_render_evidence_warning(
        true,
        &EvidenceStatus::Missing
    ));
    assert!(should_render_evidence_warning(
        false,
        &EvidenceStatus::Missing
    ));
    assert!(!should_render_evidence_warning(
        false,
        &EvidenceStatus::Verified
    ));
}

#[test]
fn terminal_tool_line_has_no_surrounding_blank_lines() {
    let mut state = TerminalOutputState::default();
    let mut out = Vec::new();

    state.write_line(&mut out, "[tool: cos_sysinfo]");
    state.finish_line(&mut out);
    state.write_text(&mut out, "Ubuntu 26.04");
    state.finish_line(&mut out);

    assert_eq!(
        String::from_utf8(out).unwrap(),
        "[tool: cos_sysinfo]\nUbuntu 26.04\n"
    );
}

#[test]
fn terminal_tool_line_separates_unfinished_text_once() {
    let mut state = TerminalOutputState::default();
    let mut out = Vec::new();

    state.write_text(&mut out, "Let me check");
    state.write_line(&mut out, "[tool: cos_sysinfo]");

    assert_eq!(
        String::from_utf8(out).unwrap(),
        "Let me check\n[tool: cos_sysinfo]\n"
    );
}

#[test]
fn terminal_heartbeat_line_finishes_before_tool_failure() {
    let mut state = TerminalOutputState::default();
    let mut out = Vec::new();

    state.write_line(&mut out, "[tool: cos_sysinfo]");
    state.write_text(&mut out, "...");
    state.finish_line(&mut out);
    state.write_line(&mut out, "[tool failed: cos_sysinfo]");

    assert_eq!(
        String::from_utf8(out).unwrap(),
        "[tool: cos_sysinfo]\n...\n[tool failed: cos_sysinfo]\n"
    );
}

#[test]
fn tool_failure_includes_elapsed_time_and_reason() {
    assert_eq!(
        format_tool_failure("cos_proc", 1_250, "session not found: status"),
        "[tool failed: cos_proc after 1.2s]\nsession not found: status"
    );
}

#[test]
fn terminal_heartbeat_line_finishes_before_next_prompt() {
    let mut state = TerminalOutputState::default();
    let mut out = Vec::new();

    state.write_line(&mut out, "[tool: cos_sysinfo]");
    state.write_text(&mut out, "..");
    state.finish_line(&mut out);
    out.extend_from_slice(b"you> ");

    assert_eq!(
        String::from_utf8(out).unwrap(),
        "[tool: cos_sysinfo]\n..\nyou> "
    );
}

// ---- stream / live async helpers ------------------------------------

/// Build a mock provider with a scripted text response and run
/// `stream_cmd_async` against it. Returns the JSON envelope.
fn run_stream_async(
    text: &str,
    cfg: &crate::config::AgentConfig,
    prompt: &str,
) -> serde_json::Value {
    use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
    let mock = MockProvider::new(&cfg.model, cfg);
    mock.push_response(MockResponse::Text(text.to_string()));
    let provider: std::sync::Arc<dyn llm::Provider> = std::sync::Arc::new(mock);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(stream_cmd_async(provider, cfg, prompt))
        .expect("stream ok")
}

#[test]
fn ask_rejects_empty_prompt() {
    let err = run("ask", &[]).unwrap_err();
    assert!(err.to_lowercase().contains("usage"), "got {err}");
    // Usage hint must document the remaining flag the handler accepts.
    assert!(
        err.contains("--full"),
        "usage hint should mention --full: {err}"
    );
    assert!(
        !err.contains("--stream"),
        "removed flag leaked into usage: {err}"
    );
    let err2 = run("ask", &["".into()]).unwrap_err();
    assert!(err2.to_lowercase().contains("usage"), "got {err2}");
}

#[test]
fn ask_flag_alone_is_not_treated_as_prompt() {
    // Regression: feeding only flags must surface the usage hint
    // rather than silently using a flag string ("--full") as the
    // prompt — which would route to clawd and either error
    // opaquely or actually consume LLM tokens.
    let err = run("ask", &["--full".into()]).unwrap_err();
    assert!(err.contains("usage:"), "got {err}");
}

#[test]
fn ask_session_requires_non_empty_id() {
    let err = run("ask", &["--session".into()]).unwrap_err();
    assert!(err.contains("--session"), "got {err}");
    let err = run("ask", &["--session".into(), "".into(), "hi".into()]).unwrap_err();
    assert!(err.contains("non-empty"), "got {err}");
}

#[test]
fn ask_timeout_requires_positive_integer() {
    for value in ["", "0", "nope"] {
        let err = run("ask", &["--timeout-secs".into(), value.into(), "hi".into()]).unwrap_err();
        assert!(err.contains("positive integer"), "got {err}");
    }
}

#[test]
fn stream_async_accumulates_text_and_returns_envelope() {
    let mut cfg = crate::config::AgentConfig::default();
    cfg.provider = "mock".into();
    cfg.model = "mock-model".into();
    let v = run_stream_async("hello world", &cfg, "say hi");
    assert_eq!(
        v.get("answer").and_then(|a| a.as_str()),
        Some("hello world")
    );
    assert_eq!(v.get("provider").and_then(|p| p.as_str()), Some("mock"));
    assert_eq!(v.get("model").and_then(|m| m.as_str()), Some("mock-model"));
    // mock's chat_stream emits Message + Done; finish_reason for
    // a plain text reply is FinishReason::Stop.
    assert_eq!(v.get("finish").and_then(|f| f.as_str()), Some("Stop"));
    assert!(v.get("tool_calls").unwrap().as_array().unwrap().is_empty());
}

#[test]
fn stream_async_surfaces_tool_calls() {
    use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
    use crate::agent::llm::types::ToolCall;
    let mut cfg = crate::config::AgentConfig::default();
    cfg.provider = "mock".into();
    cfg.model = "mock-model".into();
    let mock = MockProvider::new(&cfg.model, &cfg);
    mock.push_response(MockResponse::ToolUse(vec![ToolCall {
        id: "call_1".into(),
        name: "echo".into(),
        input: serde_json::json!({"text": "hi"}),
    }]));
    let provider: std::sync::Arc<dyn llm::Provider> = std::sync::Arc::new(mock);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let v = rt
        .block_on(stream_cmd_async(provider, &cfg, "use a tool"))
        .expect("stream ok");
    let calls = v.get("tool_calls").unwrap().as_array().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["id"], "call_1");
    assert_eq!(calls[0]["name"], "echo");
    // mock emits ToolUse via Message variant → finish ToolUse.
    assert_eq!(v.get("finish").and_then(|f| f.as_str()), Some("ToolUse"));
}

#[test]
fn stream_async_propagates_provider_error() {
    use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
    let mut cfg = crate::config::AgentConfig::default();
    cfg.provider = "mock".into();
    cfg.model = "mock-model".into();
    let mock = MockProvider::new(&cfg.model, &cfg);
    mock.push_response(MockResponse::Error(llm::LlmError::Auth));
    let provider: std::sync::Arc<dyn llm::Provider> = std::sync::Arc::new(mock);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let err = rt
        .block_on(stream_cmd_async(provider, &cfg, "hi"))
        .unwrap_err();
    assert!(
        err.contains("chat_stream") || err.contains("auth"),
        "want chat_stream/auth in err, got {err}"
    );
}

#[test]
fn stream_async_envelope_includes_usage_keys() {
    let mut cfg = crate::config::AgentConfig::default();
    cfg.provider = "mock".into();
    cfg.model = "mock-model".into();
    let v = run_stream_async("ok", &cfg, "ping");
    let usage = v.get("usage").unwrap();
    assert!(usage.get("input_tokens").is_some());
    assert!(usage.get("output_tokens").is_some());
    assert!(usage.get("cache_read_tokens").is_some());
    assert!(usage.get("cache_write_tokens").is_some());
}

async fn live_cmd_async(
    provider: std::sync::Arc<dyn llm::Provider>,
    cfg: &crate::config::AgentConfig,
    user_prompt: &str,
) -> Result<Value, String> {
    use crate::agent::llm::accumulate::StreamSink;
    use crate::agent::llm::types::StreamEvent;
    use std::collections::HashSet;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    let deps = crate::agent::tools::registry::RegistryDeps::load_current();
    let mut tools = crate::agent::tools::registry::default_registry_with_deps(&deps);
    let guardrails = runtime::loop_::guardrails_from_cfg(cfg);
    tools.set_guardrails(guardrails.clone());
    tools.set_approval(runtime::loop_::approval_from_cfg(cfg));
    let exposure = crate::agent::tools::exposure::ToolExposureContext::isolated(guardrails);
    let _mcp_handles = runtime::loop_::attach_mcp_servers_for_cli(&mut tools, cfg, &exposure).await;

    struct LiveSink {
        tool_calls: Mutex<Vec<serde_json::Value>>,
        announced_tools: Mutex<HashSet<String>>,
        warnings: Mutex<Vec<String>>,
        last_usage: Mutex<Option<crate::agent::llm::types::Usage>>,
        last_finish: Mutex<Option<crate::agent::llm::types::FinishReason>>,
        heartbeat: crate::agent::runtime::progress::Heartbeat,
    }

    impl LiveSink {
        fn announce_tool(&self, id: &str, name: &str, out: &mut impl Write) {
            let should_announce =
                id.is_empty() || mlock(&self.announced_tools).insert(id.to_string());
            if should_announce {
                let _ = writeln!(out, "\n[tool: {name}]");
            }
        }
    }

    impl StreamSink for LiveSink {
        fn on_event(&self, event: &StreamEvent) {
            let stderr = std::io::stderr();
            let mut err_lock = stderr.lock();
            match event {
                StreamEvent::TextDelta { text } => {
                    let _ = err_lock.write_all(text.as_bytes());
                    let _ = err_lock.flush();
                }
                StreamEvent::ToolUseStart { id, name } => {
                    self.announce_tool(id, name, &mut err_lock);
                }
                StreamEvent::ToolInputDelta { .. } => {}
                StreamEvent::ToolUse(call) => {
                    self.announce_tool(&call.id, &call.name, &mut err_lock);
                    mlock(&self.tool_calls).push(serde_json::json!({
                        "id": call.id,
                        "name": call.name,
                    }));
                }
                StreamEvent::Reasoning { .. } => {}
                StreamEvent::ToolState { .. } => {}
                StreamEvent::Message(resp) => {
                    for block in &resp.content {
                        if let crate::agent::llm::types::ContentBlock::Text { text } = block {
                            let _ = err_lock.write_all(text.as_bytes());
                        }
                    }
                    for call in &resp.tool_calls {
                        self.announce_tool(&call.id, &call.name, &mut err_lock);
                        mlock(&self.tool_calls).push(serde_json::json!({
                            "id": call.id,
                            "name": call.name,
                        }));
                    }
                    let _ = err_lock.flush();
                }
                StreamEvent::Done { finish, usage } => {
                    let _ = writeln!(err_lock, "\n[turn done finish={finish:?}]");
                    *mlock(&self.last_usage) = Some(usage.clone());
                    *mlock(&self.last_finish) = Some(*finish);
                }
                StreamEvent::Warning { message } => {
                    let _ = writeln!(err_lock, "\n[warning] {message}");
                    mlock(&self.warnings).push(message.clone());
                }
            }
        }
    }

    impl crate::agent::runtime::progress::ProgressSink for LiveSink {
        fn on_tool_start(&self, id: &str, name: &str, _input: &serde_json::Value) {
            self.announce_tool(id, name, &mut std::io::stderr().lock());
            self.heartbeat.start(id, "");
        }

        fn on_tool_result(
            &self,
            id: &str,
            name: &str,
            ok: bool,
            _latency_ms: u64,
            _bytes_returned: usize,
            _content_preview: &str,
        ) {
            self.heartbeat.stop(id);
            if !ok {
                let _ = writeln!(std::io::stderr().lock(), "\n[tool failed: {name}]");
            }
        }
    }

    let sink_obj = Arc::new(LiveSink {
        tool_calls: Mutex::new(Vec::new()),
        announced_tools: Mutex::new(HashSet::new()),
        warnings: Mutex::new(Vec::new()),
        last_usage: Mutex::new(None),
        last_finish: Mutex::new(None),
        heartbeat: crate::agent::runtime::progress::Heartbeat::new(),
    });
    let sink: Arc<dyn StreamSink> = sink_obj.clone();
    let progress: Arc<dyn crate::agent::runtime::progress::ProgressSink> = sink_obj.clone();

    let result = match memory::sqlite_fts::MemoryDb::open_default() {
        Ok(db) => {
            let session_id = uuid::Uuid::new_v4().to_string();
            runtime::loop_::ask_with_stream(
                provider.clone(),
                cfg,
                user_prompt,
                &tools,
                Some((&db, session_id.as_str())),
                sink,
                progress,
            )
            .await
        }
        Err(e) => {
            tracing::warn!(
                "memory: default DB unavailable ({e}); running without history recording"
            );
            runtime::loop_::ask_with_stream(
                provider.clone(),
                cfg,
                user_prompt,
                &tools,
                None,
                sink,
                progress,
            )
            .await
        }
    };

    match result {
        Ok(ask_result) => {
            let usage = mlock(&sink_obj.last_usage).clone().unwrap_or_default();
            let finish = mlock(&sink_obj.last_finish).take();
            Ok(json!({
                "answer": ask_result.answer,
                "evidence": ask_result.evidence,
                "fallback": ask_result.fallback,
                "turns": ask_result.turns,
                "provider": ask_result.provider,
                "model": ask_result.model,
                "session_id": ask_result.session_id,
                "tool_calls": *mlock(&sink_obj.tool_calls),
                "warnings": *mlock(&sink_obj.warnings),
                "finish": finish.map(|f| format!("{f:?}")),
                "usage": {
                    "input_tokens": usage.input_tokens,
                    "output_tokens": usage.output_tokens,
                    "cache_read_tokens": usage.cache_read_tokens,
                    "cache_write_tokens": usage.cache_write_tokens,
                },
            }))
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Helper for `cos agent live` integration tests. Mirrors the
/// `run_stream_async` helper above but routes through the new
/// multi-turn streaming path.
fn run_live_async(
    responses: &[(&str, Option<Vec<llm::types::ToolCall>>)],
    cfg: &crate::config::AgentConfig,
    prompt: &str,
) -> Value {
    use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", data.path().as_os_str());
    let mock = MockProvider::new(&cfg.model, cfg);
    for (text, tool_calls) in responses {
        match tool_calls {
            Some(calls) if !calls.is_empty() => {
                mock.push_response(MockResponse::ToolUse(calls.clone()));
            }
            _ => {
                mock.push_response(MockResponse::Text((*text).into()));
            }
        }
    }
    let provider: std::sync::Arc<dyn llm::Provider> = std::sync::Arc::new(mock);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(live_cmd_async(provider, cfg, prompt))
        .expect("live ok")
}

#[test]
fn live_async_returns_text_envelope() {
    let mut cfg = crate::config::AgentConfig::default();
    cfg.provider = "mock".into();
    cfg.model = "mock-model".into();
    // Disable memory recording for this test to keep it
    // hermetic — the temp data_dir scaffolding from
    // env-overrides is intentionally not set up here, so the
    // open_default() may fall back to no-recording mode anyway.
    let v = run_live_async(&[("hello world", None)], &cfg, "say hello");
    assert_eq!(v["answer"].as_str(), Some("hello world"));
    assert!(v["session_id"].as_str().unwrap().len() > 0);
    assert_eq!(v["provider"].as_str(), Some("mock"));
    assert_eq!(v["model"].as_str(), Some("mock-model"));
    // Text-only run: no tool calls.
    assert_eq!(v["tool_calls"].as_array().unwrap().len(), 0);
    // Mock emits Text via Message → Done with Stop finish.
    assert_eq!(v["finish"].as_str(), Some("Stop"));
    let usage = v.get("usage").unwrap();
    assert!(usage.get("input_tokens").is_some());
}

#[test]
fn live_async_records_tool_call_pair() {
    use crate::agent::llm::types::ToolCall;
    let mut cfg = crate::config::AgentConfig::default();
    cfg.provider = "mock".into();
    cfg.model = "mock-model".into();
    cfg.max_turns = 2; // tool-call → echo result → final text
    let v = run_live_async(
        &[
            (
                "",
                Some(vec![ToolCall {
                    id: "call_1".into(),
                    name: "echo".into(),
                    input: serde_json::json!({"text": "abc"}),
                }]),
            ),
            ("done", None),
        ],
        &cfg,
        "echo abc",
    );
    // Streaming sink records the tool_use event.
    let calls = v["tool_calls"].as_array().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["id"], "call_1");
    assert_eq!(calls[0]["name"], "echo");
    // Final answer comes from the second turn's Text response.
    assert_eq!(v["answer"].as_str(), Some("done"));
    assert!(v["turns"].as_u64().unwrap() >= 2);
}

#[test]
fn live_async_propagates_provider_error() {
    use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
    let mut cfg = crate::config::AgentConfig::default();
    cfg.provider = "mock".into();
    cfg.model = "mock-model".into();
    let mock = MockProvider::new(&cfg.model, &cfg);
    mock.push_response(MockResponse::Error(llm::LlmError::Auth));
    let provider: std::sync::Arc<dyn llm::Provider> = std::sync::Arc::new(mock);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let err = rt
        .block_on(live_cmd_async(provider, &cfg, "hi"))
        .unwrap_err();
    // AgentError::Llm wraps the provider error; the formatter
    // includes either "auth" or the provider-error prefix.
    assert!(
        err.to_lowercase().contains("auth")
            || err.to_lowercase().contains("llm")
            || err.to_lowercase().contains("provider"),
        "want auth/llm/provider in err, got {err}"
    );
}

#[test]
fn chat_cmd_max_turns_flag_rejects_non_numeric() {
    let err = chat_cmd(&["--max-turns".into(), "lots".into()]).unwrap_err();
    assert!(err.contains("--max-turns"), "got {err}");
}

#[test]
fn chat_routed_through_run() {
    // Confirm the dispatcher in `run()` reaches `chat_cmd`. Pass an
    // unknown flag so we get a deterministic error without trying
    // to read stdin.
    let err = run("chat", &["--definitely-not-real".into()]).unwrap_err();
    assert!(err.to_lowercase().contains("unknown flag"), "got {err}");
}
