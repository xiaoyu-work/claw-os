//! Claw-owned full-screen terminal presentation for the shared Agent backend.

mod backend;
mod commands;
mod models;
mod presentation;
mod state;
mod stream;
mod ui;

use std::io::{self, IsTerminal, Write};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use serde_json::Value;
use tokio::sync::mpsc;

use self::backend::{
    Backend, BrokerBackend, InitialConnectionError, NotificationMutation, ReviewDecision,
};
use self::state::{
    App, ConfirmationAction, NotificationPreferenceAction, PickerSelection, RunStatus,
};
use self::stream::RuntimeEvent;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Auto,
    Plain,
    Tui,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ChatOptions {
    mode: Mode,
    pub session_id: Option<String>,
    pub no_stream: bool,
    pub no_memory: bool,
    pub show_tools: bool,
    pub max_turns: Option<u32>,
}

impl ChatOptions {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--app" => {
                    return Err(
                        "`cos agent chat` is the Claw Agent UI and does not accept --app. \
                         For App-gated calls use `cos ai chat --app <id>`."
                            .into(),
                    )
                }
                "--plain" => options.set_mode(Mode::Plain)?,
                "--tui" => options.set_mode(Mode::Tui)?,
                "--session" => {
                    options.session_id = Some(
                        args.next()
                            .filter(|value| !value.trim().is_empty())
                            .ok_or("--session needs a non-empty id")?
                            .clone(),
                    );
                }
                "--no-stream" => options.no_stream = true,
                "--no-memory" => options.no_memory = true,
                "--show-tools" => options.show_tools = true,
                "--max-turns" => {
                    let value = args.next().ok_or("--max-turns needs <n>")?;
                    let value = value
                        .parse::<u32>()
                        .map_err(|error| format!("--max-turns: {error}"))?;
                    if value == 0 {
                        return Err("--max-turns must be greater than zero".into());
                    }
                    options.max_turns = Some(value);
                }
                "--" => {
                    return Err(
                        "the Claw TUI does not accept another frontend's command-line arguments"
                            .into(),
                    )
                }
                other => return Err(format!("unknown flag for `chat`: {other}")),
            }
        }
        if options.mode == Mode::Tui && (options.no_stream || options.show_tools) {
            return Err("--no-stream and --show-tools belong to --plain chat".into());
        }
        Ok(options)
    }

    fn set_mode(&mut self, mode: Mode) -> Result<(), String> {
        if self.mode != Mode::Auto && self.mode != mode {
            return Err("--plain and --tui cannot be combined".into());
        }
        self.mode = mode;
        Ok(())
    }

    pub fn use_tui(&self, interactive: bool, dumb_terminal: bool) -> Result<bool, String> {
        if self.mode == Mode::Plain || self.no_stream || self.show_tools {
            return Ok(false);
        }
        if !interactive || dumb_terminal {
            if self.mode == Mode::Tui {
                return Err(
                    "the Claw Agent TUI needs an interactive terminal; use --plain for pipes or TERM=dumb"
                        .into(),
                );
            }
            return Ok(false);
        }
        Ok(true)
    }
}

pub(super) fn interactive_terminal() -> bool {
    std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::io::stderr().is_terminal()
}

pub(super) fn dumb_terminal() -> bool {
    std::env::var_os("TERM").is_some_and(|value| value == "dumb")
}

#[cfg(unix)]
pub(super) fn run(options: ChatOptions) -> Result<Value, String> {
    if unsafe { libc::geteuid() } == 0 {
        return Err(crate::agentd::spawn::ROOT_OWNER_REFUSAL.into());
    }
    let config = crate::config::current_snapshot();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("terminal runtime: {error}"))?;
    runtime.block_on(async move {
        let backend = Arc::new(BrokerBackend::new(config, unsafe { libc::geteuid() }).await?);
        run_with_backend(backend, options).await
    })
}

#[cfg(not(unix))]
pub(super) fn run(_options: ChatOptions) -> Result<Value, String> {
    Err("the Claw Agent TUI requires Linux or WSL".into())
}

#[derive(Debug, Eq, PartialEq)]
enum InputAction {
    None,
    Submit(String),
    Cancel,
    Review(ReviewDecision),
    Picker(PickerSelection),
    Confirm(ConfirmationAction),
    TaskCancel(String),
    TaskRetry(String),
    FileMentions,
    ApprovalReview(String, ReviewDecision),
    MutateNotification(String, NotificationMutation),
    NotificationPreference(NotificationPreferenceAction),
    ActivityRun(String),
    ActivityTransition(String, &'static str),
    ActivityAttention(String),
    ActivityControls(String),
    ActivityEvidence(String),
    ActivityReview(String),
    OpenAgentHooks,
    ToggleAgentHook(&'static str),
    OpenMcpOverview,
    OpenExtensionsOverview,
    OpenUsageOverview(backend::UsagePeriod),
    OpenDebugOverview,
    OpenAccountOverview,
    Quit,
}

async fn run_with_backend(
    backend: Arc<dyn Backend>,
    options: ChatOptions,
) -> Result<Value, String> {
    let conversation = connect_initial_conversation(backend.as_ref(), options.session_id.as_deref())
        .await?;
    let mut app = App::new(backend.info().clone(), conversation);
    app.set_task_defaults(!options.no_memory, options.max_turns);
    let (runtime_tx, mut runtime_rx) = mpsc::unbounded_channel();
    stream::restore_conversation(&mut app, backend.clone(), runtime_tx.clone()).await;
    let mut terminal = TerminalSession::enter().map_err(|error| error.to_string())?;
    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));

    while !app.should_quit {
        terminal
            .update_title(app.terminal_title().as_deref())
            .map_err(|error| error.to_string())?;
        terminal
            .terminal
            .draw(|frame| ui::render(frame, &app))
            .map_err(|error| error.to_string())?;
        tokio::select! {
            event = input.next() => {
                let Some(event) = event else {
                    app.should_quit = true;
                    continue;
                };
                match event.map_err(|error| error.to_string())? {
                    Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                        let action = handle_key(&mut app, key);
                        apply_input_action(
                            action,
                            &mut app,
                            backend.clone(),
                            runtime_tx.clone(),
                        ).await;
                        if let Some(snapshot) = app.take_raw_scrollback() {
                            if let Err(error) = terminal.publish_scrollback(&snapshot) {
                                app.push_error(&format!(
                                    "Could not publish terminal scrollback: {error}"
                                ));
                            }
                        }
                    }
                    Event::Paste(value) => {
                        if app.current_approval().is_none()
                            && app.confirmation.is_none()
                            && app.task_detail.is_none()
                            && app.approval_detail.is_none()
                            && app.notification_detail.is_none()
                            && app.notification_preferences.is_none()
                            && app.activity_detail.is_none()
                            && app.activity_attention.is_none()
                            && app.activity_controls.is_none()
                            && app.activity_evidence.is_none()
                            && app.activity_review.is_none()
                            && app.activity_operation_preview.is_none()
                            && !app.task_controls_open
                            && !app.appearance_open
                            && !app.agents_open
                            && app.platform_overview.is_none()
                            && !app.memory_center_open
                            && app.agent_hook_settings.is_none()
                            && app.mcp_overview.is_none()
                            && app.extensions_overview.is_none()
                            && app.usage_overview.is_none()
                            && app.debug_overview.is_none()
                            && app.account_overview.is_none()
                            && app.picker.is_none()
                        {
                            app.insert_text(&value.replace("\r\n", "\n").replace('\r', "\n"));
                        }
                    }
                    Event::Resize(_, _) | Event::FocusGained | Event::FocusLost | Event::Mouse(_) => {}
                    Event::Key(_) => {}
                }
            }
            Some(event) = runtime_rx.recv() => {
                let mut next_task = None;
                match event {
                    RuntimeEvent::Record(record) => {
                        match presentation::apply_record(&mut app, &record) {
                            Ok(outcome) => {
                                if !outcome.approvals.is_empty() {
                                    match backend.approvals(&outcome.approvals).await {
                                        Ok(approvals) => app.add_approvals(approvals),
                                        Err(error) => app.push_error(&error),
                                    }
                                }
                                if outcome.resumed {
                                    app.status = RunStatus::Working;
                                }
                            }
                            Err(error) => app.push_error(&error),
                        }
                    }
                    RuntimeEvent::Finished(job) => {
                        app.finish_task(&job);
                        next_task = app.queued_tasks.pop_front();
                    }
                    RuntimeEvent::Reconnecting(error) => {
                        if app.status != RunStatus::Cancelling {
                            app.status = RunStatus::Reconnecting;
                        }
                        app.push_system(&format!(
                            "Broker connection interrupted; reconnecting to the durable task: {error}"
                        ));
                    }
                    RuntimeEvent::Reconnected => {
                        if app.status == RunStatus::Reconnecting {
                            app.status = if app.current_approval().is_some() {
                                RunStatus::WaitingApproval
                            } else {
                                RunStatus::Working
                            };
                        }
                        app.push_system("Reconnected to the durable task.");
                    }
                    RuntimeEvent::Failed(error) => {
                        app.push_error(&format!(
                            "{error}. The durable task may still be running; use /session to retain its conversation id."
                        ));
                        app.active_task = None;
                        app.active_workspace = None;
                        app.status = RunStatus::Ready;
                    }
                }
                if let Some(job) = next_task {
                    stream::attach_queued(
                        &mut app,
                        backend.clone(),
                        runtime_tx.clone(),
                        job,
                    );
                }
            }
            _ = tick.tick() => app.tick(),
        }
    }
    drop(terminal);
    Ok(Value::Null)
}

async fn connect_initial_conversation(
    backend: &dyn Backend,
    session_id: Option<&str>,
) -> Result<backend::Conversation, String> {
    const MAX_ATTEMPTS: u32 = 60;
    for attempt in 0..=MAX_ATTEMPTS {
        match backend.initial_conversation(session_id).await {
            Ok(conversation) => {
                if attempt > 0 {
                    eprintln!("Claw broker connection restored.");
                }
                return Ok(conversation);
            }
            Err(InitialConnectionError::Retryable(error)) if attempt < MAX_ATTEMPTS => {
                if attempt == 0 {
                    eprintln!("Claw broker is unavailable; retrying for up to 60 seconds: {error}");
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(InitialConnectionError::Retryable(error)) => {
                return Err(format!(
                    "Claw broker did not become available after {MAX_ATTEMPTS} retries: {error}"
                ));
            }
            Err(InitialConnectionError::Fatal(error)) => return Err(error),
        }
    }
    unreachable!("bounded startup retry loop always returns")
}

fn handle_key(app: &mut App, key: KeyEvent) -> InputAction {
    if app.current_approval().is_some() {
        return match key.code {
            KeyCode::Char('a') | KeyCode::Char('A') => {
                InputAction::Review(ReviewDecision::ApproveOnce)
            }
            KeyCode::Char('d') | KeyCode::Char('D') => InputAction::Review(ReviewDecision::Deny),
            KeyCode::Esc => InputAction::Cancel,
            _ => InputAction::None,
        };
    }
    if app.confirmation.is_some() {
        return match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => app
                .take_confirmation()
                .map_or(InputAction::None, InputAction::Confirm),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                app.close_confirmation();
                InputAction::None
            }
            _ => InputAction::None,
        };
    }
    if app.appearance_open {
        return match key.code {
            KeyCode::Esc => {
                app.close_appearance();
                InputAction::None
            }
            KeyCode::Char('t') | KeyCode::Char('T') => {
                app.cycle_theme();
                InputAction::None
            }
            KeyCode::Char('h') | KeyCode::Char('H') => {
                app.toggle_terminal_title();
                InputAction::None
            }
            KeyCode::Char('s') | KeyCode::Char('S') => {
                app.toggle_statusline();
                InputAction::None
            }
            KeyCode::Char('k') | KeyCode::Char('K') => {
                app.toggle_vim_mode();
                InputAction::None
            }
            _ => InputAction::None,
        };
    }
    if app.agents_open {
        return match key.code {
            KeyCode::Esc => {
                app.close_agents();
                InputAction::None
            }
            KeyCode::Up | KeyCode::PageUp => {
                app.agents_scroll = app.agents_scroll.saturating_add(5);
                InputAction::None
            }
            KeyCode::Down | KeyCode::PageDown => {
                app.agents_scroll = app.agents_scroll.saturating_sub(5);
                InputAction::None
            }
            _ => InputAction::None,
        };
    }
    if app.memory_center_open {
        return match key.code {
            KeyCode::Esc => {
                app.close_memory_center();
                InputAction::None
            }
            KeyCode::Char('m') | KeyCode::Char('M') => {
                app.toggle_task_memory();
                InputAction::None
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                app.confirm_memory_reset();
                InputAction::None
            }
            _ => InputAction::None,
        };
    }
    if app.agent_hook_settings.is_some() {
        return match key.code {
            KeyCode::Esc => {
                app.close_agent_hooks();
                InputAction::None
            }
            KeyCode::Char('l') | KeyCode::Char('L') => InputAction::ToggleAgentHook("logging"),
            KeyCode::Char('a') | KeyCode::Char('A') => InputAction::ToggleAgentHook("audit"),
            KeyCode::Char('c') | KeyCode::Char('C') => InputAction::ToggleAgentHook("checkpoint"),
            _ => InputAction::None,
        };
    }
    if app.mcp_overview.is_some() {
        return match key.code {
            KeyCode::Esc => {
                app.close_mcp_overview();
                InputAction::None
            }
            KeyCode::Up | KeyCode::PageUp => {
                app.mcp_scroll = app.mcp_scroll.saturating_add(5);
                InputAction::None
            }
            KeyCode::Down | KeyCode::PageDown => {
                app.mcp_scroll = app.mcp_scroll.saturating_sub(5);
                InputAction::None
            }
            _ => InputAction::None,
        };
    }
    if app.extensions_overview.is_some() {
        return match key.code {
            KeyCode::Esc => {
                app.close_extensions_overview();
                InputAction::None
            }
            KeyCode::Up | KeyCode::PageUp => {
                app.extensions_scroll = app.extensions_scroll.saturating_add(5);
                InputAction::None
            }
            KeyCode::Down | KeyCode::PageDown => {
                app.extensions_scroll = app.extensions_scroll.saturating_sub(5);
                InputAction::None
            }
            _ => InputAction::None,
        };
    }
    if app.usage_overview.is_some() {
        return match key.code {
            KeyCode::Esc => {
                app.close_usage_overview();
                InputAction::None
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                InputAction::OpenUsageOverview(backend::UsagePeriod::Daily)
            }
            KeyCode::Char('w') | KeyCode::Char('W') => {
                InputAction::OpenUsageOverview(backend::UsagePeriod::Weekly)
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                InputAction::OpenUsageOverview(backend::UsagePeriod::Cumulative)
            }
            KeyCode::Up | KeyCode::PageUp => {
                app.usage_scroll = app.usage_scroll.saturating_add(5);
                InputAction::None
            }
            KeyCode::Down | KeyCode::PageDown => {
                app.usage_scroll = app.usage_scroll.saturating_sub(5);
                InputAction::None
            }
            _ => InputAction::None,
        };
    }
    if app.debug_overview.is_some() {
        return match key.code {
            KeyCode::Esc => {
                app.close_debug_overview();
                InputAction::None
            }
            KeyCode::Up | KeyCode::PageUp => {
                app.debug_scroll = app.debug_scroll.saturating_add(5);
                InputAction::None
            }
            KeyCode::Down | KeyCode::PageDown => {
                app.debug_scroll = app.debug_scroll.saturating_sub(5);
                InputAction::None
            }
            _ => InputAction::None,
        };
    }
    if app.account_overview.is_some() {
        return match key.code {
            KeyCode::Esc => {
                app.close_account_overview();
                InputAction::None
            }
            KeyCode::Char('l') | KeyCode::Char('L') => {
                app.confirm_account_logout();
                InputAction::None
            }
            _ => InputAction::None,
        };
    }
    if app.platform_overview.is_some() {
        return match key.code {
            KeyCode::Esc => {
                app.close_platform_overview();
                InputAction::None
            }
            KeyCode::Up | KeyCode::PageUp => {
                app.platform_scroll = app.platform_scroll.saturating_add(5);
                InputAction::None
            }
            KeyCode::Down | KeyCode::PageDown => {
                app.platform_scroll = app.platform_scroll.saturating_sub(5);
                InputAction::None
            }
            KeyCode::Char('m') | KeyCode::Char('M') => {
                app.open_memory_center();
                InputAction::None
            }
            KeyCode::Char('h') | KeyCode::Char('H') => InputAction::OpenAgentHooks,
            KeyCode::Char('c') | KeyCode::Char('C') => InputAction::OpenMcpOverview,
            KeyCode::Char('e') | KeyCode::Char('E') => InputAction::OpenExtensionsOverview,
            KeyCode::Char('u') | KeyCode::Char('U') => {
                InputAction::OpenUsageOverview(backend::UsagePeriod::Cumulative)
            }
            KeyCode::Char('d') | KeyCode::Char('D') => InputAction::OpenDebugOverview,
            KeyCode::Char('a') | KeyCode::Char('A') => InputAction::OpenAccountOverview,
            _ => InputAction::None,
        };
    }
    if app.task_controls_open {
        return match key.code {
            KeyCode::Esc => {
                app.close_task_controls();
                InputAction::None
            }
            KeyCode::Char('m') | KeyCode::Char('M') => {
                app.toggle_task_memory();
                InputAction::None
            }
            KeyCode::Char('t') | KeyCode::Char('T') => {
                app.cycle_task_max_turns();
                InputAction::None
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                app.cycle_task_reasoning_effort();
                InputAction::None
            }
            KeyCode::Char('p') | KeyCode::Char('P') => {
                app.toggle_task_plan_mode();
                InputAction::None
            }
            KeyCode::Char('o') | KeyCode::Char('O') => {
                app.close_task_controls();
                app.open_model_picker();
                InputAction::None
            }
            _ => InputAction::None,
        };
    }
    if let Some(approval) = &app.approval_detail {
        let approval_id = approval.id.clone();
        let pending = approval.status == "pending";
        return match key.code {
            KeyCode::Esc => {
                app.close_approval_detail();
                InputAction::None
            }
            KeyCode::Up | KeyCode::PageUp => {
                app.approval_detail_scroll = app.approval_detail_scroll.saturating_add(5);
                InputAction::None
            }
            KeyCode::Down | KeyCode::PageDown => {
                app.approval_detail_scroll = app.approval_detail_scroll.saturating_sub(5);
                InputAction::None
            }
            KeyCode::Char('a') | KeyCode::Char('A') if pending => {
                InputAction::ApprovalReview(approval_id, ReviewDecision::ApproveOnce)
            }
            KeyCode::Char('d') | KeyCode::Char('D') if pending => {
                InputAction::ApprovalReview(approval_id, ReviewDecision::Deny)
            }
            _ => InputAction::None,
        };
    }
    if app.notification_preferences.is_some() {
        return match key.code {
            KeyCode::Esc => {
                app.close_notification_preferences();
                InputAction::None
            }
            KeyCode::Up | KeyCode::PageUp => {
                app.notification_preferences_scroll =
                    app.notification_preferences_scroll.saturating_add(5);
                InputAction::None
            }
            KeyCode::Down | KeyCode::PageDown => {
                app.notification_preferences_scroll =
                    app.notification_preferences_scroll.saturating_sub(5);
                InputAction::None
            }
            KeyCode::Char('w') | KeyCode::Char('W') => {
                InputAction::NotificationPreference(NotificationPreferenceAction::ToggleWeb)
            }
            KeyCode::Char('e') | KeyCode::Char('E') => {
                InputAction::NotificationPreference(NotificationPreferenceAction::ToggleDesktop)
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                InputAction::NotificationPreference(NotificationPreferenceAction::ToggleNtfy)
            }
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                InputAction::NotificationPreference(NotificationPreferenceAction::ToggleDnd)
            }
            _ => InputAction::None,
        };
    }
    if let Some(notification) = &app.notification_detail {
        let id = notification.id.clone();
        let state = notification.state.clone();
        return match key.code {
            KeyCode::Esc => {
                app.close_notification_detail();
                InputAction::None
            }
            KeyCode::Up | KeyCode::PageUp => {
                app.notification_detail_scroll = app.notification_detail_scroll.saturating_add(5);
                InputAction::None
            }
            KeyCode::Down | KeyCode::PageDown => {
                app.notification_detail_scroll = app.notification_detail_scroll.saturating_sub(5);
                InputAction::None
            }
            KeyCode::Char('m') | KeyCode::Char('M') if state == "unread" => {
                InputAction::MutateNotification(id, NotificationMutation::Read)
            }
            KeyCode::Char('a') | KeyCode::Char('A')
                if matches!(state.as_str(), "unread" | "read") =>
            {
                InputAction::MutateNotification(id, NotificationMutation::Acknowledge)
            }
            KeyCode::Char('d') | KeyCode::Char('D') if state != "dismissed" => {
                InputAction::MutateNotification(id, NotificationMutation::Dismiss)
            }
            _ => InputAction::None,
        };
    }
    if app.activity_attention.is_some() {
        return match key.code {
            KeyCode::Esc => {
                app.close_activity_attention();
                InputAction::None
            }
            KeyCode::Up | KeyCode::PageUp => {
                app.activity_attention_scroll = app.activity_attention_scroll.saturating_add(5);
                InputAction::None
            }
            KeyCode::Down | KeyCode::PageDown => {
                app.activity_attention_scroll = app.activity_attention_scroll.saturating_sub(5);
                InputAction::None
            }
            _ => InputAction::None,
        };
    }
    if let Some(controls) = app.activity_controls.clone() {
        return match key.code {
            KeyCode::Esc => {
                app.close_activity_controls();
                InputAction::None
            }
            KeyCode::Up | KeyCode::PageUp => {
                app.activity_controls_scroll = app.activity_controls_scroll.saturating_add(5);
                InputAction::None
            }
            KeyCode::Down | KeyCode::PageDown => {
                app.activity_controls_scroll = app.activity_controls_scroll.saturating_sub(5);
                InputAction::None
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                InputAction::ActivityControls(controls.activity_id)
            }
            KeyCode::Char('l') | KeyCode::Char('L') => {
                let command = match (
                    controls.execution_revision(),
                    controls.execution_enabled(),
                ) {
                    (Some(revision), Some(enabled)) => format!(
                        "/activity-limits-enable {} {} {}",
                        controls.activity_id,
                        if enabled { "off" } else { "on" },
                        revision
                    ),
                    _ => format!(
                        "/activity-limits-set {} new | {{\"max_attempts\":10,\"max_turns_per_attempt\":5,\"expires_at\":\"YYYY-MM-DDTHH:MM:SSZ\"}}",
                        controls.activity_id
                    ),
                };
                app.close_activity_controls();
                app.prefill_input(command);
                InputAction::None
            }
            KeyCode::Char('b') | KeyCode::Char('B') => {
                let command = match (
                    controls.monetary_revision(),
                    controls.monetary_enabled(),
                ) {
                    (Some(revision), Some(enabled)) => format!(
                        "/activity-budget-enable {} {} {}",
                        controls.activity_id,
                        if enabled { "off" } else { "on" },
                        revision
                    ),
                    _ => format!(
                        "/activity-budget-set {} new | {{\"currency\":\"USD\",\"max_total_microusd\":5000000,\"input_microusd_per_million_tokens\":250000,\"output_microusd_per_million_tokens\":1000000,\"max_output_tokens_per_turn\":4096}}",
                        controls.activity_id
                    ),
                };
                app.close_activity_controls();
                app.prefill_input(command);
                InputAction::None
            }
            KeyCode::Char('p') | KeyCode::Char('P') => {
                let (priority, revision) = match (
                    controls.scheduling_priority(),
                    controls.scheduling_revision(),
                ) {
                    (Some("foreground"), Some(revision)) => ("standard", revision.to_string()),
                    (Some("standard"), Some(revision)) => ("background", revision.to_string()),
                    (Some("background"), Some(revision)) => ("foreground", revision.to_string()),
                    _ => ("foreground", "new".into()),
                };
                app.close_activity_controls();
                app.prefill_input(format!(
                    "/activity-priority {} {} {}",
                    controls.activity_id, priority, revision
                ));
                InputAction::None
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                let command = match (
                    controls.capability_revision(),
                    controls.capability_enabled(),
                ) {
                    (Some(revision), Some(enabled)) => format!(
                        "/activity-capability-enable {} {} {}",
                        controls.activity_id,
                        if enabled { "off" } else { "on" },
                        revision
                    ),
                    _ => format!(
                        "/activity-capability-set {} new | {{\"rules\":[]}}",
                        controls.activity_id
                    ),
                };
                app.close_activity_controls();
                app.prefill_input(command);
                InputAction::None
            }
            _ => InputAction::None,
        };
    }
    if let Some(preview) = &app.activity_operation_preview {
        let activity_id = preview.activity_id.clone();
        return match key.code {
            KeyCode::Esc => {
                app.close_activity_operation_preview();
                InputAction::None
            }
            KeyCode::Up | KeyCode::PageUp => {
                app.activity_operation_preview_scroll =
                    app.activity_operation_preview_scroll.saturating_add(5);
                InputAction::None
            }
            KeyCode::Down | KeyCode::PageDown => {
                app.activity_operation_preview_scroll =
                    app.activity_operation_preview_scroll.saturating_sub(5);
                InputAction::None
            }
            KeyCode::Char('e') | KeyCode::Char('E') => InputAction::ActivityEvidence(activity_id),
            _ => InputAction::None,
        };
    }
    if let Some(review) = &app.activity_review {
        let activity_id = review.activity_id.clone();
        return match key.code {
            KeyCode::Esc => {
                app.close_activity_review();
                InputAction::None
            }
            KeyCode::Up | KeyCode::PageUp => {
                app.activity_review_scroll = app.activity_review_scroll.saturating_add(5);
                InputAction::None
            }
            KeyCode::Down | KeyCode::PageDown => {
                app.activity_review_scroll = app.activity_review_scroll.saturating_sub(5);
                InputAction::None
            }
            KeyCode::Char('e') | KeyCode::Char('E') => InputAction::ActivityEvidence(activity_id),
            _ => InputAction::None,
        };
    }
    if let Some(evidence) = &app.activity_evidence {
        let activity_id = evidence.activity_id.clone();
        return match key.code {
            KeyCode::Esc => {
                app.close_activity_evidence();
                InputAction::None
            }
            KeyCode::Up | KeyCode::PageUp => {
                app.activity_evidence_scroll = app.activity_evidence_scroll.saturating_add(5);
                InputAction::None
            }
            KeyCode::Down | KeyCode::PageDown => {
                app.activity_evidence_scroll = app.activity_evidence_scroll.saturating_sub(5);
                InputAction::None
            }
            KeyCode::Char('p') | KeyCode::Char('P') => {
                app.close_activity_evidence();
                app.prefill_input(format!("/activity-preview {activity_id} "));
                InputAction::None
            }
            KeyCode::Char('v') | KeyCode::Char('V') => {
                InputAction::ActivityReview(activity_id)
            }
            _ => InputAction::None,
        };
    }
    if let Some(detail) = &app.activity_detail {
        let id = detail.activity.id.clone();
        let state = detail.activity.state.clone();
        return match key.code {
            KeyCode::Esc => {
                app.close_activity_detail();
                InputAction::None
            }
            KeyCode::Up | KeyCode::PageUp => {
                app.activity_detail_scroll = app.activity_detail_scroll.saturating_add(5);
                InputAction::None
            }
            KeyCode::Down | KeyCode::PageDown => {
                app.activity_detail_scroll = app.activity_detail_scroll.saturating_sub(5);
                InputAction::None
            }
            KeyCode::Char('r') | KeyCode::Char('R') if state == "active" => {
                InputAction::ActivityRun(id)
            }
            KeyCode::Char('p') | KeyCode::Char('P') if state == "active" => {
                InputAction::ActivityTransition(id, "paused")
            }
            KeyCode::Char('u') | KeyCode::Char('U') if state != "active" => {
                InputAction::ActivityTransition(id, "active")
            }
            KeyCode::Char('c') | KeyCode::Char('C')
                if matches!(state.as_str(), "active" | "paused") =>
            {
                app.close_activity_detail();
                app.prefill_input(format!("/activity-complete {id} | "));
                InputAction::None
            }
            KeyCode::Char('x') | KeyCode::Char('X')
                if matches!(state.as_str(), "active" | "paused") =>
            {
                app.confirm_activity_cancel(id);
                InputAction::None
            }
            KeyCode::Char('a') | KeyCode::Char('A') => InputAction::ActivityAttention(id),
            KeyCode::Char('o') | KeyCode::Char('O') => InputAction::ActivityControls(id),
            KeyCode::Char('e') | KeyCode::Char('E') => InputAction::ActivityEvidence(id),
            KeyCode::Char('v') | KeyCode::Char('V') => InputAction::ActivityReview(id),
            _ => InputAction::None,
        };
    }
    if let Some(task) = &app.task_detail {
        let task_id = task.id.clone();
        let terminal = task.is_terminal();
        return match key.code {
            KeyCode::Esc => {
                app.close_task_detail();
                InputAction::None
            }
            KeyCode::Up | KeyCode::PageUp => {
                app.task_detail_scroll = app.task_detail_scroll.saturating_add(5);
                InputAction::None
            }
            KeyCode::Down | KeyCode::PageDown => {
                app.task_detail_scroll = app.task_detail_scroll.saturating_sub(5);
                InputAction::None
            }
            KeyCode::Char('c') | KeyCode::Char('C') if !terminal => {
                InputAction::TaskCancel(task_id)
            }
            KeyCode::Char('r') | KeyCode::Char('R') if terminal => InputAction::TaskRetry(task_id),
            _ => InputAction::None,
        };
    }
    if app.picker.is_some() {
        return match key.code {
            KeyCode::Esc => {
                app.close_picker();
                InputAction::None
            }
            KeyCode::Up => {
                app.picker_move(false);
                InputAction::None
            }
            KeyCode::Down => {
                app.picker_move(true);
                InputAction::None
            }
            KeyCode::Backspace => {
                app.picker_backspace();
                InputAction::None
            }
            KeyCode::Enter | KeyCode::Tab => app
                .take_picker_selection()
                .map_or(InputAction::None, InputAction::Picker),
            KeyCode::Char(value) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.picker_insert(value);
                InputAction::None
            }
            _ => InputAction::None,
        };
    }
    if key.code != KeyCode::Esc {
        app.disarm_backtrack();
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('c') if app.active_task.is_some() => InputAction::Cancel,
            KeyCode::Char('c') | KeyCode::Char('d') if app.active_task.is_none() => {
                InputAction::Quit
            }
            KeyCode::Char('a') => {
                app.cursor = 0;
                InputAction::None
            }
            KeyCode::Char('e') => {
                app.cursor = app.input.chars().count();
                InputAction::None
            }
            KeyCode::Char('f') => InputAction::FileMentions,
            KeyCode::Char('j') => {
                app.insert_char('\n');
                InputAction::None
            }
            KeyCode::Char('k') => {
                app.input = "/".to_string();
                app.cursor = 1;
                app.command_selection = 0;
                InputAction::None
            }
            KeyCode::Char('o') => {
                app.prefill_input("/attach ".to_string());
                InputAction::None
            }
            KeyCode::Char('t') => {
                app.open_task_controls();
                InputAction::None
            }
            KeyCode::Char('p') => {
                app.move_up();
                InputAction::None
            }
            KeyCode::Char('n') => {
                app.move_down();
                InputAction::None
            }
            _ => InputAction::None,
        };
    }
    if app.vim_mode && app.active_task.is_none() {
        if app.vim_insert_mode {
            if key.code == KeyCode::Esc {
                app.leave_vim_insert();
                return InputAction::None;
            }
        } else {
            return match key.code {
                KeyCode::Char('i') => {
                    app.enter_vim_insert();
                    InputAction::None
                }
                KeyCode::Char('a') => {
                    app.move_right();
                    app.enter_vim_insert();
                    InputAction::None
                }
                KeyCode::Char('h') | KeyCode::Left => {
                    app.move_left();
                    InputAction::None
                }
                KeyCode::Char('l') | KeyCode::Right => {
                    app.move_right();
                    InputAction::None
                }
                KeyCode::Char('x') | KeyCode::Delete => {
                    app.delete();
                    InputAction::None
                }
                KeyCode::Char('0') | KeyCode::Home => {
                    app.cursor = 0;
                    InputAction::None
                }
                KeyCode::Char('$') | KeyCode::End => {
                    app.cursor = app.input.chars().count();
                    InputAction::None
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    app.move_up();
                    InputAction::None
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    app.move_down();
                    InputAction::None
                }
                KeyCode::Char('/') => {
                    app.enter_vim_insert();
                    app.insert_char('/');
                    InputAction::None
                }
                KeyCode::Esc => {
                    app.handle_idle_escape();
                    InputAction::None
                }
                KeyCode::Enter => {
                    let input = app.take_input();
                    if input.is_empty() {
                        InputAction::None
                    } else {
                        InputAction::Submit(input)
                    }
                }
                _ => InputAction::None,
            };
        }
    }
    match key.code {
        KeyCode::Char(value) => {
            app.insert_char(value);
            InputAction::None
        }
        KeyCode::Backspace => {
            app.backspace();
            InputAction::None
        }
        KeyCode::Delete => {
            app.delete();
            InputAction::None
        }
        KeyCode::Left => {
            app.move_left();
            InputAction::None
        }
        KeyCode::Right => {
            app.move_right();
            InputAction::None
        }
        KeyCode::Home => {
            app.cursor = 0;
            InputAction::None
        }
        KeyCode::End => {
            app.cursor = app.input.chars().count();
            InputAction::None
        }
        KeyCode::Up => {
            if app.command_palette_active() {
                app.move_command_selection(false);
            } else {
                app.move_up();
            }
            InputAction::None
        }
        KeyCode::Down => {
            if app.command_palette_active() {
                app.move_command_selection(true);
            } else {
                app.move_down();
            }
            InputAction::None
        }
        KeyCode::PageUp => {
            app.scroll = app.scroll.saturating_add(5);
            InputAction::None
        }
        KeyCode::PageDown => {
            app.scroll = app.scroll.saturating_sub(5);
            InputAction::None
        }
        KeyCode::Tab => {
            app.complete_command();
            InputAction::None
        }
        KeyCode::Esc if app.active_task.is_some() => InputAction::Cancel,
        KeyCode::Esc => {
            app.handle_idle_escape();
            InputAction::None
        }
        KeyCode::Enter
            if key.modifiers.contains(KeyModifiers::SHIFT)
                || key.modifiers.contains(KeyModifiers::ALT) =>
        {
            app.insert_char('\n');
            InputAction::None
        }
        KeyCode::Enter => {
            if app.command_palette_active()
                && matches!(
                    commands::parse(&app.input),
                    None | Some(commands::Command::Unknown(_))
                )
            {
                app.complete_command();
                return InputAction::None;
            }
            let input = app.take_input();
            if input.is_empty() {
                InputAction::None
            } else {
                InputAction::Submit(input)
            }
        }
        _ => InputAction::None,
    }
}

async fn apply_input_action(
    action: InputAction,
    app: &mut App,
    backend: Arc<dyn Backend>,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
) {
    match action {
        InputAction::None => {}
        InputAction::Quit => app.should_quit = true,
        InputAction::OpenAgentHooks => match backend.agent_hooks().await {
            Ok(settings) => app.open_agent_hooks(settings),
            Err(error) => {
                app.close_platform_overview();
                app.push_error(&error);
            }
        },
        InputAction::ToggleAgentHook(kind) => {
            let Some(enabled) = app.agent_hook_enabled(kind) else {
                app.set_agent_hook_error("Agent hook settings changed; reopen the panel.");
                return;
            };
            match backend.set_agent_hook(kind, !enabled).await {
                Ok(settings) => app.open_agent_hooks(settings),
                Err(error) => app.set_agent_hook_error(&error),
            }
        }
        InputAction::OpenMcpOverview => match backend.mcp_overview().await {
            Ok(overview) => app.open_mcp_overview(overview),
            Err(error) => {
                app.close_platform_overview();
                app.push_error(&error);
            }
        },
        InputAction::OpenExtensionsOverview => match backend.extensions_overview().await {
            Ok(overview) => app.open_extensions_overview(overview),
            Err(error) => {
                app.close_platform_overview();
                app.push_error(&error);
            }
        },
        InputAction::OpenUsageOverview(period) => match backend.usage_overview(period).await {
            Ok(overview) => app.open_usage_overview(overview),
            Err(error) if app.usage_overview.is_some() => {
                app.close_usage_overview();
                app.close_platform_overview();
                app.push_error(&error);
            }
            Err(error) => {
                app.close_platform_overview();
                app.push_error(&error);
            }
        },
        InputAction::OpenDebugOverview => match backend.debug_overview().await {
            Ok(overview) => app.open_debug_overview(overview),
            Err(error) => {
                app.close_platform_overview();
                app.push_error(&error);
            }
        },
        InputAction::OpenAccountOverview => match backend.account_overview().await {
            Ok(overview) => app.open_account_overview(overview),
            Err(error) => {
                app.close_platform_overview();
                app.push_error(&error);
            }
        },
        InputAction::Cancel => {
            let Some(task_id) = app.active_task.clone() else {
                return;
            };
            app.status = RunStatus::Cancelling;
            if let Err(error) = backend.cancel(&task_id).await {
                app.push_error(&error);
                app.status = RunStatus::Working;
            }
        }
        InputAction::Review(decision) => {
            let Some(approval) = app.current_approval().cloned() else {
                return;
            };
            match backend.review(&approval.id, decision).await {
                Ok(()) => {
                    app.resolve_approval(&approval.id, decision == ReviewDecision::ApproveOnce)
                }
                Err(error) => app.push_error(&error),
            }
        }
        InputAction::Picker(selection) => match selection {
            PickerSelection::Model(model) => {
                app.selected_model = model.clone();
                app.push_system(&format!("Future tasks will use {model}."));
            }
            PickerSelection::Session(id) => match backend.get_conversation(&id).await {
                Ok(conversation) => {
                    app.replace_conversation(conversation);
                    stream::restore_conversation(app, backend, runtime_tx).await;
                }
                Err(error) => app.push_error(&error),
            },
            PickerSelection::History {
                before_user_turn,
                prompt,
            } => match backend
                .fork_conversation(&app.conversation.id, Some(before_user_turn))
                .await
            {
                Ok(conversation) => {
                    app.replace_conversation(conversation);
                    app.prefill_input(prompt);
                    app.push_system(
                        "Forked before the selected prompt. Files, processes, approvals, \
                         and other admitted effects were not rolled back.",
                    );
                    stream::restore_conversation(app, backend, runtime_tx).await;
                }
                Err(error) => app.push_error(&error),
            },
            PickerSelection::File(path) => app.insert_file_mention(&path),
            PickerSelection::Task(id) => match backend.get_task(&id).await {
                Ok(task) => app.open_task_detail(task),
                Err(error) => app.push_error(&error),
            },
            PickerSelection::Approval(approval) => app.open_approval_detail(approval),
            PickerSelection::Notification(notification) => {
                app.open_notification_detail(notification)
            }
            PickerSelection::Activity(id) => match backend.get_activity(&id).await {
                Ok(detail) => app.open_activity_detail(detail),
                Err(error) => app.push_error(&error),
            },
            PickerSelection::ActivityReview(id) => match backend.activity_review(&id).await {
                Ok(review) => app.open_activity_review(review),
                Err(error) => app.push_error(&error),
            },
        },
        InputAction::Confirm(action) => {
            if let Err(error) = commands::confirm(app, backend, action).await {
                app.push_error(&error);
            }
        }
        InputAction::TaskCancel(task_id) => match backend.cancel(&task_id).await {
            Ok(()) => {
                if app.active_task.as_deref() == Some(task_id.as_str()) {
                    app.status = RunStatus::Cancelling;
                }
                match backend.get_task(&task_id).await {
                    Ok(task) => app.open_task_detail(task),
                    Err(error) => app.push_error(&error),
                }
            }
            Err(error) => app.push_error(&error),
        },
        InputAction::TaskRetry(task_id) => match backend.retry_task(&task_id).await {
            Ok(task) => {
                let new_id = task.id.clone();
                app.open_task_detail(task);
                app.push_system(&format!("Retried as durable task {new_id}."));
            }
            Err(error) => app.push_error(&error),
        },
        InputAction::FileMentions => {
            let workspace = std::path::PathBuf::from(&app.selected_workspace);
            match tokio::task::spawn_blocking(move || {
                commands::list_workspace_files(&workspace)
            })
            .await
            {
                Ok(Ok(files)) => app.open_file_picker(files),
                Ok(Err(error)) => app.push_error(&error),
                Err(_) => app.push_error("workspace file mention reader failed"),
            }
        }
        InputAction::ApprovalReview(approval_id, decision) => {
            match backend.review(&approval_id, decision).await {
                Ok(()) => {
                    let approved = decision == ReviewDecision::ApproveOnce;
                    app.resolve_approval(&approval_id, approved);
                    if let Some(approval) = &mut app.approval_detail {
                        if approval.id == approval_id {
                            approval.status = if approved {
                                "approved".into()
                            } else {
                                "denied".into()
                            };
                        }
                    }
                    app.push_system(if approved {
                        "Approval granted once through the protected OS helper."
                    } else {
                        "Approval denied through the protected OS helper."
                    });
                }
                Err(error) => app.push_error(&error),
            }
        }
        InputAction::MutateNotification(id, mutation) => {
            match backend.mutate_notification(&id, mutation).await {
                Ok(notification) => app.open_notification_detail(notification),
                Err(error) => app.push_error(&error),
            }
        }
        InputAction::NotificationPreference(action) => {
            let Some(mut preferences) = app.notification_preferences.clone() else {
                return;
            };
            match action {
                NotificationPreferenceAction::ToggleWeb => {
                    preferences.web_enabled = !preferences.web_enabled
                }
                NotificationPreferenceAction::ToggleDesktop => {
                    preferences.desktop_enabled = !preferences.desktop_enabled
                }
                NotificationPreferenceAction::ToggleNtfy => {
                    preferences.ntfy_enabled = !preferences.ntfy_enabled
                }
                NotificationPreferenceAction::ToggleDnd => {
                    if preferences.dnd_start_minute_utc.is_some()
                        && preferences.dnd_end_minute_utc.is_some()
                    {
                        preferences.dnd_start_minute_utc = None;
                        preferences.dnd_end_minute_utc = None;
                    } else {
                        preferences.dnd_start_minute_utc = Some(0);
                        preferences.dnd_end_minute_utc = Some(0);
                    }
                }
            }
            match backend.set_notification_preferences(&preferences).await {
                Ok(preferences) => app.open_notification_preferences(preferences),
                Err(error) => app.push_error(&error),
            }
        }
        InputAction::ActivityRun(id) => match backend
            .run_activity(&id, None, &app.selected_workspace)
            .await
        {
            Ok(job) => {
                let task_id = job.id.clone();
                app.open_task_detail(job);
                app.push_system(&format!(
                    "Activity work was submitted as durable task {task_id}."
                ));
            }
            Err(error) => app.push_error(&error),
        },
        InputAction::ActivityTransition(id, state) => {
            match backend.transition_activity(&id, state, None).await {
                Ok(activity) => {
                    app.update_activity(activity);
                    app.push_system(if state == "paused" {
                        "Activity paused. In-flight tasks were not cancelled."
                    } else {
                        "Activity is active."
                    });
                }
                Err(error) => app.push_error(&error),
            }
        }
        InputAction::ActivityAttention(id) => match backend.activity_attention(&id).await {
            Ok(attention) => app.open_activity_attention(attention),
            Err(error) => app.push_error(&error),
        },
        InputAction::ActivityControls(id) => match backend.activity_controls(&id).await {
            Ok(controls) => app.open_activity_controls(controls),
            Err(error) => app.push_error(&error),
        },
        InputAction::ActivityEvidence(id) => match backend.activity_evidence(&id).await {
            Ok(evidence) => app.open_activity_evidence(evidence),
            Err(error) => app.push_error(&error),
        },
        InputAction::ActivityReview(id) => match backend.activity_review(&id).await {
            Ok(review) => app.open_activity_review(review),
            Err(error) => app.push_error(&error),
        },
        InputAction::Submit(input) => {
            if let Some(command) = commands::parse(&input) {
                if let Err(error) = commands::execute(app, backend.clone(), command).await {
                    app.push_error(&error);
                } else {
                    stream::restore_conversation(app, backend, runtime_tx).await;
                }
            } else if app.active_task.is_some() {
                let use_memory = app.task_use_memory;
                let max_turns = app.task_max_turns;
                stream::queue_prompt(
                    app,
                    backend,
                    use_memory,
                    max_turns,
                    input,
                )
                .await;
            } else {
                let workspace = app.selected_workspace.clone();
                let use_memory = app.task_use_memory;
                let max_turns = app.task_max_turns;
                stream::start_prompt(
                    app,
                    backend,
                    runtime_tx,
                    use_memory,
                    max_turns,
                    input,
                    workspace,
                )
                .await;
            }
        }
    }
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    last_title: Option<String>,
}

impl TerminalSession {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableBracketedPaste) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        let mut terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = disable_raw_mode();
                let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen);
                return Err(error);
            }
        };
        terminal.clear()?;
        Ok(Self {
            terminal,
            last_title: None,
        })
    }

    fn update_title(&mut self, title: Option<&str>) -> io::Result<()> {
        if self.last_title.as_deref() == title {
            return Ok(());
        }
        execute!(io::stdout(), SetTitle(title.unwrap_or_default()))?;
        self.last_title = title.map(str::to_string);
        Ok(())
    }

    fn publish_scrollback(&mut self, snapshot: &str) -> io::Result<()> {
        self.terminal.show_cursor()?;
        disable_raw_mode()?;
        execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen)?;

        let write_result = (|| {
            let mut stdout = io::stdout();
            writeln!(stdout, "\n--- Claw transcript snapshot ---")?;
            stdout.write_all(snapshot.as_bytes())?;
            writeln!(stdout, "--- end Claw transcript snapshot ---\n")?;
            stdout.flush()
        })();

        let raw_result = enable_raw_mode();
        let screen_result = execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste);
        let clear_result = self.terminal.clear();
        let cursor_result = self.terminal.hide_cursor();

        write_result?;
        raw_result?;
        screen_result?;
        clear_result?;
        cursor_result
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.terminal.show_cursor();
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen);
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/terminal/mod.rs"
    ));
}
