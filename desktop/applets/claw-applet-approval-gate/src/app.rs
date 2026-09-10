// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    fl,
    localize::review_label,
    queue,
    review::ReviewModel,
    review_card::{ReviewMessage, review_card},
};
use clawd_client::system_review::{PendingReviews, ReviewText, SystemReview, display_text};
use cosmic::{
    Element, Task, app,
    applet::padded_control,
    iced::{
        Event, Length, Subscription, event, keyboard,
        platform_specific::shell::wayland::commands::popup::{destroy_popup, get_popup},
        time, window,
    },
    theme,
    widget::{
        button, column, divider, row, scrollable, space::horizontal as horizontal_space, text,
    },
};
use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub fn run() -> cosmic::iced::Result {
    cosmic::applet::run::<ApprovalGate>(())
}

#[derive(Clone, Default)]
struct ApprovalGate {
    core: cosmic::app::Core,
    popup: Option<window::Id>,
    reviews: ReviewModel,
    loaded: bool,
    refresh_state: RefreshState,
}

#[derive(Debug, Clone)]
enum Message {
    Tick,
    TogglePopup,
    ClosePopup,
    CloseRequested(window::Id),
    Focus(window::Id, bool),
    Refreshed(Result<PendingReviews, String>),
    Card(ReviewMessage),
    DecisionReturned {
        id: String,
        revision: u64,
        result: Result<SystemReview, String>,
    },
    Shown {
        id: String,
        result: Result<SystemReview, String>,
    },
}

#[derive(Debug, Clone, Default)]
struct RefreshState {
    in_flight: Arc<AtomicBool>,
}

impl RefreshState {
    fn try_start(&self) -> Option<RefreshPermit> {
        self.in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| RefreshPermit {
                in_flight: Arc::clone(&self.in_flight),
            })
    }

    #[cfg(test)]
    fn is_in_flight(&self) -> bool {
        self.in_flight.load(Ordering::Acquire)
    }
}

struct RefreshPermit {
    in_flight: Arc<AtomicBool>,
}

impl Drop for RefreshPermit {
    fn drop(&mut self) {
        self.in_flight.store(false, Ordering::Release);
    }
}

impl ApprovalGate {
    fn error(&mut self, error: String) {
        tracing::warn!(%error, "OS review presentation error");
        self.reviews.load_failed(error);
    }

    fn close(&mut self) -> app::Task<Message> {
        self.reviews.closed();
        self.popup.take().map_or_else(Task::none, destroy_popup)
    }
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
        let task = refresh_task(&app.refresh_state);
        (app, task)
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
            time::every(Duration::from_millis(1500)).map(|_| Message::Tick),
            event::listen_with(|event, status, id| {
                if status != event::Status::Ignored {
                    return None;
                }
                match event {
                    Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                        navigation_message(key, modifiers, id)
                    }
                    _ => None,
                }
            }),
        ])
    }

    fn update(&mut self, message: Message) -> app::Task<Message> {
        match message {
            Message::Tick => refresh_task(&self.refresh_state),
            Message::Refreshed(result) => {
                let result = result.and_then(|rows| {
                    self.reviews
                        .receive_pending(rows)
                        .map_err(|error| error.to_string())
                });
                match result {
                    Ok(ids) => {
                        self.loaded = true;
                        if self.popup.is_none() {
                            self.reviews.closed();
                        }
                        Task::batch(ids.into_iter().map(show_task))
                    }
                    Err(error) => {
                        self.error(error);
                        Task::none()
                    }
                }
            }
            Message::TogglePopup => {
                if self.popup.is_some() {
                    return self.close();
                }
                let id = window::Id::unique();
                self.popup = Some(id);
                let settings = self.core.applet.get_popup_settings(
                    self.core.main_window_id().unwrap(),
                    id,
                    Some((520, 600)),
                    None,
                    None,
                );
                get_popup(settings)
            }
            Message::ClosePopup => self.close(),
            Message::CloseRequested(id) => {
                if self.popup == Some(id) {
                    self.close()
                } else {
                    Task::none()
                }
            }
            Message::Focus(id, backwards) => {
                if self.popup != Some(id) {
                    return Task::none();
                }
                if backwards {
                    cosmic::iced::widget::operation::focus_previous()
                } else {
                    cosmic::iced::widget::operation::focus_next()
                }
            }
            Message::Card(ReviewMessage::Select {
                id,
                revision,
                permission,
                choice,
            }) => {
                if let Err(error) = self.reviews.select(&id, revision, &permission, choice) {
                    self.error(error.to_string());
                }
                Task::none()
            }
            Message::Card(ReviewMessage::Decide {
                id,
                revision,
                action,
            }) => match self.reviews.begin(&id, revision, action) {
                Ok(decision) => {
                    let review = self
                        .reviews
                        .cards
                        .iter()
                        .find(|card| card.review.id == id)
                        .expect("begin validated the review id")
                        .review
                        .clone();
                    let revision = decision.revision;
                    Task::perform(
                        async move {
                            let result = queue::decide(&review, &decision)
                                .await
                                .map_err(|error| error.0);
                            Message::DecisionReturned {
                                id,
                                revision,
                                result,
                            }
                        },
                        cosmic::Action::App,
                    )
                }
                Err(error) => {
                    self.error(error.to_string());
                    Task::none()
                }
            },
            Message::Card(ReviewMessage::Refresh(id)) => match self.reviews.request_refresh(&id) {
                Ok(()) => show_task(id),
                Err(error) => {
                    self.error(error.to_string());
                    Task::none()
                }
            },
            Message::DecisionReturned {
                id,
                revision,
                result,
            } => match self.reviews.decision_returned(&id, revision, result) {
                Ok(()) => show_task(id),
                Err(error) => {
                    self.error(error.to_string());
                    Task::none()
                }
            },
            Message::Shown { id, result } => {
                let result = result.and_then(|review| {
                    if review.id != id {
                        return Err("OS show returned a different request".into());
                    }
                    self.reviews
                        .receive_show(review)
                        .map_err(|error| error.to_string())
                });
                if let Err(error) = result {
                    tracing::warn!(%id, %error, "OS review refresh failed");
                    if let Err(error) = self.reviews.show_failed(&id, error) {
                        self.error(error.to_string());
                    }
                }
                if self.popup.is_none() {
                    self.reviews.closed();
                }
                Task::none()
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let count = self.reviews.pending_count();
        let icon = if count == 0 && self.reviews.error.is_none() {
            "com.clawos.ApprovalGate-symbolic"
        } else {
            "dialog-warning-symbolic"
        };
        let button = self
            .core
            .applet
            .icon_button(icon)
            .on_press(Message::TogglePopup);
        if count == 0 {
            button.into()
        } else {
            row![button, text(count.to_string()).size(11)]
                .align_y(cosmic::iced::core::Alignment::Center)
                .spacing(2)
                .into()
        }
    }

    fn view_window(&self, _: window::Id) -> Element<'_, Message> {
        let spacing = theme::active().cosmic().spacing;
        let header = padded_control(
            column![
                row![
                    text::heading(review_label(ReviewText::Title)),
                    horizontal_space(),
                    text(fl!("title-pending", count = self.reviews.pending_count())).size(11),
                ]
                .align_y(cosmic::iced::core::Alignment::Center),
                row![
                    button::standard(review_label(ReviewText::Close)).on_press(Message::ClosePopup),
                    button::standard(review_label(ReviewText::Refresh)).on_press(Message::Tick),
                ]
                .spacing(spacing.space_xs),
            ]
            .spacing(spacing.space_xs),
        );
        let mut body =
            column::with_capacity(self.reviews.cards.len() * 2 + 2).spacing(spacing.space_s);
        if let Some(error) = &self.reviews.error {
            body = body.push(padded_control(column![
                text::heading(review_label(ReviewText::Error)),
                text::body(display_text(error)),
            ]));
        } else if !self.loaded {
            body = body.push(padded_control(text::body(fl!("review-loading"))));
        } else if self.reviews.cards.is_empty() {
            body = body.push(padded_control(text::body(review_label(
                ReviewText::NoPending,
            ))));
        }
        for card in &self.reviews.cards {
            body = body
                .push(review_card(card).map(Message::Card))
                .push(padded_control(divider::horizontal::default()));
        }
        self.core
            .applet
            .popup_container(
                column![
                    header,
                    divider::horizontal::default(),
                    scrollable(body).height(Length::Fill)
                ]
                .spacing(spacing.space_xs)
                .height(Length::Fixed(600.0))
                .width(Length::Fixed(520.0)),
            )
            .into()
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Self::Message> {
        Some(Message::CloseRequested(id))
    }
}

fn navigation_message(
    key: keyboard::Key,
    modifiers: keyboard::Modifiers,
    id: window::Id,
) -> Option<Message> {
    match key {
        keyboard::Key::Named(keyboard::key::Named::Tab) => {
            Some(Message::Focus(id, modifiers.shift()))
        }
        keyboard::Key::Named(keyboard::key::Named::Escape) => Some(Message::CloseRequested(id)),
        _ => None,
    }
}

fn show_task(id: String) -> app::Task<Message> {
    Task::perform(
        async move {
            let result = queue::show(&id).await.map_err(|error| error.0);
            Message::Shown { id, result }
        },
        cosmic::Action::App,
    )
}

fn refresh_task(state: &RefreshState) -> app::Task<Message> {
    let Some(permit) = state.try_start() else {
        return Task::none();
    };
    Task::perform(run_refresh(permit, queue::load_pending()), |result| {
        cosmic::Action::App(Message::Refreshed(result.map_err(|error| error.0)))
    })
}

async fn run_refresh<T, F>(permit: RefreshPermit, future: F) -> T
where
    F: Future<Output = T>,
{
    let result = future.await;
    drop(permit);
    result
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/app.rs"));
}
