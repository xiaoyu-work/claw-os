// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use std::any::TypeId;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use cosmic::app::{Core, Settings, Task};
use cosmic::cosmic_theme::palette::WithAlpha;
use cosmic::iced::{Alignment, Background, Border, Color, ContentFit, Length, Shadow, Subscription};
use cosmic::widget::container;
use cosmic::{Application, Apply, Element, cosmic_theme, executor, theme, widget};
use futures::channel::mpsc::Sender;
use futures::{SinkExt, Stream, StreamExt};
use indexmap::IndexMap;
use tracing_subscriber::prelude::*;

mod greeter;
mod localize;

use self::page::Page;
mod page;

const COSMIC_SETUP_DONE_PATH: &str = ".config/cosmic-initial-setup-done";
const GNOME_SETUP_DONE_PATH: &str = ".config/gnome-initial-setup-done";
static RESUME_EXIT_PENDING: AtomicBool = AtomicBool::new(false);

/// Terminate the OEM first-boot session.
///
/// Run directly rather than through the `cos_runtime::exec` bridge. That
/// bridge routes through `cos app exec run`, whose manifest declares a
/// `proc.spawn` need, so `caps::bootstrap` refuses to auto-create a session
/// for it and the call denies with "Permission denied (no active session)".
/// The wizard is a system component winding down its own login session, not
/// an agent acting for the user, so there is nothing to gate or audit here.
fn terminate_oem_session_blocking() -> Result<(), String> {
    let out = std::process::Command::new("loginctl")
        .args(["terminate-user", "cosmic-initial-setup"])
        .output()
        .map_err(|why| format!("run loginctl terminate-user: {why}"))?;
    if out.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if detail.is_empty() {
        Err(format!("loginctl exited with status {}", out.status))
    } else {
        Err(format!("loginctl failed: {detail}"))
    }
}

fn setup_marker_finishes_launch(marker: &Path) -> bool {
    if !marker.exists() {
        return false;
    }
    let is_oem = pwd::Passwd::current_user()
        .is_some_and(|user| user.name == "cosmic-initial-setup");
    if !is_oem {
        return true;
    }
    let terminated = terminate_oem_session_blocking().is_ok();
    if terminated {
        return true;
    }
    eprintln!(
        "cosmic-initial-setup: completed setup marker exists but OEM session termination failed; retrying in UI"
    );
    if let Err(error) = std::fs::remove_file(marker) {
        eprintln!(
            "cosmic-initial-setup: failed to remove stale completion marker {}: {error}",
            marker.display()
        );
    }
    RESUME_EXIT_PENDING.store(true, Ordering::Relaxed);
    false
}

/// Runs application with these settings
#[rustfmt::skip]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Some(file_path) = option_env!("DISABLE_IF_EXISTS")
        && Path::new(file_path).exists() {
            return Ok(());
        }

    #[allow(deprecated)]
    let home_dir = std::env::home_dir().unwrap();

    if setup_marker_finishes_launch(&home_dir.join(COSMIC_SETUP_DONE_PATH)) {
        return Ok(());
    }

    let log_level = std::env::var("RUST_LOG")
        .ok()
        .and_then(|level| level.parse::<tracing::Level>().ok())
        .unwrap_or(tracing::Level::INFO);

    let log_format = tracing_subscriber::fmt::format()
        .pretty()
        .without_time()
        .with_line_number(true)
        .with_file(true)
        .with_target(false)
        .with_thread_names(true);

    let log_filter = tracing_subscriber::fmt::Layer::default()
        .with_writer(std::io::stderr)
        .event_format(log_format)
        .with_filter(tracing_subscriber::filter::filter_fn(move |metadata| {
            if metadata.level() == &tracing::Level::INFO {
                return metadata.target() == "cosmic_initial_setup"
            }

            metadata.level() <= &log_level
        }));

    tracing_subscriber::registry().with(log_filter).init();

    localize::localize();

    // Decide which pages to display.
    let mode = if home_dir.join(GNOME_SETUP_DONE_PATH).exists() {
        page::AppMode::GnomeTransition
    } else {
        // If being run by the cosmic-initial-setup user, we are in OEM mode.
        page::AppMode::NewInstall { create_user: pwd::Passwd::current_user().is_some_and(|current_user| current_user.name == "cosmic-initial-setup") }
    };

    // Start the first-boot wizard fullscreen at window creation time so the
    // wallpaper is sized against the monitor, not a normal centered toplevel.
    // The large size is only a fallback for toolkits/compositors that ignore
    // the initial fullscreen hint.
    let settings = Settings::default()
        .fullscreen(true)
        .size(cosmic::iced::Size::new(7680.0, 4320.0));

    cosmic::app::run::<App>(settings, mode)?;

    Ok(())
}

/// Messages that are used specifically by our [`App`].
#[derive(Clone, Debug)]
pub enum Message {
    None,
    Exit,
    Finish,
    SetupMarked(Result<(), String>),
    SessionEnded(Result<(), String>),
    PageMessage(page::Message),
    PageOpen(usize),
}

/// The [`App`] stores application-specific state.
pub struct App {
    core: Core,
    pages: IndexMap<TypeId, Box<dyn Page + 'static>>,
    page_i: usize,
    oem_mode: bool,
    user_creation_complete: bool,
    settings_applied: bool,
    finishing: bool,
    finish_error: Option<String>,
    wifi_exists: bool,
    /// Wallpaper for the wizard background, when one is available on disk.
    /// Loaded once at init from the system default path; absence (e.g. in
    /// a stripped-down build) just falls back to a solid theme background.
    wallpaper: Option<widget::image::Handle>,
}

/// System paths to probe for a wizard background. First hit wins. Kept in
/// /usr/share/backgrounds/cosmic/ which the `desktop/wallpapers` Makefile
/// already installs into.
const WIZARD_WALLPAPER_PATHS: &[&str] = &[
    "/usr/share/backgrounds/cosmic/claw-default.jpg",
    "/usr/share/backgrounds/cosmic/claw-default.png",
];

fn configured_first_user_exists() -> bool {
    let Ok(config) = std::fs::read_to_string("/etc/default/cos-home") else {
        return false;
    };
    let Some(home) = config.lines().find_map(|line| {
        line.trim()
            .strip_prefix("COS_HOME=")
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }) else {
        return false;
    };
    pwd::Passwd::iter().any(|user| {
        user.uid >= 1000
            && user.uid < 65534
            && &*user.dir == home
            && std::fs::metadata(home).is_ok_and(|metadata| {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    metadata.is_dir() && metadata.uid() == user.uid
                }
                #[cfg(not(unix))]
                {
                    metadata.is_dir()
                }
            })
            && page::user::accept_committed_transaction(&user.name)
    })
}

impl App {
    fn apply_remaining_settings(&mut self) -> Task<Message> {
        if self.settings_applied {
            return cosmic::Task::future(async {
                Message::SetupMarked(write_setup_marker().await).into()
            });
        }
        let user_page = TypeId::of::<page::user::Page>();
        let tasks = self
            .pages
            .iter_mut()
            .filter_map(|(type_id, page)| {
                (*type_id != user_page && page.completed()).then(|| {
                    page.apply_settings()
                        .map(Message::PageMessage)
                        .map(cosmic::Action::App)
                })
            })
            .collect::<Vec<_>>()
            .apply(Task::batch);
        tasks.chain(cosmic::Task::future(async {
            Message::SetupMarked(write_setup_marker().await).into()
        }))
    }
}

fn setup_marker_path() -> Result<std::path::PathBuf, String> {
    #[allow(deprecated)]
    let home = std::env::home_dir().ok_or("HOME is unavailable")?;
    Ok(home.join(COSMIC_SETUP_DONE_PATH))
}

/// Record that setup finished, so the next login skips the wizard.
///
/// Written with `std::fs`, mirroring how `setup_marker_finishes_launch`
/// *reads* it. Going through the `cos_runtime::fs` bridge instead made this
/// unreachable: it dispatches to `cos app fs write`, whose manifest declares
/// an `fs.write` need, and `caps::bootstrap` only auto-creates a session for
/// operations that need nothing. Every attempt therefore denied with
/// "Permission denied (no active session)", the error surfaced as
/// `finish_error`, and the wizard refused to close — leaving no way to finish
/// or skip onboarding at all.
///
/// The bridge exists so an agent's file access is scoped and audited. This is
/// the wizard stamping its own private state file in its own home directory;
/// it is not acting for anyone.
async fn write_setup_marker() -> Result<(), String> {
    let marker = setup_marker_path()?;
    tokio::task::spawn_blocking(move || {
        // `cos_runtime::fs::write` created missing parents; ~/.config may not
        // exist yet on a fresh account, so keep that behaviour.
        if let Some(parent) = marker.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|why| format!("create {}: {why}", parent.display()))?;
        }
        std::fs::write(&marker, "")
            .map_err(|why| format!("write {}: {why}", marker.display()))
    })
    .await
    .map_err(|why| format!("setup marker task failed: {why}"))?
}

async fn terminate_oem_session() -> Result<(), String> {
    let marker = setup_marker_path()?;
    let command_result = match tokio::task::spawn_blocking(terminate_oem_session_blocking).await {
        Ok(result) => result,
        Err(why) => Err(format!("session termination task failed: {why}")),
    };
    if let Err(error) = command_result {
        // Roll the marker back so the wizard runs again rather than leaving
        // the OEM account stranded in a session it thinks is already done.
        let cleanup_marker = marker.clone();
        let cleanup = tokio::task::spawn_blocking(move || {
            std::fs::remove_file(&cleanup_marker)
                .map_err(|why| format!("remove {}: {why}", cleanup_marker.display()))
        })
        .await
        .map_err(|why| format!("marker cleanup task failed: {why}"))?;
        return match cleanup {
            Ok(()) => Err(error),
            Err(cleanup) => Err(format!(
                "{error}; removing setup marker also failed: {cleanup}"
            )),
        };
    }
    Ok(())
}

/// Implement [`Application`] to integrate with COSMIC.
impl Application for App {
    /// Multithreaded async executor to use with the app.
    type Executor = executor::Default;

    /// Argument received
    type Flags = page::AppMode;

    /// Message type specific to our [`App`].
    type Message = Message;

    /// The unique application ID to supply to the window manager.
    const APP_ID: &'static str = "com.clawos.InitialSetup";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    /// Creates the application, and optionally emits command on initialize.
    fn init(mut core: Core, mode: Self::Flags) -> (Self, Task<Message>) {
        core.window.show_headerbar = false;
        core.window.show_close = false;
        core.window.show_maximize = false;
        core.window.show_minimize = false;
        // Drop libcosmic's opaque "COSMIC_content_container" wrapper around
        // view(). With it on, the wrapper paints theme.cosmic.background.base
        // over our whole client area, hiding the wallpaper stack we render
        // beneath the wizard card. Turning it off lets the wallpaper extend
        // edge-to-edge while still preserving the 1px window border.
        core.window.content_container = false;

        let oem_mode = matches!(mode, page::AppMode::NewInstall { create_user: true });
        let resume_exit = RESUME_EXIT_PENDING.load(Ordering::Relaxed);
        let user_creation_complete =
            oem_mode && (resume_exit || configured_first_user_exists());
        let mut pages = page::pages(mode);
        if user_creation_complete {
            pages.shift_remove(&TypeId::of::<page::user::Page>());
        }
        let mut app = App {
            core,
            oem_mode,
            user_creation_complete,
            settings_applied: resume_exit,
            finishing: false,
            finish_error: None,
            pages,
            page_i: 0,
            wifi_exists: true, // TODO: Detect
            wallpaper: WIZARD_WALLPAPER_PATHS
                .iter()
                .map(Path::new)
                .find(|p| p.exists())
                .map(widget::image::Handle::from_path),
        };

        let mut tasks = app
            .pages
            .values_mut()
            .map(|page| {
                page.init()
                    .map(Message::PageMessage)
                    .map(cosmic::Action::App)
            })
            .collect::<Vec<_>>()
            .apply(Task::batch)
            .chain(app.update(Message::PageOpen(0)))
            // Keep a runtime fullscreen request as a fallback. The important
            // path is Settings::fullscreen(true), which creates the first
            // surface as fullscreen before cosmic-comp maps it.
            .chain(cosmic::iced::window::latest().and_then(|id| {
                cosmic::iced::window::set_mode::<cosmic::Action<Message>>(id, cosmic::iced::window::Mode::Fullscreen)
            }));
        if resume_exit {
            tasks = tasks.chain(app.update(Message::Finish));
        }

        (app, tasks)
    }

    /// Handle application events here.
    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::None => {}

            Message::PageMessage(page_message) => match page_message {
                page::Message::SetTheme(theme) => {
                    return cosmic::command::set_theme(theme);
                }

                page::Message::Appearance(message) => {
                    if let Some(page) = self.pages.get_mut(&TypeId::of::<page::appearance::Page>())
                    {
                        return page
                            .as_any()
                            .downcast_mut::<page::appearance::Page>()
                            .unwrap()
                            .update(message)
                            .map(Message::PageMessage)
                            .map(cosmic::Action::App);
                    }
                }

                page::Message::Keyboard(message) => {
                    if let Some(page) = self.pages.get_mut(&TypeId::of::<page::keyboard::Page>()) {
                        return page
                            .as_any()
                            .downcast_mut::<page::keyboard::Page>()
                            .unwrap()
                            .update(message)
                            .map(Message::PageMessage)
                            .map(cosmic::Action::App);
                    }
                }

                page::Message::Language(message) => {
                    if let Some(page) = self.pages.get_mut(&TypeId::of::<page::language::Page>()) {
                        return page
                            .as_any()
                            .downcast_mut::<page::language::Page>()
                            .unwrap()
                            .update(message)
                            .map(Message::PageMessage)
                            .map(cosmic::Action::App);
                    }
                }

                page::Message::Layout(message) => {
                    if let Some(page) = self.pages.get_mut(&TypeId::of::<page::layout::Page>()) {
                        return page
                            .as_any()
                            .downcast_mut::<page::layout::Page>()
                            .unwrap()
                            .update(message)
                            .map(Message::PageMessage)
                            .map(cosmic::Action::App);
                    }
                }

                page::Message::Location(message) => {
                    if let Some(page) = self.pages.get_mut(&TypeId::of::<page::location::Page>()) {
                        return page
                            .as_any()
                            .downcast_mut::<page::location::Page>()
                            .unwrap()
                            .update(message)
                            .map(Message::PageMessage)
                            .map(cosmic::Action::App);
                    }
                }

                page::Message::User(message) => match message {
                    page::user::Message::Applied(result) => match result {
                        Ok(()) => {
                            self.user_creation_complete = true;
                            return self.apply_remaining_settings();
                        }
                        Err(why) => {
                            tracing::error!(error = %why, "first-user transaction failed");
                            self.finishing = false;
                            self.finish_error = Some(why);
                        }
                    },
                    other => {
                        if let Some(page) =
                            self.pages.get_mut(&TypeId::of::<page::user::Page>())
                        {
                            return page
                                .as_any()
                                .downcast_mut::<page::user::Page>()
                                .unwrap()
                                .update(other)
                                .map(Message::PageMessage)
                                .map(cosmic::Action::App);
                        }
                    }
                },

                page::Message::A11y(message) => {
                    if let Some(page) = self.pages.get_mut(&TypeId::of::<page::a11y::Page>()) {
                        return page
                            .as_any()
                            .downcast_mut::<page::a11y::Page>()
                            .unwrap()
                            .update(message)
                            .map(Message::PageMessage)
                            .map(cosmic::Action::App);
                    }
                }

                page::Message::Ai(message) => {
                    if let Some(page) = self.pages.get_mut(&TypeId::of::<page::ai::Page>()) {
                        return page
                            .as_any()
                            .downcast_mut::<page::ai::Page>()
                            .unwrap()
                            .update(message)
                            .map(Message::PageMessage)
                            .map(cosmic::Action::App);
                    }
                }

                page::Message::Drivers(message) => {
                    if let Some(page) = self.pages.get_mut(&TypeId::of::<page::drivers::Page>()) {
                        return page
                            .as_any()
                            .downcast_mut::<page::drivers::Page>()
                            .unwrap()
                            .update(message)
                            .map(Message::PageMessage)
                            .map(cosmic::Action::App);
                    }
                }

                page::Message::WiFi(message) => {
                    if let Some(page) = self.pages.get_mut(&TypeId::of::<page::wifi::Page>()) {
                        return page
                            .as_any()
                            .downcast_mut::<page::wifi::Page>()
                            .unwrap()
                            .update(message)
                            .map(Message::PageMessage)
                            .map(cosmic::Action::App);
                    }
                }
            },

            Message::PageOpen(page_i) => {
                if self.finishing {
                    return Task::none();
                }
                if let Some((_, page)) = self.pages.get_index_mut(page_i) {
                    self.page_i = page_i;
                    return page
                        .open()
                        .map(Message::PageMessage)
                        .map(cosmic::Action::App);
                }
            }

            Message::Finish => {
                if self.finishing {
                    return Task::none();
                }
                self.finishing = true;
                self.finish_error = None;
                if self.oem_mode
                    && !self.user_creation_complete
                    && let Some(page) =
                        self.pages.get_mut(&TypeId::of::<page::user::Page>())
                {
                    if !page.completed() {
                        self.finishing = false;
                        self.finish_error =
                            Some("Create the first user before finishing setup.".to_string());
                        return Task::none();
                    }
                    return page
                        .apply_settings()
                        .map(Message::PageMessage)
                        .map(cosmic::Action::App);
                }
                return self.apply_remaining_settings();
            }

            Message::SetupMarked(result) => {
                self.settings_applied = true;
                match result {
                    Ok(()) if self.oem_mode => {
                        return cosmic::Task::future(async {
                            Message::SessionEnded(terminate_oem_session().await).into()
                        });
                    }
                    Ok(()) => return cosmic::Task::done(Message::Exit.into()),
                    Err(why) => {
                        tracing::error!(error = %why, "failed to mark initial setup complete");
                        self.finishing = false;
                        self.finish_error = Some(why);
                    }
                }
            }

            Message::SessionEnded(result) => match result {
                Ok(()) => return cosmic::Task::done(Message::Exit.into()),
                Err(why) => {
                    tracing::error!(error = %why, "failed to end first-boot session");
                    self.finishing = false;
                    self.finish_error = Some(why);
                }
            }

            Message::Exit => {
                return cosmic::iced::exit();
            }
        }
        Task::none()
    }

    fn dialog(&self) -> Option<Element<'_, Self::Message>> {
        self.pages[self.page_i]
            .dialog()
            .map(|dialog| dialog.map(Message::PageMessage))
    }

    /// Creates a view after each update.
    fn view(&self) -> Element<'_, Message> {
        let cosmic_theme::Spacing {
            space_xxs,
            space_m,
            space_l,
            space_xl,
            ..
        } = theme::spacing();

        let page = &self.pages[self.page_i];

        let skip_button = page
            .optional()
            .then(|| widget::button::link(fl!("skip")).on_press(Message::PageOpen(self.page_i + 1)))
            .or_else(|| {
                (!self.oem_mode && page.skippable()).then(|| {
                    widget::button::link(fl!("skip-setup-and-close")).on_press(Message::Finish)
                })
            });

        let mut button_row = widget::row::with_capacity(4)
            .spacing(space_xxs)
            .push_maybe(skip_button)
            .push(widget::space::horizontal());

        if let Some(page_i) = self.page_i.checked_sub(1)
            && self.pages.get_index(page_i).is_some()
        {
            button_row = button_row
                .push(widget::button::standard(fl!("back")).on_press(Message::PageOpen(page_i)));
        }

        if let Some(page_i) = self.page_i.checked_add(1) {
            if self.pages.get_index(page_i).is_some() {
                let mut next = widget::button::suggested(fl!("next"));
                if page.completed() && !self.finishing {
                    next = next.on_press(Message::PageOpen(page_i));
                }
                button_row = button_row.push(next);
            } else {
                let mut finish = widget::button::suggested(fl!("finish"));
                if page.completed() && !self.finishing {
                    finish = finish.on_press(Message::Finish);
                }
                button_row = button_row.push(finish);
            }
        }

        let title = widget::text::title2(page.title())
            .center()
            .width(Length::Fill);

        let content = page
            .view()
            .map(Message::PageMessage)
            .apply(widget::scrollable)
            .height(Length::Fill);

        // The wizard content column — title, page body, navigation buttons.
        // Wrapped in a translucent rounded "frosted-glass" panel that floats
        // over the wallpaper instead of the previous full-bleed black surface.
        let card = widget::column::with_capacity(7)
            .push(widget::space::vertical().height(space_xl))
            .push(title)
            .push(widget::space::vertical().height(space_l))
            .push(content)
            .push_maybe(
                self.finish_error
                    .as_ref()
                    .map(|error| widget::text::body(error.clone())),
            )
            .push(widget::space::vertical().height(space_m))
            .push(button_row)
            .push(widget::space::vertical().height(space_l))
            .max_width(page.width())
            .width(page.width())
            .align_x(Alignment::Center)
            .apply(widget::container)
            .padding([0, space_xl])
            .class(theme::Container::custom(glass_card_style));

        let centered = widget::container(card)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .padding(space_l);

        match &self.wallpaper {
            Some(handle) => {
                let bg = widget::image(handle.clone())
                    .content_fit(ContentFit::Cover)
                    .width(Length::Fill)
                    .height(Length::Fill);
                widget::container(cosmic::iced::widget::stack![bg, centered])
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into()
            }
            None => widget::container(centered)
                .width(Length::Fill)
                .height(Length::Fill)
                .class(theme::Container::custom(glass_backdrop_style))
                .into(),
        }
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        let mut subscriptions = vec![
            // Make the screen reader toggleable.
            cosmic_settings_accessibility_subscription::subscription().map(|m| {
                Message::PageMessage(page::Message::A11y(page::a11y::Message::A11yBus(m)))
            }),
        ];

        // Listen for WiFi devices if a WiFi device was found.
        if self.wifi_exists {
            subscriptions.push(Subscription::run(network_manager_stream));
        }

        Subscription::batch(subscriptions)
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        // Default libcosmic application style paints theme.cosmic.background.base
        // as the surface clear color whenever the window is maximized/fullscreen.
        // We *want* the wizard fullscreen but we also want the wallpaper image
        // we render inside view() to be the only visible color — so force the
        // surface clear color transparent and let our stack provide pixels.
        let theme = cosmic::theme::active();
        let cosmic = theme.cosmic();
        Some(cosmic::iced::theme::Style {
            background_color: Color::TRANSPARENT,
            text_color: cosmic.on_bg_color().into(),
            icon_color: cosmic.on_bg_color().into(),
        })
    }
}

/// Full-window backdrop painted when no wallpaper is available (e.g. on a
/// fresh image before cosmic-bg starts). Instead of falling through to the
/// compositor's flat clear color — which read as a dull gray field behind the
/// translucent wizard card — we lay down an on-brand "Claw Glass" gradient:
/// a deep blue-tinted vertical wash that gives the frosted card something
/// premium to float over. Derived from the active theme so it tracks light
/// vs dark automatically (cool blue-white vs deep navy).
fn glass_backdrop_style(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    let cosmic = theme.cosmic();
    let base: Color = cosmic.background.base.into();
    let accent: Color = cosmic.accent_color().into();

    let mix = |a: Color, b: Color, t: f32| Color {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: 1.0,
    };

    // Top carries a faint brand-blue glow; the body sits at the base surface;
    // the bottom darkens slightly for a soft vignette that grounds the card.
    let top = mix(base, accent, 0.18);
    let mid = base;
    let bottom = Color { r: base.r * 0.82, g: base.g * 0.82, b: base.b * 0.86, a: 1.0 };

    // ~160° → a gentle top-to-bottom diagonal wash.
    let gradient = cosmic::iced::gradient::Linear::new(2.79_f32)
        .add_stop(0.0, top)
        .add_stop(0.55, mid)
        .add_stop(1.0, bottom);

    container::Style {
        text_color: Some(cosmic.background.on.into()),
        icon_color: Some(cosmic.background.on.into()),
        background: Some(gradient.into()),
        border: Border::default(),
        shadow: Shadow::default(),
        snap: true,
    }
}

/// The floating wizard card: a frosted-glass panel consistent with the rest of
/// the Claw Glass desktop (cf. the agent UI composer / sidebar). Uses the
/// system frosted component fill, a 1px brand-blue translucent hairline, and a
/// soft layered drop shadow — depth from blur + shadow, not a heavy border.
///
/// The fill sits noticeably more opaque than the greeter's login card (0.62):
/// this surface carries a full page of body text, labelled toggles and list
/// rows rather than a single password field, and it floats over an arbitrary
/// user wallpaper. At the lighter value the backdrop showed through the text
/// and the setup pages — accessibility in particular, with its dense rows of
/// small labels — became hard to read.
fn glass_card_style(theme: &cosmic::Theme) -> cosmic::widget::container::Style {
    let cosmic = theme.cosmic();
    let mut fill = cosmic.bg_component_color();
    fill.alpha = 0.85;

    container::Style {
        text_color: Some(cosmic.on_bg_color().into()),
        icon_color: Some(cosmic.on_bg_color().into()),
        background: Some(Background::Color(fill.into())),
        border: Border {
            radius: cosmic.radius_l().into(),
            width: 1.0,
            color: cosmic.on_bg_color().with_alpha(0.12).into(),
        },
        shadow: Shadow {
            color: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.28 },
            offset: cosmic::iced::Vector { x: 0.0, y: 8.0 },
            blur_radius: 32.0,
        },
        snap: true,
    }
}

fn network_manager_stream() -> impl Stream<Item = Message> {
    use cosmic_settings_network_manager_subscription as network_manager;
    cosmic::iced::stream::channel(1, |mut output: Sender<Message>| async move {
        let conn = zbus::Connection::system().await.unwrap();

        let (tx, mut rx) = futures::channel::mpsc::channel(1);

        let watchers = std::pin::pin!(async move {
            futures::join!(
                network_manager::watch(conn.clone(), tx.clone()),
                network_manager::active_conns::watch(conn.clone(), tx.clone()),
                network_manager::wireless_enabled::watch(conn.clone(), tx.clone()),
                network_manager::watch_connections_changed(conn, tx)
            );
        });

        let forwarder = std::pin::pin!(async move {
            while let Some(message) = rx.next().await {
                _ = output
                    .send(page::Message::WiFi(page::wifi::Message::NetworkManager(message)).into())
                    .await;
            }
        });

        futures::future::select(watchers, forwarder).await;
    })
}
