use std::collections::HashMap;
use std::fmt::{Debug, Display};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::str::FromStr;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use clap::Parser;
use cosmic::{
    Element,
    app::{Core, CosmicFlags, Settings, Task},
    cctk::sctk::{
        self,
        shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer},
    },
    cosmic_config::{Config, CosmicConfigEntry},
    cosmic_theme::Spacing,
    dbus_activation,
    desktop::{DesktopEntryData, IconSourceExt, fde::PathSource},
    iced::{
        self, Alignment, Color, Length, Limits, Shadow, Size, Subscription, Vector,
        event::{listen_with, wayland::OverlapNotifyEvent},
        executor,
        id::Id,
        widget::{column, container, mouse_area, row, scrollable::RelativeOffset},
        window::Event as WindowEvent,
    },
    iced::{
        core::{
            Border, Padding, Rectangle,
            alignment::{Horizontal, Vertical},
            event::{
                PlatformSpecific,
                wayland::{self, LayerEvent},
            },
            keyboard::{Key, key::Named},
            widget::operation::{
                self,
                focusable::{find_focused, focus},
            },
            window::Id as SurfaceId,
        },
        platform_specific::shell::wayland::commands::{
            self,
            activation::request_token,
            layer_surface::{destroy_layer_surface, get_layer_surface, set_layer},
            overlap_notify::overlap_notify,
            popup::destroy_popup,
        },
        runtime::{
            self as iced_runtime,
            dnd::end_dnd,
            platform_specific::wayland::{
                layer_surface::SctkLayerSurfaceSettings,
                popup::{SctkPopupSettings, SctkPositioner},
            },
        },
    },
    keyboard_nav,
    theme::{self, Button, TextInput},
    widget::{
        self, Column,
        autosize::autosize,
        button, divider,
        icon::{self},
        scrollable, search_input, space, svg, text, text_input,
    },
};
use cosmic_app_list_config::AppListConfig;
use itertools::Itertools;
use log::error;
use serde::{Deserialize, Serialize};
use switcheroo_control::Gpu;

use crate::app_group::AppLibraryConfig;
use crate::fl;
use crate::subscriptions::desktop_files::desktop_files;
use crate::widgets::application::ApplicationButton;

static SEARCH_ID: LazyLock<Id> = LazyLock::new(|| Id::new("search"));

static SEARCH_PLACEHOLDER: LazyLock<String> = LazyLock::new(|| fl!("search-placeholder"));
static RUN: LazyLock<String> = LazyLock::new(|| fl!("run"));
static FLATPAK: LazyLock<String> = LazyLock::new(|| fl!("flatpak"));
static LOCAL: LazyLock<String> = LazyLock::new(|| fl!("local"));
static NIX: LazyLock<String> = LazyLock::new(|| fl!("nix"));
static SNAP: LazyLock<String> = LazyLock::new(|| fl!("snap"));
static SYSTEM: LazyLock<String> = LazyLock::new(|| fl!("system"));

pub(crate) static WINDOW_ID: LazyLock<SurfaceId> = LazyLock::new(SurfaceId::unique);
pub(crate) static MENU_ID: LazyLock<SurfaceId> = LazyLock::new(SurfaceId::unique);
pub(crate) static MENU_AUTOSIZE_ID: LazyLock<cosmic::widget::Id> =
    LazyLock::new(cosmic::widget::Id::unique);

#[derive(Parser, Debug, Serialize, Deserialize, Clone)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
pub struct Args {
    #[clap(subcommand)]
    pub subcommand: Option<ApplicationsTasks>,
}

impl CosmicFlags for Args {
    type SubCommand = ApplicationsTasks;
    type Args = Vec<String>;

    fn action(&self) -> Option<&ApplicationsTasks> {
        self.subcommand.as_ref()
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, clap::Subcommand)]
pub enum ApplicationsTasks {
    #[clap(about = "Start app-library with an input")]
    Input { input: Option<String> },
    #[clap(about = "Close app-library if open")]
    Close,
    #[clap(about = "Run a standalone instance (not single-instance)")]
    Run,
}

impl Display for ApplicationsTasks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", serde_json::ser::to_string(self).unwrap())
    }
}

impl FromStr for ApplicationsTasks {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::de::from_str(s)
    }
}

pub fn run() -> cosmic::iced::Result {
    let args = Args::parse();
    let settings = Settings::default()
        .antialiasing(true)
        .client_decorations(true)
        .debug(false)
        .default_text_size(16.0)
        .scale_factor(1.0)
        .no_main_window(true)
        .exit_on_close(false);

    // Use standalone run if requested, otherwise use single-instance
    if matches!(args.subcommand, Some(ApplicationsTasks::Run)) {
        cosmic::app::run::<CosmicAppLibrary>(settings, args)
    } else {
        cosmic::app::run_single_instance::<CosmicAppLibrary>(settings, args)
    }
}

pub struct AppSource(PathSource);

impl AppSource {
    pub fn as_icon(&self) -> Option<widget::icon::Handle> {
        let name = match &self.0 {
            PathSource::Local | PathSource::LocalDesktop => "app-source-local-symbolic",
            PathSource::System | PathSource::SystemLocal => "app-source-system-symbolic",
            PathSource::LocalFlatpak | PathSource::SystemFlatpak => "app-source-flatpak",
            PathSource::SystemSnap => "app-source-snap",
            PathSource::Nix | PathSource::LocalNix => "app-source-nix",
            PathSource::Other(_) => return None,
        };
        let handle = crate::icon_cache::icon_cache_handle(name, 16);
        Some(handle)
    }
}

impl<'a> From<&'a Path> for AppSource {
    fn from(path: &'a Path) -> Self {
        AppSource(PathSource::guess_from(path))
    }
}

impl Display for AppSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:.7}",
            match &self.0 {
                PathSource::Local | PathSource::LocalDesktop => LOCAL.as_str(),
                PathSource::SystemFlatpak | PathSource::LocalFlatpak => FLATPAK.as_str(),
                PathSource::SystemSnap => SNAP.as_str(),
                PathSource::Nix | PathSource::LocalNix => NIX.as_str(),
                PathSource::System | PathSource::SystemLocal => SYSTEM.as_str(),
                PathSource::Other(s) => s.as_str(),
            }
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SurfaceState {
    Visible,
    Hidden,
    WaitingToBeShown,
}

struct CosmicAppLibrary {
    search_value: String,
    entry_path_input: Vec<Arc<DesktopEntryData>>,
    all_entries: Vec<Arc<DesktopEntryData>>,
    menu: Option<usize>,
    helper: Option<Config>,
    config: AppLibraryConfig,
    locale: Option<String>,
    dnd_icon: Option<usize>,
    waiting_for_filtered: bool,
    scroll_offset: f32,
    core: Core,
    gpus: Option<Vec<Gpu>>,
    last_hide: Option<Instant>,
    duplicates: HashMap<PathBuf, (AppSource, Option<widget::icon::Handle>)>,
    app_list_config: AppListConfig,
    overlap: HashMap<String, Rectangle>,
    margin: f32,
    width: f32,
    height: f32,
    needs_clear: bool,
    focused_id: Option<widget::Id>,
    entry_ids: Vec<widget::Id>,
    entry_icon_handles: Vec<widget::icon::Handle>,
    scrollable_id: widget::Id,
    surface_state: SurfaceState,
    hand_over: String,
}

impl Default for CosmicAppLibrary {
    fn default() -> Self {
        Self {
            search_value: Default::default(),
            entry_path_input: Default::default(),
            all_entries: Default::default(),
            menu: Default::default(),
            helper: Default::default(),
            config: Default::default(),
            locale: Default::default(),
            dnd_icon: Default::default(),
            waiting_for_filtered: Default::default(),
            scroll_offset: Default::default(),
            core: Default::default(),
            gpus: Default::default(),
            last_hide: Default::default(),
            duplicates: Default::default(),
            app_list_config: Default::default(),
            overlap: Default::default(),
            margin: Default::default(),
            width: 1920.0,
            height: Default::default(),
            needs_clear: Default::default(),
            focused_id: Default::default(),
            entry_ids: Default::default(),
            entry_icon_handles: Default::default(),
            scrollable_id: widget::Id::unique(),
            surface_state: SurfaceState::Hidden,
            hand_over: String::default(),
        }
    }
}

async fn try_get_gpus() -> Option<Vec<Gpu>> {
    let connection = zbus::Connection::system().await.ok()?;
    let proxy = switcheroo_control::SwitcherooControlProxy::new(&connection)
        .await
        .ok()?;

    if !proxy.has_dual_gpu().await.ok()? {
        return None;
    }

    let gpus = proxy.get_gpus().await.ok()?;
    if gpus.is_empty() {
        return None;
    }
    Some(gpus)
}

impl CosmicAppLibrary {
    pub fn activate(&mut self) -> Task<Message> {
        if matches!(self.surface_state, SurfaceState::Visible) {
            return self.hide();
        } else if matches!(self.surface_state, SurfaceState::Hidden)
            && self
                .last_hide
                .is_none_or(|i| i.elapsed() >= Duration::from_millis(100))
        {
            self.surface_state = SurfaceState::WaitingToBeShown;
            self.search_value = "".to_string();
            self.scroll_offset = 0.0;
            self.load_apps();
            self.needs_clear = true;
            let fetch_gpus = Task::perform(try_get_gpus(), |gpus| {
                cosmic::Action::App(Message::GpuUpdate(gpus))
            });
            return Task::batch(vec![
                get_layer_surface(SctkLayerSurfaceSettings {
                    id: *WINDOW_ID,
                    keyboard_interactivity: KeyboardInteractivity::Exclusive,
                    anchor: Anchor::all(),
                    namespace: "app-library".into(),
                    size: Some((None, None)),
                    exclusive_zone: -1,
                    ..Default::default()
                }),
                overlap_notify(*WINDOW_ID, true),
                fetch_gpus,
            ])
            .chain(text_input::focus(SEARCH_ID.clone()))
            .chain(
                iced_runtime::task::widget(find_focused())
                    .map(|id| cosmic::Action::App(Message::UpdateFocused(Some(id)))),
            );
        }
        Task::none()
    }

    fn handle_overlap(&mut self) {
        if !matches!(self.surface_state, SurfaceState::Visible) {
            return;
        }

        let mid_height = self.height / 2.;
        self.margin = 0.;

        for o in self.overlap.values() {
            if self.margin + mid_height < o.y
                || self.margin > o.y + o.height
                || mid_height < o.y + o.height
            {
                continue;
            }

            self.margin = o.y + o.height;
        }
    }

    /// Update entry IDs and their icon handles.
    fn update_entry_metadata(&mut self) {
        self.entry_ids = (0..self.entry_path_input.len())
            .map(|_| widget::Id::unique())
            .collect();

        self.entry_icon_handles = self
            .entry_path_input
            .iter()
            .map(|e| e.icon.as_cosmic_icon())
            .collect();
    }

    fn grid_columns(&self) -> usize {
        let spacing = theme::spacing();
        grid_columns_for_width(
            self.width,
            f32::from(spacing.space_xxl) * 4.0,
            f32::from(spacing.space_s),
        )
    }
}

const MAX_GRID_COLUMNS: usize = 7;
const MIN_GRID_CELL_WIDTH: f32 = 172.0;

fn grid_columns_for_width(width: f32, horizontal_padding: f32, spacing: f32) -> usize {
    let available = (width - horizontal_padding).max(MIN_GRID_CELL_WIDTH);
    (((available + spacing) / (MIN_GRID_CELL_WIDTH + spacing)).floor() as usize)
        .clamp(1, MAX_GRID_COLUMNS)
}

/// Relative scroll offset that brings the row holding `index` into view.
///
/// The grid scrolls between the first and last *row*, so the denominator is
/// the number of scrollable rows (`rows - 1`). Dividing by the raw row count
/// would leave the final row permanently clipped on short outputs.
fn scroll_ratio(index: usize, total: usize, columns: usize) -> f32 {
    let columns = columns.max(1);
    let rows = total.div_ceil(columns);
    let scrollable_rows = rows.saturating_sub(1);
    if scrollable_rows == 0 {
        return 0.0;
    }
    ((index / columns) as f32 / scrollable_rows as f32).clamp(0.0, 1.0)
}

fn launchpad_glass_style(theme: &cosmic::Theme) -> container::Style {
    let cosmic = theme.cosmic();
    let mut background: Color = cosmic.background.base.into();
    // App labels sit directly on this scrim over an arbitrary wallpaper, so
    // keep it opaque enough to hold contrast; high contrast drops blending
    // entirely rather than relying on the wallpaper behind it.
    background.a = match (cosmic.is_high_contrast, cosmic.is_dark) {
        (true, _) => 1.0,
        (false, true) => 0.82,
        (false, false) => 0.78,
    };

    container::Style {
        text_color: Some(cosmic.background.on.into()),
        icon_color: Some(cosmic.background.on.into()),
        background: Some(iced::Background::Color(background)),
        ..Default::default()
    }
}

fn search_glass_style(theme: &cosmic::Theme) -> container::Style {
    let cosmic = theme.cosmic();
    let component = &cosmic.background.component;
    let mut background: Color = component.base.into();
    background.a = if cosmic.is_dark { 0.62 } else { 0.76 };
    let mut hairline: Color = cosmic.accent_color().into();
    hairline.a = if cosmic.is_high_contrast { 0.72 } else { 0.28 };
    let mut shadow_color: Color = cosmic.shade.into();
    shadow_color.a = if cosmic.is_dark { 0.24 } else { 0.14 };

    container::Style {
        text_color: Some(component.on.into()),
        icon_color: Some(component.on.into()),
        background: Some(iced::Background::Color(background)),
        border: Border {
            radius: cosmic.radius_l().into(),
            width: 1.0,
            color: hairline,
        },
        shadow: Shadow {
            color: shadow_color,
            offset: Vector::new(0.0, 4.0),
            blur_radius: 18.0,
        },
        ..Default::default()
    }
}

#[derive(Clone, Debug)]
enum Message {
    Activate,
    AskAi,
    UpdateFocused(Option<widget::Id>),
    InputChanged(String),
    KeyboardNav(keyboard_nav::Action),
    PrevRow,
    NextRow,
    Layer(LayerEvent, SurfaceId),
    Hide,
    ActivateApp(usize, Option<usize>),
    StartCurAppFocus,
    ActivationToken(Option<String>, String, String, Option<usize>, bool),
    LoadApps,
    FilterApps(String, Vec<Arc<DesktopEntryData>>),
    OpenContextMenu(Rectangle, usize),
    CloseContextMenu,
    SelectAction(MenuAction),
    StartDrag(usize),
    FinishDrag(bool),
    CancelDrag,
    ScrollYOffset(f32),
    GpuUpdate(Option<Vec<Gpu>>),
    PinToAppTray(usize),
    UnPinFromAppTray(usize),
    AppListConfig(AppListConfig),
    Opened(Size, SurfaceId),
    Resized(Size, SurfaceId),
    Overlap(OverlapNotifyEvent),
}

#[derive(Clone, Debug)]
enum MenuAction {
    DesktopAction(String),
}

pub fn menu_button<'a, Message: Clone + 'a>(
    content: impl Into<Element<'a, Message>>,
) -> cosmic::widget::Button<'a, Message> {
    cosmic::widget::button::custom(content)
        .class(Button::MenuItem)
        .padding(menu_control_padding())
        .width(Length::Fill)
}

pub fn menu_control_padding() -> Padding {
    let theme = cosmic::theme::active();
    let cosmic = theme.cosmic();
    [cosmic.space_xxs(), cosmic.space_m()].into()
}

impl CosmicAppLibrary {
    pub fn load_apps(&mut self) {
        let xdg_current_desktop = std::env::var("XDG_CURRENT_DESKTOP").ok();
        self.all_entries = cosmic::desktop::load_applications(
            self.locale.as_slice(),
            false,
            xdg_current_desktop.as_deref(),
        )
        .filter(|d| d.exec.is_some())
        .map(Arc::new)
        .collect();
        self.all_entries.sort_by(|a, b| a.name.cmp(&b.name));

        self.entry_path_input = self.config.filtered(&self.search_value, &self.all_entries);

        // collect duplicates
        self.duplicates.clear();
        self.duplicates = self
            .all_entries
            .iter()
            .enumerate()
            .fold(
                (std::mem::take(&mut self.duplicates), 0, "", ""),
                |(mut dups, cur_count, cur_name, cur_id): (HashMap<_, _>, usize, &str, &str),
                 (i, e)| {
                    if cur_name.to_lowercase().trim() == e.name.to_lowercase().trim()
                        || e.id == cur_id
                    {
                        if cur_count == 1 {
                            // insert previous entry
                            if let Some(path) = self.all_entries[i - 1].path.as_ref() {
                                let source = AppSource::from(path.as_ref());
                                let icon_handle = source.as_icon();
                                dups.insert(path.clone(), (source, icon_handle));
                            }
                        }
                        if let Some(path) = e.path.as_ref() {
                            let source = AppSource::from(path.as_ref());
                            let icon_handle = source.as_icon();
                            dups.insert(path.clone(), (source, icon_handle));
                        }
                        (dups, cur_count + 1, cur_name, cur_id)
                    } else {
                        (dups, 1, e.name.as_str(), e.id.as_str())
                    }
                },
            )
            .0;
        self.update_entry_metadata();
    }

    fn filter_apps(&mut self) -> Task<Message> {
        let config = self.config.clone();
        let all_entries = self.all_entries.clone();
        let input = self.search_value.clone();
        if !self.waiting_for_filtered {
            self.waiting_for_filtered = true;
            iced::Task::perform(
                async move {
                    let mut apps = config.filtered(&input, &all_entries);
                    apps.sort_by(|a, b| a.name.cmp(&b.name));
                    (input, apps)
                },
                |(input, apps)| Message::FilterApps(input, apps),
            )
            .map(cosmic::Action::App)
        } else {
            iced::Task::none()
        }
    }

    pub fn hide(&mut self) -> Task<Message> {
        if !matches!(self.surface_state, SurfaceState::Visible) {
            return Task::none();
        }
        // cancel existing dnd if it exists then try again...
        if self.dnd_icon.take().is_some() {
            return Task::batch(vec![
                end_dnd(),
                Task::perform(async {}, |_| cosmic::Action::App(Message::Hide)),
            ]);
        }
        self.focused_id = None;
        self.entry_ids.clear();
        self.entry_icon_handles.clear();
        self.search_value.clear();
        self.menu = None;
        self.scroll_offset = 0.0;
        self.surface_state = SurfaceState::Hidden;
        self.hand_over.clear();

        iced::Task::batch(vec![
            text_input::focus(SEARCH_ID.clone()),
            destroy_popup(*MENU_ID),
            destroy_layer_surface(*WINDOW_ID),
        ])
    }

    fn activate_app(
        &mut self,
        i: usize,
        gpu_idx: Option<usize>,
    ) -> Task<<Self as cosmic::Application>::Message> {
        if let Some(de) = self.entry_path_input.get(i) {
            let app_id = de.id.clone();
            let exec = de.exec.clone().unwrap();
            let terminal = de.terminal;
            request_token(
                Some(String::from(<Self as cosmic::Application>::APP_ID)),
                Some(*WINDOW_ID),
            )
            .map(move |t| {
                cosmic::Action::App(Message::ActivationToken(
                    t,
                    app_id.clone(),
                    exec.clone(),
                    gpu_idx,
                    terminal,
                ))
            })
        } else {
            Task::none()
        }
    }
}

impl cosmic::Application for CosmicAppLibrary {
    type Message = Message;
    type Executor = executor::Default;
    type Flags = Args;
    const APP_ID: &'static str = "com.clawos.AppLibrary";

    fn core(&self) -> &Core {
        &self.core
    }

    fn update(&mut self, message: Message) -> Task<Self::Message> {
        match message {
            Message::Activate => {
                return self.activate();
            }
            Message::UpdateFocused(id) => {
                self.focused_id = id;
                let grid_columns = self.grid_columns();
                let i = self
                    .focused_id
                    .as_ref()
                    .and_then(|focused| self.entry_ids.iter().position(|i| i == focused))
                    .unwrap_or(0);
                let y = scroll_ratio(i, self.entry_path_input.len(), grid_columns);

                return iced_runtime::task::widget(operation::scrollable::snap_to(
                    self.scrollable_id.clone(),
                    RelativeOffset {
                        x: None,
                        y: Some(y),
                    },
                ));
            }
            Message::KeyboardNav(message) => match message {
                keyboard_nav::Action::FocusNext => {
                    return iced::Task::batch(vec![
                        iced::widget::operation::focus_next()
                            .map(|id| cosmic::Action::App(Message::UpdateFocused(id))),
                        iced_runtime::task::widget(find_focused())
                            .map(|id| cosmic::Action::App(Message::UpdateFocused(Some(id)))),
                    ]);
                }
                keyboard_nav::Action::FocusPrevious => {
                    return iced::Task::batch(vec![
                        iced::widget::operation::focus_previous()
                            .map(|id| cosmic::Action::App(Message::UpdateFocused(id))),
                        iced_runtime::task::widget(find_focused())
                            .map(|id| cosmic::Action::App(Message::UpdateFocused(Some(id)))),
                    ]);
                }
                keyboard_nav::Action::Escape => return self.on_escape(),
                keyboard_nav::Action::Search => return self.on_search(),

                keyboard_nav::Action::Fullscreen => {}
            },

            Message::PrevRow => {
                let grid_columns = self.grid_columns();
                let mut i = self
                    .focused_id
                    .as_ref()
                    .and_then(|focused| self.entry_ids.iter().position(|i| i == focused))
                    .unwrap_or(
                        self.entry_ids
                            .len()
                            .saturating_add(grid_columns.saturating_sub(1)),
                    );
                if i == 0 {
                    self.focused_id = None;

                    return iced::Task::batch(vec![
                        iced::widget::operation::focus_previous()
                            .map(|id| cosmic::Action::App(Message::UpdateFocused(id))),
                        iced_runtime::task::widget(find_focused())
                            .map(|id| cosmic::Action::App(Message::UpdateFocused(Some(id)))),
                    ]);
                }
                i = i.saturating_sub(grid_columns);
                let y = scroll_ratio(i, self.entry_path_input.len(), grid_columns);

                let Some(focused) = self.entry_ids.get(i).cloned() else {
                    return Task::none();
                };
                self.focused_id = Some(focused.clone());
                return Task::batch(vec![
                    iced_runtime::task::widget(focus(focused))
                        .map(|id| cosmic::Action::App(Message::UpdateFocused(Some(id)))),
                    iced_runtime::task::widget(operation::scrollable::snap_to(
                        self.scrollable_id.clone(),
                        RelativeOffset {
                            x: None,
                            y: Some(y),
                        },
                    )),
                ]);
            }
            Message::NextRow => {
                let grid_columns = self.grid_columns();
                let mut i: i32 = self
                    .focused_id
                    .as_ref()
                    .and_then(|focused| self.entry_ids.iter().position(|i| i == focused))
                    .map(|i| i as i32)
                    .unwrap_or(-(grid_columns as i32));
                if i == self.entry_ids.len() as i32 - 1 {
                    self.focused_id = None;
                    return iced::Task::batch(vec![
                        iced::widget::operation::focus_next()
                            .map(|id| cosmic::Action::App(Message::UpdateFocused(id))),
                        iced_runtime::task::widget(find_focused())
                            .map(|id| cosmic::Action::App(Message::UpdateFocused(Some(id)))),
                    ]);
                }
                i += grid_columns as i32;
                i = i.min(self.entry_ids.len() as i32 - 1);
                let Some(focused) = self.entry_ids.get(i as usize).cloned() else {
                    return Task::none();
                };
                self.focused_id = Some(focused.clone());
                let y = scroll_ratio(i as usize, self.entry_path_input.len(), grid_columns);

                return Task::batch(vec![
                    iced_runtime::task::widget(operation::scrollable::snap_to(
                        self.scrollable_id.clone(),
                        RelativeOffset {
                            x: None,
                            y: Some(y),
                        },
                    )),
                    iced_runtime::task::widget(focus(focused))
                        .map(|id| cosmic::Action::App(Message::UpdateFocused(Some(id)))),
                ]);
            }
            Message::InputChanged(value) => {
                self.search_value = value;
                return self.filter_apps();
            }
            Message::Layer(e, id) => match e {
                LayerEvent::Focused => {
                    if self.menu.is_none() && id == *WINDOW_ID {
                        return text_input::focus(SEARCH_ID.clone());
                    }
                }
                LayerEvent::Unfocused => {
                    self.last_hide = Some(Instant::now());
                    if matches!(self.surface_state, SurfaceState::Visible)
                        && id == *WINDOW_ID
                        && self.menu.is_none()
                    {
                        return self.hide();
                    }
                }
                LayerEvent::Done if id == *WINDOW_ID => {
                    // no need for commands here
                    _ = self.hide();
                }
                _ => {}
            },
            Message::Hide => {
                return self.hide();
            }
            Message::AskAi => {
                let query = self.search_value.trim().to_string();
                if !query.is_empty() {
                    let mut cmd = std::process::Command::new("cos");
                    cmd.args(["app", "agent", "overlay", "--query", &query]);
                    if let Err(err) = cmd.spawn() {
                        error!("failed to launch cos app agent overlay: {err}");
                    }
                }
                return self.hide();
            }
            Message::ActivateApp(i, gpu_idx) => {
                return self.activate_app(i, gpu_idx);
            }
            Message::StartCurAppFocus => {
                let i = if self
                    .focused_id
                    .as_ref()
                    .is_some_and(|cur_focus| cur_focus == &*SEARCH_ID)
                {
                    0
                } else {
                    self.focused_id
                        .as_ref()
                        .and_then(|focus| self.entry_ids.iter().position(|id| focus == id))
                        .unwrap_or_default()
                };
                let gpu_idx = None;
                return self.activate_app(i, gpu_idx);
            }
            Message::ActivationToken(token, app_id, exec, gpu_idx, terminal) => {
                let mut env_vars = Vec::new();
                if let Some(token) = token {
                    env_vars.push(("XDG_ACTIVATION_TOKEN".to_string(), token.clone()));
                    env_vars.push(("DESKTOP_STARTUP_ID".to_string(), token));
                }
                if let (Some(gpus), Some(idx)) = (self.gpus.as_ref(), gpu_idx) {
                    env_vars.extend(gpus[idx].environment.clone());
                }
                tokio::spawn(async move {
                    cosmic::desktop::spawn_desktop_exec(exec, env_vars, Some(&app_id), terminal)
                        .await
                });
                return self.update(Message::Hide);
            }
            Message::LoadApps => {
                return self.filter_apps();
            }
            Message::OpenContextMenu(rect, i) => {
                if self.menu.take().is_some() {
                    return destroy_popup(*MENU_ID);
                } else {
                    self.menu = Some(i);
                    return commands::popup::get_popup(SctkPopupSettings {
                                        parent: *WINDOW_ID,
                                        id: *MENU_ID,
                                        positioner: SctkPositioner {
                                            size: None,
                                            size_limits: Limits::NONE.min_width(1.0).min_height(1.0).max_width(300.0).max_height(800.0),
                                            anchor_rect: Rectangle {
                                                x: rect.x as i32,
                                                y: rect.y as i32 - self.scroll_offset as i32,
                                                width: rect.width as i32,
                                                height: rect.height as i32,
                                            },
                                            anchor:
                                                sctk::reexports::protocols::xdg::shell::client::xdg_positioner::Anchor::Right,
                                            gravity: sctk::reexports::protocols::xdg::shell::client::xdg_positioner::Gravity::Right,
                                            reactive: true,
                                            ..Default::default()
                                        },
                                        grab: false,
                                        parent_size: None,
                                        close_with_children: true,
                                        input_zone: None,
                                    });
                }
            }
            Message::CloseContextMenu => {
                self.menu = None;
                return commands::popup::destroy_popup(*MENU_ID);
            }
            Message::SelectAction(action) => {
                self.menu = None;
                let mut tasks = vec![commands::popup::destroy_popup(*MENU_ID)];
                match action {
                    MenuAction::DesktopAction(exec) => {
                        let mut exec = shlex::Shlex::new(&exec);

                        let mut cmd = match exec.next() {
                            Some(cmd) if !cmd.contains('=') => tokio::process::Command::new(cmd),
                            _ => return Task::none(),
                        };
                        for arg in exec {
                            if !arg.starts_with('%') {
                                cmd.arg(arg);
                            }
                        }
                        let _ = cmd.spawn();
                        tasks.push(self.hide());
                    }
                }
                return cosmic::Task::batch(tasks);
            }
            Message::StartDrag(i) => {
                self.dnd_icon = Some(i);
                return set_layer(*WINDOW_ID, Layer::Bottom);
            }
            Message::FinishDrag(_) => {
                self.dnd_icon = None;
                return set_layer(*WINDOW_ID, Layer::Top);
            }
            Message::CancelDrag => {
                self.dnd_icon = None;
                return set_layer(*WINDOW_ID, Layer::Top);
            }
            Message::ScrollYOffset(y) => {
                self.scroll_offset = y;
            }
            Message::FilterApps(input, filtered_apps) => {
                self.entry_path_input = filtered_apps;
                self.update_entry_metadata();

                self.waiting_for_filtered = false;
                if self.search_value != input {
                    return self.filter_apps();
                }
            }
            Message::GpuUpdate(gpus) => {
                self.gpus = gpus;
            }
            Message::PinToAppTray(usize) => {
                let pinned_id = self.entry_path_input.get(usize).map(|e| e.id.clone());
                if let Some((pinned_id, app_list_helper)) = pinned_id
                    .zip(Config::new(cosmic_app_list_config::APP_ID, AppListConfig::VERSION).ok())
                {
                    self.app_list_config.add_pinned(pinned_id, &app_list_helper);
                }
                self.menu = None;
                return commands::popup::destroy_popup(*MENU_ID);
            }
            Message::UnPinFromAppTray(usize) => {
                let pinned_id = self.entry_path_input.get(usize).map(|e| e.id.clone());
                if let Some((pinned_id, app_list_helper)) = pinned_id
                    .zip(Config::new(cosmic_app_list_config::APP_ID, AppListConfig::VERSION).ok())
                {
                    self.app_list_config
                        .remove_pinned(&pinned_id, &app_list_helper);
                }
                self.menu = None;
                return commands::popup::destroy_popup(*MENU_ID);
            }
            Message::AppListConfig(config) => {
                self.app_list_config = config;
            }
            Message::Resized(size, window_id) => {
                if window_id == *WINDOW_ID {
                    self.width = size.width;
                    self.height = size.height;
                    self.handle_overlap();
                }
            }
            Message::Opened(size, window_id) => {
                if window_id == *WINDOW_ID {
                    if matches!(self.surface_state, SurfaceState::WaitingToBeShown) {
                        self.surface_state = SurfaceState::Visible;
                    }
                    self.width = size.width;
                    self.height = size.height;
                    self.handle_overlap();
                }
                if !self.hand_over.is_empty() {
                    let input = self.hand_over.clone();
                    self.hand_over.clear();
                    return self.update(Message::InputChanged(input));
                }
            }
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
        }
        Task::none()
    }

    fn dbus_activation(&mut self, msg: dbus_activation::Message) -> Task<Self::Message> {
        match msg.msg {
            dbus_activation::Details::Activate => self.activate(),
            dbus_activation::Details::ActivateAction { action, .. } => {
                let Ok(cmd) = ApplicationsTasks::from_str(&action) else {
                    return Task::none();
                };
                match cmd {
                    ApplicationsTasks::Input { input } => {
                        if let Some(input) = input {
                            self.hand_over.push_str(&input);
                        }
                        if self.surface_state == SurfaceState::Hidden {
                            return self.activate();
                        }
                        Task::none()
                    }
                    ApplicationsTasks::Close => self.hide(),
                    // Run is handled at startup, not via D-Bus
                    ApplicationsTasks::Run => Task::none(),
                }
            }
            _ => Task::none(),
        }
    }

    fn view<'a>(&'a self) -> Element<'a, Message> {
        unimplemented!()
    }

    fn view_window<'a>(&'a self, id: SurfaceId) -> Element<'a, Message> {
        let Spacing {
            space_none,
            space_xxs,
            space_xs,
            space_s,
            space_l,
            space_xxl,
            ..
        } = theme::spacing();

        if id == *MENU_ID {
            let Some((menu, i)) = self
                .menu
                .as_ref()
                .and_then(|i| self.entry_path_input.get(*i).map(|e| (e, i)))
            else {
                return container(space::horizontal())
                    .width(Length::Fixed(1.0))
                    .height(Length::Fixed(1.0))
                    .into();
            };

            let mut list_column = Vec::new();

            if let Some(gpus) = self.gpus.as_ref() {
                for (j, gpu) in gpus.iter().enumerate() {
                    let default_idx = if menu.prefers_dgpu {
                        gpus.iter().position(|gpu| !gpu.default).unwrap_or(0)
                    } else {
                        gpus.iter().position(|gpu| gpu.default).unwrap_or(0)
                    };
                    list_column.push(
                        menu_button(text::body(format!(
                            "{} {}",
                            fl!("run-on", gpu = gpu.name.as_str()),
                            if j == default_idx {
                                fl!("run-on-default")
                            } else {
                                String::new()
                            }
                        )))
                        .on_press(Message::ActivateApp(*i, Some(j)))
                        .into(),
                    )
                }
            } else {
                list_column.push(
                    menu_button(text::body(RUN.clone()))
                        .on_press(Message::ActivateApp(*i, None))
                        .into(),
                );
            }

            if !menu.desktop_actions.is_empty() {
                list_column.push(divider::horizontal::light().into());
                for action in menu.desktop_actions.iter() {
                    list_column.push(
                        menu_button(text::body(&action.name))
                            .on_press(Message::SelectAction(MenuAction::DesktopAction(
                                action.exec.clone(),
                            )))
                            .into(),
                    );
                }
            }

            // add to pinned
            let svg_accent = Rc::new(|theme: &cosmic::Theme| {
                let color = theme.cosmic().accent_color().into();
                svg::Style { color: Some(color) }
            });
            let is_pinned = self.app_list_config.favorites.iter().any(|p| p == &menu.id);
            let pin_to_app_tray = menu_button(
                if is_pinned {
                    row![
                        icon::icon(icon::from_name("checkbox-checked-symbolic").size(16).into())
                            .class(cosmic::theme::Svg::Custom(svg_accent.clone())),
                        text::body(fl!("pin-to-app-tray"))
                    ]
                } else {
                    row![
                        space::horizontal().width(16.0),
                        text::body(fl!("pin-to-app-tray"))
                    ]
                }
                .spacing(space_xxs),
            )
            .on_press(if is_pinned {
                Message::UnPinFromAppTray(*i)
            } else {
                Message::PinToAppTray(*i)
            });
            list_column.push(divider::horizontal::light().into());
            list_column.push(pin_to_app_tray.into());

            return autosize(
                container(scrollable(Column::with_children(list_column)))
                    .padding(1)
                    .class(theme::Container::custom(|theme| {
                        let cosmic = theme.cosmic();
                        let component = &cosmic.background.component;
                        container::Style {
                            icon_color: Some(component.on.into()),
                            text_color: Some(component.on.into()),
                            background: Some(iced::Background::Color(component.base.into())),
                            border: Border {
                                radius: cosmic.radius_s().map(|x| x + 1.0).into(),
                                width: 1.0,
                                color: component.divider.into(),
                            },
                            ..Default::default()
                        }
                    })),
                MENU_AUTOSIZE_ID.clone(),
            )
            .max_height(800.)
            .max_width(300.)
            .into();
        }
        // --------------------------------------------------------------
        // Launchpad-style layout
        // --------------------------------------------------------------
        // Top: prominent centered "Search" pill.
        // Below: an evenly-spaced grid of all installed apps, rendered with
        // large icons and theme-colored labels over translucent glass.
        // --------------------------------------------------------------

        let search_pill = container(
            search_input(SEARCH_PLACEHOLDER.as_str(), self.search_value.as_str())
                .on_input(Message::InputChanged)
                .on_paste(Message::InputChanged)
                .on_submit(|_| Message::StartCurAppFocus)
                .style(TextInput::Search)
                .width(Length::Fill)
                .size(15)
                .id(SEARCH_ID.clone()),
        )
        .padding([6, 12])
        .width(Length::Fill)
        .max_width(360.0)
        .class(theme::Container::custom(search_glass_style));

        let ai_hint: Option<Element<'_, Message>> = if !self.search_value.trim().is_empty() {
            let q = self.search_value.trim().to_string();
            let ai_button = button::custom(
                row![
                    container(
                        text::body(fl!("ask-claw-ai", query = q.clone()))
                            .align_x(Horizontal::Left)
                            .align_y(Vertical::Center),
                    )
                    .width(Length::FillPortion(5))
                    .padding([0, space_xxs]),
                    container(
                        text::caption(fl!("ask-claw-ai-hint"))
                            .align_x(Horizontal::Right)
                            .align_y(Vertical::Center),
                    )
                    .width(Length::FillPortion(2))
                    .padding([0, space_xxs]),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .on_press(Message::AskAi)
            .padding([space_xs, space_s])
            .class(Button::Suggested);
            Some(
                container(container(ai_button).width(Length::Fill).max_width(600.0))
                    .center_x(Length::Fill)
                    .padding([0, space_xxl, space_xs, space_xxl])
                    .into(),
            )
        } else {
            None
        };

        let header =
            container(column![container(search_pill).center_x(Length::Fill),].spacing(space_xxs))
                .width(Length::Fill)
                .padding([space_xs, space_xxl, space_xs, space_xxl]);

        // App grid: seven cells wide where space allows, with narrower outputs
        // selecting a safe count without changing the launchpad layout.
        let grid_columns = self.grid_columns();
        let app_grid_list: Vec<Element<'_, Message>> = self
            .entry_path_input
            .iter()
            .zip(self.entry_ids.iter())
            .zip(self.entry_icon_handles.iter())
            .enumerate()
            .map(|(i, ((entry, id), icon_handle))| {
                let gpu_idx = self.gpus.as_ref().map(|gpus| {
                    if entry.prefers_dgpu {
                        gpus.iter().position(|gpu| !gpu.default).unwrap_or(0)
                    } else {
                        gpus.iter().position(|gpu| gpu.default).unwrap_or(0)
                    }
                });
                let dup = entry
                    .path
                    .as_ref()
                    .and_then(|path| self.duplicates.get(path));
                let selected = self.menu.is_some_and(|m| m == i);

                let b = ApplicationButton::new(
                    id.clone(),
                    &entry.name,
                    icon_handle.clone(),
                    &entry.path,
                    move |rect| Message::OpenContextMenu(rect, i),
                    if self.menu.is_none() {
                        Some(Message::ActivateApp(i, gpu_idx))
                    } else if selected {
                        Some(Message::CloseContextMenu)
                    } else {
                        None
                    },
                    dup,
                    selected,
                    self.menu.is_none().then_some(Message::StartDrag(i)),
                    self.menu.is_none().then_some(Message::FinishDrag(false)),
                    self.menu.is_none().then_some(Message::CancelDrag),
                );

                b.into()
            })
            .chunks(grid_columns)
            .into_iter()
            .map(|row_chunk| {
                let mut new_row: Vec<Element<'_, Message>> = row_chunk.collect_vec();
                let missing = grid_columns - new_row.len();
                if missing > 0 {
                    new_row.push(
                        iced::widget::space::horizontal()
                            .width(Length::FillPortion(missing.try_into().unwrap()))
                            .into(),
                    );
                }
                row(new_row).spacing(space_s).into()
            })
            .collect();

        let mut grid_col = column![]
            .spacing(space_l)
            .padding([space_l, space_xxl * 2, space_l, space_xxl * 2])
            .width(Length::Fill);
        for item in app_grid_list {
            grid_col = grid_col.push(item);
        }

        let app_scrollable = container(
            scrollable(grid_col)
                .on_scroll(|viewport| Message::ScrollYOffset(viewport.absolute_offset().y))
                .id(self.scrollable_id.clone())
                .height(Length::Fill),
        )
        .height(Length::Fill)
        .width(Length::Fill);

        let mut body = column![header];
        if let Some(ai) = ai_hint {
            body = body.push(ai);
        }
        body = body.push(app_scrollable);
        let content = body.align_x(Alignment::Center).width(Length::Fill);

        // Theme-derived translucent window over compositor wallpaper blur. The
        // whole surface is wrapped in a `mouse_area` so
        // that clicks on empty space dismiss the launcher (or close an open
        // context menu); buttons in the grid capture their own events first
        // and don't propagate.
        let window = container(content)
            .height(Length::Fill)
            .width(Length::Fill)
            .class(theme::Container::custom(launchpad_glass_style));

        let dismiss_msg = if self.menu.is_some() {
            Message::CloseContextMenu
        } else {
            Message::Hide
        };
        mouse_area(window).on_press(dismiss_msg).into()
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch(vec![
            desktop_files(0).map(|_| Message::LoadApps),
            listen_with(|e, status, id| match e {
                cosmic::iced::Event::PlatformSpecific(PlatformSpecific::Wayland(
                    wayland::Event::Layer(e, _, id),
                )) => Some(Message::Layer(e, id)),
                cosmic::iced::Event::PlatformSpecific(PlatformSpecific::Wayland(
                    wayland::Event::OverlapNotify(event, ..),
                )) => Some(Message::Overlap(event)),
                cosmic::iced::Event::Keyboard(cosmic::iced::keyboard::Event::KeyReleased {
                    key: Key::Named(Named::Escape),
                    modifiers: _mods,
                    ..
                }) => Some(Message::Hide),
                cosmic::iced::Event::Mouse(iced::mouse::Event::ButtonPressed(_))
                    if id == *WINDOW_ID =>
                {
                    Some(Message::CloseContextMenu)
                }
                cosmic::iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key,
                    text: _,
                    modifiers,
                    ..
                }) => match key {
                    Key::Character(c) if modifiers.control() && (c == "p" || c == "k") => {
                        Some(Message::PrevRow)
                    }
                    Key::Character(c) if modifiers.control() && (c == "n" || c == "j") => {
                        Some(Message::NextRow)
                    }
                    Key::Character(c) if modifiers.control() && (c == "f" || c == "l") => {
                        Some(Message::KeyboardNav(keyboard_nav::Action::FocusNext))
                    }
                    Key::Character(c) if modifiers.control() && (c == "b" || c == "h") => {
                        Some(Message::KeyboardNav(keyboard_nav::Action::FocusPrevious))
                    }
                    Key::Named(Named::ArrowUp)
                        if matches!(status, iced::event::Status::Ignored) =>
                    {
                        Some(Message::PrevRow)
                    }
                    Key::Named(Named::ArrowDown)
                        if matches!(status, iced::event::Status::Ignored) =>
                    {
                        Some(Message::NextRow)
                    }
                    Key::Named(Named::ArrowLeft)
                        if matches!(status, iced::event::Status::Ignored) =>
                    {
                        Some(Message::KeyboardNav(keyboard_nav::Action::FocusPrevious))
                    }
                    Key::Named(Named::ArrowRight)
                        if matches!(status, iced::event::Status::Ignored) =>
                    {
                        Some(Message::KeyboardNav(keyboard_nav::Action::FocusNext))
                    }
                    _ => None,
                },
                cosmic::iced::Event::Window(WindowEvent::Opened { position: _, size }) => {
                    Some(Message::Opened(size, id))
                }
                cosmic::iced::Event::Window(WindowEvent::Resized(size)) => {
                    Some(Message::Resized(size, id))
                }
                _ => None,
            }),
            keyboard_nav::subscription().map(Message::KeyboardNav),
            self.core
                .watch_config::<cosmic_app_list_config::AppListConfig>(
                    cosmic_app_list_config::APP_ID,
                )
                .map(|config| Message::AppListConfig(config.config)),
        ])
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(mut core: Core, flags: Args) -> (Self, iced::Task<cosmic::Action<Self::Message>>) {
        core.set_keyboard_nav(false);
        let helper = AppLibraryConfig::helper();

        let config: AppLibraryConfig = helper
            .as_ref()
            .map(|helper| {
                AppLibraryConfig::get_entry(helper).unwrap_or_else(|(errors, config)| {
                    for err in errors {
                        error!("{:?}", err);
                    }
                    config
                })
            })
            .unwrap_or_default();
        let scrollable_id = Id::new("app-grid");
        let self_ = Self {
            locale: std::env::var("LANG")
                .ok()
                .and_then(|l| l.split(".").next().map(str::to_string)),
            config,
            core,
            helper,
            last_hide: None,
            margin: 0.,
            width: 1920.0,
            overlap: HashMap::new(),
            height: 100.,
            scrollable_id,
            ..Default::default()
        };

        // Auto-activate when running in standalone mode
        let task = if matches!(flags.subcommand, Some(ApplicationsTasks::Run)) {
            Task::done(cosmic::Action::App(Message::Activate))
        } else {
            Task::none()
        };

        (self_, task)
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_GRID_COLUMNS, grid_columns_for_width, scroll_ratio};

    #[test]
    fn grid_columns_remain_seven_on_wide_outputs() {
        assert_eq!(
            grid_columns_for_width(1920.0, 256.0, 16.0),
            MAX_GRID_COLUMNS
        );
    }

    #[test]
    fn grid_columns_shrink_on_narrow_outputs() {
        assert!(grid_columns_for_width(800.0, 256.0, 16.0) < MAX_GRID_COLUMNS);
        assert_eq!(grid_columns_for_width(100.0, 256.0, 16.0), 1);
    }

    #[test]
    fn scroll_ratio_reaches_the_end_of_the_grid() {
        // 18 apps over 3 columns is exactly 6 rows; the last row must scroll
        // all the way down instead of stopping at 5/6.
        assert_eq!(scroll_ratio(17, 18, 3), 1.0);
        assert_eq!(scroll_ratio(0, 18, 3), 0.0);
        assert_eq!(scroll_ratio(9, 18, 3), 0.6);
    }

    #[test]
    fn scroll_ratio_handles_degenerate_grids() {
        assert_eq!(scroll_ratio(0, 0, 7), 0.0);
        assert_eq!(scroll_ratio(3, 4, 7), 0.0);
        assert_eq!(scroll_ratio(5, 6, 0), 1.0);
    }
}
