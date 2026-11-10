// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

mod localize;
mod presentation;
mod subscriptions;
use cosmic::{
    Element, Task, app,
    applet::{
        menu_control_padding, padded_control,
        token::subscription::{TokenRequest, TokenUpdate, activation_token_subscription},
    },
    cctk::sctk::reexports::calloop,
    cosmic_theme::Spacing,
    iced::{
        Alignment, Length, Subscription,
        advanced::text::{Ellipsize, EllipsizeHeightLimit},
        platform_specific::shell::wayland::commands::popup::{destroy_popup, get_popup},
        widget::{self, column, rich_text, row},
        window,
    },
    surface, theme,
    widget::{Column, button, cards, container, divider, icon, scrollable, space, text, toggler},
};

use presentation::{PreferenceState, PresentedCard};
use std::{borrow::Cow, collections::HashMap};
use subscriptions::notifications;
use tokio::sync::mpsc::Sender;

pub fn run() -> cosmic::iced::Result {
    localize::localize();
    cosmic::applet::run::<Notifications>(())
}

struct Notifications {
    core: cosmic::app::Core,
    preferences: PreferenceState,
    icon_name: String,
    popup: Option<window::Id>,
    cards: Vec<(widget::Id, Vec<PresentedCard>, bool, String, String, String)>,
    token_tx: Option<calloop::channel::Sender<TokenRequest>>,
    notifications_tx: Option<Sender<notifications::Input>>,
}

impl Notifications {
    fn update_icon(&mut self) {
        self.icon_name = if self.preferences.muted() {
            "cosmic-applet-notification-disabled-symbolic"
        } else if self.cards.is_empty() {
            "cosmic-applet-notification-symbolic"
        } else {
            "cosmic-applet-notification-new-symbolic"
        }
        .to_string();
    }

    fn remove_card(&mut self, id: u32) {
        for group in &mut self.cards {
            group.1.retain(|notification| notification.card.id != id);
            group.3 = fl!(
                "show-more",
                HashMap::from([("more", group.1.len().saturating_sub(1))])
            );
        }
        self.cards.retain(|group| !group.1.is_empty());
    }

    fn request(&self, input: notifications::Input) -> app::Task<Message> {
        let preference = matches!(input, notifications::Input::SetMuted(_));
        let sender = self.notifications_tx.clone();
        cosmic::task::future(async move {
            let result = async {
                let sender = sender.ok_or("Notification presentation controls are unavailable.")?;
                tokio::time::timeout(notifications::CALL_TIMEOUT, sender.send(input))
                    .await
                    .map_err(|_| "Notification presentation controls timed out.")?
                    .map_err(|_| "Notification presentation controls closed.")?;
                Ok::<(), &str>(())
            }
            .await;
            Message::NotificationEvent(match result {
                Ok(()) => notifications::Output::Queued,
                Err(error) if preference => {
                    notifications::Output::PreferenceResult(Err(error.to_string()))
                }
                Err(error) => notifications::Output::RequestFailed(error.to_string()),
            })
        })
    }
}

#[derive(Debug, Clone)]
enum Message {
    TogglePopup,
    CloseRequested(window::Id),
    DoNotDisturb(bool),
    NotificationEvent(notifications::Output),
    DbusEvent(subscriptions::dbus::Output),
    Dismissed(u32),
    ActivateNotification(u32),
    ClearAll(Option<String>),
    CardsToggled(String, bool),
    Token(TokenUpdate),
    OpenSettings,
    Surface(surface::Action),
}

impl cosmic::Application for Notifications {
    type Message = Message;
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    const APP_ID: &'static str = "com.clawos.AppletNotifications";

    fn init(core: cosmic::app::Core, _flags: Self::Flags) -> (Self, app::Task<Self::Message>) {
        let mut _self = Self {
            core,
            preferences: PreferenceState::default(),
            icon_name: String::default(),
            popup: None,
            cards: Vec::new(),
            token_tx: Option::default(),
            notifications_tx: None,
        };
        _self.update_icon();
        (_self, Task::none())
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
            subscriptions::dbus::proxy().map(Message::DbusEvent),
            subscriptions::notifications::notifications().map(Message::NotificationEvent),
            activation_token_subscription(0).map(Message::Token),
        ])
    }

    fn update(&mut self, message: Self::Message) -> app::Task<Self::Message> {
        match message {
            Message::TogglePopup => {
                if let Some(p) = self.popup.take() {
                    return destroy_popup(p);
                } else {
                    let new_id = window::Id::unique();
                    self.popup.replace(new_id);

                    let popup_settings = self.core.applet.get_popup_settings(
                        self.core.main_window_id().unwrap(),
                        new_id,
                        None,
                        None,
                        None,
                    );

                    return get_popup(popup_settings);
                }
            }
            Message::DoNotDisturb(b) => {
                if self.preferences.begin(b) {
                    return self.request(notifications::Input::SetMuted(b));
                }
            }
            Message::NotificationEvent(event) => match event {
                notifications::Output::Card(card) => {
                    let n = match PresentedCard::new(card) {
                        Ok(value) => value,
                        Err(error) => {
                            self.preferences.fail(error.to_string());
                            return Task::none();
                        }
                    };
                    let label = &n.card.source_label;
                    if let Some(existing) = self.cards.iter_mut().find_map(|group| {
                        group
                            .1
                            .iter_mut()
                            .find(|old| old.card.id == n.card.id && old.card.source_label == *label)
                    }) {
                        *existing = n;
                        return Task::none();
                    }
                    self.remove_card(n.card.id);
                    if let Some(c) = self
                        .cards
                        .iter_mut()
                        .find(|c| c.1.iter().any(|notif| *label == notif.card.source_label))
                    {
                        c.1.push(n);
                        c.3 = fl!(
                            "show-more",
                            HashMap::from([("more", c.1.len().saturating_sub(1))])
                        );
                    } else {
                        self.cards.push((
                            widget::Id::new(n.card.source_label.clone()),
                            vec![n],
                            false,
                            fl!("show-more", HashMap::from([("more", "1")])),
                            fl!("show-less"),
                            fl!("clear-group"),
                        ));
                    }
                }
                notifications::Output::Ready(tx) => {
                    self.notifications_tx = Some(tx);
                    self.preferences.connected = true;
                    self.preferences.error = None;
                }
                notifications::Output::Preferences(value) => self.preferences.observe(value),
                notifications::Output::PreferenceResult(result) => self.preferences.finish(result),
                notifications::Output::Dismissed(id) => self.remove_card(id),
                notifications::Output::RequestFailed(error) => self.preferences.fail(error),
                notifications::Output::Disconnected(error) => {
                    self.notifications_tx = None;
                    self.preferences.disconnect(error);
                    self.cards.clear();
                }
                notifications::Output::Queued => {}
            },
            Message::Dismissed(id) => {
                return self.request(notifications::Input::Dismiss(id));
            }
            Message::DbusEvent(e) => match e {
                subscriptions::dbus::Output::CloseEvent(id) => self.remove_card(id),
            },
            Message::ClearAll(label) => {
                let ids = self
                    .cards
                    .iter()
                    .flat_map(|group| &group.1)
                    .filter(|notification| {
                        label
                            .as_ref()
                            .is_none_or(|label| *label == notification.card.source_label)
                    })
                    .map(|notification| notification.card.id)
                    .collect::<Vec<_>>();
                return Task::batch(
                    ids.into_iter()
                        .map(|id| self.request(notifications::Input::Dismiss(id))),
                );
            }
            Message::CardsToggled(name, expanded) => {
                if let Some((_, _, n_expanded, ..)) = self
                    .cards
                    .iter_mut()
                    .find(|c| c.1.iter().any(|notif| name == notif.card.source_label))
                {
                    *n_expanded = expanded;
                }
            }
            Message::CloseRequested(id) => {
                if Some(id) == self.popup {
                    self.popup = None;
                }
            }
            Message::OpenSettings => {
                let exec = "/usr/local/bin/cos app cosmic-settings --gui notifications".to_string();
                if let Some(tx) = self.token_tx.as_ref() {
                    let _ = tx.send(TokenRequest {
                        app_id: Self::APP_ID.to_string(),
                        exec,
                    });
                }
            }
            Message::Token(u) => match u {
                TokenUpdate::Init(tx) => {
                    self.token_tx = Some(tx);
                }
                TokenUpdate::Finished => {
                    self.token_tx = None;
                }
                TokenUpdate::ActivationToken { token, .. } => {
                    let mut cmd = std::process::Command::new("/usr/local/bin/cos");
                    cmd.args(["app", "cosmic-settings", "--gui", "notifications"]);
                    if let Some(token) = token {
                        cmd.env("XDG_ACTIVATION_TOKEN", &token);
                        cmd.env("DESKTOP_STARTUP_ID", &token);
                    }
                    tokio::spawn(cosmic::process::spawn(cmd));
                }
            },
            Message::ActivateNotification(id) => {
                let Some(notification) = self
                    .cards
                    .iter()
                    .find_map(|list| list.1.iter().find(|n| n.card.id == id))
                else {
                    return Task::none();
                };
                let Some(action) = notification.card.activation.clone() else {
                    return self.request(notifications::Input::Dismiss(id));
                };
                return self.request(notifications::Input::Activated(id, action));
            }
            Message::Surface(a) => {
                return cosmic::task::message(cosmic::Action::Cosmic(
                    cosmic::app::Action::Surface(a),
                ));
            }
        }
        self.update_icon();
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        self.core
            .applet
            .icon_button(&self.icon_name)
            .on_press_down(Message::TogglePopup)
            .into()
    }

    fn view_window(&self, _id: window::Id) -> Element<'_, Message> {
        let Spacing {
            space_xxs,
            space_xs,
            space_s,
            ..
        } = theme::active().cosmic().spacing;

        let do_not_disturb = padded_control(row![
            toggler(self.preferences.muted())
                .on_toggle_maybe(self.preferences.enabled().then_some(Message::DoNotDisturb))
                .text_size(14)
                .width(Length::Fill)
                .label(fl!("do-not-disturb"))
        ]);

        let notifications = if self.cards.is_empty() {
            let no_notifications = fl!("no-notifications");
            row![
                container(
                    column![
                        text_icon("cosmic-applet-notification-symbolic", 40),
                        text::body(no_notifications)
                    ]
                    .align_x(Alignment::Center)
                )
                .center_x(Length::Fill)
            ]
            .padding([space_xxs, 0])
            .spacing(space_xs)
        } else {
            let mut notifs: Vec<Element<_>> = Vec::with_capacity(self.cards.len());
            notifs.push(
                container(
                    cosmic::widget::button::text(fl!("clear-all"))
                        .on_press(Message::ClearAll(None)),
                )
                .width(Length::Fill)
                .align_x(Alignment::End)
                .into(),
            );
            for c in self.cards.iter().rev() {
                if c.1.is_empty() {
                    continue;
                }
                let name = c.1[0].card.source_label.clone();
                let (ids, notif_elems): (Vec<_>, Vec<_>) = c
                    .1
                    .iter()
                    .rev()
                    .map(|n| {
                        let app_name = text::caption(Cow::from(&n.card.source_label))
                            .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)))
                            .width(Length::Fill);

                        let duration_since = text::caption(duration_ago_msg(n));

                        let close_notif = button::custom(
                            icon::from_name("window-close-symbolic")
                                .size(16)
                                .symbolic(true),
                        )
                        .on_press(Message::Dismissed(n.card.id))
                        .class(cosmic::theme::Button::Text);
                        (
                            n.card.id,
                            Element::from(
                                column!(
                                    if let Some(icon) = n.icon.clone() {
                                        row![
                                            icon.icon().size(16),
                                            app_name,
                                            duration_since,
                                            close_notif
                                        ]
                                        .spacing(space_xxs)
                                        .align_y(Alignment::Center)
                                    } else {
                                        row![app_name, duration_since, close_notif]
                                            .spacing(space_xxs)
                                            .align_y(Alignment::Center)
                                    },
                                    column![
                                        text::body(n.card.title.lines().next().unwrap_or_default())
                                            .width(Length::Fill),
                                        Element::from(
                                            rich_text(presentation::spans(&n.card.body))
                                                .size(12.0)
                                                .width(Length::Fill)
                                        )
                                    ]
                                )
                                .width(Length::Fill),
                            ),
                        )
                    })
                    .unzip();
                let show_more_icon = c.1.last().and_then(|n| n.group_icon.clone());
                let card_list = cards(
                    c.0.clone(),
                    notif_elems,
                    Message::ClearAll(Some(name.clone())),
                    Some(move |e| Message::CardsToggled(name.clone(), e)),
                    Some(move |id| Message::ActivateNotification(ids[id])),
                    &c.3,
                    &c.4,
                    &c.5,
                    show_more_icon,
                    c.2,
                );
                notifs.push(card_list.into());
            }

            row!(
                scrollable(
                    Column::with_children(notifs)
                        .spacing(space_xxs)
                        .height(Length::Shrink),
                )
                .height(Length::Shrink)
            )
            .padding(menu_control_padding())
        };

        let main_content = column![
            padded_control(divider::horizontal::default()).padding([space_xxs, space_s]),
            notifications,
        ];

        let mut content = column![do_not_disturb];
        if let Some(error) = &self.preferences.error {
            content = content.push(padded_control(text::caption(error)));
        }
        let content = content
            .push(main_content)
            .align_x(Alignment::Start)
            .padding([space_xxs, 0]);

        self.core.applet.popup_container(content).into()
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        Some(Message::CloseRequested(id))
    }
}

fn text_icon(name: &str, size: u16) -> cosmic::widget::Icon {
    icon::from_name(name).size(size).symbolic(true).icon()
}

fn duration_ago_msg(notification: &PresentedCard) -> String {
    let min = notification.received.elapsed().as_secs() / 60;
    let hrs = min / 60;
    if hrs > 0 {
        fl!("hours-ago", HashMap::from([("duration", hrs)]))
    } else {
        fl!("minutes-ago", HashMap::from([("duration", min)]))
    }
}
