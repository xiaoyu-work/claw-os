use super::*;
use crate::agent::terminal::backend::{
    Activity, ActivityAttention, ActivityControlPolicy, ActivityControls, ActivityDetail,
    ActivityResource, ApprovalRequest, BackendInfo, Conversation, ConversationMessage,
    ConversationSummary, Job, NotificationAction, NotificationDelivery, NotificationItem,
    NotificationPage, NotificationPreferences, TaskSummary,
};
use crate::agent::terminal::commands::{parse as parse_command, Command, NotificationChannel};
use crate::agent::terminal::state::{
    clean_text, App, ApprovalStatus, ConfirmationAction, EntryKind, NotificationPreferenceAction,
    PickerSelection, ToolStatus,
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
        activity_id: None,
        prompt: "Run the terminal test".into(),
        status: status.into(),
        created_at: "2026-01-01T00:00:00Z".into(),
        started_at: Some("2026-01-01T00:00:01Z".into()),
        finished_at: None,
        response: None,
        error: None,
        requested_model: Some("claw-model".into()),
        provider: Some("ollama".into()),
        model: Some("claw-model".into()),
        turns_used: None,
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
        waiting_on: 0,
        cancel_requested: false,
        error: Some("failed".into()),
    }]);
    assert_eq!(
        app.take_picker_selection(),
        Some(PickerSelection::Task("task-1".into()))
    );

    let mut failed = job("error");
    failed.error = Some("token=******".into());
    failed.finished_at = Some("2026-01-01T00:00:02Z".into());
    app.open_task_detail(failed);
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
    assert!(!output.contains("******"));
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
