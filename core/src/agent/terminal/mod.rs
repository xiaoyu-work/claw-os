//! Claw-owned full-screen terminal presentation for the shared Agent backend.

mod actions;
mod approvals;
mod backend;
mod commands;
mod models;
mod nearby;
mod presentation;
mod requests;
mod state;
mod stream;
mod ui;

use std::io::{self, IsTerminal, Write};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
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
use self::stream::{RuntimeEvent, RuntimeEventKind};
use self::requests::Requests;

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
    Nearby(Option<clawd_client::nearby::NearbyAction>),
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
    OpenVoiceOverview,
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
    let mut requests = Requests::default();
    let mut terminal = TerminalSession::enter().map_err(|error| error.to_string())?;
    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut approval_tick = tokio::time::interval(Duration::from_secs(1));
    let mut nearby_tick=tokio::time::interval(Duration::from_secs(1));
    let (nearby_tx,mut nearby_rx)=mpsc::channel(2);
    let mut nearby_polling=false;

    while !app.should_quit || requests.must_settle() {
        if !requests.busy() && !app.should_quit {
            if let Some(action) = requests.take_deferred() {
                apply_input_action(
                    action, &mut app, &mut requests, backend.clone(), runtime_tx.clone(),
                );
            } else if let Some(request) =
                stream::restore_conversation(&mut app, backend.clone(), runtime_tx.clone())
            {
                requests.start(&mut app, request);
            }
        }
        terminal
            .update_title(app.terminal_title().as_deref())
            .map_err(|error| error.to_string())?;
        terminal
            .terminal
            .draw(|frame| ui::render(frame, &app))
            .map_err(|error| error.to_string())?;
        tokio::select! {
            event = input.next(), if !app.should_quit => {
                let Some(event) = event else {
                    app.should_quit = true;
                    continue;
                };
                match event.map_err(|error| error.to_string())? {
                    Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                        let action = if requests.busy()
                            && !app.nearby_open
                            && key.kind == KeyEventKind::Press
                            && (key.code == KeyCode::Esc
                                || (key.code == KeyCode::Char('c')
                                    && key.modifiers == KeyModifiers::CONTROL))
                        {
                            InputAction::Cancel
                        } else {
                            handle_key(&mut app, key)
                        };
                        apply_input_action(
                            action,
                            &mut app,
                            &mut requests,
                            backend.clone(),
                            runtime_tx.clone(),
                        );
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
                            && !app.nearby_open
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
                            && app.voice_overview.is_none()
                            && app.picker.is_none()
                        {
                            app.insert_text(&value.replace("\r\n", "\n").replace('\r', "\n"));
                        }
                    }
                    Event::Resize(_, _) | Event::FocusGained | Event::FocusLost | Event::Mouse(_) => {}
                    Event::Key(_) => {}
                }
            }
            _=nearby_tick.tick(),if app.nearby_open && !nearby_polling && !requests.busy() && app.nearby_error.is_none()=>{
                nearby_polling=true;
                let backend=backend.clone();let sender=nearby_tx.clone();let generation=app.nearby_generation;
                tokio::spawn(async move {let _=sender.send((generation,backend.nearby(None).await)).await;});
            }
            Some((generation,result))=nearby_rx.recv()=>{
                nearby_polling=false;
                if app.nearby_open && generation==app.nearby_generation {
                    match result {
                        Ok(view)=>{app.nearby_view=Some(view);app.nearby_error=None;}
                        Err(error)=>app.nearby_error=Some(error),
                    }
                }
            }
            Some(event) = runtime_rx.recv() => {
                if app.active_task.as_deref() != Some(event.task_id.as_str()) {
                    continue;
                }
                let mut next_task = None;
                match event.kind {
                    RuntimeEventKind::Record(record) => {
                        match presentation::apply_record(&mut app, &record) {
                            Ok(outcome) => {
                                if !outcome.approvals.is_empty() {
                                    app.wait_for_approvals(outcome.approvals);
                                }
                                if outcome.resumed {
                                    app.clear_approval_ui();
                                }
                            }
                            Err(error) => app.push_error(&error),
                        }
                    }
                    RuntimeEventKind::Finished(job) => {
                        app.finish_task(&job);
                        next_task = app.queued_tasks.pop_front();
                    }
                    RuntimeEventKind::Reconnecting(error) => {
                        if app.status != RunStatus::Cancelling {
                            app.status = RunStatus::Reconnecting;
                        }
                        app.push_system(&format!(
                            "Broker connection interrupted; reconnecting to the durable task: {error}"
                        ));
                    }
                    RuntimeEventKind::Reconnected => {
                        if app.status == RunStatus::Reconnecting {
                            app.status = if app.awaiting_approval() {
                                RunStatus::WaitingApproval
                            } else {
                                RunStatus::Working
                            };
                        }
                        app.push_system("Reconnected to the durable task.");
                    }
                    RuntimeEventKind::Failed(error) => {
                        app.push_error(&format!(
                            "{error}. Task {} is unconfirmed; inspect /task {} or reattach with /resume {}.",
                            event.task_id, event.task_id, app.conversation.id,
                        ));
                        app.detach_task();
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
            completion = requests.next() => {
                requests.complete(&mut app, completion, backend.clone());
            }
            _ = approval_tick.tick(), if !app.should_quit => {
                if let Some(request) = approvals::refresh(&app, backend.clone()) {
                    requests.observe(&app, request);
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
    if let Some(action)=nearby::key(app,key) {return action;}
    if app.current_approval().is_some() || app.approval_detail.is_some() {
        let control_cancel =
            key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL;
        if key.kind != KeyEventKind::Press
            || (!control_cancel && !key.modifiers.difference(KeyModifiers::SHIFT).is_empty())
        {
            return InputAction::None;
        }
    }
    if app.current_approval().is_some() {
        return match key.code {
            KeyCode::Left => {
                app.select_approval_choice(ReviewDecision::ApproveOnce);
                InputAction::None
            }
            KeyCode::Right => {
                app.select_approval_choice(ReviewDecision::Deny);
                InputAction::None
            }
            KeyCode::Tab => {
                app.cycle_approval_choice();
                InputAction::None
            }
            KeyCode::Enter => InputAction::Review(app.approval_choice),
            KeyCode::Char('a') | KeyCode::Char('A') => {
                InputAction::Review(ReviewDecision::ApproveOnce)
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                InputAction::Review(ReviewDecision::Deny)
            }
            KeyCode::Esc => InputAction::Cancel,
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => InputAction::Cancel,
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
    if app.voice_overview.is_some() {
        return match key.code {
            KeyCode::Esc => {
                app.close_voice_overview();
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
            KeyCode::Char('w') | KeyCode::Char('W') => {
                let command = match (controls.work_revision(),controls.work_enabled()) {
                    (Some(revision),Some(enabled)) => {
                        let reviewed = controls.work.as_ref().and_then(|work|work.get("phase"))
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|phase|matches!(phase,"review"|"held"));
                        if !enabled && reviewed {
                            format!("/activity-work-enable {} on {} | {{\"next_task\":\"Describe the safe next step\",\"review_note\":\"Describe what you inspected\"}}",controls.activity_id,revision)
                        } else {
                            format!("/activity-work-enable {} {} {}",controls.activity_id,
                                if enabled {"off"} else {"on"},revision)
                        }
                    }
                    _ => format!("/activity-work-set {} new | {{\"task\":\"Describe the owner goal\",\"check_interval_secs\":3600,\"wake_event\":null}}",controls.activity_id),
                };
                app.close_activity_controls();
                app.prefill_input(command);
                InputAction::None
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
    if app.active_task.is_none()
        && matches!(key.code, KeyCode::Char('a') | KeyCode::Char('A'))
        && key.modifiers.contains(KeyModifiers::SUPER)
        && key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT)
    {
        return InputAction::OpenVoiceOverview;
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

fn apply_input_action(
    action: InputAction,
    app: &mut App,
    requests: &mut Requests,
    backend: Arc<dyn Backend>,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
) {
    let cancel = matches!(&action, InputAction::Cancel)
        || matches!(&action, InputAction::TaskCancel(id) if app.active_task.as_ref() == Some(id))
        || matches!(&action, InputAction::Submit(input)
            if matches!(commands::parse(input), Some(commands::Command::Cancel)));
    if cancel {
        requests.stop(app, backend);
        return;
    }
    let quit = matches!(&action, InputAction::Quit)
        || matches!(&action, InputAction::Submit(input)
            if matches!(commands::parse(input), Some(commands::Command::Quit)));
    if quit {
        app.should_quit = true;
        return;
    }
    if requests.busy() && !matches!(action, InputAction::None) {
        requests.defer(app, action);
        return;
    }
    match actions::prepare(action, app, backend, runtime_tx) {
        Ok(Some(request)) => requests.start(app, request),
        Ok(None) => {}
        Err(error) => app.push_error(&error),
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
        if let Err(error) = execute!(
            stdout,
            EnterAlternateScreen,
            EnableBracketedPaste,
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                    | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
            )
        ) {
            let _ = disable_raw_mode();
            let _ = execute!(
                io::stdout(),
                PopKeyboardEnhancementFlags,
                DisableBracketedPaste,
                LeaveAlternateScreen
            );
            return Err(error);
        }
        let mut terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = disable_raw_mode();
                let _ = execute!(
                    io::stdout(),
                    PopKeyboardEnhancementFlags,
                    DisableBracketedPaste,
                    LeaveAlternateScreen
                );
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
        execute!(
            io::stdout(),
            PopKeyboardEnhancementFlags,
            DisableBracketedPaste,
            LeaveAlternateScreen
        )?;

        let write_result = (|| {
            let mut stdout = io::stdout();
            writeln!(stdout, "\n--- Claw transcript snapshot ---")?;
            stdout.write_all(snapshot.as_bytes())?;
            writeln!(stdout, "--- end Claw transcript snapshot ---\n")?;
            stdout.flush()
        })();

        let raw_result = enable_raw_mode();
        let screen_result = execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableBracketedPaste,
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                    | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
            )
        );
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
        let _ = execute!(
            io::stdout(),
            PopKeyboardEnhancementFlags,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/terminal/mod.rs"
    ));
}
