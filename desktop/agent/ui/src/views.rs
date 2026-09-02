use std::time::Duration;

use cosmic::iced::keyboard::{Key, key::Named};
use cosmic::iced::widget::text_editor;
use cosmic::iced::{Alignment, Length};
use cosmic::widget::{Column, Row, button, container, scrollable, text};
use cosmic::{Element, theme, widget};

use crate::bridge::{ModelsResponse, ToolCallView, ToolResultView};
use crate::fl;
use crate::session::{ChatMessage, ChatRole, HistoryState, LocalSession};
use crate::styles;
use crate::{App, CHAT_SCROLL_ID, EDITOR_ID, Message};

static SYMBOL_LIGHT: &[u8] = include_bytes!("../assets/clawos-symbol.png");
static SYMBOL_DARK: &[u8] = include_bytes!("../assets/clawos-symbol-dark.png");
static WORDMARK_LIGHT: &[u8] = include_bytes!("../assets/clawos-wordmark.png");
static WORDMARK_DARK: &[u8] = include_bytes!("../assets/clawos-wordmark-dark.png");

const SIDEBAR_WIDTH: f32 = 220.0;

impl App {
    pub(super) fn view_standalone(&self) -> Element<'_, Message> {
        let spacing = theme::active().cosmic().spacing;
        let chat_body = self.active_chat_body(false);
        let main = Column::new()
            .push(
                container(chat_body)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .padding([spacing.space_m, spacing.space_l]),
            )
            .push(container(self.input_card(false)).padding([
                0u16,
                spacing.space_l,
                spacing.space_l,
                spacing.space_l,
            ]));
        let body = Row::new()
            .push(
                container(self.sidebar_view())
                    .width(Length::Fixed(SIDEBAR_WIDTH))
                    .height(Length::Fill)
                    .padding([spacing.space_m, spacing.space_s])
                    .class(theme::Container::custom(styles::sidebar)),
            )
            .push(container(main).width(Length::Fill).height(Length::Fill));
        container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .class(theme::Container::custom(styles::page))
            .into()
    }

    pub(super) fn view_overlay(&self) -> Element<'_, Message> {
        if self.voice.is_active() {
            return self.voice_overlay();
        }
        let spacing = theme::active().cosmic().spacing;
        let header = Row::new()
            .push(Self::brand_symbol(20.0))
            .push(text(fl!("app-name")).size(13.0))
            .push(widget::space::horizontal())
            .push(text(fl!("close-hint")).size(11.0))
            .align_y(Alignment::Center)
            .spacing(spacing.space_xs);
        let has_content = self
            .active_session()
            .is_some_and(|session| !session.messages.is_empty())
            || self.error.is_some();
        let mut inner = Column::new()
            .push(container(header).padding(spacing.space_xs))
            .spacing(spacing.space_xs);
        if has_content {
            inner = inner.push(
                container(self.active_chat_body(true))
                    .width(Length::Fill)
                    .height(Length::Fixed(300.0))
                    .padding([0u16, spacing.space_xs]),
            );
        }
        inner = inner.push(container(self.input_card(true)).padding(spacing.space_xs));
        container(inner)
            .width(Length::Fixed(520.0))
            .class(theme::Container::custom(styles::page))
            .into()
    }

    fn voice_overlay(&self) -> Element<'_, Message> {
        let spacing = theme::active().cosmic().spacing;
        let (status, elapsed, peak, processing) = if let Some(metrics) = self.voice.metrics() {
            (
                fl!("listening"),
                Self::format_duration(metrics.elapsed),
                metrics.peak,
                false,
            )
        } else if self.voice.is_processing() {
            (fl!("transcribing"), String::new(), 0.25, true)
        } else {
            (String::new(), String::new(), 0.0, false)
        };
        let phase = self
            .voice
            .metrics()
            .map_or(0.0, |metrics| metrics.elapsed.as_secs_f32() * 4.0);
        let mut bars = Row::new()
            .spacing(spacing.space_xxs)
            .align_y(Alignment::Center);
        for index in 0..9 {
            let pulse = ((phase + index as f32 * 0.7).sin().abs() * 0.45 + 0.55) * peak.max(0.08);
            bars = bars.push(
                container(widget::Space::new())
                    .width(Length::Fixed(5.0))
                    .height(Length::Fixed(12.0 + pulse * 52.0))
                    .class(theme::Container::custom(styles::level_bar)),
            );
        }
        let orb = container(bars)
            .width(Length::Fixed(150.0))
            .height(Length::Fixed(150.0))
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .class(theme::Container::custom(styles::orb));
        let controls = Row::new()
            .push(if processing {
                Self::symbolic_button(
                    "window-close-symbolic",
                    fl!("cancel"),
                    Some(Message::CancelVoice),
                    true,
                )
            } else {
                Self::symbolic_button(
                    "media-playback-stop-symbolic",
                    fl!("stop"),
                    Some(Message::ToggleMic),
                    true,
                )
            })
            .push(if processing {
                widget::Space::new().into()
            } else {
                Self::symbolic_button(
                    "window-close-symbolic",
                    fl!("cancel"),
                    Some(Message::CancelVoice),
                    false,
                )
            })
            .spacing(spacing.space_s)
            .align_y(Alignment::Center);
        container(
            Column::new()
                .push(text(fl!("app-name")).size(13.0))
                .push(orb)
                .push(text(status).size(16.0))
                .push(text(elapsed).size(12.0))
                .push(controls)
                .align_x(Alignment::Center)
                .spacing(spacing.space_s)
                .padding(spacing.space_m),
        )
        .width(Length::Fixed(280.0))
        .class(theme::Container::custom(styles::page))
        .into()
    }

    fn active_chat_body(&self, compact: bool) -> Element<'_, Message> {
        let Some(session) = self.active_session() else {
            return Self::empty_state(compact);
        };
        if session.messages.is_empty()
            && let Some(error) = &self.error
        {
            return Column::new()
                .push(Self::empty_state(compact))
                .push(Self::error_card(error, None))
                .spacing(theme::active().cosmic().spacing.space_s)
                .height(Length::Fill)
                .into();
        }
        match &session.history {
            HistoryState::Loading => Self::state_card(fl!("history-loading"), None),
            HistoryState::Failed(error) => Self::state_card(
                format!("{} {error}", fl!("history-failed")),
                Some((fl!("retry"), Message::RetryHistory)),
            ),
            _ if session.messages.is_empty() => Self::empty_state(compact),
            _ => self.message_list(session, compact),
        }
    }

    fn provider_model_label(models: &ModelsResponse) -> String {
        if !models.ready {
            return fl!("model-unavailable");
        }
        if !models.label.trim().is_empty() {
            models.label.clone()
        } else if !models.model.trim().is_empty() && !models.provider.trim().is_empty() {
            format!("{} · {}", models.provider, models.model)
        } else {
            fl!("bridge-ready")
        }
    }

    fn format_duration(duration: Duration) -> String {
        format!(
            "{:02}:{:02}",
            duration.as_secs() / 60,
            duration.as_secs() % 60
        )
    }

    fn symbolic_button(
        icon_name: &'static str,
        label: String,
        on_press: Option<Message>,
        destructive: bool,
    ) -> Element<'static, Message> {
        let mut control = button::custom(widget::icon::from_name(icon_name).size(18))
            .padding(8)
            .class(if destructive {
                cosmic::theme::Button::Destructive
            } else {
                cosmic::theme::Button::Standard
            });
        if let Some(message) = on_press {
            control = control.on_press(message);
        }
        widget::tooltip(control, text(label), widget::tooltip::Position::Top).into()
    }

    fn brand_symbol(size: f32) -> Element<'static, Message> {
        widget::image(if Self::is_dark() {
            widget::image::Handle::from_bytes(SYMBOL_DARK)
        } else {
            widget::image::Handle::from_bytes(SYMBOL_LIGHT)
        })
        .height(Length::Fixed(size))
        .width(Length::Fixed(size))
        .into()
    }

    fn state_card(label: String, action: Option<(String, Message)>) -> Element<'static, Message> {
        let mut column = Column::new()
            .push(text(label).size(13.0))
            .align_x(Alignment::Center)
            .spacing(8);
        if let Some((label, message)) = action {
            column = column.push(button::text(label).on_press(message));
        }
        container(column)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    }

    fn error_card(error: &str, retry: Option<Message>) -> Element<'_, Message> {
        let spacing = theme::active().cosmic().spacing;
        let mut row = Row::new()
            .push(widget::icon::from_name("dialog-warning-symbolic").size(18))
            .push(
                Column::new()
                    .push(text(fl!("error-prefix")).size(11.0))
                    .push(text(error).size(12.0))
                    .width(Length::Fill),
            );
        if let Some(retry) = retry {
            row = row.push(Self::symbolic_button(
                "view-refresh-symbolic",
                fl!("retry"),
                Some(retry),
                false,
            ));
        }
        container(row.spacing(spacing.space_xs).align_y(Alignment::Center))
            .padding(spacing.space_xs)
            .class(theme::Container::custom(styles::tool_error_card))
            .into()
    }

    fn status_dot(active: bool) -> Element<'static, Message> {
        container(
            widget::Space::new()
                .width(Length::Fixed(8.0))
                .height(Length::Fixed(8.0)),
        )
        .class(theme::Container::custom(if active {
            styles::green_dot
        } else {
            styles::idle_dot
        }))
        .into()
    }

    fn session_row<'a>(
        session: &'a LocalSession,
        active: bool,
        index: usize,
        responding: bool,
    ) -> Element<'a, Message> {
        let spacing = theme::active().cosmic().spacing;
        let details = if session.message_count > 0 {
            format!(
                "{} · {}",
                session.relative_label(),
                fl!("messages-count", count = session.message_count)
            )
        } else {
            session.relative_label()
        };
        let row = Row::new()
            .push(text(session.display_title()).size(13.0).width(Length::Fill))
            .push(Self::status_dot(responding))
            .push(text(details).size(10.0))
            .spacing(spacing.space_xxs)
            .align_y(Alignment::Center);
        let class = if active {
            cosmic::theme::Button::Custom {
                active: Box::new(|_, _| styles::selected_session()),
                disabled: Box::new(|_| styles::selected_session()),
                hovered: Box::new(|_, _| styles::selected_session()),
                pressed: Box::new(|_, _| styles::selected_session()),
            }
        } else {
            cosmic::theme::Button::MenuItem
        };
        button::custom(row)
            .width(Length::Fill)
            .padding([spacing.space_xxs, spacing.space_xs])
            .class(class)
            .on_press(Message::SelectSession(index))
            .into()
    }

    fn empty_state(compact: bool) -> Element<'static, Message> {
        let spacing = theme::active().cosmic().spacing;
        let mut column = Column::new()
            .spacing(spacing.space_s)
            .align_x(Alignment::Center);
        if !compact {
            column = column.push(
                widget::image(if Self::is_dark() {
                    widget::image::Handle::from_bytes(WORDMARK_DARK)
                } else {
                    widget::image::Handle::from_bytes(WORDMARK_LIGHT)
                })
                .height(Length::Fixed(40.0)),
            );
        }
        column = column
            .push(
                text(if compact {
                    fl!("ready-title")
                } else {
                    fl!("empty-title")
                })
                .size(if compact { 14.0 } else { 28.0 }),
            )
            .push(
                text(if compact {
                    fl!("ready-hint")
                } else {
                    fl!("empty-hint")
                })
                .size(if compact { 11.0 } else { 13.0 }),
            );
        if !compact {
            column = column.push(
                Column::new()
                    .push(Self::example_chip(fl!("example-files")))
                    .push(Self::example_chip(fl!("example-sandbox")))
                    .push(Self::example_chip(fl!("example-battery")))
                    .spacing(spacing.space_xs)
                    .width(Length::Fill),
            );
        }
        container(column)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    }

    fn example_chip(label: String) -> Element<'static, Message> {
        let spacing = theme::active().cosmic().spacing;
        button::custom(text(label.clone()).size(12.0).width(Length::Fill))
            .class(cosmic::theme::Button::Standard)
            .padding([spacing.space_xxs, spacing.space_s])
            .width(Length::Fill)
            .on_press(Message::SetPrompt(label))
            .into()
    }

    fn message_bubble(message: &ChatMessage, index: usize, compact: bool) -> Element<'_, Message> {
        let spacing = theme::active().cosmic().spacing;
        let body_size = if compact { 12.0 } else { 14.0 };
        match message.role() {
            ChatRole::User => {
                let mut column = Column::new().spacing(spacing.space_xxs);
                if !message.content.trim().is_empty() {
                    column = column.push(
                        container(
                            container(text(message.content.clone()).size(body_size))
                                .padding([spacing.space_xs, spacing.space_s])
                                .class(theme::Container::custom(styles::user_pill)),
                        )
                        .width(Length::Fill)
                        .align_x(Alignment::End),
                    );
                }
                for result in message.tool_results.iter().filter(|result| result.is_error) {
                    column = column.push(Self::tool_result_card(result));
                }
                column.width(Length::Fill).into()
            }
            ChatRole::Assistant => {
                let mut column = Column::new().spacing(spacing.space_xxs);
                if let Some(items) = message.parsed_markdown.as_ref() {
                    let palette = if Self::is_dark() {
                        cosmic::iced::theme::Palette::DARK
                    } else {
                        cosmic::iced::theme::Palette::LIGHT
                    };
                    let settings = widget::markdown::Settings::with_text_size(
                        body_size,
                        widget::markdown::Style::from_palette(palette),
                    );
                    column = column.push(
                        widget::markdown::view(items, settings)
                            .map(|uri| Message::LinkClicked(uri.to_string())),
                    );
                } else if !message.content.is_empty() {
                    column = column.push(text(message.content.clone()).size(body_size));
                } else if message.in_progress && message.tool_calls.is_empty() {
                    column = column.push(text(fl!("streaming")).size(body_size));
                }
                for call in &message.tool_calls {
                    column = column.push(Self::tool_call_card(call, compact));
                }
                for result in message.tool_results.iter().filter(|result| result.is_error) {
                    column = column.push(Self::tool_result_card(result));
                }
                for warning in &message.warnings {
                    column = column.push(Self::warning_card(warning));
                }
                if let Some(error) = &message.error {
                    column =
                        column.push(Self::error_card(error, Some(Message::RetryMessage(index))));
                }
                if !message.content.is_empty() && !message.in_progress {
                    column = column.push(
                        Row::new()
                            .push(Self::symbolic_button(
                                "edit-copy-symbolic",
                                fl!("copy"),
                                Some(Message::CopyAssistant(index)),
                                false,
                            ))
                            .push(Self::symbolic_button(
                                "view-refresh-symbolic",
                                fl!("retry"),
                                Some(Message::RetryMessage(index)),
                                false,
                            ))
                            .spacing(spacing.space_xxs),
                    );
                }
                container(column).width(Length::Fill).into()
            }
        }
    }

    fn tool_call_card(call: &ToolCallView, compact: bool) -> Element<'_, Message> {
        let spacing = theme::active().cosmic().spacing;
        let column = Column::new()
            .push(
                Row::new()
                    .push(widget::icon::from_name("system-run-symbolic").size(16))
                    .push(
                        text(if call.name.is_empty() {
                            fl!("tool-running")
                        } else {
                            call.name.clone()
                        })
                        .size(if compact { 11.0 } else { 12.0 }),
                    )
                    .push(widget::space::horizontal())
                    .push(
                        text(if call.in_progress {
                            fl!("tool-running")
                        } else {
                            String::new()
                        })
                        .size(10.0),
                    )
                    .spacing(spacing.space_xxs)
                    .align_y(Alignment::Center),
            )
            .spacing(spacing.space_xxs);
        container(column)
            .padding([spacing.space_xxs, spacing.space_s])
            .class(theme::Container::custom(styles::tool_card))
            .width(Length::Fill)
            .into()
    }

    fn tool_result_card(result: &ToolResultView) -> Element<'_, Message> {
        let spacing = theme::active().cosmic().spacing;
        let mut label = if result.is_error {
            fl!("tool-error")
        } else {
            fl!("tool-result")
        };
        if !result.name.trim().is_empty() {
            label.push_str(": ");
            label.push_str(&result.name);
        }
        let column = Column::new().push(text(label).size(11.0));
        container(column.spacing(spacing.space_xxs))
            .padding([spacing.space_xxs, spacing.space_s])
            .class(theme::Container::custom(if result.is_error {
                styles::tool_error_card
            } else {
                styles::tool_card
            }))
            .width(Length::Fill)
            .into()
    }

    fn warning_card(warning: &str) -> Element<'_, Message> {
        let spacing = theme::active().cosmic().spacing;
        container(
            Row::new()
                .push(widget::icon::from_name("dialog-warning-symbolic").size(16))
                .push(text(format!("{}: {warning}", fl!("warning"))).size(11.0))
                .spacing(spacing.space_xxs),
        )
        .padding([spacing.space_xxs, spacing.space_s])
        .class(theme::Container::custom(styles::tool_card))
        .into()
    }

    fn is_dark() -> bool {
        theme::active().theme_type.is_dark()
    }

    fn message_list<'a>(
        &'a self,
        session: &'a LocalSession,
        compact: bool,
    ) -> Element<'a, Message> {
        let spacing = theme::active().cosmic().spacing;
        let mut column = Column::new().spacing(spacing.space_s).width(Length::Fill);
        for (index, message) in session.messages.iter().enumerate() {
            column = column.push(Self::message_bubble(message, index, compact));
        }

        if let Some(error) = &self.error {
            column = column.push(Self::error_card(error, None));
        }
        scrollable(column)
            .id(CHAT_SCROLL_ID.clone())
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn input_card(&self, compact: bool) -> Element<'_, Message> {
        let spacing = theme::active().cosmic().spacing;
        let placeholder = if self.voice.is_recording() {
            fl!("listening")
        } else if self.voice.is_processing() {
            fl!("transcribing")
        } else if compact {
            fl!("ask-anything")
        } else if self
            .active_session()
            .is_none_or(|session| session.messages.is_empty())
        {
            fl!("ask-agent")
        } else {
            fl!("request-changes")
        };
        let can_submit =
            !self.stream.is_active() && !self.voice.is_active() && self.active_history_ready();
        let voice_active = self.voice.is_active();
        let editor = widget::text_editor(&self.input)
            .id(EDITOR_ID.clone())
            .placeholder(placeholder)
            .height(Length::Fixed(if compact { 64.0 } else { 88.0 }))
            .padding(spacing.space_xs)
            .on_action(Message::EditorAction)
            .key_binding(move |press| {
                let focused = matches!(press.status, text_editor::Status::Focused { .. });
                if focused && matches!(press.key, Key::Named(Named::Enter)) {
                    if press.modifiers.shift() {
                        Some(text_editor::Binding::Enter)
                    } else if can_submit {
                        Some(text_editor::Binding::Custom(Message::Submit))
                    } else if voice_active {
                        None
                    } else {
                        Some(text_editor::Binding::Enter)
                    }
                } else {
                    text_editor::Binding::from_key_press(press)
                }
            });
        let status: Element<'_, Message> = if let Some(metrics) = self.voice.metrics() {
            text(format!(
                "{} · {}",
                fl!("recording"),
                Self::format_duration(metrics.elapsed)
            ))
            .size(11.0)
            .into()
        } else if self.voice.is_processing() {
            text(fl!("transcribing")).size(11.0).into()
        } else {
            self.connection_status()
        };
        let mic = if self.voice.is_recording() {
            Self::symbolic_button(
                "media-playback-stop-symbolic",
                fl!("stop"),
                Some(Message::ToggleMic),
                true,
            )
        } else if self.voice.is_processing() {
            Self::symbolic_button(
                "window-close-symbolic",
                fl!("cancel"),
                Some(Message::CancelVoice),
                true,
            )
        } else {
            Self::symbolic_button(
                "audio-input-microphone-symbolic",
                fl!("microphone"),
                (!self.stream.is_active()).then_some(Message::ToggleMic),
                false,
            )
        };
        let attach = Self::symbolic_button(
            "mail-attachment-symbolic",
            fl!("attach-file"),
            (!self.voice.is_active()).then_some(Message::AttachFile),
            false,
        );
        let action = if self.stream.is_active() {
            Self::symbolic_button(
                "media-playback-stop-symbolic",
                fl!("stop"),
                Some(Message::StopStream),
                true,
            )
        } else if self.stream.is_cancelling() {
            Self::symbolic_button("process-stop-symbolic", fl!("stopping"), None, true)
        } else {
            Self::symbolic_button(
                "mail-send-symbolic",
                fl!("send"),
                (!self.input.text().trim().is_empty()
                    && !self.voice.is_active()
                    && self.active_history_ready())
                .then_some(Message::Submit),
                false,
            )
        };
        let bottom = Row::new()
            .push(status)
            .push(widget::space::horizontal())
            .push(attach)
            .push(mic)
            .push(action)
            .spacing(spacing.space_xs)
            .align_y(Alignment::Center);
        container(
            Column::new()
                .push(editor)
                .push(bottom)
                .spacing(spacing.space_xs),
        )
        .padding(spacing.space_s)
        .class(theme::Container::custom(styles::input_card))
        .into()
    }

    fn connection_status(&self) -> Element<'_, Message> {
        if self.bridge.is_connecting() {
            return text(fl!("bridge-connecting")).size(11.0).into();
        }
        if self.bridge.endpoint().is_none() {
            return Row::new()
                .push(text(fl!("bridge-offline")).size(11.0))
                .push(button::text(fl!("reconnect")).on_press(Message::Reconnect))
                .spacing(6)
                .align_y(Alignment::Center)
                .into();
        }
        if self.bridge.models().is_none() && self.bridge.error().is_some() {
            return Row::new()
                .push(text(fl!("model-unavailable")).size(11.0))
                .push(button::text(fl!("retry")).on_press(Message::Reconnect))
                .spacing(6)
                .align_y(Alignment::Center)
                .into();
        }
        let label = self
            .bridge
            .models()
            .map(Self::provider_model_label)
            .unwrap_or_else(|| fl!("bridge-ready"));
        text(label).size(11.0).into()
    }

    pub(super) fn breadcrumb(&self) -> Element<'_, Message> {
        let spacing = theme::active().cosmic().spacing;
        Row::new()
            .push(Self::brand_symbol(16.0))
            .push(text(fl!("app-name")).size(13.0))
            .push(text("/").size(13.0))
            .push(text(
                self.active_session()
                    .map(LocalSession::display_title)
                    .unwrap_or_else(|| fl!("new-session")),
            ))
            .push(Self::status_dot(self.stream.is_active()))
            .spacing(spacing.space_xxs)
            .align_y(Alignment::Center)
            .into()
    }

    fn sidebar_view(&self) -> Element<'_, Message> {
        let spacing = theme::active().cosmic().spacing;
        let header = Row::new()
            .push(text(fl!("sessions").to_uppercase()).size(11.0))
            .push(widget::space::horizontal())
            .push(Self::symbolic_button(
                "list-add-symbolic",
                fl!("new-session"),
                (!self.stream.is_active()).then_some(Message::NewSession),
                false,
            ))
            .align_y(Alignment::Center);
        let mut list = Column::new().spacing(2);
        for (index, session) in self.sessions.iter().enumerate() {
            list = list.push(Self::session_row(
                session,
                index == self.sessions.active_index(),
                index,
                self.stream.is_active() && self.stream.session_index() == Some(index),
            ));
        }
        if let Some(error) = self.sessions.error() {
            list = list.push(text(error).size(11.0));
        }
        Column::new()
            .push(container(header).padding([
                0u16,
                spacing.space_xs,
                spacing.space_xs,
                spacing.space_xs,
            ]))
            .push(scrollable(list).width(Length::Fill).height(Length::Fill))
            .spacing(spacing.space_xs)
            .into()
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/views.rs"));
}
