// SPDX-License-Identifier: GPL-3.0-only

use crate::fl;
use crate::tasks::{self, Task};
use cosmic::{
    Element, Task as IcedTask, app,
    applet::padded_control,
    cosmic_theme::Spacing,
    iced::{
        Length, Subscription,
        platform_specific::shell::wayland::commands::popup::{destroy_popup, get_popup},
        time, window,
    },
    theme,
    widget::{button, column, container, divider, horizontal_space, row, text},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn run() -> cosmic::iced::Result {
    cosmic::applet::run::<AgentActivity>(())
}

#[derive(Clone, Default)]
struct AgentActivity {
    core: cosmic::app::Core,
    popup: Option<window::Id>,
    tasks: Vec<Task>,
    last_error: Option<String>,
}

#[derive(Debug, Clone)]
enum Message {
    Tick,
    TogglePopup,
    CloseRequested(window::Id),
    Refreshed(Vec<Task>),
    LoadFailed(String),
    Stop(String),
    Undo(String),
    Resume(String),
    Resolved,
}

impl cosmic::Application for AgentActivity {
    type Message = Message;
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    const APP_ID: &'static str = "com.clawos.AppletAgentActivity";

    fn init(core: cosmic::app::Core, _: ()) -> (Self, app::Task<Message>) {
        let app = Self {
            core,
            ..Default::default()
        };
        (app, refresh_task())
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
        // Poll `cos agent ls` every 2s. Cheap (one fork+exec into the
        // Rust binary) and avoids a second event-bus protocol just
        // for the panel — same trade as approval-gate.
        time::every(Duration::from_millis(2000)).map(|_| Message::Tick)
    }

    fn update(&mut self, message: Message) -> app::Task<Message> {
        match message {
            Message::Tick => refresh_task(),
            Message::Refreshed(rows) => {
                let count_changed = rows.len() != self.tasks.len();
                self.tasks = rows;
                self.last_error = None;
                // If the popup is open and the queue empties out,
                // close it — nothing to act on.
                if self.tasks.is_empty() && count_changed {
                    if let Some(id) = self.popup.take() {
                        return destroy_popup(id);
                    }
                }
                IcedTask::none()
            }
            Message::LoadFailed(msg) => {
                self.last_error = Some(msg);
                IcedTask::none()
            }
            Message::TogglePopup => {
                if let Some(id) = self.popup.take() {
                    destroy_popup(id)
                } else {
                    let new_id = window::Id::unique();
                    self.popup = Some(new_id);
                    let popup_settings = self.core.applet.get_popup_settings(
                        self.core.main_window_id().unwrap(),
                        new_id,
                        Some((520, 600)),
                        None,
                        None,
                    );
                    get_popup(popup_settings)
                }
            }
            Message::CloseRequested(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                }
                IcedTask::none()
            }
            Message::Stop(id) => IcedTask::perform(
                async move { tasks::stop_task(&id) },
                |res| match res {
                    Ok(_) => cosmic::Action::App(Message::Resolved),
                    Err(e) => cosmic::Action::App(Message::LoadFailed(e.0)),
                },
            ),
            Message::Undo(id) => IcedTask::perform(
                async move { tasks::undo_task(&id) },
                |res| match res {
                    Ok(_) => cosmic::Action::App(Message::Resolved),
                    Err(e) => cosmic::Action::App(Message::LoadFailed(e.0)),
                },
            ),
            Message::Resume(id) => IcedTask::perform(
                async move { tasks::resume_task(&id) },
                |res| match res {
                    Ok(_) => cosmic::Action::App(Message::Resolved),
                    Err(e) => cosmic::Action::App(Message::LoadFailed(e.0)),
                },
            ),
            Message::Resolved => refresh_task(),
        }
    }

    fn view(&self) -> Element<'_, Message> {
        // Stay quiet when there are no active tasks; switch to a
        // running-process icon when something is alive. Same numeric
        // badge convention as approval-gate.
        let active = self
            .tasks
            .iter()
            .filter(|t| t.status == "running" || t.status == "pending")
            .count();
        let icon = if active == 0 {
            "system-run-symbolic"
        } else {
            "media-playback-start-symbolic"
        };
        let btn = self
            .core
            .applet
            .icon_button(icon)
            .on_press(Message::TogglePopup);
        if active == 0 {
            btn.into()
        } else {
            row![btn, text(format!("{}", active)).size(11)]
                .align_y(cosmic::iced::core::Alignment::Center)
                .spacing(2)
                .into()
        }
    }

    fn view_window(&self, _id: window::Id) -> Element<'_, Message> {
        let Spacing {
            space_xxs,
            space_xs,
            space_s,
            ..
        } = theme::active().cosmic().spacing;

        let header = padded_control(row![
            text(fl!("title")).size(14),
            horizontal_space(),
            text(if self.tasks.is_empty() {
                fl!("no-tasks")
            } else {
                fl!("title-active", count = self.tasks.len())
            })
            .size(11),
        ]);

        let body: Element<'_, Message> = if let Some(err) = &self.last_error {
            padded_control(text(fl!("runtime-error", message = err.as_str())).size(12)).into()
        } else if self.tasks.is_empty() {
            padded_control(
                container(text(fl!("no-tasks")).size(12))
                    .center_x(Length::Fill)
                    .padding(space_s),
            )
            .into()
        } else {
            let mut col = column::with_capacity(self.tasks.len() * 2);
            for (i, t) in self.tasks.iter().enumerate() {
                if i > 0 {
                    col = col.push(padded_control(divider::horizontal::default()));
                }
                col = col.push(render_card(t, space_xxs, space_xs));
            }
            col.into()
        };

        self.core
            .applet
            .popup_container(
                column![header, divider::horizontal::default(), body].spacing(space_xxs),
            )
            .into()
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Self::Message> {
        Some(Message::CloseRequested(id))
    }
}

fn render_card(t: &Task, space_xxs: u16, space_xs: u16) -> Element<'_, Message> {
    let status_label = match t.status.as_str() {
        "pending" => fl!("status-pending"),
        "running" => fl!("status-running"),
        "paused" => fl!("status-paused"),
        "done" => fl!("status-done"),
        "failed" => fl!("status-failed"),
        other => other.to_string(),
    };
    let purpose = if t.purpose.is_empty() {
        fl!("no-purpose")
    } else {
        t.purpose.clone()
    };

    let head = row![
        text(purpose).size(13),
        horizontal_space(),
        text(format!("[{status_label}]")).size(11),
    ]
    .spacing(space_xs);

    let runtime = t
        .creator_runtime
        .clone()
        .unwrap_or_else(|| fl!("no-runtime"));
    let holder = match &t.lease {
        Some(l) => format!(
            "pid {} ({})",
            l.pid,
            l.runtime.clone().unwrap_or_else(|| fl!("no-runtime"))
        ),
        None => fl!("no-runtime"),
    };

    let mut body = column::with_capacity(5).spacing(space_xxs);
    body = body.push(meta_row(fl!("runtime-label"), runtime));
    body = body.push(meta_row(fl!("holder-label"), holder));
    body = body.push(meta_row(
        fl!("created-label"),
        relative_time_rfc3339(&t.created_at),
    ));
    // A short id tail keeps the card identifiable without dominating
    // the layout. Full id is still in the JSON if a power user wants
    // to grep `cos agent show <full-id>` from a terminal.
    body = body.push(meta_row("id".into(), short_id(&t.id)));

    let mut actions = row![]
        .spacing(space_xs)
        .align_y(cosmic::iced::core::Alignment::Center);
    let id_for = || t.id.clone();
    match t.status.as_str() {
        "running" | "pending" => {
            actions = actions.push(button::standard(fl!("stop")).on_press(Message::Stop(id_for())));
        }
        "paused" => {
            actions = actions
                .push(button::suggested(fl!("resume")).on_press(Message::Resume(id_for())));
        }
        _ => {}
    }
    // Undo is offered for any non-pending task — a finished task with
    // recorded mutations is still rolled back through the same path.
    if t.status != "pending" {
        actions = actions.push(button::destructive(fl!("undo")).on_press(Message::Undo(id_for())));
    }
    actions = actions.push(horizontal_space());

    padded_control(column![head, body, actions].spacing(space_xxs)).into()
}

fn meta_row(label: String, value: String) -> Element<'static, Message> {
    row![
        text(label).size(11).width(Length::Fixed(78.0)),
        text(value).size(11),
    ]
    .spacing(8)
    .into()
}

fn short_id(sid: &str) -> String {
    // `ses_<13>_<12>` — show the trailing 8 hex of the random half.
    // That's 256 bits / 4 = 64 bits' worth of entropy — plenty to
    // disambiguate amongst dozens of concurrent tasks.
    sid.rsplit_once('_')
        .map(|(_, tail)| {
            let n = tail.len();
            if n > 8 {
                format!("…{}", &tail[n - 8..])
            } else {
                format!("…{tail}")
            }
        })
        .unwrap_or_else(|| sid.to_string())
}

fn relative_time_rfc3339(rfc: &str) -> String {
    // Best-effort: parse the leading `YYYY-MM-DDTHH:MM:SSZ` to seconds
    // since epoch via the well-known formula. We don't pull in chrono
    // for this — the format is rigid enough.
    let then = parse_rfc3339_z(rfc).unwrap_or(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let delta = now.saturating_sub(then);
    match delta {
        0..=4 => fl!("just-now"),
        5..=59 => fl!("seconds-ago", n = delta as u32),
        60..=3599 => fl!("minutes-ago", n = (delta / 60) as u32),
        3600..=86399 => fl!("hours-ago", n = (delta / 3600) as u32),
        _ => fl!("days-ago", n = (delta / 86400) as u32),
    }
}

fn parse_rfc3339_z(s: &str) -> Option<u64> {
    // Expect `YYYY-MM-DDTHH:MM:SSZ` exactly. Anything else returns
    // None and the UI falls back to the "just now" branch — better
    // than crashing on a future schema tweak.
    let bytes = s.as_bytes();
    if bytes.len() < 20 || bytes[19] != b'Z' {
        return None;
    }
    let yr: i64 = s.get(0..4)?.parse().ok()?;
    let mo: u32 = s.get(5..7)?.parse().ok()?;
    let dy: u32 = s.get(8..10)?.parse().ok()?;
    let hr: u32 = s.get(11..13)?.parse().ok()?;
    let mi: u32 = s.get(14..16)?.parse().ok()?;
    let se: u32 = s.get(17..19)?.parse().ok()?;
    Some(civil_from_ymd_hms(yr, mo, dy, hr, mi, se))
}

/// Days from civil 1970-01-01 to the given y/m/d, then convert to
/// epoch seconds. Howard Hinnant's algorithm — public domain.
fn civil_from_ymd_hms(y: i64, m: u32, d: u32, h: u32, mi: u32, s: u32) -> u64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = (y - era * 400) as u32;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe as i64 - 719468;
    (days * 86400 + h as i64 * 3600 + mi as i64 * 60 + s as i64).max(0) as u64
}

fn refresh_task() -> app::Task<Message> {
    IcedTask::perform(async { tasks::load_tasks() }, |res| match res {
        Ok(rows) => cosmic::Action::App(Message::Refreshed(rows)),
        Err(e) => cosmic::Action::App(Message::LoadFailed(e.0)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_id_takes_last_8_hex_of_random_tail() {
        // tail = "e71a8d6a8ca4" (12 hex), last 8 = "8d6a8ca4"
        assert_eq!(short_id("ses_0019e2566eb1f_e71a8d6a8ca4"), "…8d6a8ca4");
    }

    #[test]
    fn short_id_handles_short_tails() {
        assert_eq!(short_id("ses_short_xyz"), "…xyz");
    }

    #[test]
    fn short_id_falls_back_on_malformed_input() {
        assert_eq!(short_id("nopeunderscores"), "nopeunderscores");
    }

    #[test]
    fn parse_rfc3339_z_handles_canonical_input() {
        // 2025-01-01T00:00:00Z = 1735689600
        assert_eq!(parse_rfc3339_z("2025-01-01T00:00:00Z"), Some(1735689600));
        // 1970-01-01T00:00:00Z = 0
        assert_eq!(parse_rfc3339_z("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn parse_rfc3339_z_rejects_garbage() {
        assert_eq!(parse_rfc3339_z(""), None);
        assert_eq!(parse_rfc3339_z("not-a-time"), None);
        assert_eq!(parse_rfc3339_z("2025-01-01T00:00:00"), None); // no Z
    }
}
