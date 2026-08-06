// SPDX-License-Identifier: GPL-3.0-only

use claw_applet_widget_rail::calendar::{self, CalendarEvent};
use cosmic::{
    Element, Task, app,
    applet::padded_control,
    cosmic_theme::{Spacing, palette::WithAlpha},
    iced::{
        Alignment, Background, Border, Color, Length, Shadow, Subscription, Vector,
        platform_specific::shell::wayland::commands::popup::{destroy_popup, get_popup},
        time,
        widget::{column, container, row, scrollable},
        window,
    },
    surface, theme,
    widget::{calendar::CalendarModel, divider, icon, space, text},
};
use jiff::{
    Timestamp,
    civil::{Date, Weekday},
    fmt::strtime,
    tz::TimeZone,
};
use std::time::Duration;

use crate::fl;

#[derive(Clone)]
enum AgendaState {
    Loading,
    Ready(Vec<CalendarEvent>),
    Unavailable(String),
}

pub struct CalendarApplet {
    core: cosmic::app::Core,
    popup: Option<window::Id>,
    calendar: CalendarModel,
    agenda: AgendaState,
    loading: bool,
    pending_day: Option<Date>,
    first_day: Weekday,
    refresh_generation: u64,
}

#[derive(Debug, Clone)]
pub enum Message {
    TogglePopup,
    CloseRequested(window::Id),
    SelectDay(Date),
    PreviousMonth,
    NextMonth,
    Minute,
    Loaded(Date, u64, Result<Vec<CalendarEvent>, String>),
    Surface(surface::Action),
}

impl cosmic::Application for CalendarApplet {
    type Message = Message;
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();

    const APP_ID: &'static str = "com.clawos.PanelCalendarButton";

    fn init(core: cosmic::app::Core, _: ()) -> (Self, app::Task<Message>) {
        (
            Self {
                core,
                popup: None,
                calendar: CalendarModel::now(),
                agenda: AgendaState::Loading,
                loading: false,
                pending_day: None,
                first_day: locale_first_day(),
                refresh_generation: 0,
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

    fn subscription(&self) -> Subscription<Message> {
        time::every(Duration::from_secs(60)).map(|_| Message::Minute)
    }

    fn update(&mut self, message: Message) -> app::Task<Message> {
        match message {
            Message::TogglePopup => {
                if let Some(id) = self.popup.take() {
                    destroy_popup(id)
                } else {
                    self.calendar = CalendarModel::now();
                    self.agenda = AgendaState::Loading;
                    self.loading = true;
                    self.pending_day = None;
                    self.refresh_generation = self.refresh_generation.wrapping_add(1);
                    let id = window::Id::unique();
                    self.popup = Some(id);
                    let settings = self.core.applet.get_popup_settings(
                        self.core.main_window_id().expect("panel window"),
                        id,
                        Some((380, 640)),
                        None,
                        None,
                    );
                    Task::batch([
                        get_popup(settings),
                        load_agenda(self.calendar.selected, self.refresh_generation),
                    ])
                }
            }
            Message::CloseRequested(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                    self.agenda = AgendaState::Loading;
                    self.loading = false;
                    self.pending_day = None;
                    self.refresh_generation = self.refresh_generation.wrapping_add(1);
                }
                Task::none()
            }
            Message::SelectDay(date) => {
                self.calendar.set_selected_visible(date);
                self.request_load()
            }
            Message::PreviousMonth => {
                self.calendar.set_prev_month();
                self.request_load()
            }
            Message::NextMonth => {
                self.calendar.set_next_month();
                self.request_load()
            }
            Message::Minute => {
                if self.popup.is_some() && !self.loading {
                    self.start_load()
                } else {
                    Task::none()
                }
            }
            Message::Loaded(day, generation, result) => {
                if generation != self.refresh_generation {
                    return Task::none();
                }
                self.loading = false;
                if self.popup.is_none() {
                    self.pending_day = None;
                    return Task::none();
                }
                if self.pending_day.take().is_some() || self.calendar.selected != day {
                    return self.start_load();
                }
                self.agenda = match result {
                    Ok(events) => AgendaState::Ready(events),
                    Err(error) => AgendaState::Unavailable(short_error(&error)),
                };
                Task::none()
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
            .icon_button("com.clawos.PanelCalendarButton-symbolic")
            .on_press_down(Message::TogglePopup);
        self.core
            .applet
            .applet_tooltip(
                button,
                fl!("tooltip"),
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

        let calendar = cosmic::widget::calendar(
            &self.calendar,
            Message::SelectDay,
            || Message::PreviousMonth,
            || Message::NextMonth,
            self.first_day,
        );

        let agenda = match &self.agenda {
            AgendaState::Loading => state_row("process-working-symbolic", fl!("agenda-loading")),
            AgendaState::Ready(events) if events.is_empty() => {
                state_row("emblem-ok-symbolic", fl!("agenda-empty"))
            }
            AgendaState::Ready(events) => {
                let mut list = column![].spacing(space_xs);
                for event in events {
                    list = list.push(event_card(event));
                }
                scrollable(list)
                    .height(Length::Fixed(210.0))
                    .width(Length::Fill)
                    .into()
            }
            AgendaState::Unavailable(error) => state_row(
                "dialog-warning-symbolic",
                format!("{}: {error}", fl!("agenda-unavailable")),
            ),
        };

        let content = column![
            calendar,
            padded_control(divider::horizontal::default()).padding([space_xxs, space_s]),
            column![text(fl!("agenda-title")).size(15), agenda]
                .spacing(space_xs)
                .padding([0, space_s, space_s, space_s]),
        ]
        .spacing(space_xxs)
        .padding([space_xxs, 0])
        .width(Length::Fill);

        self.core.applet.popup_container(container(content)).into()
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        Some(Message::CloseRequested(id))
    }
}

impl CalendarApplet {
    fn request_load(&mut self) -> app::Task<Message> {
        self.agenda = AgendaState::Loading;
        if self.loading {
            self.pending_day = Some(self.calendar.selected);
            Task::none()
        } else {
            self.start_load()
        }
    }

    fn start_load(&mut self) -> app::Task<Message> {
        self.agenda = AgendaState::Loading;
        self.loading = true;
        self.pending_day = None;
        self.refresh_generation = self.refresh_generation.wrapping_add(1);
        load_agenda(self.calendar.selected, self.refresh_generation)
    }
}

fn load_agenda(day: Date, generation: u64) -> app::Task<Message> {
    Task::perform(calendar::load_day(day), move |result| {
        cosmic::Action::App(Message::Loaded(day, generation, result))
    })
}

fn event_card(event: &CalendarEvent) -> Element<'_, Message> {
    let title = if event.title.trim().is_empty() {
        fl!("untitled-event")
    } else {
        truncate(&event.title, 52)
    };
    let when = event_time(&event.start);
    let detail = if event.location.trim().is_empty() {
        when
    } else {
        format!("{when} · {}", truncate(&event.location, 36))
    };

    container(
        row![
            container(space::horizontal().width(Length::Fixed(5.0)))
                .height(Length::Fixed(32.0))
                .class(theme::Container::custom(accent_bar)),
            column![text(title).size(13), text(detail).size(11)].spacing(2),
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    )
    .padding([8, 10])
    .width(Length::Fill)
    .class(theme::Container::custom(glass_card))
    .into()
}

fn state_row(icon_name: &'static str, label: String) -> Element<'static, Message> {
    container(
        row![
            icon::from_name(icon_name).size(16).symbolic(true),
            text(label).size(12),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding(12)
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

fn accent_bar(theme: &cosmic::Theme) -> container::Style {
    let cosmic = theme.cosmic();
    container::Style {
        background: Some(Background::Color(cosmic.accent_color().into())),
        border: Border {
            radius: cosmic.radius_s().into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn event_time(start: &str) -> String {
    if start.len() == 10 {
        return fl!("all-day");
    }
    if let Ok(timestamp) = start.parse::<Timestamp>()
        && let Ok(formatted) = strtime::format("%H:%M", &timestamp.to_zoned(TimeZone::system()))
    {
        return formatted.to_string();
    }
    start
        .get(11..16)
        .filter(|value| value.as_bytes().get(2) == Some(&b':'))
        .unwrap_or(start)
        .to_string()
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

fn short_error(error: &str) -> String {
    truncate(error.lines().next().unwrap_or_default().trim(), 96)
}

fn locale_first_day() -> Weekday {
    let locale = ["LC_TIME", "LC_ALL", "LANG"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .unwrap_or_default()
        .replace('-', "_")
        .to_ascii_uppercase();
    let sunday_regions = [
        "_US", "_CA", "_MX", "_BR", "_CO", "_PH", "_JP", "_TW", "_HK", "_IL",
    ];
    if sunday_regions.iter().any(|region| locale.contains(region)) {
        Weekday::Sunday
    } else {
        Weekday::Monday
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_unicode_without_splitting_code_points() {
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("日程表です", 3), "日程表…");
    }
}
