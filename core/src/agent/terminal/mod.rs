//! Claw-owned full-screen terminal presentation for the shared Agent backend.

mod backend;
mod commands;
mod models;
mod presentation;
mod state;
mod stream;
mod ui;

use std::io::{self, IsTerminal};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use serde_json::Value;
use tokio::sync::mpsc;

use self::backend::{Backend, BrokerBackend, ReviewDecision};
use self::state::{App, ConfirmationAction, PickerSelection, RunStatus};
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
    super::setup::is_ready(&config.agent)?;
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
    Quit,
}

async fn run_with_backend(
    backend: Arc<dyn Backend>,
    options: ChatOptions,
) -> Result<Value, String> {
    let conversation = match options.session_id.as_deref() {
        Some(id) => backend.get_conversation(id).await?,
        None => backend.create_conversation().await?,
    };
    let mut app = App::new(backend.info().clone(), conversation);
    let mut terminal = TerminalSession::enter().map_err(|error| error.to_string())?;
    let mut input = EventStream::new();
    let (runtime_tx, mut runtime_rx) = mpsc::unbounded_channel();
    let mut tick = tokio::time::interval(Duration::from_millis(100));

    while !app.should_quit {
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
                            &options,
                        ).await;
                    }
                    Event::Paste(value) => {
                        app.insert_text(&value.replace("\r\n", "\n").replace('\r', "\n"));
                    }
                    Event::Resize(_, _) | Event::FocusGained | Event::FocusLost | Event::Mouse(_) => {}
                    Event::Key(_) => {}
                }
            }
            Some(event) = runtime_rx.recv() => {
                let mut next_prompt = None;
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
                        next_prompt = app.queued_prompts.pop_front();
                    }
                    RuntimeEvent::Failed(error) => {
                        app.push_error(&format!(
                            "{error}. The durable task may still be running; use /session to retain its conversation id."
                        ));
                        app.active_task = None;
                        app.status = RunStatus::Ready;
                    }
                }
                if let Some(prompt) = next_prompt {
                    stream::start_prompt(
                        &mut app,
                        backend.clone(),
                        runtime_tx.clone(),
                        !options.no_memory,
                        options.max_turns,
                        prompt,
                    )
                    .await;
                }
            }
            _ = tick.tick() => app.tick(),
        }
    }
    drop(terminal);
    Ok(Value::Null)
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
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('c') if app.active_task.is_some() => InputAction::Cancel,
            KeyCode::Char('c') | KeyCode::Char('d') => InputAction::Quit,
            KeyCode::Char('a') => {
                app.cursor = 0;
                InputAction::None
            }
            KeyCode::Char('e') => {
                app.cursor = app.input.chars().count();
                InputAction::None
            }
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
            app.input.clear();
            app.cursor = 0;
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
    options: &ChatOptions,
) {
    match action {
        InputAction::None => {}
        InputAction::Quit => app.should_quit = true,
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
                Ok(conversation) => app.replace_conversation(conversation),
                Err(error) => app.push_error(&error),
            },
        },
        InputAction::Confirm(action) => {
            if let Err(error) = commands::confirm(app, backend, action).await {
                app.push_error(&error);
            }
        }
        InputAction::Submit(input) => {
            if let Some(command) = commands::parse(&input) {
                if let Err(error) = commands::execute(app, backend, command).await {
                    app.push_error(&error);
                }
            } else if app.active_task.is_some() {
                app.queue_prompt(input);
            } else {
                stream::start_prompt(
                    app,
                    backend,
                    runtime_tx,
                    !options.no_memory,
                    options.max_turns,
                    input,
                )
                .await;
            }
        }
    }
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
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
        Ok(Self { terminal })
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
