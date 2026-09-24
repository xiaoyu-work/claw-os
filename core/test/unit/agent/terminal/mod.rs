use super::*;
use crate::agent::terminal::backend::{
    Activity, ActivityAttention, ActivityControlPolicy, ActivityControls, ActivityDetail,
    parse_agent_hook_settings, parse_usage_overview, AccountOverview, ActivityEvidence,
    ActivityOperationPreview, ActivityResource, ActivityReview, AgentHookSettings, ApprovalRequest,
    BackendInfo, Conversation, ConversationMessage, ConversationSummary, DebugOverview,
    ExtensionSummary, ExtensionsOverview, Job, McpOverview, McpServerSummary, NotificationAction,
    NotificationDelivery,
    NotificationItem, NotificationPage, NotificationPreferences, PlatformOverview, TaskSummary,
    UsageBreakdown, UsageOverview, UsagePeriod, VoiceOverview,
};
use crate::agent::terminal::commands::{parse as parse_command, Command, NotificationChannel};
use crate::agent::terminal::state::{
    clean_text, App, ApprovalStatus, ConfirmationAction, EntryKind, NotificationPreferenceAction,
    PickerSelection, ToolStatus,
};
use base64::Engine;
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
            provider_ready: true,
            model_catalog_warning: None,
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
            jobs: Vec::new(),
            jobs_truncated: false,
        },
    )
}

fn job(status: &str) -> Job {
    Job {
        id: "task-1".into(),
        session_id: "ses_001953abcdef0_123456789abc".into(),
        activity_id: None,
        workspace: Some("/home/claw/project".into()),
        after_task_id: None,
        prompt: "Run the terminal test".into(),
        status: status.into(),
        created_at: "2026-01-01T00:00:00Z".into(),
        started_at: Some("2026-01-01T00:00:01Z".into()),
        finished_at: None,
        response: None,
        error: None,
        requested_model: Some("claw-model".into()),
        requested_reasoning_effort: None,
        plan_only: false,
        provider: Some("ollama".into()),
        model: Some("claw-model".into()),
        turns_used: None,
    }
}

#[test]
fn terminal_remains_available_before_provider_setup() {
    let mut app = app();
    app.info.provider_ready = false;
    let app = App::new(app.info, app.conversation);
    assert!(app.entries.iter().any(|entry| {
        entry
            .text
            .contains("Local sessions, tasks, approvals, notifications, and Activities")
    }));
}

#[test]
fn terminal_surfaces_live_model_catalogue_degradation() {
    let mut app = app();
    app.info.model_catalog_warning = Some("Live model discovery failed".into());
    let app = App::new(app.info, app.conversation);
    assert!(app.entries.iter().any(|entry| {
        entry
            .text
            .contains("Live model discovery failed. The configured model remains available")
    }));
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
    assert_eq!(
        parse_command("/workspace project/repo"),
        Some(Command::Workspace(Some("project/repo".into())))
    );
    assert_eq!(
        parse_command("/attach project/screen.png"),
        Some(Command::Attach(Some("project/screen.png".into())))
    );
    assert_eq!(
        parse_command("/review activity-1"),
        Some(Command::Review(Some("activity-1".into())))
    );
    assert_eq!(parse_command("/copy"), Some(Command::Copy));
    assert_eq!(
        parse_command("/export transcript.md"),
        Some(Command::Export("transcript.md".into()))
    );
    assert_eq!(parse_command("/raw"), Some(Command::Raw));
    assert_eq!(parse_command("/appearance"), Some(Command::Appearance));
    assert_eq!(parse_command("/vim"), Some(Command::Vim));
    assert_eq!(parse_command("/keymap"), Some(Command::Keymap));
    assert_eq!(parse_command("/agents"), Some(Command::Agents));
    assert_eq!(parse_command("/side"), Some(Command::Side(false)));
    assert_eq!(parse_command("/side return"), Some(Command::Side(true)));
    assert_eq!(parse_command("/platform"), Some(Command::Platform));
    assert_eq!(parse_command("/voice"), Some(Command::Voice));
    assert_eq!(parse_command("/voice settings"), Some(Command::Voice));
    assert_eq!(parse_command("/tasks"), Some(Command::Tasks));
    assert_eq!(
        parse_command("/task task-1"),
        Some(Command::Task("task-1".into()))
    );
    assert_eq!(parse_command("/approvals"), Some(Command::Approvals));
    assert_eq!(
        parse_command("/approval approval-1"),
        Some(Command::Approval("approval-1".into()))
    );
    assert_eq!(
        parse_command("/notify-channel desktop off"),
        Some(Command::NotifyChannel(NotificationChannel::Desktop, false))
    );
    assert_eq!(
        parse_command("/notify-severity ntfy critical"),
        Some(Command::NotifySeverity(
            NotificationChannel::Ntfy,
            "critical".into()
        ))
    );
    assert_eq!(
        parse_command("/dnd 22:30-06:15"),
        Some(Command::Dnd(Some((1_350, 375))))
    );
    assert_eq!(parse_command("/dnd off"), Some(Command::Dnd(None)));
    assert_eq!(
        parse_command("/activity-create Release v2 | Publish next Friday"),
        Some(Command::ActivityCreate {
            title: "Release v2".into(),
            goal: "Publish next Friday".into(),
        })
    );
    assert_eq!(
        parse_command("/activity new Release v2 | Publish next Friday"),
        Some(Command::ActivityCreate {
            title: "Release v2".into(),
            goal: "Publish next Friday".into(),
        })
    );
    assert_eq!(
        parse_command("/activity-run activity-1 Prepare a draft"),
        Some(Command::ActivityRun {
            id: "activity-1".into(),
            prompt: Some("Prepare a draft".into()),
        })
    );
    assert_eq!(
        parse_command("/activity-complete activity-1 | User reviewed it"),
        Some(Command::ActivityComplete {
            id: "activity-1".into(),
            note: "User reviewed it".into(),
        })
    );
    assert_eq!(
        parse_command(
            "/activity-limits-set activity-1 2 | {\"max_attempts\":10,\"max_turns_per_attempt\":5,\"expires_at\":\"2027-01-01T00:00:00Z\"}"
        ),
        Some(Command::ActivityControlSet {
            id: "activity-1".into(),
            policy: ActivityControlPolicy::ExecutionLimits,
            expected_revision: Some(2),
            draft: json!({
                "max_attempts": 10,
                "max_turns_per_attempt": 5,
                "expires_at": "2027-01-01T00:00:00Z",
            }),
        })
    );
    assert_eq!(
        parse_command("/activity-capability-enable activity-1 off 4"),
        Some(Command::ActivityControlEnabled {
            id: "activity-1".into(),
            policy: ActivityControlPolicy::CapabilityPolicy,
            revision: 4,
            enabled: false,
        })
    );
    assert_eq!(
        parse_command("/activity-priority activity-1 foreground new"),
        Some(Command::ActivityPriority {
            id: "activity-1".into(),
            priority: "foreground".into(),
            expected_revision: None,
        })
    );
    assert_eq!(
        parse_command(
            "/activity-preview activity-1 fs write | [\"/tmp/file\",\"--content\",\"draft\"]"
        ),
        Some(Command::ActivityPreview {
            id: "activity-1".into(),
            app_id: "fs".into(),
            operation: "write".into(),
            args: vec!["/tmp/file".into(), "--content".into(), "draft".into()],
        })
    );
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
fn image_attachment_reads_one_explicit_home_file() {
    let home = tempfile::tempdir().unwrap();
    let workspace = home.path().join("project");
    std::fs::create_dir(&workspace).unwrap();
    let image = workspace.join("screen.png");
    std::fs::write(&image, b"\x89PNG\r\n\x1a\nfixture").unwrap();
    std::fs::write(workspace.join("notes.txt"), b"not read by completion").unwrap();

    let attachment = commands::load_image_attachment("screen.png", home.path(), &workspace)
        .expect("load image");
    assert_eq!(attachment.name, "screen.png");
    assert_eq!(attachment.media_type, "image/png");

    let outside = tempfile::tempdir().unwrap();
    let outside_image = outside.path().join("outside.png");
    std::fs::write(&outside_image, b"\x89PNG\r\n\x1a\nfixture").unwrap();
    let error = commands::load_image_attachment(
        outside_image.to_str().unwrap(),
        home.path(),
        &workspace,
    )
    .unwrap_err();
    assert!(error.contains("verified owner home"));

    let mut app = app();
    app.add_attachment(attachment).unwrap();
    assert_eq!(app.pending_attachment_count(), 1);
    app.clear_attachments();
    assert_eq!(app.pending_attachment_count(), 0);

    let files = commands::list_workspace_files(&workspace).unwrap();
    assert_eq!(files.paths, ["notes.txt", "screen.png"]);
    assert!(!files.truncated);
}

#[test]
fn copy_and_export_use_only_redacted_visible_text() {
    let sequence = commands::osc52_copy_sequence("visible answer").unwrap();
    let encoded = sequence
        .strip_prefix("\u{1b}]52;c;")
        .unwrap()
        .strip_suffix('\u{7}')
        .unwrap();
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap(),
        b"visible answer"
    );

    let mut app = app();
    let secret = format!("{}-{}", "sk", "abcdef0123456789ABCDEFXYZ123");
    app.push_assistant_delta(&secret);
    app.push_assistant_delta(" visible");
    let markdown = app.export_markdown().unwrap();
    assert!(markdown.contains("## User"));
    assert!(markdown.contains("## Assistant"));
    assert!(markdown.contains("visible"));
    assert!(!markdown.contains(&secret), "{markdown}");
    app.request_raw_scrollback();
    let raw = app.take_raw_scrollback().unwrap();
    assert!(raw.contains("[assistant]"));
    assert!(!raw.contains(&secret));
    assert!(app.take_raw_scrollback().is_none());

    let home = tempfile::tempdir().unwrap();
    let workspace = home.path().join("project");
    std::fs::create_dir(&workspace).unwrap();
    let exported =
        commands::write_markdown_export("conversation.md", home.path(), &workspace, &markdown)
            .unwrap();
    assert_eq!(std::fs::read_to_string(&exported).unwrap(), markdown);
    assert!(commands::write_markdown_export(
        "conversation.md",
        home.path(),
        &workspace,
        &markdown
    )
    .unwrap_err()
    .contains("create conversation export"));
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
fn queued_tasks_retain_the_broker_acknowledged_workspace_and_dependency() {
    let mut app = app();
    let mut queued = job("pending");
    queued.prompt = "first queued task".into();
    queued.workspace = Some("/home/claw/project-a".into());
    queued.after_task_id = Some("task-active".into());
    app.queue_task(queued);
    let queued = app.queued_tasks.pop_front().unwrap();
    assert_eq!(queued.prompt, "first queued task");
    assert_eq!(queued.workspace.as_deref(), Some("/home/claw/project-a"));
    assert_eq!(queued.after_task_id.as_deref(), Some("task-active"));
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

    app.open_history_picker();
    assert_eq!(
        app.take_picker_selection(),
        Some(PickerSelection::History {
            before_user_turn: 0,
            prompt: "Earlier question".into(),
        })
    );

    app.open_file_picker(commands::WorkspaceFiles {
        paths: vec!["src/main.rs".into()],
        truncated: false,
    });
    assert_eq!(
        app.take_picker_selection(),
        Some(PickerSelection::File("src/main.rs".into()))
    );
}

#[test]
fn double_escape_opens_backtrack_only_while_idle() {
    let mut app = app();
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::None
    );
    assert!(app.backtrack_armed());
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::None
    );
    assert!(matches!(
        app.picker.as_ref().map(|picker| picker.kind),
        Some(crate::agent::terminal::state::PickerKind::History)
    ));

    app.close_picker();
    app.begin_task(&job("running"));
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::Cancel
    );
}

#[test]
fn resumed_model_uses_only_the_configured_catalogue() {
    let mut app = app();
    app.restore_selected_model(Some("other-model"));
    assert_eq!(app.selected_model, "other-model");

    app.restore_selected_model(Some("removed-model"));
    assert_eq!(app.selected_model, "other-model");
    assert!(app
        .entries
        .iter()
        .any(|entry| entry.text.contains("removed-model is no longer available")));
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
fn completed_stream_does_not_repeat_the_final_response() {
    let mut app = app();
    let mut completed = job("ok");
    completed.response = Some("Visible answer".into());
    app.begin_task(&completed);
    presentation::apply_record(
        &mut app,
        &json!({"event": {"kind": "text_delta", "text": "Visible answer"}}),
    )
    .unwrap();
    presentation::apply_record(
        &mut app,
        &json!({
            "event": {
                "kind": "done",
                "finish": "stop",
                "usage": {
                    "input_tokens": 1,
                    "output_tokens": 1,
                    "cache_read_tokens": 0,
                    "cache_write_tokens": 0
                }
            }
        }),
    )
    .unwrap();
    app.finish_task(&completed);

    assert_eq!(
        app.entries
            .iter()
            .filter(|entry| {
                matches!(entry.kind, EntryKind::Assistant) && entry.text == "Visible answer"
            })
            .count(),
        1
    );
}

#[test]
fn ctrl_j_inserts_the_advertised_newline() {
    let mut app = app();
    app.insert_text("First line");
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('j'),
                crossterm::event::KeyModifiers::CONTROL,
            ),
        ),
        InputAction::None
    );
    app.insert_text("Second line");
    assert_eq!(app.input, "First line\nSecond line");
}

#[test]
fn ctrl_d_does_not_silently_detach_active_work() {
    let mut app = app();
    app.begin_task(&job("running"));
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('d'),
                crossterm::event::KeyModifiers::CONTROL,
            ),
        ),
        InputAction::None
    );
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('c'),
                crossterm::event::KeyModifiers::CONTROL,
            ),
        ),
        InputAction::Cancel
    );
}

#[test]
fn task_controls_change_only_future_task_defaults() {
    let mut app = app();
    app.info.provider = "copilot".into();
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('t'),
                crossterm::event::KeyModifiers::CONTROL,
            ),
        ),
        InputAction::None
    );
    assert!(app.task_controls_open);
    handle_key(
        &mut app,
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('p'),
            crossterm::event::KeyModifiers::NONE,
        ),
    );
    handle_key(
        &mut app,
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('r'),
            crossterm::event::KeyModifiers::NONE,
        ),
    );
    handle_key(
        &mut app,
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('m'),
            crossterm::event::KeyModifiers::NONE,
        ),
    );
    handle_key(
        &mut app,
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('t'),
            crossterm::event::KeyModifiers::NONE,
        ),
    );
    assert!(!app.task_use_memory);
    assert_eq!(app.task_max_turns, Some(8));
    assert_eq!(app.task_reasoning_effort.as_deref(), Some("minimal"));
    assert!(app.task_plan_only);
    assert_eq!(app.selected_model, "claw-model");
}

#[test]
fn appearance_controls_are_terminal_local() {
    let mut app = app();
    app.open_appearance();
    for key in ['t', 'h', 's', 'k'] {
        assert_eq!(
            handle_key(
                &mut app,
                crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Char(key),
                    crossterm::event::KeyModifiers::NONE,
                ),
            ),
            InputAction::None
        );
    }
    assert_eq!(app.theme_name(), "blue");
    assert_eq!(
        app.terminal_title().as_deref(),
        Some("Claw - Claw terminal test")
    );
    assert!(app.compact_statusline);
    assert_eq!(app.keymap_name(), "vim");
    assert!(!app.vim_insert_mode);
}

#[test]
fn vim_keymap_edits_idle_composer_but_never_steals_task_cancel() {
    let mut app = app();
    app.toggle_vim_mode();
    handle_key(
        &mut app,
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('i'),
            crossterm::event::KeyModifiers::NONE,
        ),
    );
    handle_key(
        &mut app,
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('a'),
            crossterm::event::KeyModifiers::NONE,
        ),
    );
    handle_key(
        &mut app,
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        ),
    );
    handle_key(
        &mut app,
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('0'),
            crossterm::event::KeyModifiers::NONE,
        ),
    );
    handle_key(
        &mut app,
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('x'),
            crossterm::event::KeyModifiers::NONE,
        ),
    );
    assert!(app.input.is_empty());

    app.begin_task(&job("running"));
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::Cancel
    );
}

#[test]
fn agents_view_reports_only_real_delegate_tool_calls() {
    let mut app = app();
    app.tool_started("delegate-1", "cos_delegate");
    app.tool_started("tool-1", "cos_sysinfo");
    assert_eq!(
        app.delegate_summaries(),
        vec![("delegate-1".into(), "running")]
    );
    app.tool_finished("delegate-1", "cos_delegate", true, Some(12));
    assert_eq!(
        app.delegate_summaries(),
        vec![("delegate-1".into(), "completed")]
    );
}

#[test]
fn side_conversation_state_is_explicit_and_clearable() {
    let mut app = app();
    app.begin_side_conversation("ses_parent".into(), "ses_side".into());
    assert_eq!(app.side_conversation(), Some(("ses_parent", "ses_side")));
    assert!(app.in_side_conversation());
    app.clear_side_conversation();
    assert!(!app.in_side_conversation());
}

#[test]
fn platform_overview_is_bounded_read_only_presentation() {
    let mut app = app();
    app.open_platform_overview(PlatformOverview {
        presentation: json!({
            "authority": "Read-only verified inventory.",
            "skills": ["claw-os"],
            "mcp": {"configured_enabled": ["fixture"]},
        })
        .to_string(),
    });
    let backend = TestBackend::new(110, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| ui::render(frame, &app)).unwrap();
    let output = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(output.contains("Verified read-only inventory"));
    assert!(output.contains("claw-os"));
}

#[test]
fn platform_memory_center_controls_future_memory_and_confirms_reset() {
    let mut app = app();
    app.open_platform_overview(PlatformOverview {
        presentation: "{}".into(),
    });
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('m'),
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::None
    );
    assert!(app.memory_center_open);

    handle_key(
        &mut app,
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('m'),
            crossterm::event::KeyModifiers::NONE,
        ),
    );
    assert!(!app.task_use_memory);
    handle_key(
        &mut app,
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('r'),
            crossterm::event::KeyModifiers::NONE,
        ),
    );
    assert_eq!(
        app.confirmation.as_ref().map(|value| &value.action),
        Some(&ConfirmationAction::MemoryReset)
    );

    let backend = TestBackend::new(110, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| ui::render(frame, &app)).unwrap();
    let output = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(output.contains("Reset learned memory?"));
    assert!(output.contains("Conversation history"));
}

#[test]
fn memory_reset_refusal_is_visible_while_a_task_is_active() {
    let mut app = app();
    app.open_platform_overview(PlatformOverview {
        presentation: "{}".into(),
    });
    app.open_memory_center();
    app.active_task = Some("task-active".into());

    app.confirm_memory_reset();

    assert!(!app.memory_center_open);
    assert!(app.platform_overview.is_none());
    assert!(app.confirmation.is_none());
    assert!(app.entries.iter().any(|entry| {
        entry.kind == EntryKind::Error
            && entry
                .text
                .contains("cannot be reset while a task is active")
    }));
}

#[test]
fn platform_hooks_center_uses_closed_future_task_settings() {
    let settings = AgentHookSettings {
        logging: false,
        audit: true,
        checkpoint: false,
        updated_kind: None,
        changed: None,
    };
    let mut app = app();
    app.open_platform_overview(PlatformOverview {
        presentation: "{}".into(),
    });
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('h'),
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::OpenAgentHooks
    );
    app.open_agent_hooks(settings);
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('c'),
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::ToggleAgentHook("checkpoint")
    );

    let backend = TestBackend::new(110, 26);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| ui::render(frame, &app)).unwrap();
    let output = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(output.contains("Hooks Center"));
    assert!(output.contains("logging: off"));
    assert!(output.contains("audit: on"));
    assert!(output.contains("No arbitrary commands"));
}

#[test]
fn hook_settings_parser_rejects_open_or_incomplete_inventories() {
    let valid = json!({
        "applies_to": "future_tasks",
        "hooks": [
            {"kind": "logging", "enabled": false},
            {"kind": "audit", "enabled": true},
            {"kind": "checkpoint", "enabled": false},
        ],
    });
    assert_eq!(
        parse_agent_hook_settings(valid).unwrap(),
        AgentHookSettings {
            logging: false,
            audit: true,
            checkpoint: false,
            updated_kind: None,
            changed: None,
        }
    );
    for invalid in [
        json!({
            "applies_to": "current_task",
            "hooks": [
                {"kind": "logging", "enabled": false},
                {"kind": "audit", "enabled": false},
                {"kind": "checkpoint", "enabled": false},
            ],
        }),
        json!({
            "applies_to": "future_tasks",
            "hooks": [
                {"kind": "logging", "enabled": false},
                {"kind": "audit", "enabled": false},
                {"kind": "external", "enabled": true},
            ],
        }),
        json!({
            "applies_to": "future_tasks",
            "hooks": [
                {"kind": "logging", "enabled": false},
                {"kind": "logging", "enabled": true},
                {"kind": "checkpoint", "enabled": false},
            ],
        }),
    ] {
        assert!(parse_agent_hook_settings(invalid).is_err());
    }
}

#[test]
fn platform_mcp_center_exposes_metadata_without_launch_material() {
    let mut app = app();
    app.open_platform_overview(PlatformOverview {
        presentation: "{}".into(),
    });
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('c'),
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::OpenMcpOverview
    );
    app.open_mcp_overview(McpOverview {
        servers: vec![McpServerSummary {
            name: "fixture".into(),
            source: "operator config",
            enabled: true,
            transport: "stdio",
            timeout_secs: 30,
        }],
        discovery_enabled: false,
        truncated: false,
    });

    let backend = TestBackend::new(110, 26);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| ui::render(frame, &app)).unwrap();
    let output = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(output.contains("MCP Center"));
    assert!(output.contains("fixture"));
    assert!(output.contains("operator config"));
    assert!(output.contains("command, args, env, cwd, and URL stay hidden"));
}

#[test]
fn platform_extensions_center_preserves_one_claw_extension_model() {
    let mut app = app();
    app.open_platform_overview(PlatformOverview {
        presentation: "{}".into(),
    });
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('e'),
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::OpenExtensionsOverview
    );
    app.open_extensions_overview(ExtensionsOverview {
        entries: vec![
            ExtensionSummary {
                kind: "App",
                id: "calendar".into(),
                status: "verified",
                trust: "publisher".into(),
                diagnostic: None,
            },
            ExtensionSummary {
                kind: "Agent extension",
                id: "broken".into(),
                status: "quarantined",
                trust: "quarantined".into(),
                diagnostic: Some("signature verification failed".into()),
            },
        ],
        truncated: false,
    });

    let backend = TestBackend::new(110, 26);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| ui::render(frame, &app)).unwrap();
    let output = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(output.contains("Extensions Center"));
    assert!(output.contains("calendar"));
    assert!(output.contains("quarantined"));
    assert!(output.contains("signature verification failed"));
    assert!(output.contains("no separate Plugin authority"));
}

#[test]
fn platform_usage_center_renders_only_canonical_ledger_totals() {
    let mut app = app();
    app.open_platform_overview(PlatformOverview {
        presentation: "{}".into(),
    });
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('u'),
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::OpenUsageOverview(UsagePeriod::Cumulative)
    );
    let totals = crate::agent::llm::usage::Totals {
        calls: 3,
        success: 2,
        error: 1,
        input_tokens: 120,
        output_tokens: 40,
        cache_read_tokens: 30,
        cache_write_tokens: 10,
        total_duration_ms: 900,
        ..Default::default()
    };
    app.open_usage_overview(UsageOverview {
        period: UsagePeriod::Cumulative,
        total: totals.clone(),
        providers: vec![UsageBreakdown {
            name: "copilot".into(),
            totals: totals.clone(),
        }],
        models: vec![UsageBreakdown {
            name: "gpt-test".into(),
            totals,
        }],
        parse_errors: 0,
        log_lines: 3,
        log_bytes: 512,
        breakdown_truncated: false,
    });

    let backend = TestBackend::new(110, 28);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| ui::render(frame, &app)).unwrap();
    let output = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(output.contains("Usage Center"));
    assert!(output.contains("all retained usage"));
    assert!(output.contains("copilot"));
    assert!(output.contains("gpt-test"));
    assert!(output.contains("does not infer monetary cost"));
}

#[test]
fn usage_parser_bounds_breakdowns_and_rejects_non_overall_scope() {
    let mut providers = serde_json::Map::new();
    for index in 0..25 {
        providers.insert(
            format!("provider-{index:02}"),
            json!({
                "calls": index,
                "success": index,
                "error": 0,
                "input_tokens": index,
                "output_tokens": 0,
                "cache_read_tokens": 0,
                "cache_write_tokens": 0,
                "total_duration_ms": 0,
                "finish_reasons": {},
                "errors": 0,
            }),
        );
    }
    let value = json!({
        "scope": "overall",
        "total": {
            "calls": 25,
            "success": 25,
            "error": 0,
            "input_tokens": 300,
            "output_tokens": 0,
            "cache_read_tokens": 0,
            "cache_write_tokens": 0,
            "total_duration_ms": 0,
            "finish_reasons": {},
            "errors": 0,
        },
        "by_provider": providers,
        "by_model": {},
        "parse_errors": 0,
        "log_lines": 25,
        "log_bytes": 1000,
        "breakdown_truncated": false,
    });
    let overview = parse_usage_overview(UsagePeriod::Daily, value).unwrap();
    assert_eq!(overview.providers.len(), 20);
    assert_eq!(overview.providers[0].name, "provider-24");
    assert!(overview.breakdown_truncated);

    let invalid = json!({
        "scope": "session",
        "total": crate::agent::llm::usage::Totals::default(),
        "by_provider": {},
        "by_model": {},
        "parse_errors": 0,
        "log_lines": 0,
        "log_bytes": 0,
        "breakdown_truncated": false,
    });
    assert!(parse_usage_overview(UsagePeriod::Cumulative, invalid).is_err());
}

#[test]
fn platform_debug_center_omits_secret_bearing_configuration() {
    let mut app = app();
    app.open_platform_overview(PlatformOverview {
        presentation: "{}".into(),
    });
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('d'),
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::OpenDebugOverview
    );
    app.open_debug_overview(DebugOverview {
        daemon: "clawd".into(),
        daemon_status: "ok".into(),
        started_at: "2026-01-01T00:00:00Z".into(),
        uptime_ms: 1000,
        provider: "copilot".into(),
        model: "gpt-test".into(),
        provider_ready: true,
        model_count: 2,
        model_catalog_warning: None,
        max_turns: 16,
        reasoning_effort: Some("high".into()),
        compression_enabled: true,
        memory_redaction_enabled: true,
        progressive_tools_enabled: true,
        tool_allow_count: None,
        tool_deny_count: 1,
        configured_mcp_count: 2,
        mcp_discovery_enabled: true,
        selected_extension_count: 1,
    });

    let backend = TestBackend::new(110, 30);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| ui::render(frame, &app)).unwrap();
    let output = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(output.contains("Debug Center"));
    assert!(output.contains("clawd"));
    assert!(output.contains("gpt-test"));
    assert!(output.contains("Credentials, headers, URLs"));
    assert!(!output.contains("api_key"));
}

#[test]
fn platform_account_center_requires_logout_confirmation_and_never_scans_import_roots() {
    let mut app = app();
    app.open_platform_overview(PlatformOverview {
        presentation: "{}".into(),
    });
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('a'),
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::OpenAccountOverview
    );
    app.open_account_overview(AccountOverview {
        provider: "copilot".into(),
        credential_present: true,
    });
    handle_key(
        &mut app,
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('l'),
            crossterm::event::KeyModifiers::NONE,
        ),
    );
    assert_eq!(
        app.confirmation.as_ref().map(|value| &value.action),
        Some(&ConfirmationAction::AccountLogout)
    );

    let backend = TestBackend::new(110, 28);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| ui::render(frame, &app)).unwrap();
    let output = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(output.contains("Log out of GitHub Copilot?"));
    assert!(output.contains("Provider configuration"));

    app.close_confirmation();
    let mut terminal = ratatui::Terminal::new(TestBackend::new(110, 28)).unwrap();
    terminal.draw(|frame| ui::render(frame, &app)).unwrap();
    let output = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(output.contains("Account Center"));
    assert!(output.contains("No authenticated foreign-agent importer"));
    assert!(output.contains("will not scan Claude, Cursor, Codex"));
}

#[test]
fn voice_center_uses_claw_media_models_without_claiming_realtime_capture() {
    let mut app = app();
    app.open_voice_overview(VoiceOverview {
        stt_provider: "openai".into(),
        stt_model: "whisper-1".into(),
        stt_configured: true,
        tts_provider: "openai".into(),
        tts_model: "gpt-4o-mini-tts".into(),
        tts_voice: "alloy".into(),
        tts_format: "wav".into(),
        tts_configured: true,
        realtime_capture_available: false,
    });

    let backend = TestBackend::new(110, 26);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| ui::render(frame, &app)).unwrap();
    let output = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(output.contains("Voice Center"));
    assert!(output.contains("whisper-1"));
    assert!(output.contains("gpt-4o-mini-tts"));
    assert!(output.contains("not a Codex backend"));
    assert!(output.contains("no capability-gated microphone recorder"));
}

#[test]
fn approvals_have_one_explicit_terminal_decision() {
    let mut app = app();
    app.add_approvals(vec![ApprovalRequest {
        id: "approval-1".into(),
        verb: "fs.write".into(),
        scope: json!({"path": "/home/claw/output"}),
        reason: "Write requested output".into(),
        status: "pending".into(),
        session: "ses_001953abcdef0_123456789abc".into(),
        requested_at: 1_767_225_600,
        requester: Some("Claw Agent".into()),
        risk: Some("high".into()),
        decided_at: None,
        duration: None,
        note: None,
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
fn approval_runtime_distinguishes_missing_pkexec_and_helper() {
    let root = tempfile::tempdir().unwrap();
    let pkexec = root.path().join("pkexec");
    let helper = root.path().join("claw-approval-helper");

    let error =
        crate::agent::terminal::backend::ensure_approval_runtime(&pkexec, &helper).unwrap_err();
    assert!(error.contains("pkexec"));

    std::fs::write(&pkexec, b"fixture").unwrap();
    let error =
        crate::agent::terminal::backend::ensure_approval_runtime(&pkexec, &helper).unwrap_err();
    assert!(error.contains("claw-approval-helper"));

    std::fs::write(&helper, b"fixture").unwrap();
    crate::agent::terminal::backend::ensure_approval_runtime(&pkexec, &helper).unwrap();
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
fn activity_commands_use_one_palette_entry() {
    let suggestions = commands::suggestions("/activi");
    assert_eq!(suggestions.len(), 1);
    assert_eq!(suggestions[0].0, "/activity");
    assert_eq!(
        commands::completion("/activi", 0),
        Some("/activity ".into())
    );
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

#[test]
fn destructive_history_actions_require_explicit_confirmation() {
    let mut app = app();
    app.confirm_rewind(2);
    assert!(app
        .confirmation
        .as_ref()
        .unwrap()
        .body
        .contains("admitted effects are not rolled back"));
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
    assert!(output.contains("Rewind 2 user turn"));
    assert!(output.contains("Rewind replay"));

    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::None
    );
    assert!(app.confirmation.is_none());

    app.confirm_archive();
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('y'),
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::Confirm(ConfirmationAction::Archive)
    );
}

#[test]
fn durable_task_picker_opens_redacted_details_and_exact_actions() {
    let mut app = app();
    app.open_task_picker(vec![TaskSummary {
        id: "task-1".into(),
        title: "Inspect durable result".into(),
        status: "error".into(),
        created_at: "2026-01-01T00:00:00Z".into(),
        session_id: Some("ses_001953abcdef0_123456789abc".into()),
        activity_id: None,
        workspace: Some("/home/claw/project".into()),
        after_task_id: None,
        waiting_on: 0,
        cancel_requested: false,
        error: Some("failed".into()),
    }]);
    assert_eq!(
        app.take_picker_selection(),
        Some(PickerSelection::Task("task-1".into()))
    );

    let mut failed = job("error");
    failed.error = Some("fixture failure\u{1b}".into());
    failed.finished_at = Some("2026-01-01T00:00:02Z".into());
    app.open_task_detail(failed);
    assert!(!app
        .task_detail
        .as_ref()
        .unwrap()
        .error
        .as_deref()
        .unwrap()
        .contains('\u{1b}'));
    let backend = TestBackend::new(100, 30);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| ui::render(frame, &app)).unwrap();
    let output = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(output.contains("Durable task"));
    assert!(output.contains("[r] Retry"));
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('r'),
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::TaskRetry("task-1".into())
    );

    app.open_task_detail(job("running"));
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('c'),
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::TaskCancel("task-1".into())
    );
}

#[test]
fn approval_center_keeps_history_read_only_and_pending_decisions_exact() {
    let pending = ApprovalRequest {
        id: "approval-center-1".into(),
        verb: "fs.write".into(),
        scope: json!({"kind": "path", "value": "/home/claw/output"}),
        reason: "Write the requested output".into(),
        status: "pending".into(),
        session: "ses_001953abcdef0_123456789abc".into(),
        requested_at: 1_767_225_600,
        requester: Some("Claw Agent".into()),
        risk: Some("high".into()),
        decided_at: None,
        duration: None,
        note: None,
    };
    let mut app = app();
    app.open_approval_picker(vec![pending.clone()]);
    assert_eq!(
        app.take_picker_selection(),
        Some(PickerSelection::Approval(pending.clone()))
    );
    app.open_approval_detail(pending);
    let backend = TestBackend::new(100, 30);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| ui::render(frame, &app)).unwrap();
    let output = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(output.contains("Approval center"));
    assert!(output.contains("[a] Approve once"));
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('d'),
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::ApprovalReview("approval-center-1".into(), ReviewDecision::Deny)
    );

    app.approval_detail.as_mut().unwrap().status = "approved".into();
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('a'),
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::None
    );
}

#[test]
fn notification_inbox_uses_durable_mutations_and_preferences() {
    let notification = NotificationItem {
        id: "notification-1".into(),
        source: "agent".into(),
        kind: "task.completed".into(),
        severity: "info".into(),
        title: "Task completed".into(),
        body: "The durable Agent task completed.".into(),
        delivery_policy: "immediate".into(),
        state: "unread".into(),
        occurrences: 1,
        created_at_ms: 1_767_225_600_000,
        updated_at_ms: 1_767_225_600_000,
        task_id: Some("task-1".into()),
        session_id: Some("ses_001953abcdef0_123456789abc".into()),
        job_id: None,
        actions: vec![NotificationAction {
            label: "Open task".into(),
            uri: "clawos://task/task-1".into(),
        }],
        deliveries: vec![NotificationDelivery {
            channel: "web".into(),
            state: "delivered".into(),
            attempts: 1,
            last_error_code: None,
        }],
    };
    let mut app = app();
    app.open_notification_picker(NotificationPage {
        notifications: vec![notification.clone()],
        unread: 1,
    });
    assert_eq!(
        app.take_picker_selection(),
        Some(PickerSelection::Notification(notification.clone()))
    );
    app.open_notification_detail(notification);
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('m'),
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::MutateNotification(
            "notification-1".into(),
            crate::agent::terminal::backend::NotificationMutation::Read
        )
    );

    app.open_notification_preferences(NotificationPreferences {
        web_enabled: true,
        desktop_enabled: true,
        ntfy_enabled: false,
        web_min_severity: "info".into(),
        desktop_min_severity: "info".into(),
        ntfy_min_severity: "warning".into(),
        muted_kinds: Vec::new(),
        dnd_start_minute_utc: None,
        dnd_end_minute_utc: None,
        critical_bypasses_dnd: true,
        retention_days: 30,
        ntfy_server: "https://ntfy.sh".into(),
        ntfy_topic: None,
    });
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('w'),
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::NotificationPreference(NotificationPreferenceAction::ToggleWeb)
    );
}

#[test]
fn activity_detail_preserves_explicit_lifecycle_and_completion() {
    let activity = Activity {
        id: "00000000-0000-4000-8000-000000000001".into(),
        title: "Release v2".into(),
        goal: "Publish the reviewed release".into(),
        completion_criteria: "The user confirms publication".into(),
        boundaries: "Ask before publishing".into(),
        resources: vec![ActivityResource {
            label: "Draft".into(),
            reference: "app://files/document/release".into(),
        }],
        state: "active".into(),
        completion_note: None,
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
    };
    let mut app = app();
    app.open_activity_detail(ActivityDetail {
        activity: activity.clone(),
        jobs: Vec::new(),
        sessions: Vec::new(),
    });
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('r'),
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::ActivityRun(activity.id.clone())
    );
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('p'),
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::ActivityTransition(activity.id.clone(), "paused")
    );

    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('c'),
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::None
    );
    assert_eq!(app.input, format!("/activity-complete {} | ", activity.id));
    app.confirm_activity_complete(activity.id.clone(), "Reviewed by the user".into());
    assert!(matches!(
        app.take_confirmation(),
        Some(ConfirmationAction::ActivityComplete { id, note })
            if id == activity.id && note == "Reviewed by the user"
    ));

    app.open_activity_attention(ActivityAttention {
        activity_id: activity.id,
        activity_state: "active".into(),
        presentation: "{\"counts\":{\"running\":1}}".into(),
    });
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
    assert!(output.contains("Activity attention"));
    assert!(output.contains("running"));
}

#[test]
fn activity_controls_expose_exact_revisions_before_mutation() {
    let mut app = app();
    app.open_activity_controls(ActivityControls {
        activity_id: "00000000-0000-4000-8000-000000000001".into(),
        execution_limits: Some(json!({
            "activity_id": "00000000-0000-4000-8000-000000000001",
            "revision": 3,
            "enabled": true,
            "limits": {
                "max_attempts": 10,
                "max_turns_per_attempt": 5,
                "expires_at": "2027-01-01T00:00:00Z",
            },
            "used_attempts": 2,
        })),
        monetary_budget: None,
        scheduling_policy: Some(json!({
            "activity_id": "00000000-0000-4000-8000-000000000001",
            "revision": 2,
            "priority": "foreground",
        })),
        capability_policy: Some(json!({
            "activity_id": "00000000-0000-4000-8000-000000000001",
            "revision": 4,
            "enabled": false,
            "rules": [],
        })),
    });
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('l'),
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::None
    );
    assert_eq!(
        app.input,
        "/activity-limits-enable 00000000-0000-4000-8000-000000000001 off 3"
    );
}

#[test]
fn activity_evidence_and_previews_remain_non_authoritative() {
    let mut app = app();
    app.open_activity_evidence(ActivityEvidence {
        activity_id: "00000000-0000-4000-8000-000000000001".into(),
        presentation: "{\"objects\":[],\"receipts\":[],\"staged_file_plans\":[]}".into(),
    });
    assert_eq!(
        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('p'),
                crossterm::event::KeyModifiers::NONE,
            ),
        ),
        InputAction::None
    );
    assert_eq!(
        app.input,
        "/activity-preview 00000000-0000-4000-8000-000000000001 "
    );

    app.open_activity_operation_preview(ActivityOperationPreview {
        activity_id: "00000000-0000-4000-8000-000000000001".into(),
        presentation: json!({
            "authorization_checked": false,
            "executed": false,
            "effects_confirmed": false,
        })
        .to_string(),
    });
    let backend = TestBackend::new(110, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| ui::render(frame, &app)).unwrap();
    let output = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(output.contains("Metadata preview only"));
    assert!(output.contains("effects_confirmed"));

    app.open_activity_review(ActivityReview {
        activity_id: "00000000-0000-4000-8000-000000000001".into(),
        presentation: json!({
            "authority": "Review is presentation only.",
            "staged_file_plans": {
                "reported_receipts": [{
                    "preview": "--- before\n+++ after"
                }]
            }
        })
        .to_string(),
    });
    terminal.draw(|frame| ui::render(frame, &app)).unwrap();
    let output = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(output.contains("App-reported proposals only"));
    assert!(output.contains("staged_file_plans"));
}
