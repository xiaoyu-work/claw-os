use super::*;
use crate::agent::terminal::backend::{
    ApprovalRequest, BackendInfo, Conversation, ConversationMessage, ConversationSummary, Job,
};
use crate::agent::terminal::commands::{parse as parse_command, Command};
use crate::agent::terminal::state::{
    clean_text, App, ApprovalStatus, EntryKind, PickerSelection, ToolStatus,
};
use ratatui::backend::TestBackend;
use serde_json::json;

fn parse(args: &[&str]) -> Result<ChatOptions, String> {
    ChatOptions::parse(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
}

fn app() -> App {
    App::new(
        BackendInfo {
            home: "/home/claw".into(),
            provider: "ollama".into(),
            model: "claw-model".into(),
            models: vec!["claw-model".into(), "other-model".into()],
        },
        Conversation {
            id: "ses_001953abcdef0_123456789abc".into(),
            title: "Claw terminal test".into(),
            archived: false,
            history_truncated: false,
            messages: vec![
                ConversationMessage {
                    role: "user".into(),
                    text: "Earlier question".into(),
                },
                ConversationMessage {
                    role: "assistant".into(),
                    text: "Earlier answer".into(),
                },
            ],
        },
    )
}

fn job(status: &str) -> Job {
    Job {
        id: "task-1".into(),
        session_id: "ses_001953abcdef0_123456789abc".into(),
        prompt: "Run the terminal test".into(),
        status: status.into(),
        response: None,
        error: None,
        requested_model: Some("claw-model".into()),
    }
}

#[test]
fn terminal_selection_preserves_plain_compatibility() {
    assert!(parse(&[]).unwrap().use_tui(true, false).unwrap());
    assert!(!parse(&[]).unwrap().use_tui(false, false).unwrap());
    assert!(!parse(&["--plain"]).unwrap().use_tui(true, false).unwrap());
    assert!(!parse(&["--no-stream"])
        .unwrap()
        .use_tui(true, false)
        .unwrap());
    assert!(!parse(&["--show-tools"])
        .unwrap()
        .use_tui(true, false)
        .unwrap());
    assert!(parse(&["--tui"]).unwrap().use_tui(false, false).is_err());
}

#[test]
fn parser_accepts_only_claw_terminal_options() {
    let options = parse(&[
        "--session",
        "ses_existing",
        "--no-memory",
        "--max-turns",
        "12",
    ])
    .unwrap();
    assert_eq!(options.session_id.as_deref(), Some("ses_existing"));
    assert!(options.no_memory);
    assert_eq!(options.max_turns, Some(12));

    for args in [
        &["--session"][..],
        &["--session", ""][..],
        &["--max-turns", "0"][..],
        &["--max-turns", "many"][..],
        &["--tui", "--plain"][..],
        &["--tui", "--no-stream"][..],
        &["--"][..],
        &["--", "--no-alt-screen"][..],
        &["--app", "mail"][..],
    ] {
        assert!(parse(args).is_err(), "{args:?}");
    }
}

#[test]
fn claw_commands_are_closed_and_semantic() {
    assert_eq!(parse_command("/help"), Some(Command::Help));
    assert_eq!(
        parse_command("/resume ses_1"),
        Some(Command::Resume("ses_1".into()))
    );
    assert_eq!(parse_command("/rewind 3"), Some(Command::Rewind(3)));
    assert_eq!(parse_command("/unarchive"), Some(Command::Unarchive));
    assert_eq!(parse_command("/model"), Some(Command::Models));
    assert_eq!(parse_command("/resume"), Some(Command::Sessions));
    assert_eq!(
        parse_command("/rewind 0"),
        Some(Command::Unknown("rewind 0".into()))
    );
    assert_eq!(
        parse_command("/delete"),
        Some(Command::Unknown("delete".into()))
    );
    assert_eq!(parse_command("ordinary prompt"), None);

    let mut app = app();
    app.insert_text("/rew");
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::None
    );
    assert_eq!(app.input, "/rewind ");
    app.input = "/rew".into();
    app.cursor = app.input.chars().count();
    app.complete_command();
    assert_eq!(app.input, "/rewind ");

    app.input = "/m".into();
    app.cursor = 2;
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Down,
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::None
    );
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Tab,
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::None
    );
    assert_eq!(app.input, "/model ");
}

#[test]
fn composer_edits_unicode_by_character_not_byte() {
    let mut app = app();
    app.insert_text("Claw 配额");
    app.move_left();
    app.backspace();
    app.insert_char('好');
    assert_eq!(app.input, "Claw 好额");
    app.cursor = 0;
    app.delete();
    assert_eq!(app.input, "law 好额");

    app.input.clear();
    app.cursor = 0;
    app.insert_text("one\ntwo");
    app.move_up();
    assert_eq!(app.cursor, 3);
    app.move_down();
    assert_eq!(app.cursor, 7);
}

#[test]
fn searchable_pickers_return_typed_claw_selections() {
    let mut app = app();
    app.open_model_picker();
    app.picker_insert('o');
    app.picker_insert('t');
    assert_eq!(
        app.take_picker_selection(),
        Some(PickerSelection::Model("other-model".into()))
    );

    app.open_model_picker();
    app.picker_insert('z');
    app.picker_insert('z');
    app.picker_insert('z');
    assert_eq!(app.take_picker_selection(), None);
    assert!(app.picker.is_some());
    app.close_picker();

    app.open_session_picker(vec![
        ConversationSummary {
            id: "ses_one".into(),
            title: "First".into(),
            archived: false,
        },
        ConversationSummary {
            id: "ses_two".into(),
            title: "Second".into(),
            archived: true,
        },
    ]);
    app.picker_insert('s');
    app.picker_insert('e');
    app.picker_move(true);
    assert_eq!(
        app.take_picker_selection(),
        Some(PickerSelection::Session("ses_two".into()))
    );
}

#[test]
fn stream_projection_keeps_private_payloads_out_of_the_transcript() {
    let mut app = app();
    app.begin_task(&job("running"));
    presentation::apply_record(
        &mut app,
        &json!({"event": {"kind": "text_delta", "text": "Visible answer"}}),
    )
    .unwrap();
    presentation::apply_record(
        &mut app,
        &json!({
            "event": {
                "kind": "tool_use",
                "id": "tool-1",
                "name": "cos_fs",
                "input": {"secret": "must-not-render"}
            }
        }),
    )
    .unwrap();
    presentation::apply_record(
        &mut app,
        &json!({
            "event": {
                "kind": "reasoning",
                "id": "reasoning-1",
                "summary": ["Checked the safe path"],
                "encrypted_content": "opaque-provider-state"
            }
        }),
    )
    .unwrap();
    presentation::apply_record(
        &mut app,
        &json!({
            "progress": {
                "kind": "tool_result",
                "id": "tool-1",
                "name": "cos_fs",
                "ok": true,
                "latency_ms": 42,
                "body": "private result body"
            }
        }),
    )
    .unwrap();
    presentation::apply_record(
        &mut app,
        &json!({
            "event": {
                "kind": "done",
                "finish": "stop",
                "usage": {"input_tokens": 7, "output_tokens": 3, "cache_read_tokens": 2}
            }
        }),
    )
    .unwrap();

    let rendered = app
        .entries
        .iter()
        .map(|entry| entry.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Visible answer"));
    assert!(rendered.contains("Checked the safe path"));
    assert!(!rendered.contains("must-not-render"));
    assert!(!rendered.contains("private result body"));
    assert!(!rendered.contains("opaque-provider-state"));
    assert!(app.entries.iter().any(|entry| {
        matches!(
            entry.kind,
            EntryKind::Tool {
                status: ToolStatus::Succeeded {
                    duration_ms: Some(42)
                },
                ..
            }
        )
    }));
    assert_eq!(
        (app.usage_input, app.usage_output, app.usage_cached),
        (7, 3, 2)
    );
}

#[test]
fn approvals_have_one_explicit_terminal_decision() {
    let mut app = app();
    app.add_approvals(vec![ApprovalRequest {
        id: "approval-1".into(),
        verb: "fs.write".into(),
        scope: json!({"path": "/home/claw/output"}),
        reason: "Write requested output".into(),
    }]);
    assert_eq!(app.current_approval().unwrap().id, "approval-1");
    app.resolve_approval("approval-1", true);
    assert!(app.current_approval().is_none());
    assert!(app.entries.iter().any(|entry| {
        matches!(
            &entry.kind,
            EntryKind::Approval {
                id,
                status: ApprovalStatus::Approved,
            } if id == "approval-1"
        )
    }));
}

#[test]
fn model_output_is_redacted_and_control_safe() {
    let cleaned = clean_text("token=ghp_abcdefghijklmnopqrstuvwxyz0123456789\u{1b}[31m");
    assert!(!cleaned.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
    assert!(!cleaned.contains('\u{1b}'));
}

#[test]
fn ratatui_frame_is_claw_owned_and_contains_core_state() {
    let mut app = app();
    app.push_assistant_delta("## Result\n\nClaw completed the work.");
    app.tool_started("tool-1", "cos_sysinfo");
    let backend = TestBackend::new(100, 28);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| ui::render(frame, &app)).unwrap();
    let buffer = terminal.backend().buffer();
    let output = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(output.contains("CLAW AGENT"));
    assert!(output.contains("Claw terminal test"));
    assert!(output.contains("Claw completed the work."));
    assert!(output.contains("cos_sysinfo"));
    assert!(!output.contains("Codex"));
}

#[test]
fn slash_palette_exposes_only_supported_claw_commands() {
    let mut app = app();
    app.insert_text("/m");
    let backend = TestBackend::new(100, 28);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| ui::render(frame, &app)).unwrap();
    let output = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(output.contains("Commands - Tab completes"));
    assert!(output.contains("/models"));
    assert!(output.contains("/model"));
    assert!(!output.contains("/mcp"));
    assert!(!output.contains("/delete"));
}

#[test]
fn picker_and_working_header_render_real_claw_state() {
    let mut app = app();
    app.open_model_picker();
    app.picker_insert('o');
    app.picker_insert('t');
    let backend = TestBackend::new(100, 28);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| ui::render(frame, &app)).unwrap();
    let picker = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(picker.contains("Select model"));
    assert!(picker.contains("other-model"));
    assert!(picker.contains("filter: ot"));

    app.close_picker();
    app.begin_task(&job("running"));
    terminal.draw(|frame| ui::render(frame, &app)).unwrap();
    let working = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(working.contains("WORKING"));
}
