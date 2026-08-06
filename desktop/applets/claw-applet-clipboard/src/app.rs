// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    copyq::{self, ClipboardEntry},
    fl,
};
use claw_applet_widget_rail::policy::{self, Scope};
use cosmic::{
    Element, Task, app,
    applet::padded_control,
    cosmic_theme::{Spacing, palette::WithAlpha},
    iced::{
        Alignment, Background, Border, Color, Length, Shadow, Vector,
        platform_specific::shell::wayland::commands::popup::{destroy_popup, get_popup},
        widget::{column, container, row, scrollable},
        window,
    },
    surface, theme,
    widget::{button, divider, icon, space, text},
};
use jiff::{Timestamp, civil::DateTime, tz::TimeZone};

#[derive(Clone)]
enum HistoryState {
    Loading,
    Ready(Vec<ClipboardEntry>),
    Unavailable(String),
}

pub struct ClipboardApplet {
    core: cosmic::app::Core,
    popup: Option<window::Id>,
    history: HistoryState,
    confirm_clear: bool,
    notice: Option<String>,
    refresh_generation: u64,
    busy: bool,
}

#[derive(Debug, Clone)]
pub enum Message {
    TogglePopup,
    CloseRequested(window::Id),
    Refresh,
    Loaded(u64, Result<Vec<ClipboardEntry>, String>),
    Restore(String),
    Delete(String),
    AskClear,
    CancelClear,
    ConfirmClear,
    ActionFinished(u64, Action, Result<(), String>),
    Surface(surface::Action),
}

#[derive(Debug, Clone, Copy)]
pub enum Action {
    Restore,
    Delete,
    Clear,
}

impl cosmic::Application for ClipboardApplet {
    type Message = Message;
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();

    const APP_ID: &'static str = "com.clawos.AppletClipboard";

    fn init(core: cosmic::app::Core, _: ()) -> (Self, app::Task<Message>) {
        (
            Self {
                core,
                popup: None,
                history: HistoryState::Ready(Vec::new()),
                confirm_clear: false,
                notice: None,
                refresh_generation: 0,
                busy: false,
            },
            Task::none(),
        )
    }

    fn core(&self) -> &cosmic::app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::app::Core {
        &mut self.core
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }

    fn update(&mut self, message: Message) -> app::Task<Message> {
        match message {
            Message::TogglePopup => {
                if let Some(id) = self.popup.take() {
                    self.clear_popup_state();
                    destroy_popup(id)
                } else {
                    if self.busy {
                        return Task::none();
                    }
                    self.history = HistoryState::Loading;
                    self.notice = None;
                    self.busy = true;
                    self.refresh_generation = self.refresh_generation.wrapping_add(1);
                    let id = window::Id::unique();
                    self.popup = Some(id);
                    let settings = self.core.applet.get_popup_settings(
                        self.core.main_window_id().expect("panel window"),
                        id,
                        Some((380, 600)),
                        None,
                        None,
                    );
                    Task::batch([get_popup(settings), refresh(self.refresh_generation)])
                }
            }
            Message::CloseRequested(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                    self.clear_popup_state();
                }
                Task::none()
            }
            Message::Refresh => {
                if self.busy || self.popup.is_none() {
                    return Task::none();
                }
                self.history = HistoryState::Loading;
                self.confirm_clear = false;
                self.notice = None;
                self.busy = true;
                self.refresh_generation = self.refresh_generation.wrapping_add(1);
                refresh(self.refresh_generation)
            }
            Message::Loaded(generation, result) => {
                if generation != self.refresh_generation {
                    return Task::none();
                }
                self.busy = false;
                if self.popup.is_none() {
                    self.refresh_generation = self.refresh_generation.wrapping_add(1);
                    return Task::none();
                }
                self.history = match result {
                    Ok(entries) => HistoryState::Ready(entries),
                    Err(error) => HistoryState::Unavailable(short_error(&error)),
                };
                Task::none()
            }
            Message::Restore(identity) => {
                if self.busy || self.popup.is_none() {
                    return Task::none();
                }
                self.busy = true;
                self.notice = None;
                self.refresh_generation = self.refresh_generation.wrapping_add(1);
                action(self.refresh_generation, Action::Restore, restore(identity))
            }
            Message::Delete(identity) => {
                if self.busy || self.popup.is_none() {
                    return Task::none();
                }
                self.busy = true;
                self.notice = None;
                self.refresh_generation = self.refresh_generation.wrapping_add(1);
                action(self.refresh_generation, Action::Delete, remove(identity))
            }
            Message::AskClear => {
                if !self.busy {
                    self.confirm_clear = true;
                }
                Task::none()
            }
            Message::CancelClear => {
                if !self.busy {
                    self.confirm_clear = false;
                }
                Task::none()
            }
            Message::ConfirmClear => {
                if self.busy || self.popup.is_none() {
                    return Task::none();
                }
                self.confirm_clear = false;
                self.busy = true;
                self.notice = None;
                self.refresh_generation = self.refresh_generation.wrapping_add(1);
                action(self.refresh_generation, Action::Clear, clear())
            }
            Message::ActionFinished(generation, kind, result) => {
                if generation != self.refresh_generation {
                    return Task::none();
                }
                if self.popup.is_none() {
                    self.busy = false;
                    self.refresh_generation = self.refresh_generation.wrapping_add(1);
                    return Task::none();
                }
                match result {
                    Ok(()) => {
                        if matches!(kind, Action::Restore) {
                            self.busy = false;
                            self.notice = Some(fl!("restored"));
                            Task::none()
                        } else {
                            self.history = HistoryState::Loading;
                            self.refresh_generation = self.refresh_generation.wrapping_add(1);
                            refresh(self.refresh_generation)
                        }
                    }
                    Err(error) => {
                        self.busy = false;
                        self.notice =
                            Some(format!("{}: {}", fl!("action-failed"), short_error(&error)));
                        Task::none()
                    }
                }
            }
            Message::Surface(action) => {
                cosmic::task::message(cosmic::Action::Cosmic(cosmic::app::Action::Surface(action)))
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let button = self
            .core
            .applet
            .icon_button("edit-paste-symbolic")
            .on_press_down(Message::TogglePopup);
        self.core
            .applet
            .applet_tooltip(
                button,
                format!("{} · CopyQ", fl!("tooltip")),
                self.popup.is_some(),
                Message::Surface,
                None,
            )
            .into()
    }

    fn view_window(&self, _id: window::Id) -> Element<'_, Message> {
        let Spacing {
            space_xxs,
            space_xs,
            space_s,
            ..
        } = theme::active().cosmic().spacing;

        let header = padded_control(
            row![
                column![
                    text(fl!("title")).size(16),
                    text(fl!("privacy-note")).size(10).width(Length::Fill),
                ]
                .spacing(2)
                .width(Length::Fill),
                button::icon(icon::from_name("view-refresh-symbolic"))
                    .on_press_maybe((!self.busy).then_some(Message::Refresh)),
            ]
            .spacing(space_xs)
            .align_y(Alignment::Center),
        )
        .padding([space_xs, space_s]);

        let body: Element<'_, Message> = if self.confirm_clear {
            confirmation(space_xs, space_s, self.busy)
        } else {
            match &self.history {
                HistoryState::Loading => state_row("process-working-symbolic", fl!("loading")),
                HistoryState::Ready(entries) if entries.is_empty() => {
                    state_row("edit-paste-symbolic", fl!("empty"))
                }
                HistoryState::Ready(entries) => {
                    let mut list = column![].spacing(space_xs);
                    for entry in entries {
                        list = list.push(entry_card(entry, self.busy));
                    }
                    scrollable(list)
                        .height(Length::Fixed(440.0))
                        .width(Length::Fill)
                        .into()
                }
                HistoryState::Unavailable(error) => state_row(
                    "dialog-warning-symbolic",
                    format!("{}: {error}", fl!("unavailable")),
                ),
            }
        };

        let mut content = column![
            header,
            padded_control(divider::horizontal::default()).padding([0, space_s]),
            container(body)
                .padding([space_xs, space_s])
                .width(Length::Fill),
        ]
        .spacing(space_xxs);
        if let Some(notice) = &self.notice {
            content = content.push(
                container(text(notice).size(11))
                    .padding([space_xs, space_s])
                    .width(Length::Fill),
            );
        }
        if matches!(&self.history, HistoryState::Ready(entries) if !entries.is_empty())
            && !self.confirm_clear
        {
            content = content.push(
                padded_control(
                    button::destructive(fl!("clear-all"))
                        .on_press_maybe((!self.busy).then_some(Message::AskClear)),
                )
                .padding([0, space_s, space_s, space_s]),
            );
        }

        self.core.applet.popup_container(content).into()
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        Some(Message::CloseRequested(id))
    }
}

impl ClipboardApplet {
    fn clear_popup_state(&mut self) {
        self.history = HistoryState::Ready(Vec::new());
        self.confirm_clear = false;
        self.notice = None;
        if !self.busy {
            self.refresh_generation = self.refresh_generation.wrapping_add(1);
        }
    }
}

fn refresh(generation: u64) -> app::Task<Message> {
    Task::perform(load_history(), move |result| {
        cosmic::Action::App(Message::Loaded(generation, result))
    })
}

fn action(
    generation: u64,
    kind: Action,
    future: impl std::future::Future<Output = Result<(), String>> + Send + 'static,
) -> app::Task<Message> {
    Task::perform(future, move |result| {
        cosmic::Action::App(Message::ActionFinished(generation, kind, result))
    })
}

async fn load_history() -> Result<Vec<ClipboardEntry>, String> {
    policy::require("clipboard.read", Scope::Name("history")).await?;
    copyq::load_history().await
}

async fn restore(identity: String) -> Result<(), String> {
    policy::require("clipboard.read", Scope::Name("history")).await?;
    policy::require("clipboard.write", Scope::Name("history")).await?;
    copyq::restore(identity).await
}

async fn remove(identity: String) -> Result<(), String> {
    policy::require("clipboard.read", Scope::Name("history")).await?;
    policy::require("clipboard.write", Scope::Name("history")).await?;
    copyq::remove(identity).await
}

async fn clear() -> Result<(), String> {
    policy::require("clipboard.write", Scope::Name("history")).await?;
    copyq::clear().await
}

fn entry_card(entry: &ClipboardEntry, busy: bool) -> Element<'_, Message> {
    let age = entry
        .copied_at
        .as_deref()
        .map(format_age)
        .filter(|value| !value.is_empty());
    let mut heading = row![text(entry.preview.clone()).size(12).width(Length::Fill)]
        .spacing(8)
        .align_y(Alignment::Start);
    if let Some(age) = age {
        heading = heading.push(text(age).size(10));
    }

    container(
        column![
            heading,
            row![
                button::text(fl!("restore"))
                    .on_press_maybe((!busy).then(|| Message::Restore(entry.identity.clone()))),
                space::horizontal().width(Length::Fill),
                button::icon(icon::from_name("edit-delete-symbolic"))
                    .on_press_maybe((!busy).then(|| Message::Delete(entry.identity.clone()))),
            ]
            .align_y(Alignment::Center),
        ]
        .spacing(8),
    )
    .padding([9, 10])
    .width(Length::Fill)
    .class(theme::Container::custom(glass_card))
    .into()
}

fn confirmation(space_xs: u16, space_s: u16, busy: bool) -> Element<'static, Message> {
    container(
        column![
            text(fl!("clear-confirm-title")).size(16),
            text(fl!("clear-confirm-body")).size(12),
            row![
                space::horizontal().width(Length::Fill),
                button::text(fl!("cancel")).on_press_maybe((!busy).then_some(Message::CancelClear)),
                button::destructive(fl!("confirm-clear"))
                    .on_press_maybe((!busy).then_some(Message::ConfirmClear)),
            ]
            .spacing(space_xs),
        ]
        .spacing(space_s),
    )
    .padding(space_s)
    .width(Length::Fill)
    .class(theme::Container::custom(glass_card))
    .into()
}

fn state_row(icon_name: &'static str, label: String) -> Element<'static, Message> {
    container(
        row![
            icon::from_name(icon_name).size(16).symbolic(true),
            text(label).size(12).width(Length::Fill),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding(14)
    .width(Length::Fill)
    .class(theme::Container::custom(glass_card))
    .into()
}

fn glass_card(theme: &cosmic::Theme) -> container::Style {
    let cosmic = theme.cosmic();
    let mut fill = cosmic.bg_component_color();
    fill.alpha = if theme.theme_type.is_dark() {
        0.54
    } else {
        0.68
    };
    container::Style {
        background: Some(Background::Color(fill.into())),
        border: Border {
            radius: cosmic.radius_m().into(),
            width: 1.0,
            color: cosmic.on_bg_color().with_alpha(0.10).into(),
        },
        shadow: Shadow {
            color: Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.12,
            },
            offset: Vector { x: 0.0, y: 3.0 },
            blur_radius: 14.0,
        },
        ..Default::default()
    }
}

fn format_age(value: &str) -> String {
    let epoch_seconds = value
        .parse::<i64>()
        .ok()
        .map(|raw| {
            if raw > 10_000_000_000 {
                raw / 1000
            } else {
                raw
            }
        })
        .or_else(|| value.parse::<Timestamp>().ok().map(|time| time.as_second()))
        .or_else(|| {
            value
                .parse::<DateTime>()
                .ok()?
                .to_zoned(TimeZone::system())
                .ok()
                .map(|zoned| zoned.timestamp().as_second())
        });
    let Some(epoch_seconds) = epoch_seconds else {
        return truncate(value, 24);
    };
    let seconds = Timestamp::now()
        .as_second()
        .saturating_sub(epoch_seconds)
        .max(0) as u64;
    let minutes = seconds / 60;
    if minutes < 60 {
        fl!("minutes-ago", count = minutes)
    } else if minutes < 60 * 24 {
        let hours = minutes / 60_u64;
        fl!("hours-ago", count = hours)
    } else {
        let days = minutes / (60_u64 * 24);
        fl!("days-ago", count = days)
    }
}

fn short_error(error: &str) -> String {
    truncate(error.lines().next().unwrap_or_default().trim(), 96)
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}
