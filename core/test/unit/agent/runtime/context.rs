use super::*;
use std::sync::Arc;

use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
use crate::agent::llm::{ContentBlock, ToolCall};
use crate::agent::runtime::deps::RuntimePaths;
use crate::agent::runtime::loop_::{run_with_deps, RuntimeRequest};
use crate::agent::tools::guardrails::Guardrails;
use crate::config::AgentConfig;

struct FixtureRoot {
    _session: crate::test_env::TestSessionGuard,
    _directory: tempfile::TempDir,
}

fn context_exposure(tools: &ToolRegistry) -> ToolExposureContext {
    ToolExposureContext::from_current_session(
        None,
        None,
        crate::agent::tools::exposure::ExecutionHost::Direct,
        tools.guardrails().clone(),
    )
    .unwrap()
}

fn fixture() -> (
    FixtureRoot,
    RuntimeDeps,
    AgentConfig,
    ToolRegistry,
    MemoryDb,
) {
    let directory = tempfile::tempdir().unwrap();
    let session = crate::test_env::TestSessionGuard::admin_with_caps(
        &directory.path().join("proc"),
        [
            crate::caps::Cap::new(
                crate::caps::Verb::MEMORY_READ,
                crate::caps::Scope::self_ref("agent"),
            ),
            crate::caps::Cap::new(
                crate::caps::Verb::MEMORY_WRITE,
                crate::caps::Scope::self_ref("agent"),
            ),
        ],
    );
    let paths = RuntimePaths {
        hooks_config: directory.path().join("hooks.json"),
        audit_log: directory.path().join("audit.jsonl"),
        notes_dir: directory.path().join("notes"),
        nudges_path: directory.path().join("nudges.json"),
        system_skills_dir: directory.path().join("system-skills"),
        user_skills_dir: directory.path().join("user-skills"),
        system_skills_origin: crate::agent::skills::loader::SkillOrigin::Local,
        curation_log: directory.path().join("curation.json"),
    };
    let deps = RuntimeDeps::load(&paths, None);
    let config = AgentConfig {
        provider: "mock".into(),
        model: "mock-model".into(),
        max_turns: 4,
        ..Default::default()
    };
    let deps = deps.with_config_snapshot(Arc::new(crate::config::CosConfig {
        agent: config.clone(),
        ..Default::default()
    }));
    let db = MemoryDb::open_in_memory().unwrap();
    let mut tools = crate::agent::tools::registry::builtin_only_registry();
    tools.register(Arc::new(
        crate::agent::tools::cos_proxy::memory::CosMemoryTool::with_store(deps.notes().clone()),
    ));
    tools.register(Arc::new(
        crate::agent::tools::cos_proxy::recall::CosRecallTool::new(db.clone()),
    ));
    (
        FixtureRoot {
            _session: session,
            _directory: directory,
        },
        deps,
        config,
        tools,
        db,
    )
}

fn packet(
    deps: &RuntimeDeps,
    config: &AgentConfig,
    tools: &ToolRegistry,
    text: &str,
    transient: Option<&str>,
) -> Result<ContextPacket, ContextError> {
    prepare(
        deps,
        ContextRequest {
            budget: ContextBudget::from_config(config)?,
            system: "fixed system instructions",
            prelude: &[],
            user_prompt: text,
            transient_context: transient,
            tools,
            exposure: Some(&context_exposure(tools)),
            llm_tools: &tools.as_llm_tools_for(&context_exposure(tools)),
            redactor: None,
        },
    )
}

fn texts(messages: &[Message]) -> String {
    messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            ContentBlock::ToolResult { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn initial_context_does_not_select_knowledge_by_question_keywords() {
    let (_directory, deps, config, tools, db) = fixture();
    deps.notes()
        .write("USER.md", "# Profile\n- PROFILE_SENTINEL")
        .unwrap();
    deps.notes().write("MEMORY.md",
        "# Project scope\n- NETWORK_KNOWLEDGE\n- DATABASE_KNOWLEDGE\n- [always] PINNED_SENTINEL"
    ).unwrap();
    db.record_message("old", "user", "PRIVATE_HISTORY_SENTINEL NETWORK_KNOWLEDGE")
        .unwrap();
    let network = packet(&deps, &config, &tools, "NETWORK_KNOWLEDGE", None).unwrap();
    let database = packet(&deps, &config, &tools, "DATABASE_KNOWLEDGE", None).unwrap();
    assert_eq!(network.render(), database.render());
    assert!(network.render().contains("PROFILE_SENTINEL"));
    assert!(network.render().contains("PINNED_SENTINEL"));
    assert!(network.render().contains("# Project scope"));
    for absent in [
        "NETWORK_KNOWLEDGE",
        "DATABASE_KNOWLEDGE",
        "PRIVATE_HISTORY_SENTINEL",
    ] {
        assert!(!network.render().contains(absent));
    }
    assert!(network.render().contains("cos_recall"));
}

#[test]
fn denied_memory_is_not_read_or_disclosed() {
    let (_directory, deps, config, mut tools, _db) = fixture();
    std::fs::create_dir_all(deps.notes().dir()).unwrap();
    std::fs::write(deps.notes().dir().join("USER.md"), [0xff]).unwrap();
    tools.set_guardrails(Guardrails::permissive().with_deny(["cos_memory", "cos_recall"]));
    let result = packet(&deps, &config, &tools, "read my profile", None).unwrap();
    assert!(result.sections.is_empty());
    assert!(result.render().is_empty());
}

#[test]
fn write_only_memory_exposure_does_not_read_the_profile() {
    let (_directory, deps, config, tools, _db) = fixture();
    std::fs::create_dir_all(deps.notes().dir()).unwrap();
    std::fs::write(deps.notes().dir().join("USER.md"), [0xff]).unwrap();
    let mut caps = crate::caps::CapSet::new();
    caps.insert(crate::caps::Cap::new(
        crate::caps::Verb::MEMORY_WRITE,
        crate::caps::Scope::self_ref("agent"),
    ));
    let exposure = context_exposure(&tools).with_capabilities(caps);
    let result = prepare(
        &deps,
        ContextRequest {
            budget: ContextBudget::from_config(&config).unwrap(),
            system: "system",
            prelude: &[],
            user_prompt: "write a note",
            transient_context: None,
            tools: &tools,
            exposure: Some(&exposure),
            llm_tools: &tools.as_llm_tools_for(&exposure),
            redactor: None,
        },
    )
    .unwrap();
    assert!(result.sections.iter().all(|section| {
        !matches!(
            section.kind,
            ContextKind::UserNotes | ContextKind::MemoryNotes
        )
    }));
}

#[test]
fn corrupt_reminders_are_an_error_not_an_empty_snapshot() {
    let (_directory, deps, config, tools, _db) = fixture();
    std::fs::write(&deps.paths().unwrap().nudges_path, "not json").unwrap();
    assert!(matches!(
        packet(&deps, &config, &tools, "hello", None),
        Err(ContextError::Source { origin, .. }) if origin == "due reminders"
    ));
}

#[test]
fn request_context_is_recorded_exactly_even_above_the_message_preview_cap() {
    let (_directory, deps, config, tools, db) = fixture();
    let selected = format!("{}END_OF_REQUIRED_CONTEXT", "data ".repeat(15_000));
    let packet = packet(&deps, &config, &tools, "inspect", Some(&selected)).unwrap();
    record(&packet, Some((&db, "current"))).unwrap();
    let rows = db.recent("current", 20).unwrap();
    for segment in packet.segments() {
        let rendered = segment.render_fenced(crate::agent::trust::envelope::process_seal());
        assert!(rows.iter().any(|row| row.content == rendered));
    }
    assert!(rows
        .iter()
        .any(|row| row.content.contains("END_OF_REQUIRED_CONTEXT")));
    assert!(db.recent_replayable("current", 20).unwrap().is_empty());
}

#[test]
fn secret_app_context_is_redacted_before_both_rendering_and_recording() {
    let (_directory, deps, config, tools, db) = fixture();
    let secret = format!("ghp_{}", "a".repeat(36));
    let transient = format!("selected repository; token={secret}");
    let redactor = Redactor::default_set();
    let packet = prepare(
        &deps,
        ContextRequest {
            budget: ContextBudget::from_config(&config).unwrap(),
            system: "system",
            prelude: &[],
            user_prompt: "inspect",
            transient_context: Some(&transient),
            tools: &tools,
            exposure: Some(&context_exposure(&tools)),
            llm_tools: &[],
            redactor: Some(&redactor),
        },
    )
    .unwrap();
    assert!(!packet.render().contains(&secret));
    assert!(packet.render().contains("selected repository"));
    record(&packet, Some((&db, "current"))).unwrap();
    assert!(db
        .recent("current", 20)
        .unwrap()
        .iter()
        .all(|row| !row.content.contains(&secret)));
}

#[tokio::test]
async fn same_session_refreshes_profile_without_mutating_system_prefix() {
    let (_directory, deps, config, tools, db) = fixture();
    let mut systems = Vec::new();
    for (index, profile) in ["PROFILE_ONE", "PROFILE_TWO", ""].into_iter().enumerate() {
        if profile.is_empty() {
            deps.notes().delete("USER.md").unwrap();
        } else {
            deps.notes().write("USER.md", profile).unwrap();
        }
        let provider = Arc::new(MockProvider::new(&config.model, &config));
        provider.push_response(MockResponse::Text("answer".into()));
        let prompt = format!("question {index}");
        run_with_deps(
            &deps,
            RuntimeRequest::buffered(provider.clone(), &config, &prompt, &tools)
                .with_exposure(&context_exposure(&tools))
                .with_continuation(&db, "same-session", 100),
        )
        .await
        .unwrap();
        let request = provider.last_request().unwrap();
        let body = texts(&request.messages);
        if index == 0 {
            assert!(body.contains("PROFILE_ONE"));
        } else {
            assert!(!body.contains("PROFILE_ONE"));
            assert_eq!(body.contains("PROFILE_TWO"), index == 1);
        }
        assert!(!request.system.as_ref().unwrap().contains("PROFILE_"));
        systems.push(request.system);
    }
    assert_eq!(systems[0], systems[1]);
    assert_eq!(systems[1], systems[2]);
}

#[tokio::test]
async fn history_is_loaded_only_after_the_model_calls_a_guarded_tool() {
    let (_directory, deps, config, tools, db) = fixture();
    let id = db
        .record_message("earlier", "user", "MAGENTA_HISTORY_SENTINEL")
        .unwrap();
    let provider = Arc::new(MockProvider::new(&config.model, &config));
    provider.push_response(MockResponse::ToolUse(vec![ToolCall {
        id: "read-source".into(),
        name: "cos_recall".into(),
        input: serde_json::json!({"command": "show", "message_id": id}),
    }]));
    provider.push_response(MockResponse::Text("source inspected".into()));
    run_with_deps(
        &deps,
        RuntimeRequest::buffered(provider.clone(), &config, "continue earlier work", &tools)
            .with_exposure(&context_exposure(&tools))
            .with_memory(&db, "current"),
    )
    .await
    .unwrap();
    let request = provider.last_request().unwrap();
    assert!(!texts(&request.messages[..1]).contains("MAGENTA_HISTORY_SENTINEL"));
    assert!(request
        .messages
        .iter()
        .any(|message| message.content.iter().any(|block| {
            matches!(block, ContentBlock::ToolResult { content, is_error: false, .. }
            if content.contains("MAGENTA_HISTORY_SENTINEL"))
        })));
    let invocations = db.recent_tool_invocations("current", 10).unwrap();
    assert!(invocations
        .iter()
        .any(|call| call.tool_name == "cos_recall" && call.success == Some(true)));
}

#[tokio::test]
async fn model_can_refine_an_empty_lookup_then_read_the_original_note() {
    let (_directory, deps, config, tools, db) = fixture();
    let note = "staging deployment target is TARGET_SENTINEL";
    deps.notes().write("MEMORY.md", note).unwrap();
    let provider = Arc::new(MockProvider::new(&config.model, &config));
    for (id, input) in [
        (
            "empty",
            serde_json::json!({"command": "search", "query": "unrelated"}),
        ),
        (
            "refined",
            serde_json::json!({"command": "search", "query": "staging"}),
        ),
        (
            "original",
            serde_json::json!({
                "command": "read", "name": "MEMORY.md",
                "revision": crate::agent::memory::history::text_revision(note),
            }),
        ),
    ] {
        provider.push_response(MockResponse::ToolUse(vec![ToolCall {
            id: id.into(),
            name: "cos_memory".into(),
            input,
        }]));
    }
    provider.push_response(MockResponse::Text("target verified from its source".into()));
    run_with_deps(
        &deps,
        RuntimeRequest::buffered(
            provider.clone(),
            &config,
            "find the deployment target",
            &tools,
        )
        .with_exposure(&context_exposure(&tools))
        .with_memory(&db, "refinement"),
    )
    .await
    .unwrap();
    let request = provider.last_request().unwrap();
    assert!(!texts(&request.messages[..1]).contains("TARGET_SENTINEL"));
    let results: Vec<_> = request
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::ToolResult {
                content,
                is_error: false,
                ..
            } => Some(content),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 3);
    assert!(results[0].contains("\"hits\":[]"));
    assert!(results[1].contains("TARGET_SENTINEL"));
    assert!(results[2].contains("\"source_complete\":true"));
    assert_eq!(
        db.recent_tool_invocations("refinement", 10).unwrap().len(),
        3
    );
}

#[tokio::test]
async fn compaction_preserves_original_request_and_records_the_handoff() {
    let (_directory, deps, mut config, tools, db) = fixture();
    config.max_turns = 3;
    config.compress_trigger_tokens = 12_000;
    config.compress_keep_tail_tokens = 500;
    for index in 0..24 {
        db.record_message(
            "current",
            if index % 2 == 0 { "user" } else { "assistant" },
            &format!("historical message {index} {}", "x".repeat(2000)),
        )
        .unwrap();
    }
    let original = "Never publish. Inspect only. Explain <think>literal input</think>.";
    let provider = Arc::new(MockProvider::new(&config.model, &config));
    provider.push_response(MockResponse::Text(
        "Goal: inspect. Pending: nothing published.".into(),
    ));
    provider.push_response(MockResponse::Text("finished inspection".into()));
    run_with_deps(
        &deps,
        RuntimeRequest::buffered(provider.clone(), &config, original, &tools)
            .with_exposure(&context_exposure(&tools))
            .with_continuation(&db, "current", 100),
    )
    .await
    .unwrap();
    let request = provider.last_request().unwrap();
    assert!(texts(&request.messages).contains(original));
    let uses: std::collections::HashSet<_> = request
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    for block in request.messages.iter().flat_map(|message| &message.content) {
        if let ContentBlock::ToolResult { tool_use_id, .. } = block {
            assert!(uses.contains(tool_use_id.as_str()));
        }
    }
    assert!(texts(&request.messages).contains(crate::agent::context::compressor::SUMMARY_MARKER));
}

#[tokio::test]
async fn impossible_context_budget_never_reaches_the_model() {
    let (_directory, deps, mut config, tools, db) = fixture();
    config.compress_target_tokens = 10;
    let provider = Arc::new(MockProvider::new(&config.model, &config));
    let error = run_with_deps(
        &deps,
        RuntimeRequest::buffered(provider.clone(), &config, "original request", &tools)
            .with_exposure(&context_exposure(&tools))
            .with_memory(&db, "current"),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        crate::agent::runtime::loop_::AgentError::Context(_)
    ));
    assert!(provider.last_request().is_none());
}
