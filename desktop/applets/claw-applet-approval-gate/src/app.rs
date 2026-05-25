// SPDX-License-Identifier: GPL-3.0-only

use crate::fl;
use crate::queue::{self, GrantDuration, Request, Risk};
use cosmic::{
    Element, Task, app,
    applet::padded_control,
    cosmic_theme::Spacing,
    iced::{
        Length, Subscription,
        platform_specific::shell::wayland::commands::popup::{destroy_popup, get_popup},
        time, window,
    },
    theme,
    widget::{
        button, column, container, divider, row,
        space::horizontal as horizontal_space, space::vertical as vertical_space,
        text,
    },
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn run() -> cosmic::iced::Result {
    cosmic::applet::run::<ApprovalGate>(())
}

#[derive(Clone, Default)]
struct ApprovalGate {
    core: cosmic::app::Core,
    popup: Option<window::Id>,
    pending: Vec<Request>,
    last_error: Option<String>,
}

#[derive(Debug, Clone)]
enum Message {
    Tick,
    TogglePopup,
    CloseRequested(window::Id),
    Refreshed(Vec<Request>),
    LoadFailed(String),
    Approve(String, GrantDuration),
    Deny(String),
    Resolved,
}

impl cosmic::Application for ApprovalGate {
    type Message = Message;
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    const APP_ID: &'static str = "com.clawos.AppletApprovalGate";

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
        // Poll the approval queue. inotify would be nicer, but clawd
        // owns the authoritative store and the cost of a short RPC is
        // negligible vs the simplicity win.
        time::every(Duration::from_millis(1500)).map(|_| Message::Tick)
    }

    fn update(&mut self, message: Message) -> app::Task<Message> {
        match message {
            Message::Tick => refresh_task(),
            Message::Refreshed(rows) => {
                let count_changed = rows.len() != self.pending.len();
                self.pending = rows;
                self.last_error = None;
                // If the popup is open and the queue empties out,
                // close it — nothing more to act on.
                if self.pending.is_empty() && count_changed {
                    if let Some(id) = self.popup.take() {
                        return destroy_popup(id);
                    }
                }
                Task::none()
            }
            Message::LoadFailed(msg) => {
                self.last_error = Some(msg);
                Task::none()
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
                Task::none()
            }
            Message::Approve(id, dur) => {
                let id = id.clone();
                Task::perform(
                    async move { queue::approve(&id, dur).await },
                    |res| match res {
                        Ok(_) => cosmic::Action::App(Message::Resolved),
                        Err(e) => cosmic::Action::App(Message::LoadFailed(e.0)),
                    },
                )
            }
            Message::Deny(id) => {
                let id = id.clone();
                Task::perform(async move { queue::deny(&id).await }, |res| match res {
                    Ok(_) => cosmic::Action::App(Message::Resolved),
                    Err(e) => cosmic::Action::App(Message::LoadFailed(e.0)),
                })
            }
            Message::Resolved => refresh_task(),
        }
    }

    fn view(&self) -> Element<'_, Message> {
        // Icon choice: when the queue is empty we stay quiet
        // ("dialog-question-symbolic"). When something needs attention
        // we swap to the warning icon to draw the eye. Plain numeric
        // badges would be nicer but require theme work the COSMIC
        // toolkit doesn't expose for applets yet.
        let icon = if self.pending.is_empty() {
            "dialog-question-symbolic"
        } else {
            "dialog-warning-symbolic"
        };
        let btn = self.core.applet.icon_button(icon).on_press(Message::TogglePopup);
        if self.pending.is_empty() {
            btn.into()
        } else {
            // Append a small numeric label next to the icon. Two-column
            // mini-layout keeps the panel button compact.
            row![btn, text(format!("{}", self.pending.len())).size(11)]
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
            text(if self.pending.is_empty() {
                fl!("no-pending")
            } else {
                fl!("title-pending", count = self.pending.len())
            })
            .size(11),
        ]);

        let body: Element<'_, Message> = if let Some(err) = &self.last_error {
            padded_control(
                text(fl!("runtime-error", message = err.as_str()))
                    .size(12),
            )
            .into()
        } else if self.pending.is_empty() {
            padded_control(
                container(text(fl!("no-pending")).size(12))
                    .center_x(Length::Fill)
                    .padding(space_s),
            )
            .into()
        } else {
            let mut col = column::with_capacity(self.pending.len() * 2);
            for (i, req) in self.pending.iter().enumerate() {
                if i > 0 {
                    col = col.push(padded_control(divider::horizontal::default()));
                }
                col = col.push(render_card(req, space_xxs, space_xs));
            }
            col.into()
        };

        self.core
            .applet
            .popup_container(column![header, divider::horizontal::default(), body].spacing(space_xxs))
            .into()
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Self::Message> {
        Some(Message::CloseRequested(id))
    }
}

fn render_card(req: &Request, space_xxs: u16, space_xs: u16) -> Element<'_, Message> {
    let (risk, label_text, blurb_text) = match &req.meta {
        Some(m) => (m.risk.clone(), m.label.clone(), m.blurb.clone()),
        None => (Risk::Medium, req.verb.clone(), String::new()),
    };
    let risk_text = match risk {
        Risk::Low => fl!("risk-low"),
        Risk::Medium => fl!("risk-medium"),
        Risk::High => fl!("risk-high"),
        Risk::Critical => fl!("risk-critical"),
    };

    let head = row![
        text(req.verb.clone()).size(13),
        horizontal_space(),
        text(format!("[{risk_text}]")).size(11),
    ]
    .spacing(space_xs);

    let mut body = column::with_capacity(6).spacing(space_xxs);
    if !label_text.is_empty() && label_text != req.verb {
        body = body.push(text(label_text).size(12));
    }
    if !blurb_text.is_empty() {
        body = body.push(text(blurb_text).size(11));
    }
    body = body.push(meta_row(fl!("scope-label"), req.scope.render()));
    body = body.push(meta_row(fl!("reason-label"), req.reason.clone()));
    body = body.push(meta_row(fl!("session-label"), req.session.clone()));
    if let Some(requester) = &req.requester {
        body = body.push(meta_row(fl!("requester-label"), requester.clone()));
    }
    body = body.push(meta_row(
        fl!("requested-label"),
        relative_time(req.requested_at),
    ));

    let mut actions = row![]
        .spacing(space_xs)
        .align_y(cosmic::iced::core::Alignment::Center);
    if matches!(risk, Risk::Critical) {
        actions = actions.push(
            text(fl!("critical-warning")).size(10),
        );
    } else {
        let id = req.id.clone();
        let id_session = req.id.clone();
        let id_forever = req.id.clone();
        actions = actions
            .push(
                button::suggested(fl!("approve-once"))
                    .on_press(Message::Approve(id, GrantDuration::Once)),
            )
            .push(
                button::standard(fl!("approve-session"))
                    .on_press(Message::Approve(id_session, GrantDuration::Session)),
            )
            .push(
                button::standard(fl!("approve-forever"))
                    .on_press(Message::Approve(id_forever, GrantDuration::Forever)),
            );
    }
    actions = actions.push(horizontal_space());
    let id_deny = req.id.clone();
    actions = actions.push(
        button::destructive(fl!("deny"))
            .on_press(Message::Deny(id_deny)),
    );

    padded_control(
        column![head, body, vertical_space().height(Length::Fixed(4.0)), actions]
            .spacing(space_xxs),
    )
    .into()
}

fn meta_row(label: String, value: String) -> Element<'static, Message> {
    row![
        text(label).size(11).width(Length::Fixed(78.0)),
        text(value).size(11),
    ]
    .spacing(8)
    .into()
}

fn relative_time(then: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let delta = now.saturating_sub(then);
    match delta {
        0..=4 => fl!("just-now"),
        5..=59 => fl!("seconds-ago", n = (delta as u32)),
        60..=3599 => fl!("minutes-ago", n = ((delta / 60) as u32)),
        3600..=86399 => fl!("hours-ago", n = ((delta / 3600) as u32)),
        _ => fl!("days-ago", n = ((delta / 86400) as u32)),
    }
}

fn refresh_task() -> app::Task<Message> {
    Task::perform(async { queue::load_pending().await }, |res| match res {
        Ok(rows) => cosmic::Action::App(Message::Refreshed(rows)),
        Err(e) => cosmic::Action::App(Message::LoadFailed(e.0)),
    })
}
