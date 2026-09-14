//! Fetched owner-configured accounting and revision-bound drafts.

use cos_agent_protocol::{MonetaryBudgetDraft, MonetaryCurrency};

use super::{
    Action, Activities, ActivityDetailResponse, ActivityMonetaryBudget,
    ActivityMonetaryBudgetEnabledRequest, ActivityMonetaryBudgetResponse,
    ActivityMonetaryBudgetSetRequest, AppMessage, Column, Element, Length,
    Message as ActivityMessage, Request, Row, container, control, destructive, editable, fl,
    section, styles, text, theme, widget,
};

#[derive(Debug, Clone, Copy)]
pub enum Field {
    MaximumTotal,
    InputRate,
    OutputRate,
    MaximumOutputTokens,
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
    pub(super) response: Option<ActivityMonetaryBudgetResponse>,
    pub(super) form: Option<Form>,
}

#[derive(Debug)]
pub(super) struct Form {
    activity_id: String,
    previous: Option<Box<ActivityMonetaryBudget>>,
    maximum_total: String,
    input_rate: String,
    output_rate: String,
    maximum_output_tokens: String,
}

impl Form {
    fn new(activity_id: String, previous: Option<&ActivityMonetaryBudget>) -> Self {
        Self {
            activity_id,
            maximum_total: previous.map_or_else(
                || "5000000".into(),
                |budget| budget.budget.max_total_microusd.to_string(),
            ),
            input_rate: previous.map_or_else(
                || "250000".into(),
                |budget| budget.budget.input_microusd_per_million_tokens.to_string(),
            ),
            output_rate: previous.map_or_else(
                || "1000000".into(),
                |budget| budget.budget.output_microusd_per_million_tokens.to_string(),
            ),
            maximum_output_tokens: previous.map_or_else(
                || "4096".into(),
                |budget| budget.budget.max_output_tokens_per_turn.to_string(),
            ),
            previous: previous.cloned().map(Box::new),
        }
    }

    fn request(&self) -> Result<ActivityMonetaryBudgetSetRequest, String> {
        let parse_u64 = |value: &str, error: String| value.trim().parse::<u64>().map_err(|_| error);
        let request = ActivityMonetaryBudgetSetRequest {
            expected_revision: self.previous.as_ref().map(|budget| budget.revision),
            budget: MonetaryBudgetDraft {
                currency: MonetaryCurrency::Usd,
                max_total_microusd: parse_u64(
                    &self.maximum_total,
                    fl!("activity-monetary-budget-total-invalid"),
                )?,
                input_microusd_per_million_tokens: parse_u64(
                    &self.input_rate,
                    fl!("activity-monetary-budget-input-rate-invalid"),
                )?,
                output_microusd_per_million_tokens: parse_u64(
                    &self.output_rate,
                    fl!("activity-monetary-budget-output-rate-invalid"),
                )?,
                max_output_tokens_per_turn: self
                    .maximum_output_tokens
                    .trim()
                    .parse::<u32>()
                    .map_err(|_| fl!("activity-monetary-budget-output-limit-invalid"))?,
            },
        };
        request.validate_shape().map_err(str::to_owned)?;
        Ok(request)
    }
}

impl Activities {
    pub(super) fn update_monetary_budget(
        &mut self,
        message: Message,
        connected: bool,
    ) -> Option<Request> {
        if !self.visible
            || self.pending.is_some()
            || self.form.is_some()
            || self.object_form.is_some()
            || self.object_state.form.is_some()
            || self.execution_limits.form.is_some()
            || self.capability_policy.form.is_some()
        {
            return None;
        }
        match message {
            Message::Refresh if self.monetary_budget.form.is_none() => {
                return self.refresh_monetary_budget(connected);
            }
            Message::Configure if self.monetary_budget.form.is_none() => {
                let detail = self.detail.as_ref()?;
                if !editable(detail.activity.state) {
                    self.error = Some(fl!("activity-monetary-budget-readonly"));
                    return None;
                }
                let Some(response) = self.monetary_budget.response.as_ref() else {
                    self.error = Some(fl!("activity-monetary-budget-load-first"));
                    return None;
                };
                if self.selected.as_deref() != Some(detail.activity.id.as_str())
                    || !response.matches_activity(&detail.activity.id)
                {
                    self.error = Some(fl!("activity-monetary-budget-invalid-response"));
                    return None;
                }
                self.monetary_budget.form = Some(Form::new(
                    detail.activity.id.clone(),
                    response.monetary_budget.as_ref(),
                ));
                self.invalidate_operation_preview();
                self.error = None;
                self.notice = None;
            }
            Message::Field(field, value) => {
                if let Some(form) = &mut self.monetary_budget.form {
                    match field {
                        Field::MaximumTotal => form.maximum_total = value,
                        Field::InputRate => form.input_rate = value,
                        Field::OutputRate => form.output_rate = value,
                        Field::MaximumOutputTokens => form.maximum_output_tokens = value,
                    }
                }
            }
            Message::Save => return self.save_monetary_budget(connected),
            Message::Discard => {
                self.monetary_budget.form = None;
                self.error = None;
                return self.refresh_monetary_budget(connected);
            }
            Message::SetEnabled(enabled) if self.monetary_budget.form.is_none() => {
                return self.set_monetary_budget_enabled(enabled, connected);
            }
            _ => {}
        }
        None
    }

    fn refresh_monetary_budget(&mut self, connected: bool) -> Option<Request> {
        let id = self.selected.clone()?;
        self.monetary_budget.response = None;
        self.begin(Action::GetMonetaryBudget(id), connected)
    }

    fn save_monetary_budget(&mut self, connected: bool) -> Option<Request> {
        let form = self.monetary_budget.form.as_ref()?;
        let detail = self.detail.as_ref()?;
        if !editable(detail.activity.state) {
            self.error = Some(fl!("activity-monetary-budget-readonly"));
            return None;
        }
        if self.selected.as_deref() != Some(form.activity_id.as_str())
            || detail.activity.id != form.activity_id
        {
            self.error = Some(fl!("activity-monetary-budget-invalid-response"));
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
            Action::SetMonetaryBudget {
                activity_id: form.activity_id.clone(),
                request,
                previous: form.previous.clone(),
            },
            connected,
        )
    }

    fn set_monetary_budget_enabled(&mut self, enabled: bool, connected: bool) -> Option<Request> {
        let detail = self.detail.as_ref()?;
        if enabled && !editable(detail.activity.state) {
            self.error = Some(fl!("activity-monetary-budget-readonly"));
            return None;
        }
        let Some(current) = self
            .monetary_budget
            .response
            .as_ref()
            .and_then(|response| response.monetary_budget.as_ref())
        else {
            self.error = Some(fl!("activity-monetary-budget-load-first"));
            return None;
        };
        if self.selected.as_deref() != Some(detail.activity.id.as_str())
            || !current.matches_activity(&detail.activity.id)
        {
            self.error = Some(fl!("activity-monetary-budget-invalid-response"));
            return None;
        }
        if current.enabled == enabled {
            return None;
        }
        let request = ActivityMonetaryBudgetEnabledRequest {
            expected_revision: current.revision,
            enabled,
        };
        if let Err(error) = request.validate_shape() {
            self.error = Some(error.into());
            return None;
        }
        self.begin(
            Action::EnableMonetaryBudget {
                activity_id: detail.activity.id.clone(),
                request,
                previous: Box::new(current.clone()),
            },
            connected,
        )
    }

    pub(super) fn monetary_budget_loaded(
        &mut self,
        activity_id: &str,
        response: ActivityMonetaryBudgetResponse,
    ) {
        if self.visible
            && self.selected.as_deref() == Some(activity_id)
            && self.monetary_budget.form.is_none()
            && response.matches_activity(activity_id)
        {
            self.monetary_budget.response = Some(response);
        } else {
            self.error = Some(fl!("activity-monetary-budget-invalid-response"));
        }
    }

    pub(super) fn monetary_budget_saved(
        &mut self,
        activity_id: &str,
        request: &ActivityMonetaryBudgetSetRequest,
        previous: Option<&ActivityMonetaryBudget>,
        budget: ActivityMonetaryBudget,
        connected: bool,
    ) -> Option<Request> {
        let valid = self.visible
            && self.selected.as_deref() == Some(activity_id)
            && self.monetary_budget.form.as_ref().is_some_and(|form| {
                form.activity_id == activity_id
                    && form.previous.as_deref() == previous
                    && form.request().is_ok_and(|current| current == *request)
            })
            && budget.matches_set(activity_id, request)
            && previous.is_none_or(|previous| {
                request.expected_revision == Some(previous.revision)
                    && budget.enabled == previous.enabled
                    && budget.preserves_identity(previous)
            });
        if !valid {
            self.error = Some(fl!("activity-monetary-budget-invalid-response"));
            return None;
        }
        self.monetary_budget.form = None;
        self.notice = Some(fl!(
            "activity-monetary-budget-saved",
            revision = budget.revision.to_string()
        ));
        self.refresh_monetary_budget(connected)
    }

    pub(super) fn monetary_budget_enabled(
        &mut self,
        activity_id: &str,
        request: &ActivityMonetaryBudgetEnabledRequest,
        previous: &ActivityMonetaryBudget,
        budget: ActivityMonetaryBudget,
        connected: bool,
    ) -> Option<Request> {
        let current = self
            .monetary_budget
            .response
            .as_ref()
            .and_then(|response| response.monetary_budget.as_ref());
        if !self.visible
            || self.selected.as_deref() != Some(activity_id)
            || current != Some(previous)
            || request.expected_revision != previous.revision
            || !budget.matches_enabled(activity_id, request)
            || !budget.preserves_identity(previous)
            || budget.budget != previous.budget
        {
            self.error = Some(fl!("activity-monetary-budget-invalid-response"));
            return None;
        }
        self.notice = Some(if budget.enabled {
            fl!(
                "activity-monetary-budget-enabled-notice",
                revision = budget.revision.to_string()
            )
        } else {
            fl!(
                "activity-monetary-budget-disabled-notice",
                revision = budget.revision.to_string()
            )
        });
        self.refresh_monetary_budget(connected)
    }

    pub(super) fn monetary_budget_view<'a>(
        &'a self,
        detail: &'a ActivityDetailResponse,
        available: bool,
    ) -> Element<'a, AppMessage> {
        let can_configure = editable(detail.activity.state);
        let mut card = Column::new()
            .spacing(10)
            .push(text(fl!("activity-monetary-budget")).size(18.0))
            .push(text(fl!("activity-monetary-budget-accounting-hint")).size(12.0))
            .push(text(fl!("activity-monetary-budget-reservation-hint")).size(12.0))
            .push(text(fl!("activity-monetary-budget-authority-hint")).size(12.0));
        if let Some(form) = &self.monetary_budget.form {
            card = card.push(self.monetary_budget_form_view(form, available && can_configure));
        } else {
            card = card.push(
                Row::new()
                    .spacing(8)
                    .push(budget_control(
                        fl!("activity-monetary-budget-refresh"),
                        Message::Refresh,
                        available,
                    ))
                    .push(budget_control(
                        fl!("activity-monetary-budget-configure"),
                        Message::Configure,
                        available && can_configure && self.monetary_budget.response.is_some(),
                    )),
            );
        }
        if !can_configure {
            card = card.push(text(fl!("activity-monetary-budget-readonly")).size(12.0));
        }
        match self.monetary_budget.response.as_ref() {
            None => card = card.push(text(fl!("activity-monetary-budget-load-first")).size(12.0)),
            Some(response) => match &response.monetary_budget {
                None => {
                    card = card.push(text(fl!("activity-monetary-budget-unconfigured")).size(13.0))
                }
                Some(budget) => {
                    card = card.push(budget_summary(budget)).push(budget_control(
                        if budget.enabled {
                            fl!("activity-monetary-budget-disable")
                        } else {
                            fl!("activity-monetary-budget-enable")
                        },
                        Message::SetEnabled(!budget.enabled),
                        available
                            && self.monetary_budget.form.is_none()
                            && (budget.enabled || can_configure),
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

    fn monetary_budget_form_view<'a>(
        &'a self,
        form: &'a Form,
        available: bool,
    ) -> Element<'a, AppMessage> {
        Column::new()
            .spacing(8)
            .push(section(fl!("activity-monetary-budget-currency"), "USD"))
            .push(input(
                fl!("activity-monetary-budget-maximum-total"),
                &form.maximum_total,
                Field::MaximumTotal,
            ))
            .push(input(
                fl!("activity-monetary-budget-input-rate"),
                &form.input_rate,
                Field::InputRate,
            ))
            .push(input(
                fl!("activity-monetary-budget-output-rate"),
                &form.output_rate,
                Field::OutputRate,
            ))
            .push(input(
                fl!("activity-monetary-budget-output-limit"),
                &form.maximum_output_tokens,
                Field::MaximumOutputTokens,
            ))
            .push(text(fl!("activity-monetary-budget-cas-hint")).size(12.0))
            .push(
                text(match &form.previous {
                    Some(previous) => fl!(
                        "activity-monetary-budget-expected-revision",
                        revision = previous.revision.to_string()
                    ),
                    None => fl!("activity-monetary-budget-initial"),
                })
                .size(13.0),
            )
            .push(
                Row::new()
                    .spacing(8)
                    .push(budget_control(
                        fl!("activity-monetary-budget-save"),
                        Message::Save,
                        available,
                    ))
                    .push(budget_control(
                        fl!("activity-monetary-budget-discard"),
                        Message::Discard,
                        self.pending.is_none(),
                    )),
            )
            .into()
    }
}

fn budget_summary(budget: &ActivityMonetaryBudget) -> Element<'_, AppMessage> {
    let mut summary = Column::new()
        .spacing(6)
        .push(
            text(if budget.enabled {
                fl!("activity-monetary-budget-enabled")
            } else {
                fl!("activity-monetary-budget-disabled")
            })
            .size(15.0),
        )
        .push(
            text(fl!(
                "activity-monetary-budget-total",
                value = format_microusd(budget.budget.max_total_microusd)
            ))
            .size(14.0),
        )
        .push(
            text(fl!(
                "activity-monetary-budget-spent",
                value = format_microusd(budget.spent_microusd)
            ))
            .size(14.0),
        )
        .push(
            text(fl!(
                "activity-monetary-budget-reserved",
                value = format_microusd(budget.reserved_microusd)
            ))
            .size(14.0),
        )
        .push(
            text(fl!(
                "activity-monetary-budget-remaining",
                value = format_microusd(budget.remaining_microusd())
            ))
            .size(14.0),
        )
        .push(
            text(fl!(
                "activity-monetary-budget-input-rate-summary",
                value = budget.budget.input_microusd_per_million_tokens.to_string()
            ))
            .size(13.0),
        )
        .push(
            text(fl!(
                "activity-monetary-budget-output-rate-summary",
                value = budget.budget.output_microusd_per_million_tokens.to_string()
            ))
            .size(13.0),
        )
        .push(
            text(fl!(
                "activity-monetary-budget-output-limit-summary",
                value = budget.budget.max_output_tokens_per_turn.to_string()
            ))
            .size(13.0),
        )
        .push(
            text(fl!(
                "activity-monetary-budget-revision",
                revision = budget.revision.to_string()
            ))
            .size(12.0),
        )
        .push(section(
            fl!("activity-monetary-budget-created"),
            &budget.created_at,
        ))
        .push(section(
            fl!("activity-monetary-budget-updated"),
            &budget.updated_at,
        ))
        .push(text(fl!("activity-monetary-budget-snapshot-hint")).size(12.0));
    if budget.remaining_microusd() == 0 {
        summary = summary.push(text(fl!("activity-monetary-budget-exhausted")).size(13.0));
    }
    summary.into()
}

fn format_microusd(value: u64) -> String {
    format!("USD {}.{:06}", value / 1_000_000, value % 1_000_000)
}

fn budget_control(label: String, message: Message, enabled: bool) -> Element<'static, AppMessage> {
    if matches!(message, Message::SetEnabled(false)) {
        destructive(label, ActivityMessage::MonetaryBudget(message), enabled)
    } else {
        control(label, ActivityMessage::MonetaryBudget(message), enabled)
    }
}

fn input(label: String, value: &str, field: Field) -> Element<'_, AppMessage> {
    Column::new()
        .spacing(4)
        .push(text(label.clone()).size(13.0))
        .push(widget::text_input(label, value).on_input(move |value| {
            AppMessage::Activities(ActivityMessage::MonetaryBudget(Message::Field(
                field, value,
            )))
        }))
        .into()
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/monetary_budget.rs"
    ));
}
