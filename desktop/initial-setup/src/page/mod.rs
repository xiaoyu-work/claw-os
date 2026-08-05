use cosmic::iced::Subscription;
use cosmic::{Element, widget};
use indexmap::IndexMap;
use std::any::{Any, TypeId};

pub mod a11y;
pub mod ai;
pub mod appearance;
pub mod drivers;
pub mod keyboard;
pub mod language;
pub mod launcher;
pub mod layout;
pub mod location;
pub mod new_apps;
pub mod new_shortcuts;
pub mod selection;
pub mod user;
pub mod wifi;
pub mod workflow;

pub enum AppMode {
    NewInstall {
        create_user: bool,
    },
    /// Transitioned from GNOME.
    GnomeTransition,
}

#[inline]
pub fn pages(mode: AppMode) -> IndexMap<TypeId, Box<dyn Page>> {
    let mut pages: IndexMap<TypeId, Box<dyn Page>> = IndexMap::new();
    pages.insert(TypeId::of::<a11y::Page>(), Box::new(a11y::Page::new()));

    if let AppMode::NewInstall { create_user } = mode {
        pages.insert(TypeId::of::<wifi::Page>(), Box::new(wifi::Page::default()));

        #[cfg(not(feature = "nixos"))]
        pages.insert(
            TypeId::of::<language::Page>(),
            Box::new(language::Page::new()),
        );

        pages.insert(
            TypeId::of::<keyboard::Page>(),
            Box::new(keyboard::Page::new()),
        );

        // AI page sits after keyboard so the user picks input
        // language/layout first, then the LLM. NewInstall only —
        // GnomeTransition assumes the existing config is keepable.
        pages.insert(TypeId::of::<ai::Page>(), Box::new(ai::Page::new()));

        if create_user {
            pages.insert(TypeId::of::<user::Page>(), Box::new(user::Page::default()));
        }

        #[cfg(not(feature = "nixos"))]
        pages.insert(
            TypeId::of::<location::Page>(),
            Box::new(location::Page::new()),
        );
    }

    pages.insert(
        TypeId::of::<appearance::Page>(),
        Box::new(appearance::Page::new()),
    );

    pages.insert(
        TypeId::of::<layout::Page>(),
        Box::new(layout::Page::default()),
    );

    if matches!(mode, AppMode::GnomeTransition) {
        pages.insert(
            TypeId::of::<new_apps::Page>(),
            Box::new(new_apps::Page::default()),
        );
    }

    pages.insert(
        TypeId::of::<workflow::Page>(),
        Box::new(workflow::Page::default()),
    );

    // if matches!(mode, AppMode::GnomeTransition) {
    pages.insert(
        TypeId::of::<new_shortcuts::Page>(),
        Box::new(new_shortcuts::Page::default()),
    );
    // } else {
    pages.insert(
        TypeId::of::<launcher::Page>(),
        Box::new(launcher::Page::new()),
    );
    // }

    // Drivers is the final step: ClawOS ships open drivers for everything, so
    // this page only does real work on a bare-metal NVIDIA machine (offers the
    // proprietary driver). NewInstall only — a Gnome transition keeps the
    // existing driver setup.
    if matches!(mode, AppMode::NewInstall { .. }) {
        pages.insert(
            TypeId::of::<drivers::Page>(),
            Box::new(drivers::Page::new()),
        );
    }

    pages
}

#[derive(Clone, Debug)]
pub enum Message {
    A11y(a11y::Message),
    Ai(ai::Message),
    Appearance(appearance::Message),
    Drivers(drivers::Message),
    Keyboard(keyboard::Message),
    Language(language::Message),
    Layout(layout::Message),
    Location(location::Message),
    SetTheme(cosmic::Theme),
    User(user::Message),
    WiFi(wifi::Message),
}

impl From<Message> for super::Message {
    fn from(message: Message) -> Self {
        super::Message::PageMessage(message)
    }
}

pub trait Page {
    fn as_any(&mut self) -> &mut dyn Any;

    fn title(&self) -> String;

    fn init(&mut self) -> cosmic::Task<Message> {
        cosmic::Task::none()
    }

    fn apply_settings(&mut self) -> cosmic::Task<Message> {
        cosmic::Task::none()
    }

    fn open(&mut self) -> cosmic::Task<Message> {
        cosmic::Task::none()
    }

    fn width(&self) -> f32 {
        640.0
    }

    fn completed(&self) -> bool {
        true
    }

    fn optional(&self) -> bool {
        false
    }

    fn skippable(&self) -> bool {
        false
    }

    fn dialog(&self) -> Option<Element<'_, Message>> {
        None
    }

    fn view(&self) -> Element<'_, Message> {
        widget::text::body("TODO").into()
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }
}
