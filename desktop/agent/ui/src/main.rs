//! Native ClawOS Agent chat UI.

use std::env;
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

mod activities;
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
    BridgeEndpoint, ChatAttachment, ChatRequest, MAX_CHAT_ATTACHMENT_BYTES, ModelsResponse,
    SessionSummary, SessionUpdateRequest, StreamEvent, validate_chat_attachments,
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
    Activities(activities::Message),
    OpenActivitySession(String),
    CopyActivityObjectOperation(String),
    CopyActivityContinuity,
    EditorAction(text_editor::Action),
    SetPrompt(String),
    Submit,
    StopStream,
    RetryMessage(usize),
    CopyAssistant(usize),
    AttachFile,
    FileAttached(Result<Option<ChatAttachment>, String>),
    ClearAttachments,
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
    SessionFilterChanged(String),
    ToggleArchivedSessions,
    BeginRenameSession(usize),
    RenameSessionChanged(String),
    SaveRenamedSession,
    CancelRenameSession,
    ArchiveSession(usize),
    ForkSession(usize),
    SessionUpdated(Result<SessionSummary, String>),
    SessionForked(Result<SessionSummary, String>),
    FollowUpQueued {
        prompt: String,
        result: Result<cos_agent_protocol::TaskStarted, String>,
    },
    RetryHistory,
    SessionsFetched(Result<Vec<SessionSummary>, String>),
    HistoryFetched {
        session_id: String,
        result: Result<cos_agent_protocol::HistoryResponse, String>,
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
    activities: activities::Activities,
    sessions: SessionState,
    stream: StreamState,
    input: text_editor::Content,
    attachments: Vec<ChatAttachment>,
    session_filter: String,
    show_archived_sessions: bool,
    renaming_session: Option<usize>,
    rename_session_title: String,
    queued_followups: usize,
    queue_tail_task_id: Option<String>,
    queue_pending: bool,
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
            overlay: OverlayState::new(flags.overlay, flags.context.clone(), flags.query.is_some()),
            bridge: BridgeState::connecting(),
            activities: activities::Activities::default(),
            sessions: SessionState::default(),
            stream: StreamState::default(),
            input: text_editor::Content::with_text(flags.query.as_deref().unwrap_or_default()),
            attachments: Vec::new(),
            session_filter: String::new(),
            show_archived_sessions: false,
            renaming_session: None,
            rename_session_title: String::new(),
            queued_followups: 0,
            queue_tail_task_id: None,
            queue_pending: false,
            error: None,
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
            Message::Activities(message) => self.update_activities(message),
            Message::OpenActivitySession(id) => self.open_activity_session(id),
            Message::CopyActivityObjectOperation(reference) => {
                if self.flags.overlay || !self.activities.is_visible() {
                    return Task::none();
                }
                match self.activities.object_operation_text(&reference) {
                    Some(description) => cosmic::iced::clipboard::write(description),
                    None => Task::none(),
                }
            }
            Message::CopyActivityContinuity => {
                if self.flags.overlay || !self.activities.is_visible() {
                    return Task::none();
                }
                match self.activities.continuity_copy_text() {
                    Some(document) => cosmic::iced::clipboard::write(document),
                    None => Task::none(),
                }
            }
            Message::EditorAction(action) => {
                if !self.voice.is_active() && !self.queue_pending {
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
                if self.queue_pending {
                    return Task::none();
                }
                self.overlay.set_file_picker_open(true);
                Task::perform(
                    async {
                        let dialog = cosmic::dialog::file_chooser::open::Dialog::new()
                            .title(fl!("attach-file"));
                        match dialog.open_file().await {
                            Ok(response) => {
                                let path = response
                                    .url()
                                    .to_file_path()
                                    .map_err(|_| fl!("attachment-error"))?;
                                let metadata = tokio::fs::metadata(&path)
                                    .await
                                    .map_err(|error| error.to_string())?;
                                if metadata.len() == 0
                                    || metadata.len() > MAX_CHAT_ATTACHMENT_BYTES as u64
                                {
                                    return Err(fl!("attachment-error"));
                                }
                                let name = path
                                    .file_name()
                                    .and_then(|name| name.to_str())
                                    .ok_or_else(|| fl!("attachment-error"))?
                                    .to_string();
                                let bytes = tokio::fs::read(&path)
                                    .await
                                    .map_err(|error| error.to_string())?;
                                ChatAttachment::from_bytes(name, &bytes)
                                    .map(Some)
                                    .map_err(str::to_string)
                            }
                            Err(cosmic::dialog::file_chooser::Error::Cancelled) => Ok(None),
                            Err(error) => Err(error.to_string()),
                        }
                    },
                    |result| cosmic::Action::App(Message::FileAttached(result)),
                )
            }
            Message::FileAttached(Ok(Some(attachment))) => {
                self.overlay.set_file_picker_open(false);
                let mut attachments = self.attachments.clone();
                attachments.push(attachment);
                match validate_chat_attachments(&attachments) {
                    Ok(()) => {
                        self.attachments = attachments;
                        self.error = None;
                    }
                    Err(error) => self.error = Some(error.to_string()),
                }
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
            Message::ClearAttachments => {
                self.attachments.clear();
                self.error = None;
                focus_editor()
            }
            Message::Stream(generation, event) => self.handle_stream_event(generation, event),
            Message::TransportError(generation, error) => {
                match self
                    .stream
                    .transport_failed(generation, error.clone(), &mut self.sessions)
                {
                    StreamReduction::Failed { session_index } => {
                        self.bridge.transport_failed(error);
                        self.sessions.invalidate_history(session_index);
                        Task::batch([
                            self.confirm_provisional_session(session_index),
                            self.maybe_fetch_history(session_index),
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
                        self.sessions.invalidate_history(session_index);
                        Task::batch([
                            self.confirm_provisional_session(session_index),
                            self.maybe_fetch_history(session_index),
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
                    let mut tasks = vec![self.confirm_provisional_session(session_index)];
                    if self.queued_followups > 0 {
                        self.sessions.invalidate_history(session_index);
                        tasks.push(self.maybe_fetch_history(session_index));
                    } else {
                        self.queue_tail_task_id = None;
                    }
                    Task::batch(tasks)
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
                if self.stream.is_cancelling() || self.queue_pending {
                    return Task::none();
                }
                if self.stream.is_active() && self.stream.session_index() != Some(index) {
                    if self.stream.task_id().is_none()
                        || self
                            .active_session()
                            .and_then(|session| session.remote_id.as_deref())
                            .is_none()
                        || self.queue_pending
                    {
                        return Task::none();
                    }
                    if let Some(previous) = self.stream.detach() {
                        self.sessions.invalidate_history(previous);
                    }
                }
                if !self.sessions.select(index) {
                    return Task::none();
                }
                self.activities.hide();
                self.attachments.clear();
                self.renaming_session = None;
                self.queued_followups = 0;
                self.queue_tail_task_id = None;
                self.error = None;
                Task::batch([self.maybe_fetch_history(index), scroll_to_bottom()])
            }
            Message::NewSession => {
                if self.stream.is_cancelling() || self.queue_pending {
                    return Task::none();
                }
                if self.stream.is_active() {
                    if self.stream.task_id().is_none()
                        || self
                            .active_session()
                            .and_then(|session| session.remote_id.as_deref())
                            .is_none()
                        || self.queue_pending
                    {
                        return Task::none();
                    }
                    if let Some(previous) = self.stream.detach() {
                        self.sessions.invalidate_history(previous);
                    }
                }
                self.activities.hide();
                self.sessions.new_session();
                self.input = text_editor::Content::new();
                self.attachments.clear();
                self.renaming_session = None;
                self.queued_followups = 0;
                self.queue_tail_task_id = None;
                self.error = None;
                Task::batch([focus_editor(), scroll_to_bottom()])
            }
            Message::SessionFilterChanged(value) => {
                self.session_filter = value;
                Task::none()
            }
            Message::ToggleArchivedSessions => {
                self.show_archived_sessions = !self.show_archived_sessions;
                self.session_filter.clear();
                self.renaming_session = None;
                let Some(endpoint) = self.bridge.endpoint().cloned() else {
                    self.error = Some(fl!("bridge-offline"));
                    return Task::none();
                };
                effects::fetch_sessions_task(endpoint, self.show_archived_sessions)
            }
            Message::BeginRenameSession(index) => {
                let Some(session) = self
                    .sessions
                    .get(index)
                    .filter(|session| session.manageable)
                else {
                    return Task::none();
                };
                self.rename_session_title = session.display_title();
                self.renaming_session = Some(index);
                Task::none()
            }
            Message::RenameSessionChanged(value) => {
                self.rename_session_title = value;
                Task::none()
            }
            Message::CancelRenameSession => {
                self.renaming_session = None;
                self.rename_session_title.clear();
                Task::none()
            }
            Message::SaveRenamedSession => {
                let Some(index) = self.renaming_session else {
                    return Task::none();
                };
                let Some(id) = self
                    .sessions
                    .get(index)
                    .filter(|session| session.manageable)
                    .and_then(|session| session.remote_id.clone())
                else {
                    return Task::none();
                };
                let title = self.rename_session_title.trim().to_string();
                if title.is_empty() {
                    return Task::none();
                }
                let Some(endpoint) = self.bridge.endpoint().cloned() else {
                    self.error = Some(fl!("bridge-offline"));
                    return Task::none();
                };
                effects::update_session_task(
                    endpoint,
                    id,
                    SessionUpdateRequest {
                        title: Some(title),
                        archived: None,
                    },
                )
            }
            Message::ArchiveSession(index) => {
                let Some(session) = self
                    .sessions
                    .get(index)
                    .filter(|session| session.manageable)
                else {
                    return Task::none();
                };
                let Some(id) = session.remote_id.clone() else {
                    return Task::none();
                };
                let archived = !session.archived;
                let Some(endpoint) = self.bridge.endpoint().cloned() else {
                    self.error = Some(fl!("bridge-offline"));
                    return Task::none();
                };
                effects::update_session_task(
                    endpoint,
                    id,
                    SessionUpdateRequest {
                        title: None,
                        archived: Some(archived),
                    },
                )
            }
            Message::ForkSession(index) => {
                if self.stream.is_active() || self.stream.is_cancelling() || self.queue_pending {
                    return Task::none();
                }
                let Some(id) = self
                    .sessions
                    .get(index)
                    .filter(|session| session.manageable)
                    .and_then(|session| session.remote_id.clone())
                else {
                    return Task::none();
                };
                let Some(endpoint) = self.bridge.endpoint().cloned() else {
                    self.error = Some(fl!("bridge-offline"));
                    return Task::none();
                };
                effects::fork_session_task(endpoint, id)
            }
            Message::SessionUpdated(Ok(summary)) => {
                self.sessions.apply_remote_summary(summary, false);
                self.renaming_session = None;
                self.rename_session_title.clear();
                self.error = None;
                Task::none()
            }
            Message::SessionUpdated(Err(error)) => {
                self.error = Some(error);
                Task::none()
            }
            Message::SessionForked(Ok(summary)) => {
                let select =
                    !self.stream.is_active() && !self.stream.is_cancelling() && !self.queue_pending;
                let index = self.sessions.apply_remote_summary(summary, select);
                if !select {
                    return Task::none();
                }
                self.activities.hide();
                self.renaming_session = None;
                self.error = None;
                Task::batch([self.maybe_fetch_history(index), scroll_to_bottom()])
            }
            Message::SessionForked(Err(error)) => {
                self.error = Some(error);
                Task::none()
            }
            Message::FollowUpQueued {
                prompt,
                result: Ok(started),
            } => {
                self.queue_pending = false;
                self.queue_tail_task_id = Some(started.task_id);
                self.queued_followups = self.queued_followups.saturating_add(1);
                if self.input.text().trim() == prompt {
                    self.input = text_editor::Content::new();
                    self.attachments.clear();
                }
                self.overlay.consume_stream_context();
                self.error = None;
                if self.stream.is_active() || self.stream.is_cancelling() {
                    focus_editor()
                } else {
                    let session_index = self.sessions.active_index();
                    self.sessions.invalidate_history(session_index);
                    Task::batch([self.maybe_fetch_history(session_index), focus_editor()])
                }
            }
            Message::FollowUpQueued {
                result: Err(error), ..
            } => {
                self.queue_pending = false;
                self.error = Some(error);
                focus_editor()
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
                let is_active_session = self
                    .active_session()
                    .and_then(|session| session.remote_id.as_deref())
                    == Some(session_id.as_str());
                let Some(reattach) = self.sessions.apply_history(&session_id, result) else {
                    if is_active_session {
                        self.queued_followups = 0;
                        self.queue_tail_task_id = None;
                    }
                    return scroll_to_bottom();
                };
                if !is_active_session {
                    self.sessions.invalidate_history(reattach.session_index);
                    return Task::none();
                }
                let Some(endpoint) = self.bridge.endpoint().cloned() else {
                    self.error = Some(fl!("bridge-offline"));
                    self.sessions.invalidate_history(reattach.session_index);
                    return self.maybe_fetch_history(reattach.session_index);
                };
                let (abort_handle, abort_registration) = AbortHandle::new_pair();
                let generation = self.stream.start(reattach.session_index, abort_handle);
                self.queued_followups = reattach.queued_after;
                self.queue_tail_task_id = Some(reattach.tail_id);
                Task::batch([
                    effects::open_task_stream(
                        endpoint,
                        reattach.id,
                        generation,
                        abort_registration,
                    ),
                    scroll_to_bottom(),
                ])
            }
            Message::ProvisionalResolved {
                session_index,
                session_id,
                result,
            } => {
                let resolved = matches!(&result, Ok(true));
                if let Err(error) = &result {
                    tracing::warn!(%error, "failed to verify provisional Agent session");
                }
                self.sessions
                    .reconcile_provisional(session_index, &session_id, &result);
                if resolved
                    && self.sessions.active_index() == session_index
                    && !self.stream.is_active()
                    && !self.stream.is_cancelling()
                    && self.queue_tail_task_id.is_some()
                {
                    self.sessions.invalidate_history(session_index);
                    return self.maybe_fetch_history(session_index);
                }
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
                    tasks.push(effects::fetch_sessions_task(
                        endpoint,
                        self.show_archived_sessions,
                    ));
                    if self.activities.is_visible() {
                        tasks.push(self.update_activities(activities::Message::Refresh));
                    }
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
        if !self.flags.overlay && self.bridge.endpoint().is_some() && self.activities.should_poll()
        {
            subscriptions.push(
                cosmic::iced::time::every(Duration::from_secs(5))
                    .map(|_| Message::Activities(activities::Message::Tick)),
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
    fn update_activities(&mut self, message: activities::Message) -> Task<Message> {
        if self.flags.overlay
            || (matches!(message, activities::Message::Show)
                && (self.stream.is_active() || self.voice.is_active()))
        {
            return Task::none();
        }
        let endpoint = self.bridge.endpoint().cloned();
        match (
            self.activities.update(message, endpoint.is_some()),
            endpoint,
        ) {
            (Some(request), Some(endpoint)) => effects::activity_task(endpoint, request),
            _ => Task::none(),
        }
    }

    fn open_activity_session(&mut self, id: String) -> Task<Message> {
        if self.flags.overlay {
            return Task::none();
        }
        let Some(title) = self.activities.session_title(&id) else {
            return Task::none();
        };
        self.sessions.merge_remote(vec![SessionSummary {
            id: id.clone(),
            title,
            last_ts_ms: None,
            message_count: 0,
            manageable: true,
            ..SessionSummary::default()
        }]);
        let index = self
            .sessions
            .iter()
            .position(|session| session.remote_id.as_deref() == Some(&id));
        let Some(index) = index else {
            return Task::none();
        };
        if (!self.stream.is_active() || self.stream.session_index() != Some(index))
            && let Some(session) = self.sessions.get_mut(index)
        {
            session.history = HistoryState::NotLoaded;
        }
        self.sessions.select(index);
        self.activities.hide();
        self.error = None;
        Task::batch([self.maybe_fetch_history(index), scroll_to_bottom()])
    }

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
        if prompt.is_empty() || self.stream.is_cancelling() || self.queue_pending {
            return Task::none();
        }
        if self.stream.is_active() {
            return self.queue_follow_up(prompt);
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
        self.queued_followups = 0;
        self.queue_tail_task_id = None;
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
            attachments: self.attachments.clone(),
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

    fn queue_follow_up(&mut self, prompt: String) -> Task<Message> {
        let predecessor_id = self
            .queue_tail_task_id
            .clone()
            .or_else(|| self.stream.task_id().map(str::to_string));
        let Some(predecessor_id) = predecessor_id else {
            return Task::none();
        };
        let Some(endpoint) = self.bridge.endpoint().cloned() else {
            self.error = Some(fl!("bridge-offline"));
            return Task::none();
        };
        let request = ChatRequest {
            prompt: Some(prompt.clone()),
            attachments: self.attachments.clone(),
            model: None,
            context: self
                .overlay
                .pending_context()
                .map(str::trim)
                .filter(|context| !context.is_empty())
                .map(ToOwned::to_owned),
            branch_context: self
                .active_session()
                .and_then(|session| session.persistent_context.clone()),
            ..ChatRequest::default()
        };
        self.queue_pending = true;
        effects::queue_follow_up_task(endpoint, predecessor_id, request, prompt)
    }

    fn handle_stream_event(&mut self, generation: u64, event: StreamEvent) -> Task<Message> {
        let task_started = matches!(event, StreamEvent::TaskStarted(_));
        let stream_session_index = self.stream.session_index();
        let had_task_id = self.stream.task_id().is_some();
        let reduction = self.stream.reduce(generation, event, &mut self.sessions);
        if task_started && !matches!(reduction, StreamReduction::Stale) {
            if self.queue_tail_task_id.is_none() {
                self.queue_tail_task_id = self.stream.task_id().map(str::to_string);
            }
            self.attachments.clear();
            self.overlay.consume_stream_context();
        }
        match reduction {
            StreamReduction::CancelRemote {
                task_id,
                session_index,
                message_index,
            } => self.cancel_task(task_id, session_index, message_index),
            StreamReduction::Failed { session_index } => {
                let mut tasks = vec![
                    self.confirm_provisional_session(session_index),
                    scroll_to_bottom(),
                ];
                if had_task_id || self.queued_followups > 0 {
                    self.sessions.invalidate_history(session_index);
                    tasks.push(self.maybe_fetch_history(session_index));
                } else {
                    self.queue_tail_task_id = None;
                }
                Task::batch(tasks)
            }
            StreamReduction::Terminal => {
                if self.queued_followups > 0
                    && let Some(session_index) = stream_session_index
                {
                    self.sessions.invalidate_history(session_index);
                    return Task::batch([
                        self.maybe_fetch_history(session_index),
                        scroll_to_bottom(),
                    ]);
                }
                self.queue_tail_task_id = None;
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

fn parse_arguments() -> cos_runtime::ask_claw::UiArguments {
    let parsed = cos_runtime::ask_claw::parse_ui_arguments(env::args().skip(1));
    if parsed.help {
        eprintln!("{}", cos_runtime::ask_claw::UI_USAGE);
        std::process::exit(0);
    }
    for argument in &parsed.unknown {
        eprintln!("warning: ignoring unknown flag: {argument}");
    }
    parsed
}

fn flags_from_arguments(parsed: cos_runtime::ask_claw::UiArguments) -> Flags {
    let activation = match parsed.activation_from_process_socket() {
        Ok(activation) => activation,
        Err(error) => {
            eprintln!("failed to read Ask Claw activation: {error}");
            std::process::exit(2);
        }
    };
    let voice = activation
        .as_ref()
        .map(|activation| activation.voice)
        .unwrap_or(parsed.voice);
    let query = activation
        .as_ref()
        .and_then(|activation| activation.query.clone())
        .or(parsed.query);
    let context = activation
        .as_ref()
        .and_then(|activation| activation.context.clone())
        .or(parsed.context);
    Flags {
        overlay: parsed.overlay,
        voice,
        query,
        context,
        activation,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UiLaunchMode {
    Window,
    SharedOverlay,
    PrivateOverlay,
}

impl UiLaunchMode {
    fn from_arguments(arguments: &cos_runtime::ask_claw::UiArguments) -> Self {
        if !arguments.overlay {
            Self::Window
        } else if arguments.context_socket
            || arguments.context.is_some()
            || arguments.query.is_some()
        {
            Self::PrivateOverlay
        } else {
            Self::SharedOverlay
        }
    }

    fn exits_on_close(self) -> bool {
        self == Self::PrivateOverlay
    }
}

fn overlay_settings(exit_on_close: bool) -> Settings {
    Settings::default()
        .no_main_window(true)
        .exit_on_close(exit_on_close)
        .size_limits(
            Limits::NONE
                .min_width(1.0)
                .min_height(120.0)
                .max_width(560.0)
                .max_height(560.0),
        )
}

fn main() -> cosmic::iced::Result {
    let parsed = parse_arguments();
    let launch_mode = UiLaunchMode::from_arguments(&parsed);
    let settings = match launch_mode {
        UiLaunchMode::Window => {
            Settings::default().size_limits(Limits::NONE.min_width(640.0).min_height(420.0))
        }
        UiLaunchMode::SharedOverlay | UiLaunchMode::PrivateOverlay => {
            overlay_settings(launch_mode.exits_on_close())
        }
    };
    let flags = flags_from_arguments(parsed);

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .try_init();
    localize::localize();
    if launch_mode == UiLaunchMode::SharedOverlay {
        cosmic::app::run_single_instance::<App>(settings, flags)
    } else {
        cosmic::app::run::<App>(settings, flags)
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/main.rs"));
}
