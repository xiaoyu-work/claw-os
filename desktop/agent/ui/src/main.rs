//! Native ClawOS Agent chat UI.

use std::env;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::LazyLock;
use std::time::Duration;

use cosmic::app::{Core, CosmicFlags, Settings, Task};
use cosmic::dbus_activation::Details;
use cosmic::iced::keyboard::{Key, key::Named};
use cosmic::iced::runtime::core::event::wayland::LayerEvent;
use cosmic::iced::runtime::core::event::{PlatformSpecific, wayland};
use cosmic::iced::widget::{operation, text_editor};
use cosmic::iced::window::Id as SurfaceId;
use cosmic::iced::{Limits, Subscription, event};
use cosmic::widget::{container, text};
use cosmic::{Application, Element, executor, theme, widget};
use futures::future::AbortHandle;
use serde::{Deserialize, Serialize};
use tracing::warn;

mod bridge;
mod bridge_state;
mod effects;
mod localize;
mod overlay;
mod recorder;
mod session;
mod sse;
mod stream_state;
mod styles;
mod views;
mod voice;

use crate::bridge::{
    BridgeEndpoint, ChatRequest, HistoryMessage, ModelsResponse, SessionSummary, StreamEvent,
};
use crate::bridge_state::BridgeState;
use crate::overlay::{OverlayActivation, OverlayState};
use crate::session::{HistoryState, LocalSession, SessionState};
use crate::stream_state::{CancelRequest, StreamReduction, StreamState};
use crate::voice::{VoiceState, VoiceTick};

static EDITOR_ID: LazyLock<widget::Id> = LazyLock::new(|| widget::Id::new("agent-composer"));
static CHAT_SCROLL_ID: LazyLock<widget::Id> = LazyLock::new(|| widget::Id::new("agent-transcript"));
static OVERLAY_ID: LazyLock<SurfaceId> = LazyLock::new(SurfaceId::unique);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Flags {
    pub overlay: bool,
    pub voice: bool,
    pub query: Option<String>,
    pub context: Option<String>,
    pub context_file: Option<PathBuf>,
    #[serde(skip)]
    activation: Option<OverlayActivation>,
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

pub struct App {
    core: Core,
    flags: Flags,
    overlay: OverlayState,
    bridge: BridgeState,
    sessions: SessionState,
    stream: StreamState,
    input: text_editor::Content,
    error: Option<String>,
    voice: VoiceState,
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

    fn init(mut core: Core, mut flags: Flags) -> (Self, Task<Message>) {
        let context_error = flags
            .activation
            .as_mut()
            .and_then(|activation| activation.resolve_context_file().err());
        if let Some(activation) = &flags.activation {
            flags.context.clone_from(&activation.context);
            flags.context_file.clone_from(&activation.context_file);
        }
        if flags.overlay {
            core.window.show_headerbar = false;
            core.window.show_close = false;
            core.window.show_maximize = false;
            core.window.show_minimize = false;
        }
        let mut app = Self {
            core,
            flags: flags.clone(),
            overlay: OverlayState::new(flags.overlay, flags.context.clone(), flags.query.is_some()),
            bridge: BridgeState::connecting(),
            sessions: SessionState::default(),
            stream: StreamState::default(),
            input: text_editor::Content::with_text(flags.query.as_deref().unwrap_or_default()),
            error: context_error.map(|error| error.to_string()),
            voice: VoiceState::default(),
        };
        let mut tasks = vec![effects::connect_bridge()];
        if flags.overlay {
            tasks.push(app.overlay.open(*OVERLAY_ID));
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
        if self.flags.overlay || !self.stream.is_active() {
            Vec::new()
        } else {
            vec![
                container(text(fl!("streaming")).size(11.0))
                    .padding([0u16, 10u16])
                    .class(theme::Container::custom(styles::active_pill))
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
                    self.sessions.retry_branch(message_index)
                else {
                    return Task::none();
                };
                self.sessions.retry_session(prefix, context, title);
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
                self.overlay.set_file_picker_open(true);
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
                self.overlay.set_file_picker_open(false);
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
                self.overlay.set_file_picker_open(false);
                focus_editor()
            }
            Message::FileAttached(Err(error)) => {
                self.overlay.set_file_picker_open(false);
                self.error = Some(format!("{}: {error}", fl!("attachment-error")));
                Task::none()
            }
            Message::Stream(generation, event) => self.handle_stream_event(generation, event),
            Message::TransportError(generation, error) => {
                match self
                    .stream
                    .transport_failed(generation, error.clone(), &mut self.sessions)
                {
                    StreamReduction::Failed { session_index } => {
                        self.bridge.transport_failed(error);
                        Task::batch([
                            self.confirm_provisional_session(session_index),
                            effects::connect_bridge(),
                            scroll_to_bottom(),
                        ])
                    }
                    StreamReduction::Cancelled
                    | StreamReduction::Stale
                    | StreamReduction::Applied => Task::none(),
                    StreamReduction::Terminal | StreamReduction::CancelRemote { .. } => {
                        Task::none()
                    }
                }
            }
            Message::StreamEnded(generation) => {
                let error = fl!("bridge-offline");
                match self
                    .stream
                    .transport_failed(generation, error.clone(), &mut self.sessions)
                {
                    StreamReduction::Failed { session_index } => {
                        self.bridge.transport_failed(error);
                        Task::batch([
                            self.confirm_provisional_session(session_index),
                            effects::connect_bridge(),
                            scroll_to_bottom(),
                        ])
                    }
                    _ => Task::none(),
                }
            }
            Message::CancelFinished {
                session_index,
                message_index,
                result,
            } => {
                if let Some(session_index) = self.stream.cancel_finished(
                    session_index,
                    message_index,
                    result,
                    &mut self.sessions,
                ) {
                    self.confirm_provisional_session(session_index)
                } else {
                    Task::none()
                }
            }
            Message::EscapePressed => {
                if !self.flags.overlay {
                    return Task::none();
                }
                let action = if self.voice.is_active() {
                    self.cancel_voice()
                } else if self.stream.is_active() {
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
                if !self.voice.finish(generation) {
                    return Task::none();
                }
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
                if !self.sessions.select(index) {
                    return Task::none();
                }
                self.error = None;
                Task::batch([self.maybe_fetch_history(index), scroll_to_bottom()])
            }
            Message::NewSession => {
                if self.stream.is_active() {
                    return Task::none();
                }
                self.sessions.new_session();
                self.input = text_editor::Content::new();
                self.error = None;
                Task::batch([focus_editor(), scroll_to_bottom()])
            }
            Message::RetryHistory => {
                if !self.bridge.begin_connect() {
                    Task::none()
                } else {
                    effects::connect_bridge()
                }
            }
            Message::SessionsFetched(Ok(summaries)) => {
                self.sessions.set_error(None);
                self.sessions.merge_remote(summaries);
                scroll_to_bottom()
            }
            Message::SessionsFetched(Err(error)) => {
                self.sessions.set_error(Some(error));
                Task::none()
            }
            Message::HistoryFetched { session_id, result } => {
                self.sessions.apply_history(&session_id, result);
                scroll_to_bottom()
            }
            Message::ProvisionalResolved {
                session_index,
                session_id,
                result,
            } => {
                let resolved = result.is_ok();
                if let Err(error) = &result {
                    tracing::warn!(%error, "failed to verify provisional Agent session");
                }
                self.sessions
                    .reconcile_provisional(session_index, &session_id, &result);
                let deferred = self.overlay.take_deferred_submit();
                if resolved
                    && let Some(deferred) = deferred
                    && deferred.session_index == session_index
                    && self.sessions.active_index() == session_index
                    && self.input.text().trim() == deferred.prompt
                    && deferred.activation_generation == self.overlay.activation_generation()
                {
                    self.overlay.set_pending_context(deferred.context);
                    return self.submit();
                }
                Task::none()
            }
            Message::ModelsFetched(Ok(models)) => {
                self.bridge.models_loaded(models);
                Task::none()
            }
            Message::ModelsFetched(Err(error)) => {
                self.bridge.models_failed(error);
                Task::none()
            }
            Message::Reconnect | Message::BridgeTick => {
                if !self.bridge.begin_connect() {
                    Task::none()
                } else {
                    effects::connect_bridge()
                }
            }
            Message::BridgeConnected(Ok(endpoint)) => {
                self.bridge.connected(endpoint.clone());
                let mut tasks = vec![effects::fetch_models_task(endpoint.clone())];
                if !self.flags.overlay {
                    tasks.push(effects::fetch_sessions_task(endpoint));
                }
                if self
                    .active_session()
                    .is_some_and(|session| matches!(session.history, HistoryState::Failed(_)))
                {
                    tasks.push(self.maybe_fetch_history(self.sessions.active_index()));
                }
                if self.overlay.auto_submit() && (!self.flags.overlay || self.overlay.is_visible())
                {
                    self.overlay.take_auto_submit();
                    tasks.push(Task::done(cosmic::Action::App(Message::Submit)));
                }
                Task::batch(tasks)
            }
            Message::BridgeConnected(Err(error)) => {
                self.bridge.connection_failed(error);
                Task::none()
            }
            Message::Layer(LayerEvent::Focused) => focus_editor(),
            Message::Layer(LayerEvent::Unfocused) if self.overlay.file_picker_open() => {
                Task::none()
            }
            Message::Layer(LayerEvent::Unfocused) => {
                let action = if self.voice.is_active() {
                    self.cancel_voice()
                } else if self.stream.is_active() {
                    self.stop_stream()
                } else {
                    Task::none()
                };
                Task::batch([action, self.close_overlay()])
            }
            Message::Layer(LayerEvent::Done) => {
                self.overlay.layer_done();
                if self.voice.is_active() {
                    self.cancel_voice()
                } else if self.stream.is_active() {
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
        if self.bridge.endpoint().is_none() && !self.bridge.is_connecting() {
            subscriptions.push(
                cosmic::iced::time::every(Duration::from_secs(3)).map(|_| Message::BridgeTick),
            );
        }
        Subscription::batch(subscriptions)
    }

    fn dbus_activation(&mut self, message: cosmic::dbus_activation::Message) -> Task<Message> {
        let mut activation = match message.msg {
            Details::Activate => OverlayActivation::default(),
            Details::ActivateAction { action, .. } => {
                OverlayActivation::from_str(&action).unwrap_or_default()
            }
            Details::Open { .. } => return Task::none(),
        };
        if let Err(error) = activation.resolve_context_file() {
            self.error = Some(error.to_string());
        }
        self.apply_activation(activation)
    }
}

impl App {
    fn active_session(&self) -> Option<&LocalSession> {
        self.sessions.active()
    }

    fn open_overlay(&mut self) -> Task<Message> {
        self.overlay.open(*OVERLAY_ID)
    }

    fn close_overlay(&mut self) -> Task<Message> {
        self.input = text_editor::Content::new();
        self.overlay.close(*OVERLAY_ID)
    }

    fn apply_activation(&mut self, activation: OverlayActivation) -> Task<Message> {
        let query = activation.query.clone();
        let voice = activation.voice;
        self.overlay.begin_activation(activation);
        if let Some(query) = query {
            self.input = text_editor::Content::with_text(&query);
        }
        let mut tasks = Vec::new();
        if !self.overlay.is_visible() {
            tasks.push(self.open_overlay());
        } else {
            tasks.push(focus_editor());
        }
        if voice && !self.voice.is_active() && !self.stream.is_active() {
            tasks.push(Task::done(cosmic::Action::App(Message::ToggleMic)));
        } else if self.overlay.auto_submit()
            && self.bridge.endpoint().is_some()
            && !voice
            && !self.stream.is_active()
            && !self.stream.is_cancelling()
        {
            self.overlay.take_auto_submit();
            tasks.push(Task::done(cosmic::Action::App(Message::Submit)));
        }
        Task::batch(tasks)
    }

    fn maybe_fetch_history(&mut self, index: usize) -> Task<Message> {
        let endpoint = self.bridge.endpoint().cloned();
        let Some(remote_id) =
            self.sessions
                .begin_history_load(index, endpoint.is_some(), fl!("bridge-offline"))
        else {
            return Task::none();
        };
        let Some(endpoint) = endpoint else {
            return Task::none();
        };
        effects::fetch_history_task(endpoint, remote_id)
    }

    fn confirm_provisional_session(&self, session_index: usize) -> Task<Message> {
        let Some(endpoint) = self.bridge.endpoint().cloned() else {
            return Task::none();
        };
        let Some(session_id) = self
            .sessions
            .get(session_index)
            .and_then(|session| session.provisional_remote_id.clone())
        else {
            return Task::none();
        };
        effects::confirm_provisional_task(endpoint, session_index, session_id)
    }

    fn submit(&mut self) -> Task<Message> {
        let prompt = self.input.text().trim().to_string();
        if prompt.is_empty() || self.stream.is_active() || self.stream.is_cancelling() {
            return Task::none();
        }
        if self
            .active_session()
            .is_some_and(|session| session.provisional_remote_id.is_some())
        {
            let active = self.sessions.active_index();
            self.overlay.defer_submit(active, prompt);
            return self.confirm_provisional_session(active);
        }
        if !self.sessions.history_ready() {
            return self.maybe_fetch_history(self.sessions.active_index());
        }
        let cancel_voice = if self.voice.is_active() {
            self.cancel_voice()
        } else {
            Task::none()
        };
        let Some(endpoint) = self.bridge.endpoint().cloned() else {
            self.error = Some(fl!("bridge-offline"));
            if self.bridge.begin_connect() {
                return Task::batch([cancel_voice, effects::connect_bridge()]);
            }
            return cancel_voice;
        };

        self.input = text_editor::Content::new();
        self.error = None;
        let stream_session = self.sessions.begin_stream(prompt.clone());
        let one_shot_context = self
            .overlay
            .pending_context()
            .map(str::trim)
            .filter(|context| !context.is_empty())
            .map(ToOwned::to_owned);
        self.overlay
            .begin_stream_context(one_shot_context.is_some());
        let request = ChatRequest {
            prompt: Some(prompt),
            session_id: stream_session.remote_id,
            model: None,
            context: one_shot_context,
            branch_context: stream_session.persistent_context,
            ..ChatRequest::default()
        };
        let (abort_handle, abort_registration) = AbortHandle::new_pair();
        let generation = self.stream.start(stream_session.index, abort_handle);
        let stream_task = effects::open_stream(endpoint, request, generation, abort_registration);
        Task::batch([cancel_voice, stream_task, scroll_to_bottom()])
    }

    fn handle_stream_event(&mut self, generation: u64, event: StreamEvent) -> Task<Message> {
        let task_started = matches!(event, StreamEvent::TaskStarted(_));
        let reduction = self.stream.reduce(generation, event, &mut self.sessions);
        if task_started && !matches!(reduction, StreamReduction::Stale) {
            self.overlay.consume_stream_context();
        }
        match reduction {
            StreamReduction::CancelRemote {
                task_id,
                session_index,
                message_index,
            } => self.cancel_task(task_id, session_index, message_index),
            StreamReduction::Failed { session_index } => Task::batch([
                self.confirm_provisional_session(session_index),
                scroll_to_bottom(),
            ]),
            StreamReduction::Terminal => {
                if self.overlay.auto_submit() && (!self.flags.overlay || self.overlay.is_visible())
                {
                    self.overlay.take_auto_submit();
                    Task::batch([
                        Task::done(cosmic::Action::App(Message::Submit)),
                        scroll_to_bottom(),
                    ])
                } else {
                    scroll_to_bottom()
                }
            }
            StreamReduction::Applied => scroll_to_bottom(),
            StreamReduction::Cancelled | StreamReduction::Stale => Task::none(),
        }
    }

    fn active_history_ready(&self) -> bool {
        self.sessions.history_ready()
    }

    fn stop_stream(&mut self) -> Task<Message> {
        let Some(request) = self.stream.request_cancel(&mut self.sessions) else {
            return Task::none();
        };
        self.overlay.consume_stream_context();
        match request {
            CancelRequest::AwaitTask => scroll_to_bottom(),
            CancelRequest::Remote {
                task_id,
                session_index,
                message_index,
            } => Task::batch([
                self.cancel_task(task_id, session_index, message_index),
                scroll_to_bottom(),
            ]),
        }
    }

    fn cancel_task(
        &self,
        task_id: String,
        session_index: usize,
        message_index: usize,
    ) -> Task<Message> {
        let Some(endpoint) = self.bridge.endpoint().cloned() else {
            return Task::none();
        };
        effects::cancel_stream(endpoint, task_id, session_index, message_index)
    }

    fn start_voice(&mut self) -> Task<Message> {
        if self.stream.is_active() || self.stream.is_cancelling() {
            return Task::none();
        }
        match self.voice.start() {
            Ok(()) => self.error = None,
            Err(error) => self.error = Some(format!("{}: {error}", fl!("voice-unavailable"))),
        }
        Task::none()
    }

    fn stop_voice(&mut self) -> Task<Message> {
        match self.voice.stop(self.bridge.endpoint().cloned()) {
            Ok(task) => task,
            Err(error) => {
                self.error = Some(error);
                Task::none()
            }
        }
    }

    fn cancel_voice(&mut self) -> Task<Message> {
        self.voice.cancel()
    }

    fn voice_tick(&mut self) -> Task<Message> {
        match self.voice.tick() {
            VoiceTick::Continue => Task::none(),
            VoiceTick::Stop => self.stop_voice(),
            VoiceTick::Failed(error) => {
                self.error = Some(error);
                Task::none()
            }
        }
    }
}

fn focus_editor() -> Task<Message> {
    operation::focus(EDITOR_ID.clone())
}

fn scroll_to_bottom() -> Task<Message> {
    operation::snap_to_end(CHAT_SCROLL_ID.clone())
}

fn open_uri(uri: &str) -> std::io::Result<()> {
    std::process::Command::new("xdg-open")
        .arg(uri)
        .spawn()
        .map(|_| ())
}

fn parse_flags() -> Flags {
    let parsed = cos_runtime::ask_claw::parse_ui_arguments(env::args().skip(1));
    if parsed.help {
        eprintln!("{}", cos_runtime::ask_claw::UI_USAGE);
        std::process::exit(0);
    }
    for argument in &parsed.unknown {
        eprintln!("warning: ignoring unknown flag: {argument}");
    }
    let activation = parsed.activation();
    Flags {
        overlay: parsed.overlay,
        voice: parsed.voice,
        query: parsed.query,
        context: parsed.context,
        context_file: parsed.context_file,
        activation,
    }
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
