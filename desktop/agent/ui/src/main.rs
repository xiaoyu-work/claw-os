//! Native ClawOS Agent chat UI.

use std::env;
use std::fmt::{Display, Formatter};
use std::str::FromStr;
use std::sync::LazyLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cosmic::app::{Core, CosmicFlags, Settings, Task};
use cosmic::cosmic_theme::palette::WithAlpha;
use cosmic::dbus_activation::Details;
use cosmic::iced::keyboard::{Key, key::Named};
use cosmic::iced::platform_specific::runtime::wayland::layer_surface::{
    IcedMargin, SctkLayerSurfaceSettings,
};
use cosmic::iced::platform_specific::shell::commands::layer_surface::{
    Anchor, KeyboardInteractivity, Layer, destroy_layer_surface, get_layer_surface,
};
use cosmic::iced::runtime::core::event::wayland::LayerEvent;
use cosmic::iced::runtime::core::event::{PlatformSpecific, wayland};
use cosmic::iced::widget::{operation, text_editor};
use cosmic::iced::window::Id as SurfaceId;
use cosmic::iced::{
    Alignment, Background, Border, Color, Length, Limits, Shadow, Subscription, event,
};
use cosmic::widget::{Column, Row, button, container, scrollable, text};
use cosmic::{Application, Element, executor, theme, widget};
use futures::future::{AbortHandle, Abortable};
use serde::{Deserialize, Serialize};
use tracing::warn;

mod bridge;
mod localize;
mod recorder;
mod sse;

use crate::bridge::{
    BridgeEndpoint, ChatRequest, HistoryMessage, ModelsResponse, SessionSummary, StreamEvent,
    ToolCallView, ToolResultView, cancel_task, ensure_bridge_endpoint, fetch_history, fetch_models,
    fetch_sessions, session_exists,
};
use crate::recorder::{Recorder, RecordingMetrics};

static SYMBOL_LIGHT: &[u8] = include_bytes!("../assets/clawos-symbol.png");
static SYMBOL_DARK: &[u8] = include_bytes!("../assets/clawos-symbol-dark.png");
static WORDMARK_LIGHT: &[u8] = include_bytes!("../assets/clawos-wordmark.png");
static WORDMARK_DARK: &[u8] = include_bytes!("../assets/clawos-wordmark-dark.png");

static EDITOR_ID: LazyLock<widget::Id> = LazyLock::new(|| widget::Id::new("agent-composer"));
static CHAT_SCROLL_ID: LazyLock<widget::Id> = LazyLock::new(|| widget::Id::new("agent-transcript"));
static OVERLAY_ID: LazyLock<SurfaceId> = LazyLock::new(SurfaceId::unique);

const SIDEBAR_WIDTH: f32 = 220.0;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Flags {
    pub overlay: bool,
    pub voice: bool,
    pub query: Option<String>,
    pub context: Option<String>,
    #[serde(skip)]
    activation: Option<OverlayActivation>,
}

#[derive(Debug, Clone)]
struct DeferredSubmit {
    session_index: usize,
    prompt: String,
    context: Option<String>,
    activation_generation: u64,
}

#[derive(Debug, Clone, Copy)]
struct PendingCancel {
    generation: u64,
    session_index: usize,
    message_index: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OverlayActivation {
    voice: bool,
    query: Option<String>,
    context: Option<String>,
}

impl Display for OverlayActivation {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&serde_json::to_string(self).unwrap_or_default())
    }
}

impl FromStr for OverlayActivation {
    type Err = serde_json::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(value)
    }
}

impl CosmicFlags for Flags {
    type SubCommand = OverlayActivation;
    type Args = Vec<String>;

    fn action(&self) -> Option<&Self::SubCommand> {
        self.activation.as_ref()
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    EditorAction(text_editor::Action),
    SetPrompt(String),
    Submit,
    StopStream,
    RetryMessage(usize),
    CopyAssistant(usize),
    AttachFile,
    FileAttached(Result<Option<std::path::PathBuf>, String>),
    Stream(u64, StreamEvent),
    TransportError(u64, String),
    StreamEnded(u64),
    CancelFinished {
        session_index: usize,
        message_index: usize,
        result: Result<(), String>,
    },
    EscapePressed,
    LinkClicked(String),
    ToggleMic,
    CancelVoice,
    VoiceTick,
    VoiceFinished {
        generation: u64,
        result: Result<(String, bool), String>,
    },
    SelectSession(usize),
    NewSession,
    RetryHistory,
    SessionsFetched(Result<Vec<SessionSummary>, String>),
    HistoryFetched {
        session_id: String,
        result: Result<Vec<HistoryMessage>, String>,
    },
    ProvisionalResolved {
        session_index: usize,
        session_id: String,
        result: Result<bool, String>,
    },
    ModelsFetched(Result<ModelsResponse, String>),
    Reconnect,
    BridgeTick,
    BridgeConnected(Result<BridgeEndpoint, String>),
    Layer(LayerEvent),
}

pub enum VoiceState {
    Idle,
    Recording {
        recorder: Recorder,
        generation: u64,
        metrics: RecordingMetrics,
    },
    Processing {
        generation: u64,
    },
}

impl VoiceState {
    fn is_recording(&self) -> bool {
        matches!(self, Self::Recording { .. })
    }

    fn is_processing(&self) -> bool {
        matches!(self, Self::Processing { .. })
    }

    fn is_active(&self) -> bool {
        !matches!(self, Self::Idle)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Default)]
pub struct ChatMessage {
    pub role: Option<ChatRole>,
    pub content: String,
    pub tool_calls: Vec<ToolCallView>,
    pub tool_results: Vec<ToolResultView>,
    pub warnings: Vec<String>,
    pub error: Option<String>,
    pub parsed_markdown: Option<Vec<widget::markdown::Item>>,
    pub in_progress: bool,
}

impl ChatMessage {
    fn user(content: String) -> Self {
        Self {
            role: Some(ChatRole::User),
            content,
            ..Self::default()
        }
    }

    fn assistant_streaming() -> Self {
        Self {
            role: Some(ChatRole::Assistant),
            in_progress: true,
            ..Self::default()
        }
    }

    fn role(&self) -> ChatRole {
        self.role.clone().unwrap_or(ChatRole::Assistant)
    }

    fn refresh_markdown(&mut self) {
        self.parsed_markdown = if self.content.trim().is_empty() {
            None
        } else {
            let items = widget::markdown::parse(&self.content).collect::<Vec<_>>();
            (!items.is_empty()).then_some(items)
        };
    }

    fn is_visibly_empty(&self) -> bool {
        self.content.trim().is_empty()
            && self.tool_calls.is_empty()
            && self.tool_results.is_empty()
            && self.warnings.is_empty()
            && self.error.is_none()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum HistoryState {
    #[default]
    NotLoaded,
    Loading,
    Loaded,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct LocalSession {
    pub id: String,
    pub title: String,
    pub started_at: Instant,
    pub messages: Vec<ChatMessage>,
    pub remote_id: Option<String>,
    pub provisional_remote_id: Option<String>,
    pub persistent_context: Option<String>,
    pub history: HistoryState,
    pub last_ts_ms: Option<i64>,
    pub message_count: i64,
}

impl LocalSession {
    fn new(id: String) -> Self {
        Self {
            id,
            title: String::new(),
            started_at: Instant::now(),
            messages: Vec::new(),
            remote_id: None,
            provisional_remote_id: None,
            persistent_context: None,
            history: HistoryState::Loaded,
            last_ts_ms: None,
            message_count: 0,
        }
    }

    fn from_summary(client_id: String, summary: &SessionSummary) -> Self {
        Self {
            id: client_id,
            title: summary.title.clone(),
            started_at: Instant::now(),
            messages: Vec::new(),
            remote_id: Some(summary.id.clone()),
            provisional_remote_id: None,
            persistent_context: None,
            history: HistoryState::NotLoaded,
            last_ts_ms: summary.last_ts_ms,
            message_count: summary.message_count,
        }
    }

    fn display_title(&self) -> String {
        if self.title.trim().is_empty() {
            fl!("new-session")
        } else {
            self.title.clone()
        }
    }

    fn relative_label(&self) -> String {
        if let Some(timestamp) = self.last_ts_ms {
            return relative_time_label(timestamp, now_ms());
        }
        let seconds = self.started_at.elapsed().as_secs();
        if seconds < 60 {
            fl!("just-now")
        } else if seconds < 3_600 {
            format!("{}m", seconds / 60)
        } else if seconds < 86_400 {
            format!("{}h", seconds / 3_600)
        } else {
            format!("{}d", seconds / 86_400)
        }
    }
}

pub struct App {
    core: Core,
    flags: Flags,
    overlay_visible: bool,
    bridge_endpoint: Option<BridgeEndpoint>,
    bridge_error: Option<String>,
    bridge_connecting: bool,
    models: Option<ModelsResponse>,
    sessions_error: Option<String>,
    sessions: Vec<LocalSession>,
    active: usize,
    streaming_session: Option<usize>,
    stream_generation: u64,
    active_task_id: Option<String>,
    stream_abort: Option<AbortHandle>,
    pending_cancel: Option<PendingCancel>,
    input: text_editor::Content,
    streaming: bool,
    error: Option<String>,
    voice: VoiceState,
    voice_generation: u64,
    voice_abort: Option<AbortHandle>,
    pending_context: Option<String>,
    activation_generation: u64,
    stream_context_generation: Option<u64>,
    auto_submit: bool,
    submit_after_provisional: Option<DeferredSubmit>,
    file_picker_open: bool,
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
        let mut app = Self {
            core,
            flags: flags.clone(),
            overlay_visible: flags.overlay,
            bridge_endpoint: None,
            bridge_error: None,
            bridge_connecting: true,
            models: None,
            sessions_error: None,
            sessions: vec![LocalSession::new("session-1".into())],
            active: 0,
            streaming_session: None,
            stream_generation: 0,
            active_task_id: None,
            stream_abort: None,
            pending_cancel: None,
            input: text_editor::Content::with_text(flags.query.as_deref().unwrap_or_default()),
            streaming: false,
            error: None,
            voice: VoiceState::Idle,
            voice_generation: 0,
            voice_abort: None,
            pending_context: flags.context.clone(),
            activation_generation: 0,
            stream_context_generation: None,
            auto_submit: flags.query.is_some(),
            submit_after_provisional: None,
            file_picker_open: false,
        };
        let mut tasks = vec![app.connect_bridge()];
        if flags.overlay {
            tasks.push(app.open_overlay());
        } else {
            tasks.push(focus_editor());
        }
        if flags.voice {
            tasks.push(Task::done(cosmic::Action::App(Message::ToggleMic)));
        }
        (app, Task::batch(tasks))
    }

    fn header_start(&self) -> Vec<Element<'_, Message>> {
        if self.flags.overlay {
            Vec::new()
        } else {
            vec![self.breadcrumb()]
        }
    }

    fn header_end(&self) -> Vec<Element<'_, Message>> {
        if self.flags.overlay || !self.streaming {
            Vec::new()
        } else {
            vec![
                container(text(fl!("streaming")).size(11.0))
                    .padding([0u16, 10u16])
                    .class(theme::Container::custom(active_pill_style))
                    .into(),
            ]
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::EditorAction(action) => {
                if !self.voice.is_active() {
                    self.input.perform(action);
                }
                Task::none()
            }
            Message::SetPrompt(prompt) => {
                self.input = text_editor::Content::with_text(&prompt);
                focus_editor()
            }
            Message::Submit => self.submit(),
            Message::StopStream => self.stop_stream(),
            Message::RetryMessage(message_index) => {
                let Some((prefix, prompt, context, title)) =
                    self.active_session().and_then(|session| {
                        retry_branch(&session.messages, message_index, &session.display_title())
                    })
                else {
                    return Task::none();
                };
                let mut session = LocalSession::new(format!("session-{}", self.sessions.len() + 1));
                session.title = fl!("retry-title", title = title);
                session.messages = prefix;
                session.message_count = session.messages.len() as i64;
                session.history = HistoryState::Loaded;
                session.persistent_context = (!context.is_empty()).then_some(context);
                self.sessions.push(session);
                self.active = self.sessions.len() - 1;
                self.input = text_editor::Content::with_text(&prompt);
                self.submit()
            }
            Message::CopyAssistant(index) => {
                let Some(content) = self
                    .active_session()
                    .and_then(|session| session.messages.get(index))
                    .map(|message| message.content.clone())
                    .filter(|content| !content.is_empty())
                else {
                    return Task::none();
                };
                cosmic::iced::clipboard::write(content)
            }
            Message::AttachFile => {
                self.file_picker_open = true;
                Task::perform(
                    async {
                        let dialog = cosmic::dialog::file_chooser::open::Dialog::new()
                            .title(fl!("attach-file"));
                        match dialog.open_file().await {
                            Ok(response) => response
                                .url()
                                .to_file_path()
                                .map(Some)
                                .map_err(|_| fl!("attachment-error")),
                            Err(cosmic::dialog::file_chooser::Error::Cancelled) => Ok(None),
                            Err(error) => Err(error.to_string()),
                        }
                    },
                    |result| cosmic::Action::App(Message::FileAttached(result)),
                )
            }
            Message::FileAttached(Ok(Some(path))) => {
                self.file_picker_open = false;
                let path = path.display().to_string();
                let marker = format!("[{}: {path}]", fl!("attached-file-label"));
                let existing = self.input.text();
                let prompt = if existing.trim().is_empty() {
                    marker
                } else {
                    format!("{existing}\n{marker}")
                };
                self.input = text_editor::Content::with_text(&prompt);
                focus_editor()
            }
            Message::FileAttached(Ok(None)) => {
                self.file_picker_open = false;
                focus_editor()
            }
            Message::FileAttached(Err(error)) => {
                self.file_picker_open = false;
                self.error = Some(format!("{}: {error}", fl!("attachment-error")));
                Task::none()
            }
            Message::Stream(generation, event) => self.handle_stream_event(generation, event),
            Message::TransportError(generation, error) => {
                if self
                    .pending_cancel
                    .is_some_and(|pending| pending.generation == generation)
                {
                    self.pending_cancel = None;
                    self.stream_abort = None;
                    self.stream_generation = self.stream_generation.wrapping_add(1);
                    return Task::none();
                }
                if generation != self.stream_generation || !self.streaming {
                    return Task::none();
                }
                self.bridge_endpoint = None;
                self.models = None;
                self.bridge_error = Some(error.clone());
                let failed = self.fail_stream(error);
                self.bridge_connecting = true;
                Task::batch([failed, self.connect_bridge()])
            }
            Message::StreamEnded(generation) => {
                if self
                    .pending_cancel
                    .is_some_and(|pending| pending.generation == generation)
                {
                    self.pending_cancel = None;
                    self.stream_abort = None;
                    self.stream_generation = self.stream_generation.wrapping_add(1);
                    return Task::none();
                }
                if generation == self.stream_generation && self.streaming {
                    self.bridge_endpoint = None;
                    self.models = None;
                    self.bridge_error = Some(fl!("bridge-offline"));
                    let failed = self.fail_stream(fl!("bridge-offline"));
                    self.bridge_connecting = true;
                    Task::batch([failed, self.connect_bridge()])
                } else {
                    Task::none()
                }
            }
            Message::CancelFinished {
                session_index,
                message_index,
                result,
            } => {
                if self.pending_cancel.is_some_and(|pending| {
                    pending.session_index == session_index && pending.message_index == message_index
                }) {
                    self.pending_cancel = None;
                }
                if let Err(error) = result
                    && let Some(message) = self
                        .sessions
                        .get_mut(session_index)
                        .and_then(|session| session.messages.get_mut(message_index))
                        .filter(|message| message.role() == ChatRole::Assistant)
                {
                    message.error = Some(error);
                }
                self.confirm_provisional_session(session_index)
            }
            Message::EscapePressed => {
                if !self.flags.overlay {
                    return Task::none();
                }
                let action = if self.voice.is_active() {
                    self.cancel_voice()
                } else if self.streaming {
                    self.stop_stream()
                } else {
                    Task::none()
                };
                Task::batch([action, self.close_overlay()])
            }
            Message::LinkClicked(uri) => {
                if let Err(error) = open_uri(&uri) {
                    warn!("failed to open link {uri}: {error}");
                }
                Task::none()
            }
            Message::ToggleMic => {
                if self.voice.is_recording() {
                    self.stop_voice()
                } else if self.voice.is_processing() {
                    Task::none()
                } else {
                    self.start_voice()
                }
            }
            Message::CancelVoice => self.cancel_voice(),
            Message::VoiceTick => self.voice_tick(),
            Message::VoiceFinished { generation, result } => {
                if !accept_voice_completion(self.voice_generation, &self.voice, generation) {
                    return Task::none();
                }
                self.voice_abort = None;
                self.voice = VoiceState::Idle;
                match result {
                    Ok((_text, placeholder)) if placeholder => {
                        self.error = Some(fl!("voice-placeholder"));
                    }
                    Ok((text, _)) if !text.trim().is_empty() => {
                        let existing = self.input.text();
                        self.input =
                            text_editor::Content::with_text(&if existing.trim().is_empty() {
                                text
                            } else {
                                format!("{existing} {text}")
                            });
                    }
                    Ok(_) => self.error = Some(fl!("voice-empty")),
                    Err(error) => self.error = Some(error),
                }
                focus_editor()
            }
            Message::SelectSession(index) => {
                if index >= self.sessions.len() {
                    return Task::none();
                }
                self.active = index;
                self.error = None;
                Task::batch([self.maybe_fetch_history(index), scroll_to_bottom()])
            }
            Message::NewSession => {
                if self.streaming {
                    return Task::none();
                }
                self.sessions.push(LocalSession::new(format!(
                    "session-{}",
                    self.sessions.len() + 1
                )));
                self.active = self.sessions.len() - 1;
                self.input = text_editor::Content::new();
                self.error = None;
                Task::batch([focus_editor(), scroll_to_bottom()])
            }
            Message::RetryHistory => {
                if self.bridge_connecting {
                    Task::none()
                } else {
                    self.bridge_connecting = true;
                    self.connect_bridge()
                }
            }
            Message::SessionsFetched(Ok(summaries)) => {
                self.sessions_error = None;
                self.merge_remote_sessions(summaries);
                scroll_to_bottom()
            }
            Message::SessionsFetched(Err(error)) => {
                self.sessions_error = Some(error);
                Task::none()
            }
            Message::HistoryFetched { session_id, result } => {
                self.apply_history(&session_id, result);
                scroll_to_bottom()
            }
            Message::ProvisionalResolved {
                session_index,
                session_id,
                result,
            } => {
                let resolved = result.is_ok();
                if let Some(session) = self.sessions.get_mut(session_index)
                    && session.provisional_remote_id.as_deref() == Some(session_id.as_str())
                {
                    match result {
                        Ok(true) => {
                            session.remote_id = Some(session_id);
                            session.provisional_remote_id = None;
                            session.history = HistoryState::Loaded;
                        }
                        Ok(false) => {
                            session.provisional_remote_id = None;
                            if session.persistent_context.is_none() {
                                session.persistent_context =
                                    build_branch_context(&session.messages);
                            }
                        }
                        Err(error) => {
                            tracing::warn!(%error, "failed to verify provisional Agent session");
                        }
                    }
                }
                let deferred = self.submit_after_provisional.take();
                if resolved
                    && let Some(deferred) = deferred
                    && deferred.session_index == session_index
                    && self.active == session_index
                    && self.input.text().trim() == deferred.prompt
                    && deferred.activation_generation == self.activation_generation
                {
                    self.pending_context = deferred.context;
                    return self.submit();
                }
                Task::none()
            }
            Message::ModelsFetched(Ok(models)) => {
                self.models = Some(models);
                Task::none()
            }
            Message::ModelsFetched(Err(error)) => {
                self.bridge_error = Some(error);
                Task::none()
            }
            Message::Reconnect | Message::BridgeTick => {
                if self.bridge_connecting {
                    Task::none()
                } else {
                    self.bridge_connecting = true;
                    self.connect_bridge()
                }
            }
            Message::BridgeConnected(Ok(endpoint)) => {
                self.bridge_connecting = false;
                self.bridge_error = None;
                self.bridge_endpoint = Some(endpoint.clone());
                let mut tasks = vec![self.fetch_models_task(endpoint.clone())];
                if !self.flags.overlay {
                    tasks.push(self.fetch_sessions_task(endpoint));
                }
                if self
                    .active_session()
                    .is_some_and(|session| matches!(session.history, HistoryState::Failed(_)))
                {
                    tasks.push(self.maybe_fetch_history(self.active));
                }
                if self.auto_submit && (!self.flags.overlay || self.overlay_visible) {
                    self.auto_submit = false;
                    tasks.push(Task::done(cosmic::Action::App(Message::Submit)));
                }
                Task::batch(tasks)
            }
            Message::BridgeConnected(Err(error)) => {
                self.bridge_connecting = false;
                self.bridge_endpoint = None;
                self.models = None;
                self.bridge_error = Some(error);
                Task::none()
            }
            Message::Layer(LayerEvent::Focused) => focus_editor(),
            Message::Layer(LayerEvent::Unfocused) if self.file_picker_open => Task::none(),
            Message::Layer(LayerEvent::Unfocused) => {
                let action = if self.voice.is_active() {
                    self.cancel_voice()
                } else if self.streaming {
                    self.stop_stream()
                } else {
                    Task::none()
                };
                Task::batch([action, self.close_overlay()])
            }
            Message::Layer(LayerEvent::Done) => {
                self.overlay_visible = false;
                self.activation_generation = self.activation_generation.wrapping_add(1);
                self.auto_submit = false;
                self.submit_after_provisional = None;
                self.pending_context = None;
                self.stream_context_generation = None;
                if self.voice.is_active() {
                    self.cancel_voice()
                } else if self.streaming {
                    self.stop_stream()
                } else {
                    Task::none()
                }
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        if self.flags.overlay {
            container(widget::Space::new()).into()
        } else {
            self.view_standalone()
        }
    }

    fn view_window(&self, id: SurfaceId) -> Element<'_, Message> {
        if self.flags.overlay && id == *OVERLAY_ID {
            self.view_overlay()
        } else {
            container(widget::Space::new()).into()
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = Vec::new();
        if self.flags.overlay {
            subscriptions.push(event::listen_with(|event, _, _| match event {
                cosmic::iced::Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed {
                    key: Key::Named(Named::Escape),
                    ..
                }) => Some(Message::EscapePressed),
                cosmic::iced::Event::PlatformSpecific(PlatformSpecific::Wayland(
                    wayland::Event::Layer(layer, ..),
                )) => Some(Message::Layer(layer)),
                _ => None,
            }));
        }
        if self.voice.is_recording() {
            subscriptions.push(
                cosmic::iced::time::every(Duration::from_millis(100)).map(|_| Message::VoiceTick),
            );
        }
        if self.bridge_endpoint.is_none() && !self.bridge_connecting {
            subscriptions.push(
                cosmic::iced::time::every(Duration::from_secs(3)).map(|_| Message::BridgeTick),
            );
        }
        Subscription::batch(subscriptions)
    }

    fn dbus_activation(&mut self, message: cosmic::dbus_activation::Message) -> Task<Message> {
        let activation = match message.msg {
            Details::Activate => OverlayActivation::default(),
            Details::ActivateAction { action, .. } => {
                OverlayActivation::from_str(&action).unwrap_or_default()
            }
            Details::Open { .. } => return Task::none(),
        };
        self.apply_activation(activation)
    }
}

impl App {
    fn active_session(&self) -> Option<&LocalSession> {
        self.sessions.get(self.active)
    }

    fn active_session_mut(&mut self) -> Option<&mut LocalSession> {
        self.sessions.get_mut(self.active)
    }

    fn consume_stream_context(&mut self) {
        if self.stream_context_generation == Some(self.activation_generation) {
            self.pending_context = None;
        }
        self.stream_context_generation = None;
    }

    fn connect_bridge(&self) -> Task<Message> {
        Task::perform(
            async {
                ensure_bridge_endpoint()
                    .await
                    .map_err(|error| format!("{error:#}"))
            },
            |result| cosmic::Action::App(Message::BridgeConnected(result)),
        )
    }

    fn fetch_models_task(&self, endpoint: BridgeEndpoint) -> Task<Message> {
        Task::perform(
            async move {
                fetch_models(endpoint)
                    .await
                    .map_err(|error| format!("{error:#}"))
            },
            |result| cosmic::Action::App(Message::ModelsFetched(result)),
        )
    }

    fn fetch_sessions_task(&self, endpoint: BridgeEndpoint) -> Task<Message> {
        Task::perform(
            async move {
                fetch_sessions(endpoint)
                    .await
                    .map_err(|error| format!("{error:#}"))
            },
            |result| cosmic::Action::App(Message::SessionsFetched(result)),
        )
    }

    fn open_overlay(&mut self) -> Task<Message> {
        self.overlay_visible = true;
        Task::batch([get_layer_surface(SctkLayerSurfaceSettings {
            id: *OVERLAY_ID,
            layer: Layer::Overlay,
            keyboard_interactivity: KeyboardInteractivity::Exclusive,
            anchor: Anchor::TOP,
            output: Default::default(),
            namespace: "clawos-agent".into(),
            margin: IcedMargin {
                top: 72,
                ..Default::default()
            },
            size: None,
            exclusive_zone: 0,
            size_limits: Limits::NONE
                .min_width(1.0)
                .min_height(120.0)
                .max_width(560.0)
                .max_height(560.0),
            ..Default::default()
        })])
    }

    fn close_overlay(&mut self) -> Task<Message> {
        if !self.overlay_visible {
            return Task::none();
        }
        self.overlay_visible = false;
        self.activation_generation = self.activation_generation.wrapping_add(1);
        self.auto_submit = false;
        self.submit_after_provisional = None;
        self.pending_context = None;
        self.stream_context_generation = None;
        self.input = text_editor::Content::new();
        destroy_layer_surface(*OVERLAY_ID)
    }

    fn apply_activation(&mut self, activation: OverlayActivation) -> Task<Message> {
        self.activation_generation = self.activation_generation.wrapping_add(1);
        self.pending_context = activation.context;
        self.auto_submit = activation.query.is_some();
        if let Some(query) = activation.query {
            self.input = text_editor::Content::with_text(&query);
        }
        let mut tasks = Vec::new();
        if !self.overlay_visible {
            tasks.push(self.open_overlay());
        } else {
            tasks.push(focus_editor());
        }
        if activation.voice && !self.voice.is_active() && !self.streaming {
            tasks.push(Task::done(cosmic::Action::App(Message::ToggleMic)));
        } else if self.auto_submit
            && self.bridge_endpoint.is_some()
            && !activation.voice
            && !self.streaming
            && self.pending_cancel.is_none()
        {
            self.auto_submit = false;
            tasks.push(Task::done(cosmic::Action::App(Message::Submit)));
        }
        Task::batch(tasks)
    }

    fn merge_remote_sessions(&mut self, summaries: Vec<SessionSummary>) {
        for summary in summaries {
            if let Some(existing) = self.sessions.iter_mut().find(|session| {
                session.remote_id.as_deref() == Some(summary.id.as_str())
                    || session.provisional_remote_id.as_deref() == Some(summary.id.as_str())
            }) {
                if existing.title.trim().is_empty() && !summary.title.trim().is_empty() {
                    existing.title = summary.title;
                }
                existing.last_ts_ms = summary.last_ts_ms.or(existing.last_ts_ms);
                existing.message_count = existing.message_count.max(summary.message_count);
                continue;
            }
            let client_id = format!("session-remote-{}", self.sessions.len() + 1);
            self.sessions
                .push(LocalSession::from_summary(client_id, &summary));
        }
    }

    fn maybe_fetch_history(&mut self, index: usize) -> Task<Message> {
        let Some(session) = self.sessions.get_mut(index) else {
            return Task::none();
        };
        if !matches!(
            session.history,
            HistoryState::NotLoaded | HistoryState::Failed(_)
        ) {
            return Task::none();
        }
        let Some(remote_id) = session.remote_id.clone() else {
            session.history = HistoryState::Loaded;
            return Task::none();
        };
        let Some(endpoint) = self.bridge_endpoint.clone() else {
            session.history = HistoryState::Failed(fl!("bridge-offline"));
            return Task::none();
        };
        session.history = HistoryState::Loading;
        Task::perform(
            async move {
                let result = fetch_history(endpoint, &remote_id)
                    .await
                    .map_err(|error| format!("{error:#}"));
                (remote_id, result)
            },
            |(session_id, result)| {
                cosmic::Action::App(Message::HistoryFetched { session_id, result })
            },
        )
    }

    fn confirm_provisional_session(&self, session_index: usize) -> Task<Message> {
        let Some(endpoint) = self.bridge_endpoint.clone() else {
            return Task::none();
        };
        let Some(session_id) = self
            .sessions
            .get(session_index)
            .and_then(|session| session.provisional_remote_id.clone())
        else {
            return Task::none();
        };
        Task::perform(
            async move {
                for attempt in 0..5 {
                    match session_exists(endpoint.clone(), &session_id).await {
                        Ok(true) => return (session_id, Ok(true)),
                        Ok(false) if attempt < 4 => {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        Ok(false) => return (session_id, Ok(false)),
                        Err(error) => return (session_id, Err(format!("{error:#}"))),
                    }
                }
                (session_id, Ok(false))
            },
            move |(session_id, result)| {
                cosmic::Action::App(Message::ProvisionalResolved {
                    session_index,
                    session_id,
                    result,
                })
            },
        )
    }

    fn apply_history(&mut self, session_id: &str, result: Result<Vec<HistoryMessage>, String>) {
        let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.remote_id.as_deref() == Some(session_id))
        else {
            return;
        };
        match result {
            Ok(rows) => {
                session.messages.clear();
                for row in rows {
                    if row.role == "system" {
                        continue;
                    }
                    let role = if row.role == "assistant" {
                        ChatRole::Assistant
                    } else {
                        ChatRole::User
                    };
                    let mut message = ChatMessage {
                        role: Some(role.clone()),
                        content: row.text,
                        tool_calls: row.tool_calls,
                        tool_results: row.tool_results,
                        ..ChatMessage::default()
                    };
                    if role == ChatRole::Assistant {
                        message.refresh_markdown();
                    }
                    session.messages.push(message);
                }
                session.message_count = session.messages.len() as i64;
                session.history = HistoryState::Loaded;
            }
            Err(error) => session.history = HistoryState::Failed(error),
        }
    }

    fn submit(&mut self) -> Task<Message> {
        let prompt = self.input.text().trim().to_string();
        if prompt.is_empty() || self.streaming || self.pending_cancel.is_some() {
            return Task::none();
        }
        if self
            .active_session()
            .is_some_and(|session| session.provisional_remote_id.is_some())
        {
            self.submit_after_provisional = Some(DeferredSubmit {
                session_index: self.active,
                prompt,
                context: self.pending_context.clone(),
                activation_generation: self.activation_generation,
            });
            return self.confirm_provisional_session(self.active);
        }
        if !self.active_history_ready() {
            return self.maybe_fetch_history(self.active);
        }
        let cancel_voice = if self.voice.is_active() {
            self.cancel_voice()
        } else {
            Task::none()
        };
        let Some(endpoint) = self.bridge_endpoint.clone() else {
            self.error = Some(fl!("bridge-offline"));
            if !self.bridge_connecting {
                self.bridge_connecting = true;
                return Task::batch([cancel_voice, self.connect_bridge()]);
            }
            return cancel_voice;
        };

        self.input = text_editor::Content::new();
        self.error = None;
        self.streaming = true;
        self.streaming_session = Some(self.active);
        self.active_task_id = None;
        self.stream_generation = self.stream_generation.wrapping_add(1);
        let generation = self.stream_generation;
        let remote_id = self
            .active_session()
            .and_then(|session| session.remote_id.clone());
        if let Some(session) = self.active_session_mut() {
            if session.title.trim().is_empty() {
                session.title = title_from_prompt(&prompt);
            }
            session.messages.push(ChatMessage::user(prompt.clone()));
            session.messages.push(ChatMessage::assistant_streaming());
            session.message_count = session.messages.len() as i64;
        }
        let persistent_context = self
            .active_session()
            .and_then(|session| session.persistent_context.as_deref())
            .map(str::trim)
            .filter(|context| !context.is_empty())
            .map(ToOwned::to_owned);
        let one_shot_context = self
            .pending_context
            .as_deref()
            .map(str::trim)
            .filter(|context| !context.is_empty())
            .map(ToOwned::to_owned);
        self.stream_context_generation = one_shot_context
            .as_ref()
            .map(|_| self.activation_generation);
        let request = ChatRequest {
            prompt: Some(prompt),
            session_id: remote_id,
            model: None,
            context: one_shot_context,
            branch_context: persistent_context,
            ..ChatRequest::default()
        };
        let (abort_handle, abort_registration) = AbortHandle::new_pair();
        self.stream_abort = Some(abort_handle);
        let stream_task = cosmic::Task::stream(cosmic::iced::stream::channel(
            32,
            move |mut sender: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
                use futures::SinkExt;
                use futures_util::StreamExt;
                let stream_future = async move {
                    match sse::open_chat_stream(endpoint, request).await {
                        Ok(stream) => {
                            let mut stream = std::pin::pin!(stream);
                            while let Some(item) = stream.next().await {
                                let message = match item {
                                    Ok(event) => Message::Stream(generation, event),
                                    Err(error) => {
                                        Message::TransportError(generation, format!("{error:#}"))
                                    }
                                };
                                let terminal = matches!(
                                    message,
                                    Message::Stream(_, StreamEvent::Error(_))
                                        | Message::TransportError(_, _)
                                );
                                if sender.send(message).await.is_err() || terminal {
                                    return;
                                }
                            }
                            let _ = sender.send(Message::StreamEnded(generation)).await;
                        }
                        Err(error) => {
                            let _ = sender
                                .send(Message::TransportError(generation, format!("{error:#}")))
                                .await;
                        }
                    }
                };
                let _ = Abortable::new(stream_future, abort_registration).await;
            },
        ))
        .map(cosmic::Action::App);
        Task::batch([cancel_voice, stream_task, scroll_to_bottom()])
    }

    fn handle_stream_event(&mut self, generation: u64, event: StreamEvent) -> Task<Message> {
        if let StreamEvent::TaskStarted(started) = &event
            && let Some(pending) = self
                .pending_cancel
                .filter(|pending| pending.generation == generation)
        {
            if let Some(session_id) = started.session_id.as_deref().filter(|id| !id.is_empty())
                && let Some(session) = self.sessions.get_mut(pending.session_index)
                && session.remote_id.is_none()
            {
                session.provisional_remote_id = Some(session_id.to_string());
            }
            self.consume_stream_context();
            self.pending_cancel = None;
            if let Some(abort) = self.stream_abort.take() {
                abort.abort();
            }
            self.stream_generation = self.stream_generation.wrapping_add(1);
            let Some(endpoint) = self.bridge_endpoint.clone() else {
                return Task::none();
            };
            let task_id = started.task_id.clone();
            return Task::perform(
                async move {
                    cancel_task(endpoint, &task_id)
                        .await
                        .map_err(|error| format!("{error:#}"))
                },
                move |result| {
                    cosmic::Action::App(Message::CancelFinished {
                        session_index: pending.session_index,
                        message_index: pending.message_index,
                        result,
                    })
                },
            );
        }
        if self
            .pending_cancel
            .is_some_and(|pending| pending.generation == generation)
            && matches!(&event, StreamEvent::Error(_) | StreamEvent::Done(_))
        {
            self.pending_cancel = None;
            self.stream_abort = None;
            self.stream_generation = self.stream_generation.wrapping_add(1);
            return Task::none();
        }
        if generation != self.stream_generation || !self.streaming {
            return Task::none();
        }

        match event {
            StreamEvent::TaskStarted(started) => {
                self.active_task_id = (!started.task_id.is_empty()).then_some(started.task_id);
                self.consume_stream_context();
                if let Some(session_id) = started.session_id.filter(|id| !id.is_empty())
                    && let Some(index) = self.streaming_session
                    && let Some(session) = self.sessions.get_mut(index)
                    && session.remote_id.is_none()
                {
                    session.provisional_remote_id = Some(session_id);
                }
            }
            StreamEvent::Delta(delta) => {
                if let Some(message) = self.streaming_assistant_mut() {
                    message.content.push_str(&delta.text);
                }
            }
            StreamEvent::ToolUseStart(payload) => {
                if let Some(message) = self.streaming_assistant_mut() {
                    upsert_tool_call(
                        message,
                        ToolCallView {
                            id: payload.id,
                            name: payload.name,
                            input: serde_json::Value::Null,
                            partial_json: String::new(),
                            in_progress: true,
                        },
                    );
                }
            }
            StreamEvent::ToolInputDelta(payload) => {
                if let Some(message) = self.streaming_assistant_mut() {
                    if let Some(call) = message
                        .tool_calls
                        .iter_mut()
                        .find(|call| call.id == payload.id)
                    {
                        call.partial_json.push_str(&payload.delta);
                        call.in_progress = true;
                    } else {
                        upsert_tool_call(
                            message,
                            ToolCallView {
                                id: payload.id,
                                name: fl!("tool-running"),
                                input: serde_json::Value::Null,
                                partial_json: payload.delta,
                                in_progress: true,
                            },
                        );
                    }
                }
            }
            StreamEvent::ToolUse(payload) => {
                if let Some(message) = self.streaming_assistant_mut() {
                    upsert_tool_call(
                        message,
                        ToolCallView {
                            id: payload.id,
                            name: payload.name,
                            input: payload.input.unwrap_or(serde_json::Value::Null),
                            partial_json: String::new(),
                            in_progress: false,
                        },
                    );
                }
            }
            StreamEvent::ToolStart(payload) => {
                if let Some(message) = self.streaming_assistant_mut() {
                    upsert_tool_call(
                        message,
                        ToolCallView {
                            id: payload.id,
                            name: payload.name,
                            input: payload.input.unwrap_or(serde_json::Value::Null),
                            partial_json: String::new(),
                            in_progress: true,
                        },
                    );
                }
            }
            StreamEvent::ToolResult(payload) => {
                let text = payload.presented_text();
                let is_error = payload.presented_is_error();
                if let Some(message) = self.streaming_assistant_mut() {
                    if let Some(call) = message
                        .tool_calls
                        .iter_mut()
                        .find(|call| !payload.id.is_empty() && call.id == payload.id)
                    {
                        call.in_progress = false;
                    }
                    upsert_tool_result(
                        message,
                        ToolResultView {
                            id: payload.id,
                            name: payload.name,
                            text,
                            is_error,
                        },
                    );
                }
            }
            StreamEvent::Warning(warning) => {
                if let Some(message) = self.streaming_assistant_mut()
                    && !message.warnings.contains(&warning.message)
                {
                    message.warnings.push(warning.message);
                }
            }
            StreamEvent::TurnDone(_) => {}
            StreamEvent::Done(envelope) => {
                self.capture_remote_session(&envelope);
                let fallback = envelope.presented_answer();
                self.finalize_stream(fallback, false);
                if self.auto_submit && (!self.flags.overlay || self.overlay_visible) {
                    self.auto_submit = false;
                    return Task::batch([
                        Task::done(cosmic::Action::App(Message::Submit)),
                        scroll_to_bottom(),
                    ]);
                }
            }
            StreamEvent::Error(error) => return self.fail_stream(error.presented_message()),
        }
        scroll_to_bottom()
    }

    fn streaming_assistant_mut(&mut self) -> Option<&mut ChatMessage> {
        let index = self.streaming_session?;
        let message = self.sessions.get_mut(index)?.messages.last_mut()?;
        (message.role() == ChatRole::Assistant).then_some(message)
    }

    fn active_history_ready(&self) -> bool {
        self.active_session().is_none_or(|session| {
            session.remote_id.is_none() || matches!(session.history, HistoryState::Loaded)
        })
    }

    fn capture_remote_session(&mut self, envelope: &crate::bridge::DonePayload) {
        let Some(index) = self.streaming_session else {
            return;
        };
        let Some(session) = self.sessions.get_mut(index) else {
            return;
        };
        if let Some(id) = envelope.session_id.as_deref().filter(|id| !id.is_empty()) {
            session.remote_id = Some(id.to_string());
            session.provisional_remote_id = None;
            session.persistent_context = None;
            session.history = HistoryState::Loaded;
        }
    }

    fn finalize_stream(&mut self, fallback: Option<String>, interrupted: bool) {
        if let Some(index) = self.streaming_session.take()
            && let Some(session) = self.sessions.get_mut(index)
            && let Some(message) = session.messages.last_mut()
            && message.role() == ChatRole::Assistant
        {
            if message.content.trim().is_empty() {
                if let Some(answer) = fallback.filter(|answer| !answer.trim().is_empty()) {
                    message.content = answer;
                } else if interrupted {
                    message.content = fl!("interrupted");
                }
            }
            if interrupted
                && !message.content.trim().is_empty()
                && message.content != fl!("interrupted")
                && !message.warnings.contains(&fl!("interrupted"))
            {
                message.warnings.push(fl!("interrupted"));
            }
            message.in_progress = false;
            message.refresh_markdown();
            if message.is_visibly_empty() {
                session.messages.pop();
            }
            session.message_count = session.messages.len() as i64;
            session.last_ts_ms = Some(now_ms());
        }
        self.streaming = false;
        self.active_task_id = None;
        self.stream_abort = None;
    }

    fn fail_stream(&mut self, error: String) -> Task<Message> {
        let session_index = self.streaming_session;
        if let Some(message) = self.streaming_assistant_mut() {
            message.error = Some(error);
        }
        self.finalize_stream(None, false);
        if let Some(session_index) = session_index {
            Task::batch([
                self.confirm_provisional_session(session_index),
                scroll_to_bottom(),
            ])
        } else {
            scroll_to_bottom()
        }
    }

    fn stop_stream(&mut self) -> Task<Message> {
        if !self.streaming {
            return Task::none();
        }
        let generation = self.stream_generation;
        let session_index = self.streaming_session.unwrap_or(self.active);
        let message_index = self
            .sessions
            .get(session_index)
            .and_then(|session| session.messages.len().checked_sub(1))
            .unwrap_or(0);
        self.consume_stream_context();
        let abort = self.stream_abort.take();
        let task_id = self.active_task_id.clone();
        self.finalize_stream(None, true);
        let Some(task_id) = task_id else {
            self.stream_abort = abort;
            self.pending_cancel = Some(PendingCancel {
                generation,
                session_index,
                message_index,
            });
            return scroll_to_bottom();
        };
        if let Some(abort) = abort {
            abort.abort();
        }
        self.pending_cancel = Some(PendingCancel {
            generation,
            session_index,
            message_index,
        });
        self.stream_generation = self.stream_generation.wrapping_add(1);
        let Some(endpoint) = self.bridge_endpoint.clone() else {
            return scroll_to_bottom();
        };
        Task::batch([
            Task::perform(
                async move {
                    cancel_task(endpoint, &task_id)
                        .await
                        .map_err(|error| format!("{error:#}"))
                },
                move |result| {
                    cosmic::Action::App(Message::CancelFinished {
                        session_index,
                        message_index,
                        result,
                    })
                },
            ),
            scroll_to_bottom(),
        ])
    }

    fn start_voice(&mut self) -> Task<Message> {
        if self.streaming || self.pending_cancel.is_some() {
            return Task::none();
        }
        match Recorder::start() {
            Ok(recorder) => {
                self.voice_generation = self.voice_generation.wrapping_add(1);
                let generation = self.voice_generation;
                let metrics = recorder.metrics();
                self.voice = VoiceState::Recording {
                    recorder,
                    generation,
                    metrics,
                };
                self.error = None;
            }
            Err(error) => self.error = Some(format!("{}: {error}", fl!("voice-unavailable"))),
        }
        Task::none()
    }

    fn stop_voice(&mut self) -> Task<Message> {
        let state = std::mem::replace(&mut self.voice, VoiceState::Idle);
        let VoiceState::Recording {
            recorder,
            generation,
            ..
        } = state
        else {
            self.voice = state;
            return Task::none();
        };
        let Some(endpoint) = self.bridge_endpoint.clone() else {
            self.error = Some(fl!("bridge-offline"));
            return Task::none();
        };
        self.voice = VoiceState::Processing { generation };
        let (abort_handle, abort_registration) = AbortHandle::new_pair();
        self.voice_abort = Some(abort_handle);
        Task::perform(
            async move {
                let work = async move {
                    let wav = tokio::task::spawn_blocking(move || recorder.stop())
                        .await
                        .map_err(|error| format!("{}: {error}", fl!("recorder-task-error")))?
                        .map_err(|error| format!("{}: {error}", fl!("recording-error")))?;
                    let response = recorder::upload(endpoint, wav)
                        .await
                        .map_err(|error| format!("{}: {error}", fl!("upload-error")))?;
                    Ok((response.text, response.placeholder))
                };
                match Abortable::new(work, abort_registration).await {
                    Ok(result) => result,
                    Err(_) => Err(fl!("cancel")),
                }
            },
            move |result| cosmic::Action::App(Message::VoiceFinished { generation, result }),
        )
    }

    fn cancel_voice(&mut self) -> Task<Message> {
        self.voice_generation = self.voice_generation.wrapping_add(1);
        if let Some(abort) = self.voice_abort.take() {
            abort.abort();
        }
        let state = std::mem::replace(&mut self.voice, VoiceState::Idle);
        match state {
            VoiceState::Recording { recorder, .. } => Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || recorder.cancel())
                        .await
                        .map_err(|error| error.to_string())
                        .and_then(|result| result.map_err(|error| error.to_string()))
                },
                |_| cosmic::Action::None,
            ),
            VoiceState::Idle | VoiceState::Processing { .. } => Task::none(),
        }
    }

    fn voice_tick(&mut self) -> Task<Message> {
        let VoiceState::Recording {
            recorder, metrics, ..
        } = &mut self.voice
        else {
            return Task::none();
        };
        if let Some(error) = recorder.stream_error() {
            self.voice_generation = self.voice_generation.wrapping_add(1);
            self.voice = VoiceState::Idle;
            self.error = Some(error);
            return Task::none();
        }
        *metrics = recorder.metrics();
        if metrics.elapsed >= Duration::from_secs(recorder::MAX_RECORDING_SECS) {
            return self.stop_voice();
        }
        Task::none()
    }

    fn view_standalone(&self) -> Element<'_, Message> {
        let spacing = theme::active().cosmic().spacing;
        let chat_body = self.active_chat_body(false);
        let main = Column::new()
            .push(
                container(chat_body)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .padding([spacing.space_m, spacing.space_l]),
            )
            .push(container(self.input_card(false)).padding([
                0u16,
                spacing.space_l,
                spacing.space_l,
                spacing.space_l,
            ]));
        let body = Row::new()
            .push(
                container(self.sidebar_view())
                    .width(Length::Fixed(SIDEBAR_WIDTH))
                    .height(Length::Fill)
                    .padding([spacing.space_m, spacing.space_s])
                    .class(theme::Container::custom(sidebar_style)),
            )
            .push(container(main).width(Length::Fill).height(Length::Fill));
        container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .class(theme::Container::custom(page_style))
            .into()
    }

    fn view_overlay(&self) -> Element<'_, Message> {
        if self.voice.is_active() {
            return self.voice_overlay();
        }
        let spacing = theme::active().cosmic().spacing;
        let header = Row::new()
            .push(brand_symbol(20.0))
            .push(text(fl!("app-name")).size(13.0))
            .push(widget::space::horizontal())
            .push(text(fl!("close-hint")).size(11.0))
            .align_y(Alignment::Center)
            .spacing(spacing.space_xs);
        let has_content = self
            .active_session()
            .is_some_and(|session| !session.messages.is_empty())
            || self.error.is_some();
        let mut inner = Column::new()
            .push(container(header).padding(spacing.space_xs))
            .spacing(spacing.space_xs);
        if has_content {
            inner = inner.push(
                container(self.active_chat_body(true))
                    .width(Length::Fill)
                    .height(Length::Fixed(300.0))
                    .padding([0u16, spacing.space_xs]),
            );
        }
        inner = inner.push(container(self.input_card(true)).padding(spacing.space_xs));
        container(inner)
            .width(Length::Fixed(520.0))
            .class(theme::Container::custom(page_style))
            .into()
    }

    fn voice_overlay(&self) -> Element<'_, Message> {
        let spacing = theme::active().cosmic().spacing;
        let (status, elapsed, peak, processing) = match &self.voice {
            VoiceState::Recording { metrics, .. } => (
                fl!("listening"),
                format_duration(metrics.elapsed),
                metrics.peak,
                false,
            ),
            VoiceState::Processing { .. } => (fl!("transcribing"), String::new(), 0.25, true),
            VoiceState::Idle => (String::new(), String::new(), 0.0, false),
        };
        let phase = match &self.voice {
            VoiceState::Recording { metrics, .. } => metrics.elapsed.as_secs_f32() * 4.0,
            _ => 0.0,
        };
        let mut bars = Row::new()
            .spacing(spacing.space_xxs)
            .align_y(Alignment::Center);
        for index in 0..9 {
            let pulse = ((phase + index as f32 * 0.7).sin().abs() * 0.45 + 0.55) * peak.max(0.08);
            bars = bars.push(
                container(widget::Space::new())
                    .width(Length::Fixed(5.0))
                    .height(Length::Fixed(12.0 + pulse * 52.0))
                    .class(theme::Container::custom(level_bar_style)),
            );
        }
        let orb = container(bars)
            .width(Length::Fixed(150.0))
            .height(Length::Fixed(150.0))
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .class(theme::Container::custom(orb_style));
        let controls = Row::new()
            .push(if processing {
                symbolic_button(
                    "window-close-symbolic",
                    fl!("cancel"),
                    Some(Message::CancelVoice),
                    true,
                )
            } else {
                symbolic_button(
                    "media-playback-stop-symbolic",
                    fl!("stop"),
                    Some(Message::ToggleMic),
                    true,
                )
            })
            .push(if processing {
                widget::Space::new().into()
            } else {
                symbolic_button(
                    "window-close-symbolic",
                    fl!("cancel"),
                    Some(Message::CancelVoice),
                    false,
                )
            })
            .spacing(spacing.space_s)
            .align_y(Alignment::Center);
        container(
            Column::new()
                .push(text(fl!("app-name")).size(13.0))
                .push(orb)
                .push(text(status).size(16.0))
                .push(text(elapsed).size(12.0))
                .push(controls)
                .align_x(Alignment::Center)
                .spacing(spacing.space_s)
                .padding(spacing.space_m),
        )
        .width(Length::Fixed(280.0))
        .class(theme::Container::custom(page_style))
        .into()
    }

    fn active_chat_body(&self, compact: bool) -> Element<'_, Message> {
        let Some(session) = self.active_session() else {
            return empty_state(compact);
        };
        if session.messages.is_empty()
            && let Some(error) = &self.error
        {
            return Column::new()
                .push(empty_state(compact))
                .push(error_card(error, None))
                .spacing(theme::active().cosmic().spacing.space_s)
                .height(Length::Fill)
                .into();
        }
        match &session.history {
            HistoryState::Loading => state_card(fl!("history-loading"), None),
            HistoryState::Failed(error) => state_card(
                format!("{} {error}", fl!("history-failed")),
                Some((fl!("retry"), Message::RetryHistory)),
            ),
            _ if session.messages.is_empty() => empty_state(compact),
            _ => self.message_list(session, compact),
        }
    }

    fn message_list<'a>(
        &'a self,
        session: &'a LocalSession,
        compact: bool,
    ) -> Element<'a, Message> {
        let spacing = theme::active().cosmic().spacing;
        let mut column = Column::new().spacing(spacing.space_s).width(Length::Fill);
        for (index, message) in session.messages.iter().enumerate() {
            column = column.push(message_bubble(message, index, compact));
        }
        if let Some(error) = &self.error {
            column = column.push(error_card(error, None));
        }
        scrollable(column)
            .id(CHAT_SCROLL_ID.clone())
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn input_card(&self, compact: bool) -> Element<'_, Message> {
        let spacing = theme::active().cosmic().spacing;
        let placeholder = if self.voice.is_recording() {
            fl!("listening")
        } else if self.voice.is_processing() {
            fl!("transcribing")
        } else if compact {
            fl!("ask-anything")
        } else if self
            .active_session()
            .is_none_or(|session| session.messages.is_empty())
        {
            fl!("ask-agent")
        } else {
            fl!("request-changes")
        };
        let can_submit = !self.streaming && !self.voice.is_active() && self.active_history_ready();
        let voice_active = self.voice.is_active();
        let editor = widget::text_editor(&self.input)
            .id(EDITOR_ID.clone())
            .placeholder(placeholder)
            .height(Length::Fixed(if compact { 64.0 } else { 88.0 }))
            .padding(spacing.space_xs)
            .on_action(Message::EditorAction)
            .key_binding(move |press| {
                let focused = matches!(press.status, text_editor::Status::Focused { .. });
                if focused && matches!(press.key, Key::Named(Named::Enter)) {
                    if press.modifiers.shift() {
                        Some(text_editor::Binding::Enter)
                    } else if can_submit {
                        Some(text_editor::Binding::Custom(Message::Submit))
                    } else if voice_active {
                        None
                    } else {
                        Some(text_editor::Binding::Enter)
                    }
                } else {
                    text_editor::Binding::from_key_press(press)
                }
            });
        let status: Element<'_, Message> = match &self.voice {
            VoiceState::Recording { metrics, .. } => text(format!(
                "{} · {}",
                fl!("recording"),
                format_duration(metrics.elapsed)
            ))
            .size(11.0)
            .into(),
            VoiceState::Processing { .. } => text(fl!("transcribing")).size(11.0).into(),
            VoiceState::Idle => self.connection_status(),
        };
        let mic = if self.voice.is_recording() {
            symbolic_button(
                "media-playback-stop-symbolic",
                fl!("stop"),
                Some(Message::ToggleMic),
                true,
            )
        } else if self.voice.is_processing() {
            symbolic_button(
                "window-close-symbolic",
                fl!("cancel"),
                Some(Message::CancelVoice),
                true,
            )
        } else {
            symbolic_button(
                "audio-input-microphone-symbolic",
                fl!("microphone"),
                (!self.streaming).then_some(Message::ToggleMic),
                false,
            )
        };
        let attach = symbolic_button(
            "mail-attachment-symbolic",
            fl!("attach-file"),
            (!self.voice.is_active()).then_some(Message::AttachFile),
            false,
        );
        let action = if self.streaming {
            symbolic_button(
                "media-playback-stop-symbolic",
                fl!("stop"),
                Some(Message::StopStream),
                true,
            )
        } else if self.pending_cancel.is_some() {
            symbolic_button("process-stop-symbolic", fl!("stopping"), None, true)
        } else {
            symbolic_button(
                "mail-send-symbolic",
                fl!("send"),
                (!self.input.text().trim().is_empty()
                    && !self.voice.is_active()
                    && self.active_history_ready())
                .then_some(Message::Submit),
                false,
            )
        };
        let bottom = Row::new()
            .push(status)
            .push(widget::space::horizontal())
            .push(attach)
            .push(mic)
            .push(action)
            .spacing(spacing.space_xs)
            .align_y(Alignment::Center);
        container(
            Column::new()
                .push(editor)
                .push(bottom)
                .spacing(spacing.space_xs),
        )
        .padding(spacing.space_s)
        .class(theme::Container::custom(input_card_style))
        .into()
    }

    fn connection_status(&self) -> Element<'_, Message> {
        if self.bridge_connecting {
            return text(fl!("bridge-connecting")).size(11.0).into();
        }
        if self.bridge_endpoint.is_none() {
            return Row::new()
                .push(text(fl!("bridge-offline")).size(11.0))
                .push(button::text(fl!("reconnect")).on_press(Message::Reconnect))
                .spacing(6)
                .align_y(Alignment::Center)
                .into();
        }
        if self.models.is_none() && self.bridge_error.is_some() {
            return Row::new()
                .push(text(fl!("model-unavailable")).size(11.0))
                .push(button::text(fl!("retry")).on_press(Message::Reconnect))
                .spacing(6)
                .align_y(Alignment::Center)
                .into();
        }
        let label = self
            .models
            .as_ref()
            .map(provider_model_label)
            .unwrap_or_else(|| fl!("bridge-ready"));
        text(label).size(11.0).into()
    }

    fn breadcrumb(&self) -> Element<'_, Message> {
        let spacing = theme::active().cosmic().spacing;
        Row::new()
            .push(brand_symbol(16.0))
            .push(text(fl!("app-name")).size(13.0))
            .push(text("/").size(13.0))
            .push(text(
                self.active_session()
                    .map(LocalSession::display_title)
                    .unwrap_or_else(|| fl!("new-session")),
            ))
            .push(status_dot(self.streaming))
            .spacing(spacing.space_xxs)
            .align_y(Alignment::Center)
            .into()
    }

    fn sidebar_view(&self) -> Element<'_, Message> {
        let spacing = theme::active().cosmic().spacing;
        let header = Row::new()
            .push(text(fl!("sessions").to_uppercase()).size(11.0))
            .push(widget::space::horizontal())
            .push(symbolic_button(
                "list-add-symbolic",
                fl!("new-session"),
                (!self.streaming).then_some(Message::NewSession),
                false,
            ))
            .align_y(Alignment::Center);
        let mut list = Column::new().spacing(2);
        for (index, session) in self.sessions.iter().enumerate() {
            list = list.push(session_row(
                session,
                index == self.active,
                index,
                self.streaming && self.streaming_session == Some(index),
            ));
        }
        if let Some(error) = &self.sessions_error {
            list = list.push(text(error).size(11.0));
        }
        Column::new()
            .push(container(header).padding([
                0u16,
                spacing.space_xs,
                spacing.space_xs,
                spacing.space_xs,
            ]))
            .push(scrollable(list).width(Length::Fill).height(Length::Fill))
            .spacing(spacing.space_xs)
            .into()
    }
}

fn focus_editor() -> Task<Message> {
    operation::focus(EDITOR_ID.clone())
}

fn scroll_to_bottom() -> Task<Message> {
    operation::snap_to_end(CHAT_SCROLL_ID.clone())
}

fn upsert_tool_call(message: &mut ChatMessage, call: ToolCallView) {
    if !call.id.is_empty()
        && let Some(existing) = message
            .tool_calls
            .iter_mut()
            .find(|existing| existing.id == call.id)
    {
        *existing = call;
    } else {
        message.tool_calls.push(call);
    }
}

fn upsert_tool_result(message: &mut ChatMessage, result: ToolResultView) {
    if !result.id.is_empty()
        && let Some(existing) = message
            .tool_results
            .iter_mut()
            .find(|existing| existing.id == result.id)
    {
        *existing = result;
    } else {
        message.tool_results.push(result);
    }
}

fn retry_branch(
    messages: &[ChatMessage],
    assistant_index: usize,
    title: &str,
) -> Option<(Vec<ChatMessage>, String, String, String)> {
    let user_index = messages
        .get(..assistant_index)
        .unwrap_or(messages)
        .iter()
        .rposition(|message| {
            message.role() == ChatRole::User && !message.content.trim().is_empty()
        })?;
    let prompt = messages[user_index].content.clone();
    let prefix = messages[..user_index].to_vec();
    let context = build_branch_context(&prefix).unwrap_or_default();
    Some((prefix, prompt, context, title.to_string()))
}

const MAX_BRANCH_CONTEXT_CHARS: usize = 32 * 1024;
const MAX_BRANCH_MESSAGE_CHARS: usize = 4 * 1024;

fn build_branch_context(messages: &[ChatMessage]) -> Option<String> {
    let mut chunks = Vec::new();
    let mut used = 0usize;
    for message in messages.iter().rev() {
        if message.content.trim().is_empty() {
            continue;
        }
        let role = match message.role() {
            ChatRole::User => "User",
            ChatRole::Assistant => "Assistant",
        };
        let chunk = format!(
            "{role}: {}",
            clip_preview(&message.content, MAX_BRANCH_MESSAGE_CHARS)
        );
        let chars = chunk.chars().count() + 1;
        if used + chars > MAX_BRANCH_CONTEXT_CHARS {
            break;
        }
        used += chars;
        chunks.push(chunk);
    }
    if chunks.is_empty() {
        return None;
    }
    chunks.reverse();
    Some(chunks.join("\n"))
}

fn accept_voice_completion(current: u64, state: &VoiceState, generation: u64) -> bool {
    generation == current
        && matches!(
            state,
            VoiceState::Processing {
                generation: active
            } if *active == generation
        )
}

fn relative_time_label(timestamp_ms: i64, current_ms: i64) -> String {
    let seconds = current_ms.saturating_sub(timestamp_ms).max(0) / 1_000;
    if seconds < 60 {
        fl!("just-now")
    } else if seconds < 3_600 {
        let count: i64 = seconds / 60;
        fl!("minutes-ago", count = count)
    } else if seconds < 86_400 {
        let count: i64 = seconds / 3_600;
        fl!("hours-ago", count = count)
    } else {
        let count: i64 = seconds / 86_400;
        fl!("days-ago", count = count)
    }
}

fn provider_model_label(models: &ModelsResponse) -> String {
    if !models.ready {
        return fl!("model-unavailable");
    }
    if !models.label.trim().is_empty() {
        models.label.clone()
    } else if !models.model.trim().is_empty() && !models.provider.trim().is_empty() {
        format!("{} · {}", models.provider, models.model)
    } else {
        fl!("bridge-ready")
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn format_duration(duration: Duration) -> String {
    format!(
        "{:02}:{:02}",
        duration.as_secs() / 60,
        duration.as_secs() % 60
    )
}

fn symbolic_button(
    icon_name: &'static str,
    label: String,
    on_press: Option<Message>,
    destructive: bool,
) -> Element<'static, Message> {
    let mut control = button::custom(widget::icon::from_name(icon_name).size(18))
        .padding(8)
        .class(if destructive {
            cosmic::theme::Button::Destructive
        } else {
            cosmic::theme::Button::Standard
        });
    if let Some(message) = on_press {
        control = control.on_press(message);
    }
    widget::tooltip(control, text(label), widget::tooltip::Position::Top).into()
}

fn brand_symbol(size: f32) -> Element<'static, Message> {
    widget::image(if is_dark() {
        widget::image::Handle::from_bytes(SYMBOL_DARK)
    } else {
        widget::image::Handle::from_bytes(SYMBOL_LIGHT)
    })
    .height(Length::Fixed(size))
    .width(Length::Fixed(size))
    .into()
}

fn state_card(label: String, action: Option<(String, Message)>) -> Element<'static, Message> {
    let mut column = Column::new()
        .push(text(label).size(13.0))
        .align_x(Alignment::Center)
        .spacing(8);
    if let Some((label, message)) = action {
        column = column.push(button::text(label).on_press(message));
    }
    container(column)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

fn error_card(error: &str, retry: Option<Message>) -> Element<'_, Message> {
    let spacing = theme::active().cosmic().spacing;
    let mut row = Row::new()
        .push(widget::icon::from_name("dialog-warning-symbolic").size(18))
        .push(
            Column::new()
                .push(text(fl!("error-prefix")).size(11.0))
                .push(text(error).size(12.0))
                .width(Length::Fill),
        );
    if let Some(retry) = retry {
        row = row.push(symbolic_button(
            "view-refresh-symbolic",
            fl!("retry"),
            Some(retry),
            false,
        ));
    }
    container(row.spacing(spacing.space_xs).align_y(Alignment::Center))
        .padding(spacing.space_xs)
        .class(theme::Container::custom(tool_error_card_style))
        .into()
}

fn status_dot(active: bool) -> Element<'static, Message> {
    container(
        widget::Space::new()
            .width(Length::Fixed(8.0))
            .height(Length::Fixed(8.0)),
    )
    .class(theme::Container::custom(if active {
        green_dot_style
    } else {
        idle_dot_style
    }))
    .into()
}

fn session_row<'a>(
    session: &'a LocalSession,
    active: bool,
    index: usize,
    responding: bool,
) -> Element<'a, Message> {
    let spacing = theme::active().cosmic().spacing;
    let details = if session.message_count > 0 {
        format!(
            "{} · {}",
            session.relative_label(),
            fl!("messages-count", count = session.message_count)
        )
    } else {
        session.relative_label()
    };
    let row = Row::new()
        .push(text(session.display_title()).size(13.0).width(Length::Fill))
        .push(status_dot(responding))
        .push(text(details).size(10.0))
        .spacing(spacing.space_xxs)
        .align_y(Alignment::Center);
    let class = if active {
        cosmic::theme::Button::Custom {
            active: Box::new(|_, _| selected_session_active_style()),
            disabled: Box::new(|_| selected_session_active_style()),
            hovered: Box::new(|_, _| selected_session_active_style()),
            pressed: Box::new(|_, _| selected_session_active_style()),
        }
    } else {
        cosmic::theme::Button::MenuItem
    };
    button::custom(row)
        .width(Length::Fill)
        .padding([spacing.space_xxs, spacing.space_xs])
        .class(class)
        .on_press(Message::SelectSession(index))
        .into()
}

fn empty_state(compact: bool) -> Element<'static, Message> {
    let spacing = theme::active().cosmic().spacing;
    let mut column = Column::new()
        .spacing(spacing.space_s)
        .align_x(Alignment::Center);
    if !compact {
        column = column.push(
            widget::image(if is_dark() {
                widget::image::Handle::from_bytes(WORDMARK_DARK)
            } else {
                widget::image::Handle::from_bytes(WORDMARK_LIGHT)
            })
            .height(Length::Fixed(40.0)),
        );
    }
    column = column
        .push(
            text(if compact {
                fl!("ready-title")
            } else {
                fl!("empty-title")
            })
            .size(if compact { 14.0 } else { 28.0 }),
        )
        .push(
            text(if compact {
                fl!("ready-hint")
            } else {
                fl!("empty-hint")
            })
            .size(if compact { 11.0 } else { 13.0 }),
        );
    if !compact {
        column = column.push(
            Column::new()
                .push(example_chip(fl!("example-files")))
                .push(example_chip(fl!("example-sandbox")))
                .push(example_chip(fl!("example-battery")))
                .spacing(spacing.space_xs)
                .width(Length::Fill),
        );
    }
    container(column)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

fn example_chip(label: String) -> Element<'static, Message> {
    let spacing = theme::active().cosmic().spacing;
    button::custom(text(label.clone()).size(12.0).width(Length::Fill))
        .class(cosmic::theme::Button::Standard)
        .padding([spacing.space_xxs, spacing.space_s])
        .width(Length::Fill)
        .on_press(Message::SetPrompt(label))
        .into()
}

fn message_bubble(message: &ChatMessage, index: usize, compact: bool) -> Element<'_, Message> {
    let spacing = theme::active().cosmic().spacing;
    let body_size = if compact { 12.0 } else { 14.0 };
    match message.role() {
        ChatRole::User => {
            let mut column = Column::new().spacing(spacing.space_xxs);
            if !message.content.trim().is_empty() {
                column = column.push(
                    container(
                        container(text(message.content.clone()).size(body_size))
                            .padding([spacing.space_xs, spacing.space_s])
                            .class(theme::Container::custom(user_pill_style)),
                    )
                    .width(Length::Fill)
                    .align_x(Alignment::End),
                );
            }
            for result in message.tool_results.iter().filter(|result| result.is_error) {
                column = column.push(tool_result_card(result, compact));
            }
            column.width(Length::Fill).into()
        }
        ChatRole::Assistant => {
            let mut column = Column::new().spacing(spacing.space_xxs);
            if let Some(items) = message.parsed_markdown.as_ref() {
                let palette = if is_dark() {
                    cosmic::iced::theme::Palette::DARK
                } else {
                    cosmic::iced::theme::Palette::LIGHT
                };
                let settings = widget::markdown::Settings::with_text_size(
                    body_size,
                    widget::markdown::Style::from_palette(palette),
                );
                column = column.push(
                    widget::markdown::view(items, settings)
                        .map(|uri| Message::LinkClicked(uri.to_string())),
                );
            } else if !message.content.is_empty() {
                column = column.push(text(message.content.clone()).size(body_size));
            } else if message.in_progress && message.tool_calls.is_empty() {
                column = column.push(text(fl!("streaming")).size(body_size));
            }
            for call in &message.tool_calls {
                column = column.push(tool_call_card(call, compact));
            }
            for result in message.tool_results.iter().filter(|result| result.is_error) {
                column = column.push(tool_result_card(result, compact));
            }
            for warning in &message.warnings {
                column = column.push(warning_card(warning));
            }
            if let Some(error) = &message.error {
                column = column.push(error_card(error, Some(Message::RetryMessage(index))));
            }
            if !message.content.is_empty() && !message.in_progress {
                column = column.push(
                    Row::new()
                        .push(symbolic_button(
                            "edit-copy-symbolic",
                            fl!("copy"),
                            Some(Message::CopyAssistant(index)),
                            false,
                        ))
                        .push(symbolic_button(
                            "view-refresh-symbolic",
                            fl!("retry"),
                            Some(Message::RetryMessage(index)),
                            false,
                        ))
                        .spacing(spacing.space_xxs),
                );
            }
            container(column).width(Length::Fill).into()
        }
    }
}

fn tool_call_card(call: &ToolCallView, compact: bool) -> Element<'_, Message> {
    let spacing = theme::active().cosmic().spacing;
    let column = Column::new()
        .push(
            Row::new()
                .push(widget::icon::from_name("system-run-symbolic").size(16))
                .push(
                    text(if call.name.is_empty() {
                        fl!("tool-running")
                    } else {
                        call.name.clone()
                    })
                    .size(if compact { 11.0 } else { 12.0 }),
                )
                .push(widget::space::horizontal())
                .push(
                    text(if call.in_progress {
                        fl!("tool-running")
                    } else {
                        String::new()
                    })
                    .size(10.0),
                )
                .spacing(spacing.space_xxs)
                .align_y(Alignment::Center),
        )
        .spacing(spacing.space_xxs);
    container(column)
        .padding([spacing.space_xxs, spacing.space_s])
        .class(theme::Container::custom(tool_card_style))
        .width(Length::Fill)
        .into()
}

fn tool_result_card(result: &ToolResultView, _compact: bool) -> Element<'_, Message> {
    let spacing = theme::active().cosmic().spacing;
    let mut label = if result.is_error {
        fl!("tool-error")
    } else {
        fl!("tool-result")
    };
    if !result.name.trim().is_empty() {
        label.push_str(": ");
        label.push_str(&result.name);
    }
    let column = Column::new().push(text(label).size(11.0));
    container(column.spacing(spacing.space_xxs))
        .padding([spacing.space_xxs, spacing.space_s])
        .class(theme::Container::custom(if result.is_error {
            tool_error_card_style
        } else {
            tool_card_style
        }))
        .width(Length::Fill)
        .into()
}

fn warning_card(warning: &str) -> Element<'_, Message> {
    let spacing = theme::active().cosmic().spacing;
    container(
        Row::new()
            .push(widget::icon::from_name("dialog-warning-symbolic").size(16))
            .push(text(format!("{}: {warning}", fl!("warning"))).size(11.0))
            .spacing(spacing.space_xxs),
    )
    .padding([spacing.space_xxs, spacing.space_s])
    .class(theme::Container::custom(tool_card_style))
    .into()
}

fn clip_preview(input: &str, max_chars: usize) -> String {
    let trimmed = input.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut output = trimmed.chars().take(max_chars).collect::<String>();
    output.push_str(" …");
    output
}

fn title_from_prompt(prompt: &str) -> String {
    let first_line = prompt.lines().next().unwrap_or_default().trim();
    let mut title = first_line.chars().take(40).collect::<String>();
    if first_line.chars().count() > 40 {
        title.push('…');
    }
    if title.is_empty() {
        fl!("new-session")
    } else {
        title
    }
}

fn open_uri(uri: &str) -> std::io::Result<()> {
    std::process::Command::new("xdg-open")
        .arg(uri)
        .spawn()
        .map(|_| ())
}

fn is_dark() -> bool {
    theme::active().theme_type.is_dark()
}

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
    let mut fill = cosmic.bg_component_color();
    fill.alpha = 0.55;
    cosmic::widget::container::Style {
        text_color: Some(cosmic.on_bg_color().into()),
        background: Some(Background::Color(fill.into())),
        border: Border::default(),
        shadow: Shadow::default(),
        icon_color: Some(cosmic.on_bg_color().into()),
        snap: true,
    }
}

fn input_card_style(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    let cosmic = theme.cosmic();
    let mut fill = cosmic.bg_component_color();
    fill.alpha = 0.60;
    cosmic::widget::container::Style {
        text_color: Some(cosmic.on_bg_color().into()),
        background: Some(Background::Color(fill.into())),
        border: Border {
            radius: cosmic.radius_l().into(),
            width: 1.0,
            color: cosmic.on_bg_color().with_alpha(0.10).into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.10),
            offset: cosmic::iced::Vector::new(0.0, 2.0),
            blur_radius: 16.0,
        },
        icon_color: Some(cosmic.on_bg_color().into()),
        snap: true,
    }
}

fn user_pill_style(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    let cosmic = theme.cosmic();
    cosmic::widget::container::Style {
        text_color: Some(cosmic.accent.on.into()),
        background: Some(Background::Color(Color::from(cosmic.accent.base))),
        border: Border {
            radius: cosmic.corner_radii.radius_l.into(),
            ..Border::default()
        },
        shadow: Shadow::default(),
        icon_color: Some(cosmic.accent.on.into()),
        snap: true,
    }
}

fn active_pill_style(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    tool_card_style(theme)
}

fn tool_card_style(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    let cosmic = theme.cosmic();
    let mut fill = cosmic.bg_component_color();
    fill.alpha = 0.55;
    cosmic::widget::container::Style {
        text_color: Some(cosmic.on_bg_color().into()),
        background: Some(Background::Color(fill.into())),
        border: Border {
            radius: cosmic.radius_m().into(),
            width: 1.0,
            color: cosmic.on_bg_color().with_alpha(0.10).into(),
        },
        shadow: Shadow::default(),
        icon_color: Some(cosmic.on_bg_color().into()),
        snap: true,
    }
}

fn tool_error_card_style(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    let cosmic = theme.cosmic();
    let mut style = tool_card_style(theme);
    style.border.color = cosmic.destructive.base.into();
    style
}

fn orb_style(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    let cosmic = theme.cosmic();
    let mut fill = cosmic.accent_color();
    fill.alpha = 0.16;
    cosmic::widget::container::Style {
        text_color: Some(cosmic.on_bg_color().into()),
        background: Some(Background::Color(fill.into())),
        border: Border {
            radius: 75.0.into(),
            width: 4.0,
            color: cosmic.accent_color().with_alpha(0.85).into(),
        },
        shadow: Shadow {
            color: cosmic.accent_color().with_alpha(0.25).into(),
            offset: cosmic::iced::Vector::new(0.0, 0.0),
            blur_radius: 24.0,
        },
        icon_color: Some(cosmic.on_bg_color().into()),
        snap: true,
    }
}

fn level_bar_style(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    let cosmic = theme.cosmic();
    cosmic::widget::container::Style {
        background: Some(Background::Color(cosmic.accent_color().into())),
        border: Border {
            radius: 3.0.into(),
            ..Border::default()
        },
        ..Default::default()
    }
}

fn green_dot_style(_: &cosmic::Theme) -> cosmic::widget::container::Style {
    cosmic::widget::container::Style {
        background: Some(Background::Color(Color::from_rgb(0.22, 0.78, 0.36))),
        border: Border {
            radius: 4.0.into(),
            ..Border::default()
        },
        ..Default::default()
    }
}

fn idle_dot_style(_: &cosmic::Theme) -> cosmic::widget::container::Style {
    cosmic::widget::container::Style::default()
}

fn selected_session_active_style() -> cosmic::widget::button::Style {
    let cosmic = theme::active().cosmic().clone();
    cosmic::widget::button::Style {
        background: Some(Background::Color(Color::from(cosmic.accent.base))),
        border_radius: cosmic.corner_radii.radius_s.into(),
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

fn parse_flags() -> Flags {
    let mut flags = Flags::default();
    let mut args = env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--overlay" => flags.overlay = true,
            "--voice" => flags.voice = true,
            "--query" => flags.query = args.next(),
            "--context" => flags.context = args.next(),
            "-h" | "--help" => {
                eprintln!("cos-agent-ui [--overlay] [--voice] [--query TEXT] [--context TEXT]");
                std::process::exit(0);
            }
            other => eprintln!("warning: ignoring unknown flag: {other}"),
        }
    }
    if flags.overlay {
        flags.activation = Some(OverlayActivation {
            voice: flags.voice,
            query: flags.query.clone(),
            context: flags.context.clone(),
        });
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
    localize::localize();
    let flags = parse_flags();
    if flags.overlay {
        cosmic::app::run_single_instance::<App>(
            Settings::default()
                .no_main_window(true)
                .exit_on_close(false)
                .size_limits(
                    Limits::NONE
                        .min_width(1.0)
                        .min_height(120.0)
                        .max_width(560.0)
                        .max_height(560.0),
                ),
            flags,
        )
    } else {
        cosmic::app::run::<App>(
            Settings::default().size_limits(Limits::NONE.min_width(640.0).min_height(420.0)),
            flags,
        )
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/main.rs"));
}
