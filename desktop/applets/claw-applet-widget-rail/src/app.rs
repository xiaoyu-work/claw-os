// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    calendar::{self, CalendarEvent},
    fl,
    policy::{self, Scope},
    system::{self, RawSample, SystemSummary, Usage},
};
use claw_applet_agent_activity::tasks::{self, Task};
use cosmic::{
    Element, Task as IcedTask, app,
    cosmic_theme::palette::WithAlpha,
    iced::{
        Alignment, Background, Border, Color, Length, Limits, Shadow, Subscription, Vector, time,
        widget::{column, container, row, scrollable, space::horizontal as horizontal_space, text},
    },
    theme,
    widget::icon,
};
use jiff::{Timestamp, Zoned, fmt::strtime, tz::TimeZone};
use std::time::Duration;

const REFRESH_SECONDS: u64 = 5;
const RAIL_HEIGHT: f32 = 620.0;

#[derive(Clone)]
enum SourceState<T> {
    Loading,
    Ready(T),
    Empty,
    Unavailable(String),
}

impl<T> Default for SourceState<T> {
    fn default() -> Self {
        Self::Loading
    }
}

#[derive(Clone, Default)]
pub struct WidgetRail {
    core: cosmic::app::Core,
    calendar: SourceState<Vec<CalendarEvent>>,
    tasks: SourceState<Vec<Task>>,
    system: SourceState<SystemSummary>,
    raw_system: RawSample,
    calendar_in_flight: bool,
    tasks_in_flight: bool,
    system_in_flight: bool,
}

#[derive(Debug, Clone)]
pub enum Message {
    RefreshFast,
    RefreshCalendar,
    CalendarLoaded(Result<Vec<CalendarEvent>, String>),
    TasksLoaded(Result<Vec<Task>, String>),
    SystemLoaded(Result<(SystemSummary, RawSample), String>),
}

impl cosmic::Application for WidgetRail {
    type Message = Message;
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();

    const APP_ID: &'static str = "com.clawos.AppletWidgetRail";

    fn init(core: cosmic::app::Core, _: ()) -> (Self, app::Task<Message>) {
        let rail = Self {
            core,
            calendar_in_flight: true,
            tasks_in_flight: true,
            system_in_flight: true,
            ..Default::default()
        };
        (
            rail,
            IcedTask::batch([
                refresh_calendar(),
                refresh_tasks(),
                refresh_system(RawSample::default()),
            ]),
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
        Subscription::batch([
            time::every(Duration::from_secs(REFRESH_SECONDS)).map(|_| Message::RefreshFast),
            time::every(Duration::from_secs(60)).map(|_| Message::RefreshCalendar),
        ])
    }

    fn update(&mut self, message: Message) -> app::Task<Message> {
        match message {
            Message::RefreshFast => {
                let mut refreshes = Vec::new();
                if !self.tasks_in_flight {
                    self.tasks_in_flight = true;
                    refreshes.push(refresh_tasks());
                }
                if !self.system_in_flight {
                    self.system_in_flight = true;
                    refreshes.push(refresh_system(self.raw_system));
                }
                IcedTask::batch(refreshes)
            }
            Message::RefreshCalendar => {
                if self.calendar_in_flight {
                    IcedTask::none()
                } else {
                    self.calendar_in_flight = true;
                    refresh_calendar()
                }
            }
            Message::CalendarLoaded(result) => {
                self.calendar_in_flight = false;
                self.calendar = match result {
                    Ok(events) if events.is_empty() => SourceState::Empty,
                    Ok(events) => SourceState::Ready(events),
                    Err(error) => SourceState::Unavailable(short_error(error)),
                };
                IcedTask::none()
            }
            Message::TasksLoaded(result) => {
                self.tasks_in_flight = false;
                self.tasks = match result {
                    Ok(tasks) => {
                        let tasks = select_tasks(tasks);
                        if tasks.is_empty() {
                            SourceState::Empty
                        } else {
                            SourceState::Ready(tasks)
                        }
                    }
                    Err(error) => SourceState::Unavailable(short_error(error)),
                };
                IcedTask::none()
            }
            Message::SystemLoaded(result) => {
                self.system_in_flight = false;
                self.system = match result {
                    Ok((summary, raw)) => {
                        self.raw_system = raw;
                        SourceState::Ready(summary)
                    }
                    Err(error) => SourceState::Unavailable(short_error(error)),
                };
                IcedTask::none()
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let content = column![
            rail_header(),
            self.today_card(),
            self.tasks_card(),
            self.system_card(),
            self.suggestions_card(),
        ]
        .spacing(12)
        .width(Length::Fill);

        let content = container(
            scrollable(content)
                .height(Length::Fixed(RAIL_HEIGHT))
                .width(Length::Fill),
        )
        .width(Length::Fill)
        .padding(4);

        let mut limits = Limits::NONE.min_width(1.0).min_height(1.0);
        if let Some(bounds) = self.core.applet.suggested_bounds {
            if bounds.width > 0.0 {
                limits = limits.max_width(bounds.width);
            }
            if bounds.height > 0.0 {
                limits = limits.max_height(bounds.height);
            }
        }
        self.core
            .applet
            .autosize_window(content)
            .limits(limits)
            .into()
    }
}

impl WidgetRail {
    fn today_card(&self) -> Element<'_, Message> {
        let date = strtime::format("%A, %B %-d", &Zoned::now())
            .map(|formatted| formatted.to_string())
            .unwrap_or_else(|_| Zoned::now().date().to_string());
        let mut body = column![text(date).size(20)].spacing(8);

        match &self.calendar {
            SourceState::Loading => {
                body = body.push(state_row(
                    "process-working-symbolic",
                    fl!("calendar-loading"),
                ));
            }
            SourceState::Empty => {
                body = body.push(state_row("emblem-ok-symbolic", fl!("calendar-empty")));
            }
            SourceState::Unavailable(error) => {
                body = body.push(unavailable(fl!("calendar-unavailable"), error.to_string()));
            }
            SourceState::Ready(events) => {
                for event in events.iter().take(3) {
                    body = body.push(event_row(event));
                }
            }
        }

        card(
            fl!("today-title"),
            "x-office-calendar-symbolic",
            body.into(),
        )
    }

    fn tasks_card(&self) -> Element<'_, Message> {
        let body: Element<'_, Message> = match &self.tasks {
            SourceState::Loading => state_row("process-working-symbolic", fl!("tasks-loading")),
            SourceState::Empty => state_row("emblem-ok-symbolic", fl!("tasks-empty")),
            SourceState::Unavailable(error) => {
                unavailable(fl!("tasks-unavailable"), error.to_string())
            }
            SourceState::Ready(tasks) => {
                let active = tasks.iter().filter(|task| is_active(task)).count();
                let mut content = column![
                    text(if active == 0 {
                        fl!("recent")
                    } else {
                        fl!("active-count", count = active)
                    })
                    .size(11)
                ]
                .spacing(7);
                for task in tasks.iter().take(3) {
                    content = content.push(task_row(task));
                }
                content.into()
            }
        };
        card(fl!("tasks-title"), "system-run-symbolic", body)
    }

    fn system_card(&self) -> Element<'_, Message> {
        let body: Element<'_, Message> = match &self.system {
            SourceState::Loading => state_row("process-working-symbolic", fl!("system-loading")),
            SourceState::Empty => state_row("dialog-information-symbolic", fl!("not-available")),
            SourceState::Unavailable(error) => {
                unavailable(fl!("system-unavailable"), error.to_string())
            }
            SourceState::Ready(summary) => {
                let cpu = summary
                    .cpu_percent
                    .map(|value| format!("{value:.0}%"))
                    .unwrap_or_else(|| fl!("warming-up"));
                let memory = summary
                    .memory
                    .map(format_usage)
                    .unwrap_or_else(|| fl!("not-available"));
                let storage = summary
                    .storage
                    .map(format_usage)
                    .unwrap_or_else(|| fl!("not-available"));
                let network = match (summary.network_down_bps, summary.network_up_bps) {
                    (Some(down), Some(up)) => fl!(
                        "network-rate",
                        down = format_rate(down),
                        up = format_rate(up)
                    ),
                    _ => fl!("warming-up"),
                };

                column![
                    row![metric(fl!("cpu"), cpu), metric(fl!("memory"), memory),].spacing(8),
                    row![
                        metric(fl!("storage"), storage),
                        metric(fl!("network"), network),
                    ]
                    .spacing(8),
                ]
                .spacing(8)
                .into()
            }
        };
        card(
            fl!("system-title"),
            "utilities-system-monitor-symbolic",
            body,
        )
    }

    fn suggestions_card(&self) -> Element<'_, Message> {
        let suggestions = suggestions(&self.calendar, &self.tasks, &self.system);
        let body: Element<'_, Message> = if suggestions.is_empty() {
            state_row("process-working-symbolic", fl!("suggestions-loading"))
        } else {
            let mut content = column![].spacing(8);
            for suggestion in suggestions {
                content = content.push(
                    row![
                        status_dot(Tone::Accent),
                        text(suggestion).size(12).width(Length::Fill),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Start),
                );
            }
            content.into()
        };
        card(fl!("suggestions-title"), "starred-symbolic", body)
    }
}

fn rail_header() -> Element<'static, Message> {
    column![
        text(fl!("rail-eyebrow")).size(10),
        text(fl!("rail-title")).size(24),
    ]
    .spacing(1)
    .padding([2, 4, 4, 4])
    .into()
}

fn card<'a>(
    title: String,
    icon_name: &'static str,
    body: Element<'a, Message>,
) -> Element<'a, Message> {
    let header = row![
        icon::from_name(icon_name).size(17).symbolic(true),
        text(title).size(14),
        horizontal_space(),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    container(column![header, body].spacing(10))
        .width(Length::Fill)
        .padding(14)
        .class(theme::Container::custom(glass_card_style))
        .into()
}

fn glass_card_style(theme: &cosmic::Theme) -> container::Style {
    let cosmic = theme.cosmic();
    let mut fill = cosmic.bg_component_color();
    fill.alpha = if theme.theme_type.is_dark() {
        0.66
    } else {
        0.72
    };

    container::Style {
        text_color: Some(cosmic.on_bg_color().into()),
        icon_color: Some(cosmic.on_bg_color().into()),
        background: Some(Background::Color(fill.into())),
        border: Border {
            radius: cosmic.radius_l().into(),
            width: 1.0,
            color: cosmic.on_bg_color().with_alpha(0.10).into(),
        },
        shadow: Shadow {
            color: Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: if theme.theme_type.is_dark() {
                    0.30
                } else {
                    0.14
                },
            },
            offset: Vector { x: 0.0, y: 6.0 },
            blur_radius: 24.0,
        },
        snap: true,
    }
}

fn metric(label: String, value: String) -> Element<'static, Message> {
    container(column![text(label).size(10), text(value).size(12)].spacing(3))
        .width(Length::FillPortion(1))
        .padding([7, 8])
        .class(theme::Container::custom(|theme| {
            let cosmic = theme.cosmic();
            let mut fill = cosmic.bg_component_color();
            fill.alpha = 0.18;
            container::Style {
                background: Some(Background::Color(fill.into())),
                border: Border {
                    radius: cosmic.radius_s().into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        }))
        .into()
}

fn event_row(event: &CalendarEvent) -> Element<'_, Message> {
    let title = if event.title.trim().is_empty() {
        fl!("no-title")
    } else {
        truncate(&event.title, 42)
    };
    let time = event_time(&event.start);
    let detail = if event.location.trim().is_empty() {
        time
    } else {
        format!("{time} · {}", truncate(&event.location, 24))
    };

    row![
        status_dot(Tone::Accent),
        column![text(title).size(12), text(detail).size(10)].spacing(2),
    ]
    .spacing(8)
    .align_y(Alignment::Start)
    .into()
}

fn task_row(task: &Task) -> Element<'_, Message> {
    let title = if task.purpose.trim().is_empty() {
        format!(
            "…{}",
            task.id
                .chars()
                .rev()
                .take(8)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>()
        )
    } else {
        truncate(&task.purpose, 46)
    };
    let tone = match task.status.as_str() {
        "running" | "pending" => Tone::Accent,
        "failed" => Tone::Danger,
        _ => Tone::Muted,
    };
    row![
        status_dot(tone),
        column![
            text(title).size(12),
            text(task.status.to_ascii_uppercase()).size(9),
        ]
        .spacing(2),
    ]
    .spacing(8)
    .align_y(Alignment::Start)
    .into()
}

fn state_row(icon_name: &'static str, message: String) -> Element<'static, Message> {
    row![
        icon::from_name(icon_name).size(16).symbolic(true),
        text(message).size(12).width(Length::Fill),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

fn unavailable(title: String, detail: String) -> Element<'static, Message> {
    row![
        icon::from_name("dialog-warning-symbolic")
            .size(16)
            .symbolic(true),
        column![text(title).size(12), text(detail).size(10)].spacing(2),
    ]
    .spacing(8)
    .align_y(Alignment::Start)
    .into()
}

#[derive(Clone, Copy)]
enum Tone {
    Accent,
    Muted,
    Danger,
}

fn status_dot(tone: Tone) -> Element<'static, Message> {
    const SIZE: f32 = 7.0;
    container(horizontal_space().width(Length::Fixed(SIZE)))
        .height(Length::Fixed(SIZE))
        .class(theme::Container::custom(move |theme| {
            let cosmic = theme.cosmic();
            let color = match tone {
                Tone::Accent => cosmic.accent_color(),
                Tone::Muted => cosmic.palette.neutral_6,
                Tone::Danger => cosmic.destructive_color(),
            };
            container::Style {
                background: Some(Background::Color(color.into())),
                border: Border {
                    radius: (SIZE / 2.0).into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        }))
        .into()
}

fn select_tasks(mut tasks: Vec<Task>) -> Vec<Task> {
    tasks.sort_by(|a, b| {
        let a_active = is_active(a);
        let b_active = is_active(b);
        b_active
            .cmp(&a_active)
            .then_with(|| b.created_at.cmp(&a.created_at))
    });
    tasks
}

fn is_active(task: &Task) -> bool {
    matches!(task.status.as_str(), "running" | "pending")
}

fn suggestions(
    calendar: &SourceState<Vec<CalendarEvent>>,
    tasks: &SourceState<Vec<Task>>,
    system: &SourceState<SystemSummary>,
) -> Vec<String> {
    if matches!(calendar, SourceState::Loading)
        && matches!(tasks, SourceState::Loading)
        && matches!(system, SourceState::Loading)
    {
        return Vec::new();
    }

    let mut suggestions = Vec::new();
    if let SourceState::Ready(events) = calendar {
        if let Some(event) = events.first() {
            let event_title = if event.title.trim().is_empty() {
                fl!("no-title")
            } else {
                event.title.clone()
            };
            suggestions.push(fl!(
                "suggestion-prepare",
                title = truncate(&event_title, 34)
            ));
        }
    } else if matches!(calendar, SourceState::Empty) {
        suggestions.push(fl!("suggestion-clear"));
    }

    if let SourceState::Ready(tasks) = tasks {
        let active = tasks.iter().filter(|task| is_active(task)).count();
        if active > 0 {
            suggestions.push(fl!("suggestion-tasks", count = active));
        }
    }

    if let SourceState::Ready(summary) = system {
        if summary.memory.is_some_and(|usage| usage.percent() >= 85) {
            suggestions.push(fl!("suggestion-memory"));
        }
        if summary.storage.is_some_and(|usage| usage.percent() >= 90) {
            suggestions.push(fl!("suggestion-storage"));
        }
    }

    if suggestions.is_empty() {
        suggestions.push(fl!("suggestion-balanced"));
    }
    suggestions.truncate(3);
    suggestions
}

fn event_time(start: &str) -> String {
    event_time_in_zone(start, TimeZone::system())
}

fn event_time_in_zone(start: &str, time_zone: TimeZone) -> String {
    if start.len() == 10 {
        return fl!("all-day");
    }
    if let Ok(timestamp) = start.parse::<Timestamp>() {
        if let Ok(formatted) = strtime::format("%H:%M", &timestamp.to_zoned(time_zone)) {
            return formatted.to_string();
        }
    }
    start
        .get(11..16)
        .filter(|time| time.as_bytes().get(2) == Some(&b':'))
        .unwrap_or(start)
        .to_string()
}

fn format_usage(usage: Usage) -> String {
    format!(
        "{}% · {:.1} GB",
        usage.percent(),
        usage.used_mb as f64 / 1024.0
    )
}

fn format_rate(bytes_per_second: u64) -> String {
    if bytes_per_second >= 1_048_576 {
        format!("{:.1} MB/s", bytes_per_second as f64 / 1_048_576.0)
    } else if bytes_per_second >= 1024 {
        format!("{:.0} KB/s", bytes_per_second as f64 / 1024.0)
    } else {
        format!("{bytes_per_second} B/s")
    }
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

fn short_error(error: String) -> String {
    truncate(error.lines().next().unwrap_or_default().trim(), 96)
}

fn refresh_calendar() -> app::Task<Message> {
    IcedTask::perform(calendar::load_today(), |result| {
        cosmic::Action::App(Message::CalendarLoaded(result))
    })
}

fn refresh_tasks() -> app::Task<Message> {
    IcedTask::perform(
        async {
            policy::require("agent.observe", Scope::Name("tasks")).await?;
            tasks::load_tasks_async().await.map_err(|error| error.0)
        },
        |result| cosmic::Action::App(Message::TasksLoaded(result)),
    )
}

fn refresh_system(previous: RawSample) -> app::Task<Message> {
    IcedTask::perform(async move { system::load(previous).await }, |result| {
        cosmic::Action::App(Message::SystemLoaded(result))
    })
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/app.rs"
    ));
}
