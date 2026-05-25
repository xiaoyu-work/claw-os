//! `cos-agent-ui` — native libcosmic chat client for the Claw OS agent.
//!
//! Replaces the React + WebView app under `desktop/agent/web/`. The
//! bridge under `desktop/agent/bridge/` stays in place during this
//! transition and serves as the single contract: this UI POSTs to
//! `http://127.0.0.1:<port>/api/chat` and consumes the same SSE
//! stream the React app did.
//!
//! Two visual modes:
//!
//!   * **Standalone** — full window with a sidebar of local
//!                       sessions, a breadcrumb headerbar, and a
//!                       large card composer. Mirrors the look of
//!                       contemporary "agent workbench" desktop UIs.
//!   * **Overlay**    — compact, anchored, Esc closes (for the
//!                       global `Super+A` summon hotkey).
//!
//! Selected with `--overlay` on the command line. Falls back to
//! standalone.
//!
//! ### Session sidebar
//!
//! The sidebar combines real persisted sessions fetched from the
//! bridge (`GET /api/sessions`) with any in-progress "new session"
//! tabs the user has opened locally. Clicking a persisted entry
//! lazily fetches its transcript via `GET /api/sessions/:id/history`
//! and replays the parsed `tool_calls` / `tool_results` blocks into
//! the message column. Subsequent turns in that conversation reuse
//! the `session_id` on `POST /api/chat`, so clawd continues into the
//! same memory thread instead of forking a fresh one.

use std::env;
use std::time::Instant;

use cosmic::app::{Core, Settings, Task};
use cosmic::iced::keyboard::{Key, key::Named};
use cosmic::iced::{
    Alignment, Background, Border, Color, Length, Limits, Shadow, Subscription, event,
};
use cosmic::widget::{
    Column, Row, button, container, scrollable, text, text_input,
};
use cosmic::{Application, Element, executor, theme, widget};
use tracing::warn;

mod bridge;
mod recorder;
mod sse;

use crate::bridge::{
    ChatRequest, HistoryMessage, SessionSummary, StreamEvent, ToolCallView, ToolResultView,
    fetch_history, fetch_sessions, read_bridge_port,
};
use crate::recorder::Recorder;

/// Square symbol used both in the breadcrumb and the overlay header.
static SYMBOL_LIGHT: &[u8] = include_bytes!("../assets/clawos-symbol.png");
static SYMBOL_DARK: &[u8] = include_bytes!("../assets/clawos-symbol-dark.png");

/// Wordmark — only used by the standalone empty-state hero card now
/// that the breadcrumb has taken over the previous top-of-window
/// branding slot.
static WORDMARK_LIGHT: &[u8] = include_bytes!("../assets/clawos-wordmark.png");
static WORDMARK_DARK: &[u8] = include_bytes!("../assets/clawos-wordmark-dark.png");

const SIDEBAR_WIDTH: f32 = 220.0;

/// Application-level configuration parsed from argv.
#[derive(Debug, Clone, Default)]
pub struct Flags {
    pub overlay: bool,
    /// Auto-arm the microphone on launch (used by the Super+Shift+A
    /// hotkey routing through `cos app agent overlay --voice`).
    pub voice: bool,
    /// Pre-filled prompt that is also auto-submitted on launch.
    /// Used by the global launcher's "Ask Claw AI" entry to forward
    /// the user's natural-language query into the agent overlay.
    pub query: Option<String>,
    /// One-shot context hint that gets prepended (invisibly to the
    /// user) to the first prompt this session sends to the bridge.
    /// Lets per-app "Ask Claw" buttons tell the agent which app they
    /// were invoked from, what file is open, and so on — without
    /// cluttering the user-visible message history.
    pub context: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    /// User edited the input field.
    InputChanged(String),
    /// User pressed Enter or clicked Send. Captures the current input.
    Submit,
    /// Token delta from the SSE stream.
    StreamDelta(String),
    /// Terminal envelope from the stream — currently unused beyond
    /// flipping `streaming` off.
    StreamDone(serde_json::Value),
    /// Bridge-side or subprocess error during a streamed reply.
    StreamError(String),
    /// SSE connection closed (clean or otherwise). Tail of every stream.
    StreamEnded,
    /// User pressed Esc — only meaningful in overlay mode.
    EscapePressed,
    /// Markdown link clicked in a rendered assistant message.
    LinkClicked(String),
    /// User clicked the mic — toggles between idle and recording.
    ToggleMic,
    /// Recording finished and the WAV was uploaded; populate the input.
    VoiceTranscribed { text: String, placeholder: bool },
    /// Mic open / encode / upload failed.
    VoiceError(String),
    /// Sidebar: switch which session is visible. Triggers a lazy
    /// history fetch when the target session has not been loaded yet.
    SelectSession(usize),
    /// Sidebar "+" button: start a new local session.
    NewSession,
    /// Background task finished fetching the bridge's session list.
    SessionsFetched(Result<Vec<SessionSummary>, String>),
    /// Background task finished loading a remote session's history.
    HistoryFetched {
        session_id: String,
        result: Result<Vec<HistoryMessage>, String>,
    },
}

/// Microphone capture state. Mirrors the React `useAudioRecording`
/// hook's `state: "idle" | "recording" | "processing"`.
#[derive(Default)]
pub enum VoiceState {
    #[default]
    Idle,
    Recording(Recorder),
    /// Encoding the WAV and waiting on `POST /api/voice/upload`.
    Processing,
}

impl VoiceState {
    fn is_recording(&self) -> bool {
        matches!(self, VoiceState::Recording(_))
    }

    fn is_processing(&self) -> bool {
        matches!(self, VoiceState::Processing)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatRole {
    User,
    Assistant,
}

/// One rendered message in a conversation.
///
/// Tool calls and tool results from the agent's live stream and from
/// loaded history both populate the `tool_calls` / `tool_results`
/// vectors so the UI can paint structured cards instead of dumping
/// raw `[tool_use:NAME] {…}` markers.
///
/// `parsed_markdown` is populated lazily once the assistant message
/// has finished streaming (or has been loaded from history). The
/// widget renders it via `cosmic::widget::markdown::view`. We avoid
/// re-parsing on every delta because the message can change shape
/// mid-stream (open code fence, unbalanced lists) and a momentary
/// "broken" render is worse than waiting for the final shape.
#[derive(Debug, Clone, Default)]
pub struct ChatMessage {
    pub role: Option<ChatRole>,
    pub content: String,
    pub tool_calls: Vec<ToolCallView>,
    pub tool_results: Vec<ToolResultView>,
    pub parsed_markdown: Option<Vec<widget::markdown::Item>>,
    /// True while the assistant is still streaming this message.
    pub in_progress: bool,
}

impl ChatMessage {
    fn user(content: String) -> Self {
        Self {
            role: Some(ChatRole::User),
            content,
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            parsed_markdown: None,
            in_progress: false,
        }
    }

    fn assistant_streaming() -> Self {
        Self {
            role: Some(ChatRole::Assistant),
            content: String::new(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            parsed_markdown: None,
            in_progress: true,
        }
    }

    fn role(&self) -> ChatRole {
        self.role.clone().unwrap_or(ChatRole::Assistant)
    }

    /// Parse `content` into markdown items. Idempotent — repeated
    /// calls overwrite the cached vector with the latest parse, which
    /// is what we want after a history reload or stream finalize.
    fn refresh_markdown(&mut self) {
        if self.content.trim().is_empty() {
            self.parsed_markdown = None;
            return;
        }
        let items: Vec<widget::markdown::Item> =
            widget::markdown::parse(&self.content).collect();
        if items.is_empty() {
            self.parsed_markdown = None;
        } else {
            self.parsed_markdown = Some(items);
        }
    }
}

/// Loading state for the remote history of a session that lives in
/// the bridge's `/api/sessions` listing but whose messages have not
/// been pulled yet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum HistoryState {
    /// History has never been requested. The first SelectSession
    /// trips a fetch.
    #[default]
    NotLoaded,
    /// A `fetch_history` task is in flight.
    Loading,
    /// History has been merged into `messages` (or the server
    /// returned an empty conversation).
    Loaded,
    /// Last fetch failed; clicking again retries.
    Failed(String),
}

/// One conversation tab.
///
/// `remote_id` is `Some` once the bridge has persisted this
/// conversation (either it was fetched from `/api/sessions` or the
/// first `done` envelope carried back a clawd-assigned session id).
/// We replay this id on subsequent `POST /api/chat` calls so clawd
/// continues the same memory thread.
#[derive(Debug, Clone)]
pub struct LocalSession {
    /// Stable client-side id used as the React-like `key` for sidebar
    /// rows. Independent of `remote_id`.
    pub id: String,
    /// Display label — populated lazily from the first user prompt or
    /// the title clawd persisted alongside the session.
    pub title: String,
    pub started_at: Instant,
    pub messages: Vec<ChatMessage>,
    pub remote_id: Option<String>,
    pub history: HistoryState,
}

impl LocalSession {
    fn new(id: String) -> Self {
        Self {
            id,
            title: String::new(),
            started_at: Instant::now(),
            messages: Vec::new(),
            remote_id: None,
            history: HistoryState::NotLoaded,
        }
    }

    fn from_summary(client_id: String, summary: &SessionSummary) -> Self {
        Self {
            id: client_id,
            title: summary.title.clone(),
            started_at: Instant::now(),
            messages: Vec::new(),
            remote_id: Some(summary.id.clone()),
            history: HistoryState::NotLoaded,
        }
    }

    fn display_title(&self) -> &str {
        if self.title.trim().is_empty() {
            "New session"
        } else {
            self.title.as_str()
        }
    }

    fn duration_label(&self) -> String {
        let secs = self.started_at.elapsed().as_secs();
        if secs < 60 {
            "<1m".into()
        } else if secs < 60 * 60 {
            format!("{}m", secs / 60)
        } else if secs < 60 * 60 * 24 {
            format!("{}h", secs / 3_600)
        } else {
            format!("{}d", secs / (60 * 60 * 24))
        }
    }
}

pub struct App {
    core: Core,
    flags: Flags,
    bridge_port: Option<u16>,
    bridge_error: Option<String>,

    sessions: Vec<LocalSession>,
    active: usize,
    /// Which session the in-flight stream is appending to. We track
    /// this separately from `active` so the user can switch tabs
    /// mid-stream without breaking the delta accumulator.
    streaming_session: Option<usize>,

    input: String,
    streaming: bool,
    error: Option<String>,
    voice: VoiceState,

    /// One-shot context prefix consumed on the first `submit()` call.
    /// Set by `--context`, then drained the first time the user
    /// sends a message so the agent learns about the host app
    /// without the prefix bloating subsequent turns.
    pending_context: Option<String>,
}

impl Application for App {
    type Executor = executor::Default;
    type Flags = Flags;
    type Message = Message;
    const APP_ID: &'static str = "com.clawos.Agent";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(mut core: Core, flags: Flags) -> (Self, Task<Message>) {
        if flags.overlay {
            core.window.show_headerbar = false;
            core.window.show_close = false;
            core.window.show_maximize = false;
            core.window.show_minimize = false;
        }

        let (bridge_port, bridge_error) = match read_bridge_port() {
            Ok(p) => (Some(p), None),
            Err(e) => {
                warn!("cos-agent-bridge unreachable: {e:#}");
                (None, Some(format!("Bridge unavailable: {e}")))
            }
        };

        let mut app = App {
            core,
            flags: flags.clone(),
            bridge_port,
            bridge_error,
            sessions: Vec::new(),
            active: 0,
            streaming_session: None,
            input: flags.query.clone().unwrap_or_default(),
            streaming: false,
            error: None,
            voice: VoiceState::Idle,
            pending_context: flags.context.clone(),
        };
        // Always have at least one session so the sidebar renders a
        // meaningful row immediately. Without this the first launch
        // shows an empty sidebar which looks like a regression of
        // the new layout.
        app.sessions.push(LocalSession::new("session-1".into()));

        // Kick off a background fetch of the bridge's session list so
        // the sidebar starts populated with the user's prior chats.
        // Standalone-only — the overlay is a one-shot launcher.
        let fetch_sessions = if !flags.overlay {
            if let Some(p) = app.bridge_port {
                cosmic::Task::perform(
                    async move {
                        fetch_sessions(p)
                            .await
                            .map_err(|err| format!("{err:#}"))
                    },
                    Message::SessionsFetched,
                )
                .map(cosmic::Action::App)
            } else {
                Task::none()
            }
        } else {
            Task::none()
        };

        let initial = if flags.query.is_some() {
            cosmic::Task::done(cosmic::Action::App(Message::Submit))
        } else if flags.voice {
            cosmic::Task::done(cosmic::Action::App(Message::ToggleMic))
        } else {
            Task::none()
        };
        (app, Task::batch([fetch_sessions, initial]))
    }

    fn header_start(&self) -> Vec<Element<'_, Self::Message>> {
        if self.flags.overlay {
            return Vec::new();
        }
        vec![self.breadcrumb()]
    }

    fn header_end(&self) -> Vec<Element<'_, Self::Message>> {
        if self.flags.overlay || !self.streaming {
            return Vec::new();
        }
        vec![active_pill()]
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::InputChanged(s) => {
                self.input = s;
                Task::none()
            }

            Message::Submit => self.submit(),

            Message::StreamDelta(chunk) => {
                if let Some(idx) = self.streaming_session
                    && let Some(sess) = self.sessions.get_mut(idx)
                    && let Some(last) = sess.messages.last_mut()
                    && last.role() == ChatRole::Assistant
                {
                    last.content.push_str(&chunk);
                }
                Task::none()
            }

            Message::StreamDone(envelope) => {
                self.capture_remote_session(&envelope);
                self.finalize_stream();
                Task::none()
            }

            Message::StreamError(msg) => {
                self.finalize_stream();
                self.error = Some(msg);
                Task::none()
            }

            Message::StreamEnded => {
                self.finalize_stream();
                Task::none()
            }

            Message::EscapePressed => {
                if self.flags.overlay {
                    std::process::exit(0);
                }
                Task::none()
            }

            Message::LinkClicked(uri) => {
                if let Err(e) = open_uri(&uri) {
                    warn!("failed to open link {uri}: {e:#}");
                }
                Task::none()
            }

            Message::ToggleMic => self.toggle_mic(),

            Message::VoiceTranscribed { text, placeholder } => {
                self.voice = VoiceState::Idle;
                if placeholder {
                    self.error = Some(
                        "Voice transcription isn't enabled on this system yet.".into(),
                    );
                } else if !text.is_empty() {
                    if self.input.is_empty() {
                        self.input = text;
                    } else {
                        self.input.push(' ');
                        self.input.push_str(&text);
                    }
                }
                Task::none()
            }

            Message::VoiceError(msg) => {
                self.voice = VoiceState::Idle;
                self.error = Some(msg);
                Task::none()
            }

            Message::SelectSession(idx) => {
                // Disallow switching while a reply is still streaming —
                // the stream targets `streaming_session` so we wouldn't
                // *lose* deltas, but switching mid-stream creates the
                // confusing illusion of a paused agent in the tab the
                // user actually wants to read.
                if self.streaming || idx >= self.sessions.len() {
                    return Task::none();
                }
                self.active = idx;
                self.error = None;
                return self.maybe_fetch_history(idx);
            }

            Message::NewSession => {
                if !self.streaming {
                    let id = format!("session-{}", self.sessions.len() + 1);
                    self.sessions.push(LocalSession::new(id));
                    self.active = self.sessions.len() - 1;
                    self.input.clear();
                    self.error = None;
                }
                Task::none()
            }

            Message::SessionsFetched(Ok(summaries)) => {
                self.merge_remote_sessions(summaries);
                Task::none()
            }
            Message::SessionsFetched(Err(err)) => {
                tracing::warn!("failed to fetch bridge sessions: {err}");
                Task::none()
            }

            Message::HistoryFetched { session_id, result } => {
                self.apply_history(&session_id, result);
                Task::none()
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        if self.flags.overlay {
            self.view_overlay()
        } else {
            self.view_standalone()
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        // Overlay mode swallows Esc to close itself.
        if !self.flags.overlay {
            return Subscription::none();
        }
        event::listen_with(|ev, _status, _id| {
            if let cosmic::iced::Event::Keyboard(
                cosmic::iced::keyboard::Event::KeyPressed { key, .. },
            ) = ev
                && matches!(key, Key::Named(Named::Escape))
            {
                return Some(Message::EscapePressed);
            }
            None
        })
    }
}

impl App {
    // ------------------------------------------------------------------
    // State helpers
    // ------------------------------------------------------------------

    fn active_session(&self) -> Option<&LocalSession> {
        self.sessions.get(self.active)
    }

    fn active_session_mut(&mut self) -> Option<&mut LocalSession> {
        self.sessions.get_mut(self.active)
    }

    fn finalize_stream(&mut self) {
        if let Some(idx) = self.streaming_session.take()
            && let Some(sess) = self.sessions.get_mut(idx)
            && let Some(last) = sess.messages.last_mut()
        {
            last.in_progress = false;
            if last.role() == ChatRole::Assistant {
                last.refresh_markdown();
            }
        }
        self.streaming = false;
    }

    /// Pull `session_id` out of the bridge's `done` envelope and
    /// pin it to whichever session was the in-flight target so we
    /// reuse the same clawd memory thread on the next turn.
    fn capture_remote_session(&mut self, envelope: &serde_json::Value) {
        let Some(idx) = self.streaming_session else {
            return;
        };
        let Some(sess) = self.sessions.get_mut(idx) else {
            return;
        };
        if sess.remote_id.is_some() {
            return;
        }
        let candidate = envelope
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                envelope
                    .get("job")
                    .and_then(|j| j.get("session_id"))
                    .and_then(serde_json::Value::as_str)
            });
        if let Some(id) = candidate.filter(|s| !s.is_empty()) {
            sess.remote_id = Some(id.to_string());
            sess.history = HistoryState::Loaded;
        }
    }

    /// Merge bridge-listed persisted sessions into the sidebar.
    /// Sessions already represented by `remote_id` are left alone so
    /// in-progress conversations don't get clobbered by a refresh.
    fn merge_remote_sessions(&mut self, summaries: Vec<SessionSummary>) {
        // Pre-compute what we already know about so the O(n*m)
        // diff stays cheap.
        let known: std::collections::HashSet<String> = self
            .sessions
            .iter()
            .filter_map(|s| s.remote_id.clone())
            .collect();
        let mut next_id = self.sessions.len() + 1;
        for summary in summaries {
            if known.contains(&summary.id) {
                continue;
            }
            let client_id = format!("session-remote-{}", next_id);
            next_id += 1;
            self.sessions
                .push(LocalSession::from_summary(client_id, &summary));
        }
    }

    /// Trigger a lazy history fetch for `idx` if the session has a
    /// remote id and we have not loaded it yet.
    fn maybe_fetch_history(&mut self, idx: usize) -> Task<Message> {
        let Some(sess) = self.sessions.get_mut(idx) else {
            return Task::none();
        };
        if sess.history != HistoryState::NotLoaded && !matches!(sess.history, HistoryState::Failed(_)) {
            return Task::none();
        }
        let Some(remote_id) = sess.remote_id.clone() else {
            sess.history = HistoryState::Loaded;
            return Task::none();
        };
        let Some(port) = self.bridge_port else {
            return Task::none();
        };
        sess.history = HistoryState::Loading;
        cosmic::Task::perform(
            async move {
                let result = fetch_history(port, &remote_id)
                    .await
                    .map_err(|err| format!("{err:#}"));
                Message::HistoryFetched {
                    session_id: remote_id,
                    result,
                }
            },
            |m| m,
        )
        .map(cosmic::Action::App)
    }

    /// Replay history rows into the matching session's message column.
    fn apply_history(
        &mut self,
        session_id: &str,
        result: Result<Vec<HistoryMessage>, String>,
    ) {
        let Some(sess) = self
            .sessions
            .iter_mut()
            .find(|s| s.remote_id.as_deref() == Some(session_id))
        else {
            return;
        };
        match result {
            Ok(rows) => {
                sess.messages.clear();
                for row in rows {
                    let role = match row.role.as_str() {
                        "assistant" => ChatRole::Assistant,
                        _ => ChatRole::User,
                    };
                    let mut msg = ChatMessage {
                        role: Some(role.clone()),
                        content: row.text,
                        tool_calls: row.tool_calls,
                        tool_results: row.tool_results,
                        parsed_markdown: None,
                        in_progress: false,
                    };
                    if role == ChatRole::Assistant {
                        msg.refresh_markdown();
                    }
                    sess.messages.push(msg);
                }
                sess.history = HistoryState::Loaded;
            }
            Err(err) => {
                sess.history = HistoryState::Failed(err);
            }
        }
    }

    fn submit(&mut self) -> Task<Message> {
        let prompt = self.input.trim().to_string();
        if prompt.is_empty() || self.streaming {
            return Task::none();
        }
        let Some(port) = self.bridge_port else {
            self.error = Some(
                self.bridge_error
                    .clone()
                    .unwrap_or_else(|| "Bridge not running".into()),
            );
            return Task::none();
        };

        self.input.clear();
        self.error = None;
        self.streaming = true;
        self.streaming_session = Some(self.active);

        let remote_id = self
            .active_session()
            .and_then(|s| s.remote_id.clone());

        if let Some(sess) = self.active_session_mut() {
            if sess.title.trim().is_empty() {
                sess.title = title_from_prompt(&prompt);
            }
            sess.messages.push(ChatMessage::user(prompt.clone()));
            sess.messages.push(ChatMessage::assistant_streaming());
        }

        // Drain any one-shot context hint into a prefix line on the
        // first bridge-side prompt. The user-visible message we just
        // pushed to `sess.messages` above stays unmodified, so the
        // sidebar / transcript don't expose the host-app metadata.
        let bridge_prompt = match self.pending_context.take() {
            Some(ctx) if !ctx.trim().is_empty() => {
                format!("[App context: {}]\n\n{}", ctx.trim(), prompt)
            }
            _ => prompt,
        };

        let request = ChatRequest {
            prompt: bridge_prompt,
            session_id: remote_id,
            model: None,
        };
        cosmic::Task::stream(cosmic::iced::stream::channel(
            16,
            move |mut tx: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
                use futures::SinkExt;
                use futures_util::StreamExt;
                match sse::open_chat_stream(port, request).await {
                    Ok(stream) => {
                        let mut stream = std::pin::pin!(stream);
                        while let Some(item) = stream.next().await {
                            let msg = match item {
                                Ok(StreamEvent::Delta(t)) => Message::StreamDelta(t),
                                Ok(StreamEvent::Done(v)) => Message::StreamDone(v),
                                Ok(StreamEvent::Error(e)) => Message::StreamError(e),
                                Err(e) => Message::StreamError(format!("{e:#}")),
                            };
                            if tx.send(msg).await.is_err() {
                                return;
                            }
                        }
                        let _ = tx.send(Message::StreamEnded).await;
                    }
                    Err(e) => {
                        let _ = tx
                            .send(Message::StreamError(format!("{e:#}")))
                            .await;
                    }
                }
            },
        ))
        .map(cosmic::Action::App)
    }

    // ------------------------------------------------------------------
    // Standalone layout
    // ------------------------------------------------------------------

    fn view_standalone(&self) -> Element<'_, Message> {
        let spacing = theme::active().cosmic().spacing;

        let chat_body: Element<Message> = match self.active_session() {
            Some(sess) if !sess.messages.is_empty() => self.message_list(sess, false),
            _ => empty_state(false),
        };

        let chat_area = container(chat_body)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding([spacing.space_m, spacing.space_l]);

        let main_column = Column::new()
            .push(chat_area)
            .push(
                container(self.input_card(false))
                    .padding([
                        0u16,
                        spacing.space_l,
                        spacing.space_l,
                        spacing.space_l,
                    ]),
            );

        let body_row = Row::new()
            .push(
                container(self.sidebar_view())
                    .width(Length::Fixed(SIDEBAR_WIDTH))
                    .height(Length::Fill)
                    .padding([spacing.space_m, spacing.space_s])
                    .class(theme::Container::custom(sidebar_style)),
            )
            .push(container(main_column).width(Length::Fill).height(Length::Fill));

        container(body_row)
            .width(Length::Fill)
            .height(Length::Fill)
            .class(theme::Container::custom(page_style))
            .into()
    }

    fn breadcrumb(&self) -> Element<'_, Message> {
        let spacing = theme::active().cosmic().spacing;
        let title = self
            .active_session()
            .map(|s| s.display_title().to_string())
            .unwrap_or_else(|| "New session".into());

        let symbol = widget::image(if is_dark() {
            widget::image::Handle::from_bytes(SYMBOL_DARK)
        } else {
            widget::image::Handle::from_bytes(SYMBOL_LIGHT)
        })
        .height(Length::Fixed(16.0))
        .width(Length::Fixed(16.0));

        Row::new()
            .push(symbol)
            .push(text("clawOS").size(13.0))
            .push(separator())
            .push(text("Agent").size(13.0))
            .push(separator())
            .push(text(title).size(13.0))
            .push(status_dot(self.streaming))
            .spacing(spacing.space_xxs)
            .align_y(Alignment::Center)
            .into()
    }

    fn sidebar_view(&self) -> Element<'_, Message> {
        let spacing = theme::active().cosmic().spacing;

        let header = Row::new()
            .push(text("SESSIONS").size(11.0))
            .push(widget::space::horizontal())
            .push(
                button::text("+")
                    .on_press(Message::NewSession)
                    .padding([0u16, spacing.space_xs]),
            )
            .align_y(Alignment::Center);

        let mut list = Column::new().spacing(2);
        for (idx, sess) in self.sessions.iter().enumerate() {
            list = list.push(session_row(sess, idx == self.active, idx, self.streaming));
        }

        Column::new()
            .push(
                container(header).padding([
                    0u16,
                    spacing.space_xs,
                    spacing.space_xs,
                    spacing.space_xs,
                ]),
            )
            .push(scrollable(list).width(Length::Fill).height(Length::Fill))
            .spacing(spacing.space_xs)
            .into()
    }

    fn message_list<'a>(
        &'a self,
        sess: &'a LocalSession,
        compact: bool,
    ) -> Element<'a, Message> {
        let spacing = theme::active().cosmic().spacing;

        let mut col = Column::new().spacing(spacing.space_s).width(Length::Fill);
        for msg in &sess.messages {
            col = col.push(message_bubble(msg, compact));
        }
        if let Some(err) = &self.error {
            col = col.push(
                container(text(format!("⚠ {err}")).size(12.0))
                    .padding(spacing.space_xxs)
                    .class(theme::Container::Card),
            );
        }
        scrollable(col).width(Length::Fill).height(Length::Fill).into()
    }

    fn input_card(&self, compact: bool) -> Element<'_, Message> {
        let spacing = theme::active().cosmic().spacing;

        let recording = self.voice.is_recording();
        let processing = self.voice.is_processing();

        let placeholder = if recording {
            "Listening…"
        } else if processing {
            "Transcribing…"
        } else if compact {
            "Ask anything…"
        } else if self
            .active_session()
            .map_or(true, |s| s.messages.is_empty())
        {
            "Ask the agent anything."
        } else {
            "Request changes or ask a question…"
        };

        let input = text_input(placeholder, &self.input)
            .on_input(Message::InputChanged)
            .on_submit(|_| Message::Submit)
            .padding(spacing.space_xs)
            .width(Length::Fill);

        let model_caption = match self.bridge_port {
            Some(port) => format!("Bridge :{port}"),
            None => "Bridge offline".into(),
        };

        let mic = self.mic_button();
        let send = self.send_button(compact);

        let bottom = Row::new()
            .push(text(model_caption).size(11.0))
            .push(widget::space::horizontal())
            .push(mic)
            .push(send)
            .spacing(spacing.space_xs)
            .align_y(Alignment::Center);

        let card = Column::new()
            .push(input)
            .push(bottom)
            .spacing(spacing.space_xs);

        container(card)
            .padding(spacing.space_s)
            .class(theme::Container::custom(input_card_style))
            .into()
    }

    fn mic_button(&self) -> Element<'_, Message> {
        let recording = self.voice.is_recording();
        let processing = self.voice.is_processing();

        let label = if recording {
            "⏺"
        } else if processing {
            "⌛"
        } else {
            "🎙"
        };
        let mut b = if recording {
            button::destructive(label)
        } else {
            button::standard(label)
        };
        if !processing && !self.streaming {
            b = b.on_press(Message::ToggleMic);
        }
        b.into()
    }

    fn send_button(&self, compact: bool) -> Element<'_, Message> {
        let label_text = if self.streaming {
            "…"
        } else if compact {
            "Send"
        } else {
            "↑"
        };
        // `button::suggested` only accepts `Into<Cow<str>>` — for a
        // bespoke text size we wrap the styled `text` in a custom
        // button instead and re-apply the Suggested theme variant.
        let mut b = button::custom(text(label_text).size(14.0))
            .class(cosmic::theme::Button::Suggested);
        if !self.streaming && !self.input.trim().is_empty() {
            b = b.on_press(Message::Submit);
        }
        b.into()
    }

    // ------------------------------------------------------------------
    // Overlay layout — intentionally kept compact and unchanged in
    // spirit; the redesign is for the long-lived standalone window.
    // ------------------------------------------------------------------

    fn view_overlay(&self) -> Element<'_, Message> {
        let spacing = theme::active().cosmic().spacing;

        let header = Row::new()
            .push(
                widget::image(if is_dark() {
                    widget::image::Handle::from_bytes(SYMBOL_DARK)
                } else {
                    widget::image::Handle::from_bytes(SYMBOL_LIGHT)
                })
                .height(Length::Fixed(20.0))
                .width(Length::Fixed(20.0)),
            )
            .push(text("Claw OS Agent").size(13.0))
            .push(widget::space::horizontal())
            .push(text("Esc to close").size(11.0))
            .align_y(Alignment::Center)
            .spacing(spacing.space_xs);

        let body: Element<Message> = match self.active_session() {
            Some(sess) if !sess.messages.is_empty() => self.message_list(sess, true),
            _ => empty_state(true),
        };

        let inner = Column::new()
            .push(container(header).padding(spacing.space_xs))
            .push(
                container(body)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .padding([0u16, spacing.space_xs]),
            )
            .push(container(self.input_card(true)).padding(spacing.space_xs))
            .spacing(spacing.space_xs);

        container(inner)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// Mic toggle handler. Idle → start capture; Recording → stop +
    /// upload + populate input on success.
    fn toggle_mic(&mut self) -> Task<Message> {
        match std::mem::take(&mut self.voice) {
            VoiceState::Idle => match Recorder::start() {
                Ok(rec) => {
                    self.voice = VoiceState::Recording(rec);
                    self.error = None;
                    Task::none()
                }
                Err(e) => {
                    self.voice = VoiceState::Idle;
                    self.error = Some(format!("Microphone unavailable: {e}"));
                    Task::none()
                }
            },
            VoiceState::Recording(rec) => {
                self.voice = VoiceState::Processing;
                let Some(port) = self.bridge_port else {
                    self.voice = VoiceState::Idle;
                    self.error = Some(
                        self.bridge_error
                            .clone()
                            .unwrap_or_else(|| "Bridge not running".into()),
                    );
                    return Task::none();
                };
                cosmic::Task::perform(
                    async move {
                        let wav = match tokio::task::spawn_blocking(move || rec.stop())
                            .await
                        {
                            Ok(Ok(wav)) => wav,
                            Ok(Err(e)) => {
                                return Message::VoiceError(format!("recording: {e}"));
                            }
                            Err(e) => {
                                return Message::VoiceError(format!("recorder task: {e}"));
                            }
                        };
                        match recorder::upload(port, wav).await {
                            Ok(resp) => Message::VoiceTranscribed {
                                text: resp.text,
                                placeholder: resp.placeholder,
                            },
                            Err(e) => Message::VoiceError(format!("upload: {e}")),
                        }
                    },
                    |m| m,
                )
                .map(cosmic::Action::App)
            }
            VoiceState::Processing => {
                self.voice = VoiceState::Processing;
                Task::none()
            }
        }
    }
}

// ----------------------------------------------------------------------
// View helpers (pure functions / borrowless widgets)
// ----------------------------------------------------------------------

fn separator() -> Element<'static, Message> {
    text("/").size(13.0).into()
}

fn status_dot(active: bool) -> Element<'static, Message> {
    let class = if active {
        theme::Container::custom(green_dot_style)
    } else {
        theme::Container::custom(idle_dot_style)
    };
    container(
        widget::Space::new()
            .width(Length::Fixed(8.0))
            .height(Length::Fixed(8.0)),
    )
    .class(class)
    .into()
}

fn active_pill() -> Element<'static, Message> {
    let spacing = theme::active().cosmic().spacing;
    container(text("active").size(11.0))
        .padding([0u16, spacing.space_s])
        .class(theme::Container::custom(active_pill_style))
        .into()
}

fn session_row<'a>(
    sess: &'a LocalSession,
    is_active: bool,
    idx: usize,
    streaming: bool,
) -> Element<'a, Message> {
    let spacing = theme::active().cosmic().spacing;

    let title_text = text(sess.display_title()).size(13.0).width(Length::Fill);
    let dur_text = text(sess.duration_label()).size(11.0);

    let row_content = Row::new()
        .push(title_text)
        .push(dur_text)
        .spacing(spacing.space_xxs)
        .align_y(Alignment::Center);

    // ListItem gives us the COSMIC-styled hover background + matching
    // corner radius without needing a fully-custom style.
    let class = if is_active {
        cosmic::theme::Button::Custom {
            active: Box::new(|_focused, _theme| selected_session_active_style()),
            disabled: Box::new(|_theme| selected_session_active_style()),
            hovered: Box::new(|_focused, _theme| selected_session_active_style()),
            pressed: Box::new(|_focused, _theme| selected_session_active_style()),
        }
    } else {
        cosmic::theme::Button::MenuItem
    };

    let mut b = button::custom(row_content)
        .width(Length::Fill)
        .padding([spacing.space_xxs, spacing.space_xs])
        .class(class);
    if !streaming {
        b = b.on_press(Message::SelectSession(idx));
    }
    b.into()
}

fn empty_state(compact: bool) -> Element<'static, Message> {
    let spacing = theme::active().cosmic().spacing;

    let title = if compact {
        text("Ready when you are.").size(14.0)
    } else {
        text("How can I help?").size(28.0)
    };
    let hint = if compact {
        text("Type below or paste anything.").size(11.0)
    } else {
        text("Pick an example below, or press Super+A from anywhere to summon me.")
            .size(13.0)
    };

    let mut col = Column::new()
        .spacing(spacing.space_s)
        .align_x(Alignment::Center);

    if !compact {
        col = col.push(
            widget::image(if is_dark() {
                widget::image::Handle::from_bytes(WORDMARK_DARK)
            } else {
                widget::image::Handle::from_bytes(WORDMARK_LIGHT)
            })
            .height(Length::Fixed(40.0)),
        );
    }
    col = col.push(title).push(hint);

    // Three example prompts that prefill the composer on click. These
    // are chosen to showcase capabilities a Copilot CLI / coding agent
    // can't easily do — system inspection, scoped exec, and
    // approvals-gated permissions — rather than rehearsed coding
    // problems.
    if !compact {
        let prompt_row = Row::new()
            .spacing(spacing.space_xs)
            .push(example_chip("Largest files on this system"))
            .push(example_chip("Run a quick repro in a sandbox"))
            .push(example_chip("Why is the panel battery red?"));
        col = col
            .push(widget::space::vertical().height(Length::Fixed(spacing.space_s as f32)))
            .push(prompt_row);
    }

    container(col)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

fn example_chip(label: &'static str) -> Element<'static, Message> {
    let spacing = theme::active().cosmic().spacing;
    button::custom(text(label).size(12.0))
        .class(cosmic::theme::Button::Standard)
        .padding([spacing.space_xxs, spacing.space_s])
        .on_press(Message::InputChanged(label.to_string()))
        .into()
}

fn message_bubble(msg: &ChatMessage, compact: bool) -> Element<'_, Message> {
    let spacing = theme::active().cosmic().spacing;
    let body_size = if compact { 12.0 } else { 14.0 };

    match msg.role() {
        ChatRole::User => {
            // Right-aligned gray pill. The COSMIC `Button::Suggested`
            // would tint with the accent color, so we use a custom
            // neutral surface to match the reference look.
            let mut col = Column::new().spacing(spacing.space_xxs);
            if !msg.content.trim().is_empty() {
                let body = text(msg.content.clone()).size(body_size);
                let pill = container(body)
                    .padding([spacing.space_xs, spacing.space_s])
                    .class(theme::Container::custom(user_pill_style));
                col = col.push(
                    container(pill)
                        .width(Length::Fill)
                        .align_x(Alignment::End),
                );
            }
            // Tool results stream back inside `role="user"` rows
            // (Anthropic convention) — render them as their own cards
            // so the user sees something coherent for that turn even
            // when the row itself was just a tool result.
            for result in &msg.tool_results {
                col = col.push(tool_result_card(result, compact));
            }
            col.width(Length::Fill).into()
        }
        ChatRole::Assistant => {
            // Left-aligned plain text; the reference draws no bubble
            // around assistant turns so structure (paragraphs, code,
            // …) reads naturally.
            //
            // Once the message has finished streaming we render its
            // parsed markdown items via the iced markdown widget so
            // headings, lists, inline code, and code fences land
            // correctly. During streaming we paint plain text — the
            // markdown renderer doesn't degrade gracefully when an
            // unfinished fence or list bullet is left dangling.
            let mut col = Column::new().spacing(spacing.space_xxs);
            if let Some(items) = msg.parsed_markdown.as_ref() {
                let palette = if is_dark() {
                    cosmic::iced::theme::Palette::DARK
                } else {
                    cosmic::iced::theme::Palette::LIGHT
                };
                let settings = widget::markdown::Settings::with_text_size(
                    body_size,
                    widget::markdown::Style::from_palette(palette),
                );
                let view = widget::markdown::view(items, settings)
                    .map(|uri| Message::LinkClicked(uri.to_string()));
                col = col.push(view);
            } else if !msg.content.is_empty() {
                col = col.push(text(msg.content.clone()).size(body_size));
            } else if msg.in_progress && msg.tool_calls.is_empty() {
                col = col.push(text("…").size(body_size));
            }
            for call in &msg.tool_calls {
                col = col.push(tool_call_card(call, compact));
            }
            // Tool results occasionally land on the assistant row too
            // when the runtime stitches them inline.
            for result in &msg.tool_results {
                col = col.push(tool_result_card(result, compact));
            }
            container(col).width(Length::Fill).into()
        }
    }
}

fn tool_call_card(call: &ToolCallView, compact: bool) -> Element<'_, Message> {
    let spacing = theme::active().cosmic().spacing;
    let title_size = if compact { 11.0 } else { 12.0 };
    let body_size = if compact { 11.0 } else { 12.0 };

    let header = Row::new()
        .push(text("⚙").size(title_size))
        .push(text(format!("{}", call.name)).size(title_size))
        .spacing(spacing.space_xxs)
        .align_y(Alignment::Center);

    let mut col = Column::new().push(header).spacing(spacing.space_xxs);
    let preview = format_input_preview(&call.input);
    if !preview.is_empty() {
        col = col.push(text(preview).size(body_size));
    }

    container(col)
        .padding([spacing.space_xxs, spacing.space_s])
        .class(theme::Container::custom(tool_card_style))
        .width(Length::Fill)
        .into()
}

fn tool_result_card(result: &ToolResultView, compact: bool) -> Element<'_, Message> {
    let spacing = theme::active().cosmic().spacing;
    let title_size = if compact { 11.0 } else { 12.0 };
    let body_size = if compact { 11.0 } else { 12.0 };

    let header_label = if result.is_error {
        "✗ tool error"
    } else {
        "✓ tool result"
    };
    let header = text(header_label).size(title_size);

    let preview = clip_preview(&result.text, 600);
    let mut col = Column::new().push(header).spacing(spacing.space_xxs);
    if !preview.is_empty() {
        col = col.push(text(preview).size(body_size));
    }

    let style = if result.is_error {
        theme::Container::custom(tool_error_card_style)
    } else {
        theme::Container::custom(tool_card_style)
    };
    container(col)
        .padding([spacing.space_xxs, spacing.space_s])
        .class(style)
        .width(Length::Fill)
        .into()
}

fn format_input_preview(value: &serde_json::Value) -> String {
    if value.is_null() {
        return String::new();
    }
    let text = if value.is_string() {
        value.as_str().unwrap_or_default().to_string()
    } else {
        serde_json::to_string(value).unwrap_or_default()
    };
    clip_preview(&text, 240)
}

fn clip_preview(input: &str, max_chars: usize) -> String {
    let trimmed = input.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max_chars).collect();
    out.push_str(" …");
    out
}

// ----------------------------------------------------------------------
// Container styles — written by hand because the COSMIC palette
// variants don't include "page off-white" / "raised card with a soft
// border" / "small green status dot" out of the box.
// ----------------------------------------------------------------------

fn page_style(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    let cosmic = theme.cosmic();
    cosmic::widget::container::Style {
        text_color: Some(cosmic.background.on.into()),
        background: Some(Background::Color(Color::from(cosmic.background.base))),
        border: Border::default(),
        shadow: Shadow::default(),
        icon_color: Some(cosmic.background.on.into()),
        snap: true,
    }
}

fn sidebar_style(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    let cosmic = theme.cosmic();
    cosmic::widget::container::Style {
        text_color: Some(cosmic.primary.on.into()),
        background: Some(Background::Color(Color::from(cosmic.primary.base))),
        border: Border {
            radius: 0.0.into(),
            width: 0.0,
            color: cosmic.primary.divider.into(),
        },
        shadow: Shadow::default(),
        icon_color: Some(cosmic.primary.on.into()),
        snap: true,
    }
}

fn input_card_style(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    let cosmic = theme.cosmic();
    // macOS chat composers (Messages, Mail) lean on a soft raised
    // card: subtle accent-tinted border, neutral component fill, and
    // a faint drop shadow so the input feels lifted off the page.
    let radius = cosmic.corner_radii.radius_m;
    cosmic::widget::container::Style {
        text_color: Some(cosmic.background.component.on.into()),
        background: Some(Background::Color(Color::from(cosmic.background.component.base))),
        border: Border {
            radius: radius.into(),
            width: 1.0,
            color: cosmic.background.divider.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.08),
            offset: cosmic::iced::Vector::new(0.0, 1.0),
            blur_radius: 4.0,
        },
        icon_color: Some(cosmic.background.component.on.into()),
        snap: true,
    }
}


fn user_pill_style(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    let cosmic = theme.cosmic();
    // macOS Messages-style user bubble: filled with the system accent
    // (blue) so the user's turns stand out from the assistant's plain
    // body. We pin to radius_l (10px after the theme rebrand) rather
    // than radius_xl so the bubble is rounded but recognizably a
    // rectangle, mirroring iMessage's continuous-curvature look.
    let radius = cosmic.corner_radii.radius_l;
    cosmic::widget::container::Style {
        text_color: Some(cosmic.accent.on.into()),
        background: Some(Background::Color(Color::from(cosmic.accent.base))),
        border: Border {
            radius: radius.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        shadow: Shadow::default(),
        icon_color: Some(cosmic.accent.on.into()),
        snap: true,
    }
}

fn active_pill_style(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    let cosmic = theme.cosmic();
    let radius = cosmic.corner_radii.radius_xl;
    cosmic::widget::container::Style {
        text_color: Some(cosmic.background.component.on.into()),
        background: Some(Background::Color(Color::from(cosmic.background.component.base))),
        border: Border {
            radius: radius.into(),
            width: 1.0,
            color: cosmic.background.divider.into(),
        },
        shadow: Shadow::default(),
        icon_color: Some(cosmic.background.component.on.into()),
        snap: true,
    }
}

fn selected_session_active_style() -> cosmic::widget::button::Style {
    let cosmic = theme::active().cosmic().clone();
    let radius = cosmic.corner_radii.radius_s;
    // macOS Finder-style selection: filled with the system accent
    // (blue) so the active session jumps out of the sidebar list.
    cosmic::widget::button::Style {
        background: Some(Background::Color(Color::from(cosmic.accent.base))),
        border_radius: radius.into(),
        border_color: Color::TRANSPARENT,
        border_width: 0.0,
        outline_color: Color::TRANSPARENT,
        outline_width: 0.0,
        icon_color: Some(cosmic.accent.on.into()),
        text_color: Some(cosmic.accent.on.into()),
        overlay: None,
        shadow_offset: cosmic::iced::Vector::new(0.0, 0.0),
    }
}

fn green_dot_style(_theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    cosmic::widget::container::Style {
        text_color: None,
        // Solid green — matches the "session active" cue in the
        // reference design. We intentionally don't use the COSMIC
        // accent color because that one tracks the user's chosen
        // theme accent (could be blue / purple / etc.) and would
        // muddy the "this thing is currently running" signal.
        background: Some(Background::Color(Color::from_rgb(0.22, 0.78, 0.36))),
        border: Border {
            radius: 4.0.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        shadow: Shadow::default(),
        icon_color: None,
        snap: true,
    }
}

fn idle_dot_style(_theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    cosmic::widget::container::Style {
        text_color: None,
        background: None,
        border: Border::default(),
        shadow: Shadow::default(),
        icon_color: None,
        snap: true,
    }
}

fn tool_card_style(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    let cosmic = theme.cosmic();
    let radius = cosmic.corner_radii.radius_s;
    cosmic::widget::container::Style {
        text_color: Some(cosmic.background.component.on.into()),
        background: Some(Background::Color(Color::from(cosmic.background.component.base))),
        border: Border {
            radius: radius.into(),
            width: 1.0,
            color: cosmic.background.divider.into(),
        },
        shadow: Shadow::default(),
        icon_color: Some(cosmic.background.component.on.into()),
        snap: true,
    }
}

fn tool_error_card_style(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    let cosmic = theme.cosmic();
    let radius = cosmic.corner_radii.radius_s;
    cosmic::widget::container::Style {
        text_color: Some(cosmic.destructive.on.into()),
        background: Some(Background::Color(Color::from(cosmic.destructive.base))),
        border: Border {
            radius: radius.into(),
            width: 1.0,
            color: cosmic.destructive.on.into(),
        },
        shadow: Shadow::default(),
        icon_color: Some(cosmic.destructive.on.into()),
        snap: true,
    }
}

fn title_from_prompt(prompt: &str) -> String {
    let first_line = prompt.lines().next().unwrap_or("").trim();
    let mut title: String = first_line.chars().take(40).collect();
    if first_line.chars().count() > 40 {
        title.push('…');
    }
    if title.is_empty() {
        title = "New session".into();
    }
    title
}

/// Best-effort URL opener used by the markdown link handler.
fn open_uri(uri: &str) -> std::io::Result<()> {
    std::process::Command::new("xdg-open")
        .arg(uri)
        .spawn()
        .map(|_| ())
}

fn is_dark() -> bool {
    theme::active().theme_type.is_dark()
}

fn parse_flags() -> Flags {
    let mut flags = Flags::default();
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--overlay" => flags.overlay = true,
            "--voice" => flags.voice = true,
            "--query" => {
                if let Some(value) = args.next() {
                    flags.query = Some(value);
                } else {
                    eprintln!("warning: --query requires an argument");
                }
            }
            "--context" => {
                if let Some(value) = args.next() {
                    flags.context = Some(value);
                } else {
                    eprintln!("warning: --context requires an argument");
                }
            }
            "-h" | "--help" => {
                eprintln!(
                    "cos-agent-ui [--overlay] [--voice] [--query <text>] [--context <text>]\n  --overlay         compact, Esc-to-close mode for global summon\n  --voice           auto-arm the microphone on launch\n  --query <text>    pre-fill the prompt and submit it immediately\n  --context <text>  invisible one-shot context line prepended to the\n                    first user prompt (used by per-app 'Ask Claw' buttons)"
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("warning: ignoring unknown flag: {other}");
            }
        }
    }
    flags
}

fn main() -> cosmic::iced::Result {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let flags = parse_flags();

    let mut settings = Settings::default();
    if flags.overlay {
        settings = settings.size_limits(
            Limits::NONE
                .min_width(360.0)
                .min_height(220.0)
                .max_width(560.0)
                .max_height(420.0),
        );
    } else {
        settings = settings.size_limits(Limits::NONE.min_width(640.0).min_height(420.0));
    }

    cosmic::app::run::<App>(settings, flags)
}
