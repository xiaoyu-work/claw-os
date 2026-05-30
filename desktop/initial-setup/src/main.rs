// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use std::any::TypeId;
use std::path::Path;

use cosmic::app::{Core, Settings, Task};
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

/// Runs application with these settings
#[rustfmt::skip]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Some(file_path) = option_env!("DISABLE_IF_EXISTS")
        && Path::new(file_path).exists() {
            return Ok(());
        }

    #[allow(deprecated)]
    let home_dir = std::env::home_dir().unwrap();

    if home_dir.join(COSMIC_SETUP_DONE_PATH).exists() {
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
    PageMessage(page::Message),
    PageOpen(usize),
}

/// The [`App`] stores application-specific state.
pub struct App {
    core: Core,
    pages: IndexMap<TypeId, Box<dyn Page + 'static>>,
    page_i: usize,
    oem_mode: bool,
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

        let mut app = App {
            core,
            oem_mode: matches!(mode, page::AppMode::NewInstall { create_user: true }),
            pages: page::pages(mode),
            page_i: 0,
            wifi_exists: true, // TODO: Detect
            wallpaper: WIZARD_WALLPAPER_PATHS
                .iter()
                .map(Path::new)
                .find(|p| p.exists())
                .map(widget::image::Handle::from_path),
        };

        let tasks = app
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

                page::Message::User(message) => {
                    if let Some(page) = self.pages.get_mut(&TypeId::of::<page::user::Page>()) {
                        return page
                            .as_any()
                            .downcast_mut::<page::user::Page>()
                            .unwrap()
                            .update(message)
                            .map(Message::PageMessage)
                            .map(cosmic::Action::App);
                    }
                }

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
                if let Some((_, page)) = self.pages.get_index_mut(page_i) {
                    self.page_i = page_i;
                    return page
                        .open()
                        .map(Message::PageMessage)
                        .map(cosmic::Action::App);
                }
            }

            Message::Finish => {
                let mark_initial_setup_finish = cosmic::Task::future(async {
                    #[allow(deprecated)]
                    let home = std::env::home_dir().unwrap();
                    let marker = home.join(COSMIC_SETUP_DONE_PATH);
                    let marker_str = marker.to_string_lossy().into_owned();
                    let bridge_res = tokio::task::spawn_blocking(move || {
                        cos_runtime::fs::write(&marker_str, "")
                    })
                    .await;
                    if let Ok(Err(why)) = bridge_res {
                        if why.is_denied() {
                            tracing::warn!(?why, "fs.write setup-done marker denied by claw-os-sdk");
                        } else {
                            tracing::error!(?why, "fs.write setup-done marker failed");
                        }
                    }
                })
                .discard();

                let mut tasks = self
                    .pages
                    .values_mut()
                    .filter_map(|page| {
                        page.completed().then(|| {
                            page.apply_settings()
                                .map(Message::PageMessage)
                                .map(cosmic::Action::App)
                        })
                    })
                    .chain(std::iter::once(mark_initial_setup_finish))
                    .collect::<Vec<_>>()
                    .apply(Task::batch);

                if self.oem_mode {
                    // Automatically log out from the OEM mode after tasks are finished.
                    tasks = tasks.chain(
                        cosmic::Task::future(async {
                            let bridge_res = tokio::task::spawn_blocking(|| {
                                cos_runtime::exec::run(
                                    &["loginctl", "terminate-user", "cosmic-initial-setup"],
                                    None,
                                )
                            })
                            .await;
                            if let Ok(Err(why)) = bridge_res {
                                if why.is_denied() {
                                    tracing::warn!(
                                        ?why,
                                        "exec.run loginctl terminate-user denied by claw-os-sdk"
                                    );
                                } else {
                                    tracing::error!(?why, "exec.run loginctl terminate-user failed");
                                }
                            }
                        })
                        .discard(),
                    );
                }

                return tasks.chain(cosmic::Task::done(Message::Exit.into()));
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
                page.skippable().then(|| {
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
                if page.completed() {
                    next = next.on_press(Message::PageOpen(page_i));
                }
                button_row = button_row.push(next);
            } else {
                let mut finish = widget::button::suggested(fl!("finish"));
                if page.completed() {
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
            .push(widget::space::vertical().height(space_m))
            .push(button_row)
            .push(widget::space::vertical().height(space_l))
            .max_width(page.width())
            .width(page.width())
            .align_x(Alignment::Center)
            .apply(widget::container)
            .padding([0, space_xl])
            .class(theme::Container::custom(|theme| {
                let cosmic = theme.cosmic();
                // base = solid bg color; we knock the alpha down to ~55% so
                // the wallpaper bleeds through while widgets inside still
                // read clearly. Surface seeds are already blue-tinted glass.
                // The hairline is brand-blue (accent @ 0.20) instead of a
                // neutral divider so the whole panel reads as Claw Glass, and
                // a soft drop shadow keeps it from melting into the photo.
                let mut bg: Color = cosmic.background.base.into();
                bg.a = 0.55;
                let border_color: Color = cosmic.accent_color().with_alpha(0.20).into();
                container::Style {
                    text_color: Some(cosmic.background.on.into()),
                    icon_color: Some(cosmic.background.on.into()),
                    background: Some(Background::Color(bg)),
                    border: Border {
                        radius: cosmic.radius_l().into(),
                        width: 1.0,
                        color: border_color,
                    },
                    shadow: Shadow {
                        color: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.25 },
                        offset: cosmic::iced::Vector { x: 0.0, y: 6.0 },
                        blur_radius: 24.0,
                    },
                    snap: true,
                }
            }));

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
