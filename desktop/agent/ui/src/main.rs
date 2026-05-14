//! `cos-agent-ui` — native libcosmic chat client for the Claw OS agent.
//!
//! Replaces the React + WebView app under `desktop/agent/web/`. The
//! bridge under `desktop/agent/bridge/` stays in place during this
//! transition and serves as the single contract: this UI POSTs to
//! `http://127.0.0.1:<port>/api/chat` and consumes the same SSE
//! stream the React app did.
//!
//! Two visual modes mirror the original:
//!
//!   * **Standalone** — full window, centered, larger brand.
//!   * **Overlay**    — compact, anchored, Esc closes (for the
//!                      global `Super+A` summon hotkey).
//!
//! Selected with `--overlay` on the command line. Falls back to
//! standalone.

use std::env;

use cosmic::app::{Core, Settings, Task};
use cosmic::iced::keyboard::{Key, key::Named};
use cosmic::iced::{Alignment, Length, Limits, Subscription, event};
use cosmic::widget::{button, column, container, markdown, row, scrollable, text, text_input};
use cosmic::{Application, Element, executor, theme, widget};
use tracing::warn;

mod bridge;
mod recorder;
mod sse;

use crate::bridge::{ChatRequest, StreamEvent, read_bridge_port};
use crate::recorder::Recorder;

/// Symbol PNGs (square logo) for the header.
static SYMBOL_LIGHT: &[u8] = include_bytes!("../assets/clawos-symbol.png");
static SYMBOL_DARK: &[u8] = include_bytes!("../assets/clawos-symbol-dark.png");

/// Wordmark PNGs (logotype) for the standalone header.
static WORDMARK_LIGHT: &[u8] = include_bytes!("../assets/clawos-wordmark.png");
static WORDMARK_DARK: &[u8] = include_bytes!("../assets/clawos-wordmark-dark.png");

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

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
    /// True while the assistant is still streaming this message.
    pub in_progress: bool,
}

pub struct App {
    core: Core,
    flags: Flags,
    bridge_port: Option<u16>,
    bridge_error: Option<String>,

    messages: Vec<ChatMessage>,
    input: String,
    streaming: bool,
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

        let (bridge_port, bridge_error) = match read_bridge_port() {
            Ok(p) => (Some(p), None),
            Err(e) => {
                warn!("cos-agent-bridge unreachable: {e:#}");
                (None, Some(format!("Bridge unavailable: {e}")))
            }
        };

        let app = App {
            core,
            flags: flags.clone(),
            bridge_port,
            bridge_error,
            messages: Vec::new(),
            input: flags.query.clone().unwrap_or_default(),
            streaming: false,
            error: None,
            voice: VoiceState::Idle,
        };
        // When launched with --voice (Super+Shift+A path), pre-arm the
        // mic so the user can start speaking immediately. Wrapping in
        // a Task::done lets init() return synchronously and the mic
        // opens on the first frame.
        let initial = if flags.query.is_some() {
            cosmic::Task::done(cosmic::Action::App(Message::Submit))
        } else if flags.voice {
            cosmic::Task::done(cosmic::Action::App(Message::ToggleMic))
        } else {
            Task::none()
        };
        (app, initial)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::InputChanged(s) => {
                self.input = s;
                Task::none()
            }

            Message::Submit => self.submit(),

            Message::StreamDelta(chunk) => {
                if let Some(last) = self.messages.last_mut()
                    && last.role == ChatRole::Assistant
                {
                    last.content.push_str(&chunk);
                }
                Task::none()
            }

            Message::StreamDone(_envelope) => {
                if let Some(last) = self.messages.last_mut() {
                    last.in_progress = false;
                }
                self.streaming = false;
                Task::none()
            }

            Message::StreamError(msg) => {
                if let Some(last) = self.messages.last_mut() {
                    last.in_progress = false;
                }
                self.streaming = false;
                self.error = Some(msg);
                Task::none()
            }

            Message::StreamEnded => {
                if let Some(last) = self.messages.last_mut() {
                    last.in_progress = false;
                }
                self.streaming = false;
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
                    // Surface as an error/tip rather than dropping the
                    // stub string into the user's input. Once the
                    // bridge wires a real STT backend this branch goes
                    // away — `placeholder=false` will populate input
                    // verbatim like the React app did.
                    self.error = Some(
                        "Voice transcription isn't enabled on this system yet.".into(),
                    );
                } else if !text.is_empty() {
                    if self.input.is_empty() {
                        self.input = text;
                    } else {
                        // Append with a space so users can chain
                        // dictation onto a partial draft.
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
        }
    }

    fn view(&self) -> Element<Message> {
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
        self.messages.push(ChatMessage {
            role: ChatRole::User,
            content: prompt.clone(),
            in_progress: false,
        });
        self.messages.push(ChatMessage {
            role: ChatRole::Assistant,
            content: String::new(),
            in_progress: true,
        });

        let request = ChatRequest {
            prompt,
            session_id: None,
            model: None,
        };
        cosmic::Task::stream(cosmic::iced::stream::channel(
            16,
            move |mut tx| async move {
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

    fn view_standalone(&self) -> Element<Message> {
        let cosmic_theme = theme::active().cosmic().clone();
        let spacing = cosmic_theme.spacing;

        let header = container(
            widget::image(if is_dark() {
                widget::image::Handle::from_bytes(WORDMARK_DARK)
            } else {
                widget::image::Handle::from_bytes(WORDMARK_LIGHT)
            })
            .height(Length::Fixed(40.0)),
        )
        .center_x(Length::Fill)
        .padding([spacing.space_l, 0u16]);

        let body = if self.messages.is_empty() {
            empty_state(false)
        } else {
            self.message_list(false)
        };

        let input = self.input_row(false);

        let inner = column()
            .push(header)
            .push(
                container(body)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .padding(spacing.space_m),
            )
            .push(container(input).padding([0u16, spacing.space_l, spacing.space_m, spacing.space_l]))
            .spacing(spacing.space_xs);

        container(inner)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .into()
    }

    fn view_overlay(&self) -> Element<Message> {
        let cosmic_theme = theme::active().cosmic().clone();
        let spacing = cosmic_theme.spacing;

        let header = row()
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
            .push(cosmic::widget::space::horizontal())
            .push(text("Esc to close").size(11.0))
            .align_y(Alignment::Center)
            .spacing(spacing.space_xs);

        let body = if self.messages.is_empty() {
            empty_state(true)
        } else {
            self.message_list(true)
        };

        let inner = column()
            .push(container(header).padding(spacing.space_xs))
            .push(
                container(body)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .padding([0u16, spacing.space_xs]),
            )
            .push(container(self.input_row(true)).padding(spacing.space_xs))
            .spacing(spacing.space_xs);

        container(inner)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn message_list(&self, compact: bool) -> Element<Message> {
        let cosmic_theme = theme::active().cosmic().clone();
        let spacing = cosmic_theme.spacing;

        let mut col = column().spacing(spacing.space_s).width(Length::Fill);

        for msg in &self.messages {
            col = col.push(message_bubble(msg, compact));
        }

        if let Some(err) = &self.error {
            col = col.push(
                container(text(format!("⚠ {err}")).size(12.0))
                    .padding(spacing.space_xxs)
                    .class(theme::Container::Card),
            );
        }

        scrollable(col).width(Length::Fill).into()
    }

    fn input_row(&self, compact: bool) -> Element<Message> {
        let cosmic_theme = theme::active().cosmic().clone();
        let spacing = cosmic_theme.spacing;

        let recording = self.voice.is_recording();
        let processing = self.voice.is_processing();

        let placeholder = if recording {
            "Listening…"
        } else if processing {
            "Transcribing…"
        } else if compact {
            "Ask anything…"
        } else {
            "Ask the agent anything."
        };

        let send_label = if self.streaming { "…" } else { "Send" };

        let input = text_input(placeholder, &self.input)
            .on_input(Message::InputChanged)
            .on_submit(|_| Message::Submit)
            .padding(spacing.space_xs)
            .width(Length::Fill);

        // Mic toggle. Mirrors the React composer button:
        //   idle       → 🎙 (start)
        //   recording  → ⏺ (stop, with red accent via destructive variant)
        //   processing → ⌛ disabled
        let mic = {
            let label = if recording {
                "⏺ Stop"
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
            b
        };

        let send = {
            let mut b = button::suggested(send_label);
            if !self.streaming && !self.input.trim().is_empty() {
                b = b.on_press(Message::Submit);
            }
            b
        };

        row()
            .push(input)
            .push(mic)
            .push(send)
            .spacing(spacing.space_xs)
            .align_y(Alignment::Center)
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
                        // Encoding + the thread-join are blocking, so
                        // hop to a blocking pool to keep the UI loop
                        // responsive. The upload itself is async.
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
            // Mid-processing clicks are ignored — the UI button is
            // disabled during processing so this is defensive.
            VoiceState::Processing => {
                self.voice = VoiceState::Processing;
                Task::none()
            }
        }
    }
}

fn empty_state(compact: bool) -> Element<'static, Message> {
    let cosmic_theme = theme::active().cosmic().clone();
    let spacing = cosmic_theme.spacing;

    let title = if compact {
        text("Ready when you are.").size(14.0)
    } else {
        text("How can I help?").size(22.0)
    };
    let hint = if compact {
        text("Type below or paste anything.").size(11.0)
    } else {
        text("Press Super+A from anywhere to summon me.").size(13.0)
    };
    container(
        column()
            .push(title)
            .push(hint)
            .spacing(spacing.space_xxs)
            .align_x(Alignment::Center),
    )
    .center_x(Length::Fill)
    .center_y(Length::Fill)
    .into()
}

fn message_bubble(msg: &ChatMessage, compact: bool) -> Element<Message> {
    let cosmic_theme = theme::active().cosmic().clone();
    let spacing = cosmic_theme.spacing;

    let role_label = match msg.role {
        ChatRole::User => "You",
        ChatRole::Assistant => "Agent",
    };

    let body_size = if compact { 12.0 } else { 14.0 };

    let body: Element<Message> = if msg.content.is_empty() && msg.in_progress {
        text("…").size(body_size).into()
    } else {
        match msg.role {
            // The user side never carries markdown — render verbatim so
            // they see exactly what they typed.
            ChatRole::User => text(msg.content.clone()).size(body_size).into(),
            // The agent stream is markdown by convention (the kernel's
            // system prompt is markdown-friendly, and tool messages get
            // serialized as fenced code blocks). Parse and render it
            // through cosmic's catalog-themed markdown widget.
            ChatRole::Assistant => {
                let items: Vec<markdown::Item> = markdown::parse(&msg.content).collect();
                // Cosmic Theme isn't directly assignable to the iced
                // toolkit's markdown::Style (different `Theme` type), so
                // we pick a built-in palette by current mode. The
                // Catalog impl on cosmic::Theme handles surface colors
                // separately.
                let palette = if is_dark() {
                    cosmic::iced::theme::Palette::DARK
                } else {
                    cosmic::iced::theme::Palette::LIGHT
                };
                let settings = markdown::Settings::with_text_size(body_size, palette);
                markdown::view(&items, settings)
                    .map(|uri: markdown::Uri| Message::LinkClicked(uri))
            }
        }
    };

    container(
        column()
            .push(text(role_label).size(11.0))
            .push(body)
            .spacing(spacing.space_xxs),
    )
    .padding(spacing.space_xs)
    .class(match msg.role {
        ChatRole::User => theme::Container::Primary,
        ChatRole::Assistant => theme::Container::Card,
    })
    .width(Length::Fill)
    .into()
}

/// Best-effort URL opener used by the markdown link handler.
///
/// We deliberately don't pull in `webbrowser` or `xdg-open` Rust
/// bindings — both are thin wrappers that just spawn the system
/// handler. Spawning `xdg-open` directly is one less dependency to
/// audit and matches what every other COSMIC app does.
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
            "-h" | "--help" => {
                eprintln!(
                    "cos-agent-ui [--overlay] [--voice] [--query <text>]\n  --overlay         compact, Esc-to-close mode for global summon\n  --voice           auto-arm the microphone on launch\n  --query <text>    pre-fill the prompt and submit it immediately"
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
        settings = settings.size_limits(Limits::NONE.min_width(480.0).min_height(320.0));
    }

    cosmic::app::run::<App>(settings, flags)
}
