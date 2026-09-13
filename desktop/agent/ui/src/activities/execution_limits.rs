//! Fetched constraints and CAS-bound drafts, not permission or lifecycle policy.

use std::time::SystemTime;

use cos_agent_protocol::ExecutionLimitsDraft;

use super::{
    Action, Activities, ActivityDetailResponse, ActivityExecutionLimits,
    ActivityExecutionLimitsEnabledRequest, ActivityExecutionLimitsResponse,
    ActivityExecutionLimitsSetRequest, AppMessage, Column, Element, Length,
    Message as ActivityMessage, Request, Row, container, control, destructive, editable,
    error_card, fl, section, styles, text, theme, widget,
};

#[derive(Debug, Clone, Copy)]
pub enum Field {
    MaxAttempts,
    MaxTurns,
    ExpiresAt,
}

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    Configure,
    Field(Field, String),
    Save,
    Discard,
    SetEnabled(bool),
}

#[derive(Debug, Default)]
pub(super) struct State {
    pub(super) response: Option<ActivityExecutionLimitsResponse>,
    pub(super) form: Option<Form>,
}

#[derive(Debug)]
pub(super) struct Form {
    activity_id: String,
    previous: Option<Box<ActivityExecutionLimits>>,
    max_attempts: String,
    max_turns: String,
    expires_at: String,
}

impl Form {
    fn new(activity_id: String, previous: Option<&ActivityExecutionLimits>) -> Self {
        Self {
            activity_id,
            max_attempts: previous.map_or_else(
                || "1".into(),
                |limits| limits.limits.max_attempts.to_string(),
            ),
            max_turns: previous.map_or_else(
                || "20".into(),
                |limits| limits.limits.max_turns_per_attempt.to_string(),
            ),
            expires_at: previous
                .map_or_else(String::new, |limits| limits.limits.expires_at.clone()),
            previous: previous.cloned().map(Box::new),
        }
    }

    fn request(&self) -> Result<ActivityExecutionLimitsSetRequest, String> {
        let max_attempts = self
            .max_attempts
            .trim()
            .parse::<u32>()
            .map_err(|_| fl!("activity-execution-limits-attempts-invalid"))?;
        let max_turns_per_attempt = self
            .max_turns
            .trim()
            .parse::<u32>()
            .map_err(|_| fl!("activity-execution-limits-turns-invalid"))?;
        let request = ActivityExecutionLimitsSetRequest {
            expected_revision: self.previous.as_ref().map(|limits| limits.revision),
            limits: ExecutionLimitsDraft {
                max_attempts,
                max_turns_per_attempt,
                expires_at: self.expires_at.trim().to_owned(),
            },
        };
        request.validate_shape().map_err(str::to_owned)?;
        Ok(request)
    }
}

impl Activities {
    pub(super) fn update_execution_limits(
        &mut self,
        message: Message,
        connected: bool,
    ) -> Option<Request> {
        if !self.visible
            || self.pending.is_some()
            || self.form.is_some()
            || self.object_form.is_some()
            || self.object_state.form.is_some()
        {
            return None;
        }
        match message {
            Message::Refresh if self.execution_limits.form.is_none() => {
                return self.refresh_execution_limits(connected);
            }
            Message::Configure if self.execution_limits.form.is_none() => {
                let detail = self.detail.as_ref()?;
                if !editable(detail.activity.state) {
                    self.error = Some(fl!("activity-execution-limits-readonly"));
                    return None;
                }
                let Some(response) = self.execution_limits.response.as_ref() else {
                    self.error = Some(fl!("activity-execution-limits-load-first"));
                    return None;
                };
                if self.selected.as_deref() != Some(detail.activity.id.as_str())
                    || !response.matches_activity(&detail.activity.id)
                {
                    self.error = Some(fl!("activity-execution-limits-invalid-response"));
                    return None;
                }
                self.execution_limits.form = Some(Form::new(
                    detail.activity.id.clone(),
                    response.execution_limits.as_ref(),
                ));
                self.invalidate_operation_preview();
                self.error = None;
                self.notice = None;
            }
            Message::Field(field, value) => {
                if let Some(form) = &mut self.execution_limits.form {
                    match field {
                        Field::MaxAttempts => form.max_attempts = value,
                        Field::MaxTurns => form.max_turns = value,
                        Field::ExpiresAt => form.expires_at = value,
                    }
                }
            }
            Message::Save => return self.save_execution_limits(connected),
            Message::Discard => {
                self.execution_limits.form = None;
                self.error = None;
                return self.refresh_execution_limits(connected);
            }
            Message::SetEnabled(enabled) if self.execution_limits.form.is_none() => {
                return self.set_execution_limits_enabled(enabled, connected);
            }
            _ => {}
        }
        None
    }

    fn refresh_execution_limits(&mut self, connected: bool) -> Option<Request> {
        let id = self.selected.clone()?;
        self.execution_limits.response = None;
        self.begin(Action::GetExecutionLimits(id), connected)
    }

    fn save_execution_limits(&mut self, connected: bool) -> Option<Request> {
        let form = self.execution_limits.form.as_ref()?;
        let detail = self.detail.as_ref()?;
        if !editable(detail.activity.state) {
            self.error = Some(fl!("activity-execution-limits-readonly"));
            return None;
        }
        if self.selected.as_deref() != Some(form.activity_id.as_str())
            || detail.activity.id != form.activity_id
        {
            self.error = Some(fl!("activity-execution-limits-invalid-response"));
            return None;
        }
        let request = match form.request() {
            Ok(request) => request,
            Err(error) => {
                self.error = Some(error);
                return None;
            }
        };
        self.begin(
            Action::SetExecutionLimits {
                activity_id: form.activity_id.clone(),
                request,
                previous: form.previous.clone(),
            },
            connected,
        )
    }

    fn set_execution_limits_enabled(&mut self, enabled: bool, connected: bool) -> Option<Request> {
        let detail = self.detail.as_ref()?;
        if enabled && !editable(detail.activity.state) {
            self.error = Some(fl!("activity-execution-limits-readonly"));
            return None;
        }
        let Some(current) = self
            .execution_limits
            .response
            .as_ref()
            .and_then(|response| response.execution_limits.as_ref())
        else {
            self.error = Some(fl!("activity-execution-limits-load-first"));
            return None;
        };
        if self.selected.as_deref() != Some(detail.activity.id.as_str())
            || !current.matches_activity(&detail.activity.id)
        {
            self.error = Some(fl!("activity-execution-limits-invalid-response"));
            return None;
        }
        if current.enabled == enabled {
            return None;
        }
        let request = ActivityExecutionLimitsEnabledRequest {
            expected_revision: current.revision,
            enabled,
        };
        if let Err(error) = request.validate_shape() {
            self.error = Some(error.into());
            return None;
        }
        self.begin(
            Action::EnableExecutionLimits {
                activity_id: detail.activity.id.clone(),
                request,
                previous: Box::new(current.clone()),
            },
            connected,
        )
    }

    pub(super) fn execution_limits_loaded(
        &mut self,
        activity_id: &str,
        response: ActivityExecutionLimitsResponse,
    ) {
        if self.visible
            && self.selected.as_deref() == Some(activity_id)
            && self.execution_limits.form.is_none()
            && response.matches_activity(activity_id)
        {
            self.execution_limits.response = Some(response);
        } else {
            self.error = Some(fl!("activity-execution-limits-invalid-response"));
        }
    }

    pub(super) fn execution_limits_saved(
        &mut self,
        activity_id: &str,
        request: &ActivityExecutionLimitsSetRequest,
        previous: Option<&ActivityExecutionLimits>,
        limits: ActivityExecutionLimits,
        connected: bool,
    ) -> Option<Request> {
        let valid = self.visible
            && self.selected.as_deref() == Some(activity_id)
            && self.execution_limits.form.as_ref().is_some_and(|form| {
                form.activity_id == activity_id
                    && form.previous.as_deref() == previous
                    && form.request().is_ok_and(|current| current == *request)
            })
            && limits.matches_set(activity_id, request)
            && previous.is_none_or(|previous| {
                request.expected_revision == Some(previous.revision)
                    && limits.enabled == previous.enabled
                    && limits.preserves_lifetime(previous)
            });
        if !valid {
            self.error = Some(fl!("activity-execution-limits-invalid-response"));
            return None;
        }
        self.execution_limits.form = None;
        self.notice = Some(fl!(
            "activity-execution-limits-saved",
            revision = limits.revision.to_string()
        ));
        self.refresh_execution_limits(connected)
    }

    pub(super) fn execution_limits_enabled(
        &mut self,
        activity_id: &str,
        request: &ActivityExecutionLimitsEnabledRequest,
        previous: &ActivityExecutionLimits,
        limits: ActivityExecutionLimits,
        connected: bool,
    ) -> Option<Request> {
        let current = self
            .execution_limits
            .response
            .as_ref()
            .and_then(|response| response.execution_limits.as_ref());
        if !self.visible
            || self.selected.as_deref() != Some(activity_id)
            || current != Some(previous)
            || request.expected_revision != previous.revision
            || !limits.matches_enabled(activity_id, request)
            || !limits.preserves_lifetime(previous)
            || !limits.limits.matches_normalized(&previous.limits)
        {
            self.error = Some(fl!("activity-execution-limits-invalid-response"));
            return None;
        }
        self.notice = Some(if limits.enabled {
            fl!(
                "activity-execution-limits-enabled-notice",
                revision = limits.revision.to_string()
            )
        } else {
            fl!(
                "activity-execution-limits-disabled-notice",
                revision = limits.revision.to_string()
            )
        });
        self.refresh_execution_limits(connected)
    }

    pub(super) fn execution_limits_view<'a>(
        &'a self,
        detail: &'a ActivityDetailResponse,
        available: bool,
    ) -> Element<'a, AppMessage> {
        let can_configure = editable(detail.activity.state);
        let mut card = Column::new()
            .spacing(10)
            .push(text(fl!("activity-execution-limits")).size(18.0))
            .push(text(fl!("activity-execution-limits-authority-hint")).size(12.0))
            .push(text(fl!("activity-execution-limits-reservations-hint")).size(12.0))
            .push(text(fl!("activity-execution-limits-cancellation-hint")).size(12.0))
            .push(text(fl!("activity-execution-limits-disable-hint")).size(12.0));
        if let Some(form) = &self.execution_limits.form {
            card = card.push(self.execution_limits_form_view(form, available && can_configure));
        } else {
            card = card.push(
                Row::new()
                    .spacing(8)
                    .push(limits_control(
                        fl!("activity-execution-limits-refresh"),
                        Message::Refresh,
                        available,
                    ))
                    .push(limits_control(
                        fl!("activity-execution-limits-configure"),
                        Message::Configure,
                        available && can_configure && self.execution_limits.response.is_some(),
                    )),
            );
        }
        if !can_configure {
            card = card.push(text(fl!("activity-execution-limits-readonly")).size(12.0));
        }
        match self.execution_limits.response.as_ref() {
            None => card = card.push(text(fl!("activity-execution-limits-load-first")).size(12.0)),
            Some(response) => match &response.execution_limits {
                None => {
                    card = card.push(text(fl!("activity-execution-limits-unconfigured")).size(13.0))
                }
                Some(limits) => {
                    card =
                        card.push(limits_summary(limits, SystemTime::now()))
                            .push(limits_control(
                                if limits.enabled {
                                    fl!("activity-execution-limits-disable")
                                } else {
                                    fl!("activity-execution-limits-enable")
                                },
                                Message::SetEnabled(!limits.enabled),
                                available
                                    && self.execution_limits.form.is_none()
                                    && (limits.enabled || can_configure),
                            ));
                }
            },
        }
        container(card)
            .padding(12)
            .width(Length::Fill)
            .class(theme::Container::custom(styles::tool_card))
            .into()
    }

    fn execution_limits_form_view<'a>(
        &'a self,
        form: &'a Form,
        available: bool,
    ) -> Element<'a, AppMessage> {
        let mut form_view = Column::new()
            .spacing(8)
            .push(input(
                fl!("activity-execution-limits-max-attempts"),
                &form.max_attempts,
                Field::MaxAttempts,
            ))
            .push(input(
                fl!("activity-execution-limits-max-turns"),
                &form.max_turns,
                Field::MaxTurns,
            ))
            .push(input(
                fl!("activity-execution-limits-expiry"),
                &form.expires_at,
                Field::ExpiresAt,
            ))
            .push(text(fl!("activity-execution-limits-expiry-hint")).size(12.0))
            .push(text(fl!("activity-execution-limits-cas-hint")).size(12.0))
            .push(text(fl!("activity-execution-limits-no-reset-hint")).size(12.0));
        form_view = form_view.push(
            text(match &form.previous {
                Some(previous) => fl!(
                    "activity-execution-limits-expected-revision",
                    revision = previous.revision.to_string()
                ),
                None => fl!("activity-execution-limits-initial"),
            })
            .size(13.0),
        );
        form_view
            .push(
                Row::new()
                    .spacing(8)
                    .push(limits_control(
                        fl!("activity-execution-limits-save"),
                        Message::Save,
                        available,
                    ))
                    .push(limits_control(
                        fl!("activity-execution-limits-discard"),
                        Message::Discard,
                        self.pending.is_none(),
                    )),
            )
            .into()
    }
}

fn limits_summary(limits: &ActivityExecutionLimits, now: SystemTime) -> Element<'_, AppMessage> {
    let mut summary = Column::new()
        .spacing(6)
        .push(text(enabled_label(limits.enabled)).size(15.0))
        .push(
            text(fl!(
                "activity-execution-limits-attempt-count",
                used = limits.used_attempts.to_string(),
                maximum = limits.limits.max_attempts.to_string(),
                remaining = limits.remaining_attempts().to_string()
            ))
            .size(14.0),
        )
        .push(
            text(fl!(
                "activity-execution-limits-turn-count",
                maximum = limits.limits.max_turns_per_attempt.to_string()
            ))
            .size(13.0),
        )
        .push(section(
            fl!("activity-execution-limits-expiry"),
            &limits.limits.expires_at,
        ))
        .push(
            text(fl!(
                "activity-execution-limits-revision",
                revision = limits.revision.to_string()
            ))
            .size(12.0),
        )
        .push(section(
            fl!("activity-execution-limits-created"),
            &limits.created_at,
        ))
        .push(section(
            fl!("activity-execution-limits-updated"),
            &limits.updated_at,
        ))
        .push(text(fl!("activity-execution-limits-snapshot-hint")).size(12.0));
    match limits.limits.is_expired_at(now) {
        Ok(expired) => summary = summary.push(text(expiry_label(expired)).size(13.0)),
        Err(error) => summary = summary.push(error_card(error)),
    }
    if limits.remaining_attempts() == 0 {
        summary = summary.push(text(fl!("activity-execution-limits-exhausted")).size(13.0));
    }
    summary.into()
}

fn enabled_label(enabled: bool) -> String {
    if enabled {
        fl!("activity-execution-limits-enabled")
    } else {
        fl!("activity-execution-limits-disabled")
    }
}

fn expiry_label(expired: bool) -> String {
    if expired {
        fl!("activity-execution-limits-expired")
    } else {
        fl!("activity-execution-limits-not-expired")
    }
}

fn limits_control(label: String, message: Message, enabled: bool) -> Element<'static, AppMessage> {
    if matches!(message, Message::SetEnabled(false)) {
        destructive(label, ActivityMessage::ExecutionLimits(message), enabled)
    } else {
        control(label, ActivityMessage::ExecutionLimits(message), enabled)
    }
}

fn input(label: String, value: &str, field: Field) -> Element<'_, AppMessage> {
    Column::new()
        .spacing(4)
        .push(text(label.clone()).size(13.0))
        .push(widget::text_input(label, value).on_input(move |value| {
            AppMessage::Activities(ActivityMessage::ExecutionLimits(Message::Field(
                field, value,
            )))
        }))
        .into()
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/execution_limits.rs"
    ));
}
