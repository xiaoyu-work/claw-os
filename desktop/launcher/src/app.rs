use crate::app::iced::event::listen_raw;
use crate::subscriptions::launcher;
use crate::{components, fl};
use clap::Parser;
use cosmic::app::{Core, CosmicFlags, Settings, Task};
use cosmic::cosmic_theme::palette::WithAlpha;
use cosmic::cctk::sctk;
use cosmic::cctk::sctk::shell::wlr_layer;
use cosmic::dbus_activation::Details;
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::core::text::{Ellipsize, EllipsizeHeightLimit};
use cosmic::iced::event::Status;
use cosmic::iced::event::wayland::OverlapNotifyEvent;
use cosmic::iced::id::Id;
use cosmic::iced::keyboard::key::Named;
use cosmic::iced::platform_specific::runtime::wayland::layer_surface::SctkLayerSurfaceSettings;
use cosmic::iced::platform_specific::runtime::wayland::popup::{SctkPopupSettings, SctkPositioner};
use cosmic::iced::platform_specific::shell::commands::activation::request_token;
use cosmic::iced::platform_specific::shell::commands::layer_surface::{
    Anchor, KeyboardInteractivity, destroy_layer_surface, get_layer_surface,
};
use cosmic::iced::platform_specific::shell::commands::{self};
use cosmic::iced::platform_specific::shell::wayland::commands::overlap_notify::overlap_notify;
use cosmic::iced::runtime::core::event::wayland::LayerEvent;
use cosmic::iced::runtime::core::event::{PlatformSpecific, wayland};
use cosmic::iced::runtime::core::layout::Limits;
use cosmic::iced::runtime::core::window::{Event as WindowEvent, Id as SurfaceId};
use cosmic::iced::runtime::platform_specific::wayland::layer_surface::IcedMargin;
use cosmic::iced::widget::scrollable::RelativeOffset;
use cosmic::iced::widget::{Column, column, container, operation, row};
use cosmic::iced::{
    self, Border, Length, Padding, Point, Rectangle, Shadow, Size, Subscription, window,
};
use cosmic::theme::{self, Button, Container};
use cosmic::widget::icon::IconFallback;
use cosmic::widget::space::{horizontal as horizontal_space, vertical as vertical_space};
use cosmic::widget::text_input;
use cosmic::widget::{autosize, button, divider, icon, id_container, mouse_area, scrollable, text};
use cosmic::{Element, keyboard_nav, surface};
use iced::keyboard::Key;
use iced::{Alignment, Color};
use pop_launcher::{ContextOption, GpuPreference, IconSource, SearchResult};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fmt::Display;
use std::path::Path;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, error, info};

static AUTOSIZE_ID: LazyLock<Id> = LazyLock::new(|| Id::new("autosize"));
static MAIN_ID: LazyLock<Id> = LazyLock::new(|| Id::new("main"));
static INPUT_ID: LazyLock<Id> = LazyLock::new(|| Id::new("input_id"));
static SCROLLABLE: LazyLock<Id> = LazyLock::new(|| Id::new("scrollable"));

pub(crate) static MENU_ID: LazyLock<SurfaceId> = LazyLock::new(SurfaceId::unique);
const SCROLL_MIN: usize = 8;

/// Frosted "Claw Glass" search pill for the launcher.
///
/// The pill floats over an arbitrary wallpaper like macOS Spotlight, so the
/// glass keeps a luminance anchor (dark-enough fill in dark mode, light-enough
/// in light mode) for readable contrast. Instead of a neutral gray/white frost
/// we blend a small amount of the brand accent (`#005CFE`) into the fill, the
/// foreground, and the hairline so the surface reads as brand-blue Claw Glass:
/// a deep navy-blue glass in dark mode and a cool blue-white glass in light
/// mode — never flat gray, never dead charcoal. Selection / focus is brand blue.
fn spotlight_pill_appearance(theme: &cosmic::Theme) -> cosmic::widget::text_input::Appearance {
    let cosmic = theme.cosmic();
    let is_dark = theme.theme_type.is_dark();
    let accent = cosmic.accent_color();

    // `base` = neutral luminance anchor (preserves contrast over any wallpaper),
    // `tint` = how much brand accent to blend into the frost for the blue cast.
    let (base, fill_alpha, border_alpha, tint) = if is_dark {
        (0.16_f32, 0.58_f32, 0.24_f32, 0.16_f32)
    } else {
        (0.98_f32, 0.64_f32, 0.32_f32, 0.07_f32)
    };
    // Blend a channel of the neutral anchor toward the matching accent channel.
    let mix = |chan: f32| base * (1.0 - tint) + chan * tint;
    let fill_color = Color::from_rgba(
        mix(accent.red),
        mix(accent.green),
        mix(accent.blue),
        fill_alpha,
    );

    // Foreground stays high-contrast but carries a faint blue cast so text and
    // icons belong to the same glass family rather than reading neutral.
    let fg = if is_dark { 1.0_f32 } else { 0.05_f32 };
    let fg_tint = if is_dark { 0.06_f32 } else { 0.10_f32 };
    let fg_color = Color::from_rgba(
        fg * (1.0 - fg_tint) + accent.red * fg_tint,
        fg * (1.0 - fg_tint) + accent.green * fg_tint,
        fg * (1.0 - fg_tint) + accent.blue * fg_tint,
        1.0,
    );
    let placeholder = Color { a: 0.55, ..fg_color };
    let icon_color = Color { a: 0.85, ..fg_color };

    cosmic::widget::text_input::Appearance {
        background: cosmic::iced::Background::Color(fill_color),
        border_radius: cosmic.corner_radii.radius_xl.into(),
        border_offset: None,
        border_width: 1.0,
        // Blue-tinted translucent hairline (brand accent), not white/gray.
        border_color: accent.with_alpha(border_alpha).into(),
        label_color: fg_color,
        placeholder_color: placeholder,
        selected_text_color: cosmic.on_accent_color().into(),
        icon_color: Some(icon_color),
        text_color: Some(fg_color),
        // Brand-blue text selection highlight.
        selected_fill: accent.into(),
    }
}

#[derive(Parser, Debug, Serialize, Deserialize, Clone)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
pub struct Args {
    #[clap(subcommand)]
    pub subcommand: Option<LauncherTasks>,
}

#[derive(Debug, Serialize, Deserialize, Clone, clap::Subcommand)]
pub enum LauncherTasks {
    #[clap(about = "Toggle the launcher and switch to the alt-tab view")]
    AltTab,
    #[clap(about = "Toggle the launcher and switch to the alt-tab view")]
    ShiftAltTab,
    #[clap(about = "Start the launcher with an input")]
    Input { input: Option<String> },
    #[clap(about = "Close the launcher if open")]
    Close,
}

impl Display for LauncherTasks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", serde_json::ser::to_string(self).unwrap())
    }
}

impl FromStr for LauncherTasks {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::de::from_str(s)
    }
}

impl CosmicFlags for Args {
    type SubCommand = LauncherTasks;
    type Args = Vec<String>;

    fn action(&self) -> Option<&LauncherTasks> {
        self.subcommand.as_ref()
    }
}

pub fn run() -> cosmic::iced::Result {
    let args = Args::parse();
    cosmic::app::run_single_instance::<CosmicLauncher>(
        Settings::default()
            .antialiasing(true)
            .client_decorations(true)
            .debug(false)
            .default_text_size(16.0)
            .scale_factor(1.0)
            .no_main_window(true)
            .exit_on_close(false),
        args,
    )
}

pub fn menu_button<'a, Message: Clone + 'a>(
    content: impl Into<Element<'a, Message>>,
) -> cosmic::widget::Button<'a, Message> {
    button::custom(content)
        .class(Button::AppletMenu)
        .padding(menu_control_padding())
        .width(Length::Fill)
}

pub fn menu_control_padding() -> Padding {
    let theme = cosmic::theme::active();
    let cosmic = theme.cosmic();
    [cosmic.space_xxs(), cosmic.space_m()].into()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SurfaceState {
    Visible,
    Hidden,
    WaitingToBeShown,
}

#[derive(Clone)]
pub struct CosmicLauncher {
    core: Core,
    input_value: String,
    surface_state: SurfaceState,
    launcher_items: Vec<SearchResult>,
    launcher_item_icon_handles: Vec<Option<cosmic::widget::icon::Handle>>,
    tx: Option<mpsc::Sender<launcher::Request>>,
    menu: Option<(u32, Vec<ContextOption>)>,
    cursor_position: Option<Point<f32>>,
    focused: usize,
    last_hide: Instant,
    alt_tab: bool,
    window_id: window::Id,
    queue: VecDeque<Message>,
    result_ids: Vec<Id>,
    overlap: HashMap<String, Rectangle>,
    margin: f32,
    height: f32,
    needs_clear: bool,
    hand_over: String,
    dummy_id: window::Id,
    ai_inline: AiInlineState,
    ai_token: u64,
}

/// State machine for the inline AI answer card shown when the user
/// prefixes their launcher query with `?` (or `？`).
///
/// Transitions:
/// `Idle` -> `Pending` on `InputChanged` with AI prefix (debounce starts).
/// `Pending` -> `Streaming` once the debounce fires and `cos agent ask`
/// is spawned.
/// `Streaming` -> `Done` when the child process exits with a final
/// answer on stdout, or `Error` on non-zero exit / spawn failure.
/// Any transition out of an in-flight state increments `ai_token` so
/// late deltas from a previous request are ignored.
#[derive(Debug, Clone)]
pub enum AiInlineState {
    Idle,
    Pending { token: u64, query: String },
    Streaming { token: u64, query: String, partial: String },
    Done { query: String, answer: String },
    Error { query: String, message: String },
}

#[derive(Debug, Clone)]
pub enum Message {
    InputChanged(String),
    Backspace,
    TabPress,
    CompleteFocusedId(Id),
    Activate(Option<usize>),
    AskAi,
    Context(usize),
    MenuButton(u32, u32),
    CloseContextMenu,
    CursorMoved(Point<f32>),
    Hide,
    LauncherEvent(launcher::Event),
    Layer(LayerEvent),
    KeyboardNav(keyboard_nav::Action),
    ActivationToken(Option<String>, String, String, GpuPreference, bool),
    AltTab,
    ShiftAltTab,
    Opened(Size, window::Id),
    AltRelease,
    Overlap(OverlapNotifyEvent),
    Surface(surface::Action),
    StartAiInline(u64, String),
    AiInlineDelta(u64, String),
    AiInlineDone(u64, String),
    AiInlineError(u64, String),
}

/// Extract the AI prompt from a launcher input value when the user has
/// prefixed it with `?` or fullwidth `？`. Returns `None` for non-AI
/// queries and for bare prefixes (`?` with no content).
fn ai_prompt(input: &str) -> Option<&str> {
    let trimmed = input.trim_start();
    let rest = trimmed
        .strip_prefix('?')
        .or_else(|| trimmed.strip_prefix('？'))?;
    let rest = rest.trim();
    if rest.is_empty() { None } else { Some(rest) }
}

impl CosmicLauncher {
    fn request(&self, r: launcher::Request) {
        debug!("request: {:?}", r);
        if let Some(tx) = &self.tx {
            if let Err(e) = tx.blocking_send(r) {
                error!("tx: {e}");
            }
        } else {
            info!("tx not found");
        }
    }

    fn show(&mut self) -> Task<Message> {
        self.surface_state = SurfaceState::Visible;
        self.needs_clear = true;

        Task::batch(vec![
            get_layer_surface(SctkLayerSurfaceSettings {
                id: self.window_id,
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                anchor: Anchor::TOP,
                namespace: "launcher".into(),
                size: None,
                size_limits: Limits::NONE.min_width(1.0).min_height(1.0).max_width(720.0),
                exclusive_zone: -1,
                ..Default::default()
            }),
            overlap_notify(self.window_id, true),
        ])
    }

    fn hide(&mut self) -> Task<Message> {
        self.input_value.clear();
        self.focused = 0;
        self.alt_tab = false;
        self.queue.clear();
        self.hand_over.clear();

        self.request(launcher::Request::Close);

        let mut tasks = Vec::new();

        if self.surface_state == SurfaceState::Visible {
            tasks.push(destroy_layer_surface(self.window_id));
            if self.menu.take().is_some() {
                tasks.push(commands::popup::destroy_popup(*MENU_ID));
            }
        }

        self.surface_state = SurfaceState::Hidden;

        Task::batch(tasks)
    }

    fn focus_next(&mut self) {
        if self.launcher_items.is_empty() {
            return;
        }
        self.focused = (self.focused + 1) % self.launcher_items.len();
    }

    fn focus_previous(&mut self) {
        if self.launcher_items.is_empty() {
            return;
        }
        self.focused = (self.focused + self.launcher_items.len() - 1) % self.launcher_items.len();
    }

    fn handle_overlap(&mut self) {
        if matches!(self.surface_state, SurfaceState::Hidden) {
            return;
        }
        let mid_height = self.height / 2.;
        self.margin = 0.;

        for o in self.overlap.values() {
            if self.margin + mid_height < o.y
                || self.margin > o.y + o.height
                || mid_height < o.y + o.height / 2.0
            {
                continue;
            }
            self.margin = o.y + o.height;
        }
    }

    /// Render the inline AI answer card for the current `ai_inline`
    /// state. Returns `None` when there is nothing to show (Idle).
    /// The card stays clickable in every non-Idle state: pressing it
    /// (or hitting Enter) opens the full overlay with the same query
    /// via `Message::AskAi`.
    fn ai_inline_card(&self) -> Option<Element<'_, Message>> {
        let (title, body, hint, is_error) = match &self.ai_inline {
            AiInlineState::Idle => return None,
            AiInlineState::Pending { query, .. } => (
                fl!("ai-card-title"),
                fl!("ai-card-thinking", query = query.clone()),
                fl!("ai-card-hint-thinking"),
                false,
            ),
            AiInlineState::Streaming { query, partial, .. } => {
                let body = if partial.trim().is_empty() {
                    fl!("ai-card-thinking", query = query.clone())
                } else {
                    partial.clone()
                };
                (
                    fl!("ai-card-title"),
                    body,
                    fl!("ai-card-hint-streaming"),
                    false,
                )
            }
            AiInlineState::Done { answer, .. } => (
                fl!("ai-card-title"),
                answer.clone(),
                fl!("ai-card-hint-open"),
                false,
            ),
            AiInlineState::Error { message, .. } => (
                fl!("ai-card-error-title"),
                message.clone(),
                fl!("ai-card-hint-open"),
                true,
            ),
        };

        let header = row![
            text::heading(title)
                .align_y(Vertical::Center)
                .class(if is_error {
                    theme::Text::Custom(|t| cosmic::iced::widget::text::Style {
                        color: Some(t.cosmic().destructive_color().into()),
                    })
                } else {
                    theme::Text::Custom(|t| cosmic::iced::widget::text::Style {
                        color: Some(t.cosmic().accent_color().into()),
                    })
                }),
            horizontal_space().width(Length::Fill),
            text::caption(hint)
                .align_y(Vertical::Center)
                .class(theme::Text::Custom(|t| cosmic::iced::widget::text::Style {
                    color: Some(t.cosmic().on_bg_color().into()),
                })),
        ]
        .align_y(Alignment::Center)
        .spacing(8);

        let body_text = text::body(body)
            .align_x(Horizontal::Left)
            .align_y(Vertical::Top)
            .class(theme::Text::Custom(|t| cosmic::iced::widget::text::Style {
                color: Some(t.cosmic().on_bg_color().into()),
            }));

        // Cap height + scroll long answers so the card doesn't push
        // the rest of the launcher offscreen.
        let body_area = container(scrollable(body_text).height(Length::Shrink))
            .max_height(220.0)
            .width(Length::Fill);

        let card = column![header, body_area]
            .spacing(6)
            .width(Length::Fixed(660.0));

        let clickable = cosmic::widget::button::custom(card)
            .width(Length::Fill)
            .on_press(Message::AskAi)
            .padding([8, 24])
            .class(Button::Text);

        Some(clickable.into())
    }
}

async fn launch(
    token: Option<String>,
    app_id: String,
    exec: String,
    gpu: GpuPreference,
    terminal: bool,
) {
    let mut envs = Vec::new();
    if let Some(token) = token {
        envs.push(("XDG_ACTIVATION_TOKEN".to_string(), token.clone()));
        envs.push(("DESKTOP_STARTUP_ID".to_string(), token));
    }

    if let Some(gpu_envs) = try_get_gpu_envs(gpu).await {
        envs.extend(gpu_envs);
    }

    cosmic::desktop::spawn_desktop_exec(exec, envs, Some(&app_id), terminal).await;
}

async fn try_get_gpu_envs(gpu: GpuPreference) -> Option<HashMap<String, String>> {
    let connection = zbus::Connection::system().await.ok()?;
    let proxy = switcheroo_control::SwitcherooControlProxy::new(&connection)
        .await
        .ok()?;
    let gpus = proxy.get_gpus().await.ok()?;
    match gpu {
        GpuPreference::Default => gpus.into_iter().find(|gpu| gpu.default),
        GpuPreference::NonDefault => gpus.into_iter().find(|gpu| !gpu.default),
        GpuPreference::SpecificIdx(idx) => gpus.into_iter().nth(idx as usize),
    }
    .map(|gpu| gpu.environment)
}

impl cosmic::Application for CosmicLauncher {
    type Message = Message;
    type Executor = cosmic::executor::single::Executor;
    type Flags = Args;
    const APP_ID: &'static str = "com.clawos.Launcher";

    fn init(mut core: Core, _flags: Args) -> (Self, Task<Message>) {
        let dummy_id = window::Id::unique();

        core.set_keyboard_nav(false);
        (
            CosmicLauncher {
                core,
                input_value: String::new(),
                surface_state: SurfaceState::Hidden,
                launcher_items: Vec::new(),
                launcher_item_icon_handles: Vec::new(),
                tx: None,
                menu: None,
                cursor_position: None,
                focused: 0,
                last_hide: Instant::now(),
                alt_tab: false,
                window_id: SurfaceId::unique(),
                queue: VecDeque::new(),
                result_ids: (0..10)
                    .map(|id| Id::new(id.to_string()))
                    .collect::<Vec<_>>(),
                margin: 0.,
                overlap: HashMap::new(),
                height: 100.,
                needs_clear: false,
                hand_over: String::default(),
                                dummy_id,
                ai_inline: AiInlineState::Idle,
                ai_token: 0,

            },
            get_layer_surface(SctkLayerSurfaceSettings {
                id: dummy_id,
                layer: wlr_layer::Layer::Bottom,
                keyboard_interactivity: wlr_layer::KeyboardInteractivity::None,
                input_zone: Some(Vec::new()),
                anchor: wlr_layer::Anchor::empty(),
                output: cosmic::iced::runtime::platform_specific::wayland::layer_surface::IcedOutput::Active,
                namespace: "cosmic_launcher_dummy".into(),
                margin: IcedMargin::default(),
                size: Some((Some(6), Some(6))),
                exclusive_zone: -1,
                size_limits: Limits::NONE,
            }),
        )
    }

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    #[allow(clippy::too_many_lines)]
    fn update(&mut self, message: Message) -> Task<Self::Message> {
        match message {
            Message::InputChanged(value) => {
                self.input_value.clone_from(&value);
                if let Some(prompt) = ai_prompt(&value) {
                    // AI mode: blow away regular launcher results so the
                    // result list doesn't compete with the inline answer
                    // card. Also push an empty Search request so pop-launcher
                    // doesn't keep dripping stale results in.
                    self.launcher_items.clear();
                    self.launcher_item_icon_handles.clear();
                    self.focused = 0;
                    self.request(launcher::Request::Search(String::new()));

                    // Each keystroke invalidates any in-flight stream by
                    // bumping the token; the debounced StartAiInline below
                    // only fires the latest token.
                    self.ai_token = self.ai_token.wrapping_add(1);
                    let token = self.ai_token;
                    let prompt = prompt.to_string();
                    self.ai_inline = AiInlineState::Pending {
                        token,
                        query: prompt.clone(),
                    };
                    return cosmic::Task::perform(
                        async move {
                            tokio::time::sleep(Duration::from_millis(350)).await;
                            (token, prompt)
                        },
                        |(t, q)| cosmic::action::app(Message::StartAiInline(t, q)),
                    );
                } else {
                    if !matches!(self.ai_inline, AiInlineState::Idle) {
                        self.ai_inline = AiInlineState::Idle;
                        self.ai_token = self.ai_token.wrapping_add(1);
                    }
                    self.request(launcher::Request::Search(value));
                }
            }
            Message::Backspace => {
                self.input_value.pop();
                self.request(launcher::Request::Search(self.input_value.clone()));
            }
            Message::TabPress if !self.alt_tab => {
                let focused = self.focused;
                self.focused = 0;
                return cosmic::task::message(cosmic::Action::App(
                    Self::Message::CompleteFocusedId(self.result_ids[focused].clone()),
                ));
            }
            Message::TabPress => {}
            Message::CompleteFocusedId(id) => {
                let i = self
                    .result_ids
                    .iter()
                    .position(|res_id| res_id == &id)
                    .unwrap_or_default();

                if let Some(id) = self.launcher_items.get(i).map(|res| res.id) {
                    self.request(launcher::Request::Complete(id));
                }
            }
            Message::Activate(i) => {
                if let Some(item) = self.launcher_items.get(i.unwrap_or(self.focused)) {
                    self.request(launcher::Request::Activate(item.id));
                } else {
                    return self.hide();
                }
            }
            Message::AskAi => {
                // Strip `?` / `？` prefix so the overlay starts the
                // conversation with the actual question, not the
                // launcher-mode trigger character.
                let raw = self.input_value.trim();
                let query = raw
                    .strip_prefix('?')
                    .or_else(|| raw.strip_prefix('？'))
                    .unwrap_or(raw)
                    .trim()
                    .to_string();
                if !query.is_empty() {
                    let mut cmd = std::process::Command::new("cos");
                    cmd.args(["app", "agent", "overlay", "--query", &query]);
                    if let Err(err) = cmd.spawn() {
                        error!("failed to launch cos app agent overlay: {err}");
                    }
                }
                return self.hide();
            }
            Message::Context(i) => {
                if self.menu.take().is_some() {
                    return commands::popup::destroy_popup(*MENU_ID);
                }

                if let Some(item) = self.launcher_items.get(i) {
                    self.request(launcher::Request::Context(item.id));
                }
            }
            Message::CursorMoved(pos) => {
                self.cursor_position = Some(pos);
            }
            Message::MenuButton(i, context) => {
                self.request(launcher::Request::ActivateContext(i, context));

                if self.menu.take().is_some() {
                    return commands::popup::destroy_popup(*MENU_ID);
                }
            }
            Message::Opened(size, window_id) => {
                if window_id == self.window_id {
                    self.height = size.height;
                    self.handle_overlap();
                }
                if !self.hand_over.is_empty() {
                    let input = self.hand_over.clone();
                    self.hand_over.clear();
                    return self.update(Message::InputChanged(input));
                }
            }
            Message::LauncherEvent(e) => match e {
                launcher::Event::Started(tx) => {
                    self.tx.replace(tx);
                    self.request(launcher::Request::Search(self.input_value.clone()));
                }
                launcher::Event::ServiceIsClosed => {
                    self.request(launcher::Request::ServiceIsClosed);
                }
                launcher::Event::Response(response) => match response {
                    pop_launcher::Response::Close => {
                        return self.hide();
                    }
                    #[allow(clippy::cast_possible_truncation)]
                    pop_launcher::Response::Context { id, options } => {
                        if options.is_empty() {
                            return Task::none();
                        }

                        self.menu = Some((id, options));
                        let Some(pos) = self.cursor_position.as_ref() else {
                            return Task::none();
                        };
                        let rect = Rectangle {
                            x: pos.x.round() as i32,
                            y: pos.y.round() as i32,
                            width: 1,
                            height: 1,
                        };
                        return commands::popup::get_popup(SctkPopupSettings {
                                    parent: self.window_id,
                                    id: *MENU_ID,
                                    positioner: SctkPositioner {
                                        size: None,
                                        size_limits: Limits::NONE.min_width(1.0).min_height(1.0).max_width(300.0).max_height(800.0),
                                        anchor_rect: rect,
                                        anchor:
                                            sctk::reexports::protocols::xdg::shell::client::xdg_positioner::Anchor::Right,
                                        gravity: sctk::reexports::protocols::xdg::shell::client::xdg_positioner::Gravity::Right,
                                        reactive: true,
                                        ..Default::default()
                                    },
                                    grab: true,
                                    parent_size: None,
                                    close_with_children: false,
                                    input_zone: None,
                                });
                    }
                    pop_launcher::Response::DesktopEntry {
                        path,
                        gpu_preference,
                        action_name,
                    } => {
                        if let Some(entry) = cosmic::desktop::load_desktop_file(&[], path) {
                            let exec = if let Some(action_name) = action_name {
                                entry
                                    .desktop_actions
                                    .into_iter()
                                    .find(|action| action.name == action_name)
                                    .map(|action| action.exec)
                            } else {
                                entry.exec
                            };

                            let Some(exec) = exec else {
                                return Task::none();
                            };
                            return request_token(
                                Some(String::from(Self::APP_ID)),
                                Some(self.window_id),
                            )
                            .map(move |token| {
                                cosmic::Action::App(Message::ActivationToken(
                                    token,
                                    entry.id.to_string(),
                                    exec.clone(),
                                    gpu_preference,
                                    entry.terminal,
                                ))
                            });
                        }
                    }
                    pop_launcher::Response::Update(mut list) => {
                        if self.alt_tab && list.is_empty() {
                            return self.hide();
                        }
                        if self.alt_tab || self.input_value.is_empty() {
                            list.reverse();
                        }
                        list.sort_by(|a, b| {
                            let a = i32::from(a.window.is_none());
                            let b = i32::from(b.window.is_none());
                            a.cmp(&b)
                        });
                        self.launcher_items.splice(.., list);
                        if self.result_ids.len() < self.launcher_items.len() {
                            self.result_ids.extend(
                                (self.result_ids.len()..self.launcher_items.len())
                                    .map(|id| Id::new((id).to_string()))
                                    .collect::<Vec<_>>(),
                            );
                        }

                        self.launcher_item_icon_handles.clear();
                        self.launcher_item_icon_handles = self
                            .launcher_items
                            .iter()
                            .map(|item| {
                                item.icon.as_ref().map(|icon_source| match icon_source {
                                    // Check if the name is actually a path
                                    IconSource::Name(name) if name.contains('/') => {
                                        let path = Path::new(&**name);
                                        if path.exists() {
                                            icon::from_path(path.into())
                                        } else {
                                            icon::from_name("application-default")
                                                .prefer_svg(true)
                                                .size(64)
                                                .fallback(Some(IconFallback::Names(vec![
                                                    "application-x-executable".into(),
                                                ])))
                                                .handle()
                                        }
                                    }
                                    // Fetch icon by name
                                    IconSource::Name(name) => icon::from_name(&**name)
                                        .prefer_svg(true)
                                        .size(64)
                                        .fallback(Some(IconFallback::Names(vec![
                                            "application-default".into(),
                                            "application-x-executable".into(),
                                        ])))
                                        .handle(),
                                    // By mime
                                    IconSource::Mime(mime) => {
                                        icon::from_name(mime.as_ref().replace('/', "-"))
                                            .prefer_svg(true)
                                            .size(64)
                                            .fallback(Some(IconFallback::Names(vec![
                                                "application-default".into(),
                                                "application-x-executable".into(),
                                            ])))
                                            .handle()
                                    }
                                })
                            })
                            .collect();

                        let mut cmds = Vec::new();

                        while let Some(element) = self.queue.pop_front() {
                            let updated = self.update(element);
                            cmds.push(updated);
                        }

                        if self.surface_state == SurfaceState::WaitingToBeShown {
                            cmds.push(self.show());
                        }
                        return Task::batch(cmds);
                    }
                    pop_launcher::Response::Fill(s) => {
                        self.input_value = s;
                        self.request(launcher::Request::Search(self.input_value.clone()));
                    }
                },
            },
            Message::Layer(e) => match e {
                LayerEvent::Focused | LayerEvent::Done => {}
                LayerEvent::Unfocused => {
                    self.last_hide = Instant::now();
                    return self.hide();
                }
            },
            Message::Overlap(overlap_notify_event) => match overlap_notify_event {
                OverlapNotifyEvent::OverlapLayerAdd {
                    identifier,
                    namespace,
                    logical_rect,
                    exclusive,
                    ..
                } => {
                    if self.needs_clear {
                        self.needs_clear = false;
                        self.overlap.clear();
                    }
                    if exclusive > 0 || namespace == "Dock" || namespace == "Panel" {
                        self.overlap.insert(identifier, logical_rect);
                    }
                    self.handle_overlap();
                }
                OverlapNotifyEvent::OverlapLayerRemove { identifier } => {
                    self.overlap.remove(&identifier);
                    self.handle_overlap();
                }
                _ => {}
            },
            Message::CloseContextMenu => {
                if self.menu.take().is_some() {
                    return commands::popup::destroy_popup(*MENU_ID);
                }
            }
            Message::Hide => {
                if self.menu.take().is_some() {
                    return commands::popup::destroy_popup(*MENU_ID);
                }
                return self.hide();
            }
            Message::KeyboardNav(e) => {
                match e {
                    keyboard_nav::Action::FocusNext => {
                        self.focus_next();
                        // TODO ideally we could use an operation to scroll exactly to a specific widget.
                        return operation::snap_to(
                            SCROLLABLE.clone(),
                            RelativeOffset {
                                x: None,
                                y: Some(
                                    (self.focused as f32
                                        / (self.launcher_items.len() as f32 - 1.).max(1.))
                                    .max(0.0),
                                ),
                            },
                        );
                    }
                    keyboard_nav::Action::FocusPrevious => {
                        self.focus_previous();
                        return operation::snap_to(
                            SCROLLABLE.clone(),
                            RelativeOffset {
                                x: None,
                                y: Some(
                                    (self.focused as f32
                                        / (self.launcher_items.len() as f32 - 1.).max(1.))
                                    .max(0.0),
                                ),
                            },
                        );
                    }
                    keyboard_nav::Action::Escape => {
                        self.input_value.clear();
                        self.request(launcher::Request::Search(String::new()));
                    }
                    _ => {}
                };
            }
            Message::ActivationToken(token, app_id, exec, dgpu, terminal) => {
                return Task::perform(launch(token, app_id, exec, dgpu, terminal), |()| {
                    cosmic::action::app(Message::Hide)
                });
            }
            Message::AltTab => {
                self.focus_next();
                return operation::snap_to(
                    SCROLLABLE.clone(),
                    RelativeOffset {
                        x: None,
                        y: Some(
                            (self.focused as f32 / (self.launcher_items.len() as f32 - 1.).max(1.))
                                .max(0.0),
                        ),
                    },
                );
            }
            Message::ShiftAltTab => {
                self.focus_previous();
                return operation::snap_to(
                    SCROLLABLE.clone(),
                    RelativeOffset {
                        x: None,
                        y: Some(
                            (self.focused as f32 / (self.launcher_items.len() as f32 - 1.).max(1.))
                                .max(0.0),
                        ),
                    },
                );
            }
            Message::AltRelease => {
                if self.alt_tab {
                    return self.update(Message::Activate(None));
                }
            }
            Message::Surface(a) => {
                return cosmic::task::message(cosmic::Action::Cosmic(
                    cosmic::app::Action::Surface(a),
                ));
            }
            Message::StartAiInline(token, prompt) => {
                if token != self.ai_token {
                    // A newer keystroke obsoleted this debounce.
                    return Task::none();
                }
                // Only proceed if we're still in Pending for this token
                // (defensive: another transition could have happened).
                if !matches!(&self.ai_inline, AiInlineState::Pending { token: t, .. } if *t == token)
                {
                    return Task::none();
                }
                self.ai_inline = AiInlineState::Streaming {
                    token,
                    query: prompt.clone(),
                    partial: String::new(),
                };
                return cosmic::Task::stream(cosmic::iced::stream::channel(
                    16,
                    move |mut tx: cosmic::iced::futures::channel::mpsc::Sender<Message>| {
                        let prompt = prompt.clone();
                        async move {
                            use cosmic::iced::futures::SinkExt;
                            use std::process::Stdio;
                            use tokio::io::AsyncReadExt;
                            use tokio::process::Command;
                            let mut child = match Command::new("cos")
                                .args(["agent", "ask", "--stream", &prompt])
                                .stdin(Stdio::null())
                                .stdout(Stdio::piped())
                                .stderr(Stdio::piped())
                                .kill_on_drop(true)
                                .spawn()
                            {
                                Ok(c) => c,
                                Err(e) => {
                                    let _ = tx
                                        .send(Message::AiInlineError(
                                            token,
                                            format!("failed to spawn cos: {e}"),
                                        ))
                                        .await;
                                    return;
                                }
                            };
                            let mut stdout = match child.stdout.take() {
                                Some(s) => s,
                                None => return,
                            };
                            let mut stderr = match child.stderr.take() {
                                Some(s) => s,
                                None => return,
                            };
                            // Forward stderr bytes as deltas. Hold a queue of
                            // pending UTF-8 bytes so we don't split a
                            // codepoint between chunks.
                            let mut tx_stderr = tx.clone();
                            let stderr_task = tokio::spawn(async move {
                                let mut buf = [0u8; 1024];
                                let mut leftover: Vec<u8> = Vec::new();
                                loop {
                                    match stderr.read(&mut buf).await {
                                        Ok(0) => break,
                                        Ok(n) => {
                                            leftover.extend_from_slice(&buf[..n]);
                                            let valid_end = match std::str::from_utf8(&leftover) {
                                                Ok(s) => s.len(),
                                                Err(e) => e.valid_up_to(),
                                            };
                                            if valid_end == 0 {
                                                continue;
                                            }
                                            let bytes: Vec<u8> =
                                                leftover.drain(..valid_end).collect();
                                            if let Ok(chunk) = String::from_utf8(bytes)
                                                && tx_stderr
                                                    .send(Message::AiInlineDelta(token, chunk))
                                                    .await
                                                    .is_err()
                                            {
                                                return;
                                            }
                                        }
                                        Err(_) => break,
                                    }
                                }
                            });
                            let stdout_task = tokio::spawn(async move {
                                let mut buf = Vec::new();
                                let _ = stdout.read_to_end(&mut buf).await;
                                String::from_utf8_lossy(&buf).trim().to_string()
                            });
                            let _ = stderr_task.await;
                            let answer = stdout_task.await.unwrap_or_default();
                            let status = child.wait().await;
                            match status {
                                Ok(s) if s.success() => {
                                    let _ = tx
                                        .send(Message::AiInlineDone(token, answer))
                                        .await;
                                }
                                _ => {
                                    let _ = tx
                                        .send(Message::AiInlineError(
                                            token,
                                            "agent returned non-zero".into(),
                                        ))
                                        .await;
                                }
                            }
                        }
                    },
                ))
                .map(cosmic::Action::App);
            }
            Message::AiInlineDelta(token, text) => {
                if token != self.ai_token {
                    return Task::none();
                }
                if let AiInlineState::Streaming { partial, .. } = &mut self.ai_inline {
                    partial.push_str(&text);
                }
            }
            Message::AiInlineDone(token, answer) => {
                if token != self.ai_token {
                    return Task::none();
                }
                let query = match &self.ai_inline {
                    AiInlineState::Streaming { query, .. }
                    | AiInlineState::Pending { query, .. } => query.clone(),
                    _ => String::new(),
                };
                self.ai_inline = AiInlineState::Done { query, answer };
            }
            Message::AiInlineError(token, message) => {
                if token != self.ai_token {
                    return Task::none();
                }
                let query = match &self.ai_inline {
                    AiInlineState::Streaming { query, .. }
                    | AiInlineState::Pending { query, .. } => query.clone(),
                    _ => String::new(),
                };
                self.ai_inline = AiInlineState::Error { query, message };
            }
        }
        Task::none()
    }

    fn dbus_activation(
        &mut self,
        msg: cosmic::dbus_activation::Message,
    ) -> iced::Task<cosmic::Action<Self::Message>> {
        match msg.msg {
            Details::Activate => {
                if self.surface_state != SurfaceState::Hidden {
                    return self.hide();
                }
                // hack: allow to close the launcher from the panel button
                if self.last_hide.elapsed().as_millis() > 100 {
                    self.request(launcher::Request::Search(String::new()));

                    self.surface_state = SurfaceState::WaitingToBeShown;
                    return Task::none();
                }
            }
            Details::ActivateAction { action, .. } => {
                debug!("ActivateAction {}", action);

                let Ok(cmd) = LauncherTasks::from_str(&action) else {
                    return Task::none();
                };

                if self.surface_state == SurfaceState::Hidden {
                    self.surface_state = SurfaceState::WaitingToBeShown;
                }

                match cmd {
                    LauncherTasks::AltTab => {
                        if self.alt_tab {
                            return self.update(Message::AltTab);
                        }

                        self.alt_tab = true;
                        self.request(launcher::Request::Search(String::new()));
                        self.queue.push_back(Message::AltTab);
                    }
                    LauncherTasks::ShiftAltTab => {
                        if self.alt_tab {
                            return self.update(Message::ShiftAltTab);
                        }

                        self.alt_tab = true;
                        self.request(launcher::Request::Search(String::new()));
                        self.queue.push_back(Message::ShiftAltTab);
                    }
                    LauncherTasks::Input { input } => {
                        self.request(launcher::Request::Search(String::new()));
                        if let Some(input) = input {
                            self.hand_over.push_str(&input);
                        };
                    }
                    LauncherTasks::Close => {
                        return self.update(Message::Hide);
                    }
                }
            }
            Details::Open { .. } => {}
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Self::Message> {
        unreachable!("No main window")
    }

    #[allow(clippy::too_many_lines)]
    fn view_window(&self, id: SurfaceId) -> Element<'_, Self::Message> {
        if id == self.window_id {
            let launcher_entry = text_input::search_input(fl!("type-to-search"), &self.input_value)
                .on_input(Message::InputChanged)
                .on_paste(Message::InputChanged)
                .on_submit(|val| {
                    let trimmed = val.trim_start();
                    if trimmed.starts_with('?') || trimmed.starts_with('？') {
                        Message::AskAi
                    } else {
                        Message::Activate(None)
                    }
                })
                .on_tab(Message::TabPress)
                .style(cosmic::theme::TextInput::Custom {
                    active: Box::new(spotlight_pill_appearance),
                    error: Box::new(spotlight_pill_appearance),
                    hovered: Box::new(spotlight_pill_appearance),
                    focused: Box::new(spotlight_pill_appearance),
                    disabled: Box::new(spotlight_pill_appearance),
                })
                .width(Length::Fixed(660.))
                .id(INPUT_ID.clone())
                .always_active();

            let buttons: Vec<_> = self
                .launcher_items
                .iter()
                .enumerate()
                .flat_map(|(i, item)| {
                    let (name, desc) = if item.window.is_some() {
                        (&item.description, &item.name)
                    } else {
                        (&item.name, &item.description)
                    };

                    let name = Column::with_children(name.lines().map(|line| {
                        text::body(line.to_string())
                            .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)))
                            .align_x(Horizontal::Left)
                            .align_y(Vertical::Center)
                            .class(cosmic::theme::Text::Custom(|t| {
                                cosmic::iced::widget::text::Style {
                                    color: Some(t.cosmic().on_bg_color().into()),
                                }
                            }))
                            .into()
                    }));

                    let desc = Column::with_children(desc.lines().map(|line| {
                        text::caption(line.to_string())
                            .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)))
                            .align_x(Horizontal::Left)
                            .align_y(Vertical::Center)
                            .class(theme::Text::Custom(|t| cosmic::iced::widget::text::Style {
                                color: Some(t.cosmic().on_bg_color().into()),
                            }))
                            .into()
                    }));

                    let mut button_content = Vec::new();
                    if !self.alt_tab
                        && let Some(source) = item.category_icon.as_ref()
                    {
                        let icon_handle = match source {
                            IconSource::Name(name) => {
                                if Path::new(name.as_ref()).exists() {
                                    icon::from_path(Path::new(name.as_ref()).into())
                                } else {
                                    icon::from_name(name.as_ref()).handle()
                                }
                            }

                            IconSource::Mime(mime) => {
                                icon::from_name(mime.as_ref().replace('/', "-")).handle()
                            }
                        };

                        button_content.push(
                            icon(icon_handle)
                                .width(Length::Fixed(16.0))
                                .height(Length::Fixed(16.0))
                                .class(cosmic::theme::Svg::Custom(Rc::new(|theme| {
                                    cosmic::iced::widget::svg::Style {
                                        color: Some(theme.cosmic().on_bg_color().into()),
                                    }
                                })))
                                .into(),
                        );
                    }
                    if let Some(Some(icon_handle)) = self.launcher_item_icon_handles.get(i) {
                        button_content.push(
                            icon(icon_handle.clone())
                                .width(Length::Fixed(32.0))
                                .height(Length::Fixed(32.0))
                                .into(),
                        );
                    }

                    button_content.push(column![name, desc].width(Length::FillPortion(5)).into());
                    if i < 10 {
                        button_content.push(
                            container(
                                text::body(format!("Ctrl + {}", (i + 1) % 10))
                                    .align_y(Vertical::Center)
                                    .align_x(Horizontal::Right)
                                    .class(theme::Text::Custom(|t| {
                                        cosmic::iced::widget::text::Style {
                                            color: Some(t.cosmic().on_bg_color().into()),
                                        }
                                    })),
                            )
                            .width(Length::FillPortion(1))
                            .center_y(Length::Shrink)
                            .align_y(Vertical::Center)
                            .align_x(Horizontal::Right)
                            .into(),
                        );
                    }
                    let is_focused = i == self.focused;
                    let btn = mouse_area(
                        cosmic::widget::button::custom(
                            row(button_content).spacing(8).align_y(Alignment::Center),
                        )
                        .id(self.result_ids[i].clone())
                        .width(Length::Fill)
                        .on_press(Message::Activate(Some(i)))
                        .padding([8, 24])
                        .class(Button::Custom {
                            active: Box::new(move |focused, theme| {
                                let focused = is_focused || focused;
                                let cosmic = theme.cosmic();
                                let rad_s = cosmic.corner_radii.radius_s;
                                let mut a = if focused {
                                    button::Catalog::hovered(theme, focused, focused, &Button::Text)
                                } else {
                                    button::Catalog::active(theme, focused, focused, &Button::Text)
                                };
                                if focused {
                                    // Brand-blue translucent selection highlight
                                    // (Claw Glass selection is accent, never gray).
                                    a.background = Some(cosmic::iced::Background::Color(
                                        cosmic.accent_color().with_alpha(0.20).into(),
                                    ));
                                }
                                button::Style {
                                    border_radius: rad_s.into(),
                                    outline_width: 0.0,
                                    ..a
                                }
                            }),
                            hovered: Box::new(move |focused, theme| {
                                let focused = is_focused || focused;
                                let cosmic = theme.cosmic();
                                let rad_s = cosmic.corner_radii.radius_s;

                                let mut text = button::Catalog::hovered(
                                    theme,
                                    focused,
                                    focused,
                                    &Button::Text,
                                );
                                // Brand-blue translucent hover/selection fill —
                                // slightly stronger when this row is the active
                                // selection, fainter on plain hover.
                                let alpha = if focused { 0.24 } else { 0.12 };
                                text.background = Some(cosmic::iced::Background::Color(
                                    cosmic.accent_color().with_alpha(alpha).into(),
                                ));
                                button::Style {
                                    border_radius: rad_s.into(),
                                    outline_width: 0.0,
                                    ..text
                                }
                            }),
                            disabled: Box::new(|theme| {
                                let rad_s = theme.cosmic().corner_radii.radius_s;

                                let text = button::Catalog::disabled(theme, &Button::Text);
                                button::Style {
                                    border_radius: rad_s.into(),
                                    outline_width: 0.0,
                                    ..text
                                }
                            }),
                            pressed: Box::new(move |focused, theme| {
                                let focused = is_focused || focused;
                                let rad_s = theme.cosmic().corner_radii.radius_s;

                                let text = button::Catalog::pressed(
                                    theme,
                                    focused,
                                    focused,
                                    &Button::Text,
                                );
                                button::Style {
                                    border_radius: rad_s.into(),
                                    outline_width: 0.0,
                                    ..text
                                }
                            }),
                        }),
                    )
                    .on_right_release(Message::Context(i));
                    if i == self.launcher_items.len() - 1 {
                        vec![btn.into()]
                    } else {
                        vec![btn.into(), divider::horizontal::light().into()]
                    }
                })
                .collect();

            let mut content = if self.alt_tab {
                Column::new()
                    .max_width(660)
                    .spacing(16)
                    .width(Length::Fixed(660.))
                    .height(Length::Shrink)
            } else {
                column![launcher_entry]
                    .max_width(660)
                    .width(Length::Shrink)
                    .height(Length::Shrink)
                    .spacing(16)
            };

            if buttons.len() > SCROLL_MIN {
                content = content.push(
                    container(scrollable(components::list::column(buttons)).id(SCROLLABLE.clone()))
                        .max_height(504),
                );
            } else if !buttons.is_empty() {
                content = content.push(components::list::column(buttons));
            }

            // Inline AI answer card. Rendered above the regular "Ask Claw AI"
            // footer when the user has prefixed their query with `?` (or
            // fullwidth `？`). Tokens stream into `partial`; pressing Enter
            // continues the conversation in the full overlay via
            // Message::AskAi.
            if !self.alt_tab {
                if let Some(card) = self.ai_inline_card() {
                    content = content.push(divider::horizontal::light());
                    content = content.push(card);
                }
            }

            // "Ask Claw AI" footer — offered when the user has typed
            // something AND is not already in inline-AI mode (the card
            // above takes over for `?`-prefixed queries). Routes the
            // raw query into the agent overlay via:
            //   cos app agent overlay --query <text>
            if !self.alt_tab
                && !self.input_value.trim().is_empty()
                && matches!(self.ai_inline, AiInlineState::Idle)
            {
                let q = self.input_value.trim().to_string();
                let ai_row = row![
                    container(
                        text::body(fl!("ask-claw-ai", query = q.clone()))
                            .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)))
                            .align_x(Horizontal::Left)
                            .align_y(Vertical::Center),
                    )
                    .width(Length::FillPortion(5)),
                    container(
                        text::caption(fl!("ask-claw-ai-hint"))
                            .align_x(Horizontal::Right)
                            .align_y(Vertical::Center),
                    )
                    .width(Length::FillPortion(2)),
                ]
                .spacing(8)
                .align_y(Alignment::Center);
                let ai_button = cosmic::widget::button::custom(ai_row)
                    .width(Length::Fill)
                    .on_press(Message::AskAi)
                    .padding([8, 24])
                    .class(Button::Suggested);
                content = content.push(divider::horizontal::light());
                content = content.push(ai_button);
            }

            let window = Column::new()
                .push(vertical_space().height(Length::Fixed(self.margin + 16.)))
                .push(
                    // Use the built-in `Container::Transparent` variant
                    // (the default of `theme::Container`) so the launcher
                    // surface has no background / border / padding of its
                    // own. The `search_input` widget renders the macOS
                    // Spotlight-style translucent pill via
                    // `spotlight_pill_appearance`; result rows below it
                    // carry their own visuals. Together this floats on
                    // the wallpaper like macOS Spotlight.
                    container(id_container(content, MAIN_ID.clone()))
                        .width(Length::Shrink)
                        .height(Length::Shrink)
                        .class(Container::Transparent),
                );

            let autosize = autosize::autosize(
                if self.menu.is_some() {
                    Element::from(
                        mouse_area(window)
                            .on_release(Message::CloseContextMenu)
                            .on_right_release(Message::CloseContextMenu),
                    )
                } else {
                    window.into()
                },
                AUTOSIZE_ID.clone(),
            );
            return Element::from(autosize);
        }
        if id == *MENU_ID {
            let Some((i, options)) = self.menu.as_ref() else {
                return container(horizontal_space().width(Length::Fixed(1.0)))
                    .width(Length::Fixed(1.0))
                    .height(Length::Fixed(1.0))
                    .into();
            };
            let list_column = Column::with_children(options.iter().map(|option| {
                menu_button(text::body(&option.name))
                    .on_press(Message::MenuButton(*i, option.id))
                    .into()
            }))
            .padding([8, 0]);

            return container(
                container(scrollable(list_column)).class(theme::Container::custom(|theme| {
                    let cosmic = theme.cosmic();
                    let corners = cosmic.corner_radii;
                    container::Style {
                        text_color: Some(cosmic.background.on.into()),
                        background: Some(Color::from(cosmic.background.base).into()),
                        border: Border {
                            radius: corners.radius_m.into(),
                            width: 1.0,
                            color: cosmic.background.divider.into(),
                        },
                        shadow: Shadow::default(),
                        icon_color: Some(cosmic.background.on.into()),
                        snap: true,
                    }
                })),
            )
            .width(Length::Shrink)
            .height(Length::Shrink)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Top)
            .into();
        }

        vertical_space().height(Length::Fixed(1.0)).into()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        Subscription::batch(vec![
            launcher::subscription(0).map(Message::LauncherEvent),
            listen_raw(|e, status, id| match e {
                cosmic::iced::Event::PlatformSpecific(PlatformSpecific::Wayland(
                    wayland::Event::Layer(e, ..),
                )) => Some(Message::Layer(e)),
                cosmic::iced::Event::PlatformSpecific(PlatformSpecific::Wayland(
                    wayland::Event::OverlapNotify(event, ..),
                )) => Some(Message::Overlap(event)),
                cosmic::iced::Event::Keyboard(iced::keyboard::Event::KeyReleased {
                    key: Key::Named(Named::Alt | Named::Super),
                    ..
                }) => Some(Message::AltRelease),
                cosmic::iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key,
                    text: _,
                    modifiers,
                    ..
                }) => match key {
                    Key::Character(c) if modifiers.control() && (c == "p" || c == "k") => {
                        Some(Message::KeyboardNav(keyboard_nav::Action::FocusPrevious))
                    }
                    Key::Character(c) if modifiers.control() && (c == "n" || c == "j") => {
                        Some(Message::KeyboardNav(keyboard_nav::Action::FocusNext))
                    }
                    Key::Character(c) if modifiers.control() => {
                        let nums = (1..10)
                            .map(|n| (n.to_string(), ((n + 10) % 10) - 1))
                            .collect::<Vec<_>>();
                        nums.iter()
                            .find_map(|n| (n.0 == c).then(|| Message::Activate(Some(n.1))))
                    }
                    Key::Named(Named::ArrowUp) => {
                        Some(Message::KeyboardNav(keyboard_nav::Action::FocusPrevious))
                    }
                    Key::Named(Named::ArrowDown) => {
                        Some(Message::KeyboardNav(keyboard_nav::Action::FocusNext))
                    }
                    Key::Named(Named::Escape) => Some(Message::Hide),
                    Key::Named(Named::Tab) => Some(Message::TabPress),
                    Key::Named(Named::Backspace)
                        if matches!(status, Status::Ignored) && modifiers.is_empty() =>
                    {
                        Some(Message::Backspace)
                    }
                    _ => None,
                },
                cosmic::iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
                    Some(Message::CursorMoved(position))
                }
                cosmic::iced::Event::Window(WindowEvent::Opened { position: _, size }) => {
                    Some(Message::Opened(size, id))
                }
                cosmic::iced::Event::Window(WindowEvent::Resized(s)) => {
                    Some(Message::Opened(s, id))
                }
                _ => None,
            }),
        ])
    }
}
