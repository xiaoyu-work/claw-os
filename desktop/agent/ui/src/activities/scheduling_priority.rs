//! Fetched scheduling metadata and an unsaved exact-revision form.

use super::{
    Action, Activities, ActivityDetailResponse, ActivitySchedulingPolicy,
    ActivitySchedulingPriority, ActivitySchedulingPriorityResponse,
    ActivitySchedulingPrioritySetRequest, AppMessage, Column, Element, Length,
    Message as ActivityMessage, Request, Row, container, control, editable, fl, styles, text,
    theme,
};

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    Configure,
    Select(ActivitySchedulingPriority),
    Save,
    Discard,
}

#[derive(Debug, Default)]
pub(super) struct State {
    pub(super) response: Option<ActivitySchedulingPriorityResponse>,
    pub(super) form: Option<Form>,
}

#[derive(Debug)]
pub(super) struct Form {
    activity_id: String,
    previous: Option<Box<ActivitySchedulingPolicy>>,
    priority: ActivitySchedulingPriority,
}

impl Form {
    fn new(activity_id: String, previous: Option<&ActivitySchedulingPolicy>) -> Self {
        Self {
            activity_id,
            previous: previous.cloned().map(Box::new),
            priority: previous.map_or(ActivitySchedulingPriority::Standard, |policy| {
                policy.priority
            }),
        }
    }

    fn request(&self) -> ActivitySchedulingPrioritySetRequest {
        ActivitySchedulingPrioritySetRequest {
            expected_revision: self.previous.as_ref().map(|policy| policy.revision),
            priority: self.priority,
        }
    }
}

impl Activities {
    pub(super) fn update_scheduling_priority(
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
            || self.monetary_budget.form.is_some()
            || self.capability_policy.form.is_some()
        {
            return None;
        }
        match message {
            Message::Refresh if self.scheduling_priority.form.is_none() => {
                return self.refresh_scheduling_priority(connected);
            }
            Message::Configure if self.scheduling_priority.form.is_none() => {
                let detail = self.detail.as_ref()?;
                if !editable(detail.activity.state) {
                    self.error = Some(fl!("activity-scheduling-priority-readonly"));
                    return None;
                }
                let Some(response) = self.scheduling_priority.response.as_ref() else {
                    self.error = Some(fl!("activity-scheduling-priority-load-first"));
                    return None;
                };
                if self.selected.as_deref() != Some(detail.activity.id.as_str())
                    || !response.matches_activity(&detail.activity.id)
                {
                    self.error = Some(fl!("activity-scheduling-priority-invalid-response"));
                    return None;
                }
                self.scheduling_priority.form = Some(Form::new(
                    detail.activity.id.clone(),
                    response.scheduling_policy.as_ref(),
                ));
                self.invalidate_operation_preview();
                self.error = None;
                self.notice = None;
            }
            Message::Select(priority) => {
                if let Some(form) = &mut self.scheduling_priority.form {
                    form.priority = priority;
                }
            }
            Message::Save => return self.save_scheduling_priority(connected),
            Message::Discard => {
                self.scheduling_priority.form = None;
                self.error = None;
                return self.refresh_scheduling_priority(connected);
            }
            _ => {}
        }
        None
    }

    fn refresh_scheduling_priority(&mut self, connected: bool) -> Option<Request> {
        let id = self.selected.clone()?;
        self.scheduling_priority.response = None;
        self.begin(Action::GetSchedulingPriority(id), connected)
    }

    fn save_scheduling_priority(&mut self, connected: bool) -> Option<Request> {
        let form = self.scheduling_priority.form.as_ref()?;
        let detail = self.detail.as_ref()?;
        if !editable(detail.activity.state) {
            self.error = Some(fl!("activity-scheduling-priority-readonly"));
            return None;
        }
        if self.selected.as_deref() != Some(form.activity_id.as_str())
            || detail.activity.id != form.activity_id
        {
            self.error = Some(fl!("activity-scheduling-priority-invalid-response"));
            return None;
        }
        let request = form.request();
        if let Err(error) = request.validate_shape() {
            self.error = Some(error.into());
            return None;
        }
        self.begin(
            Action::SetSchedulingPriority {
                activity_id: form.activity_id.clone(),
                request,
                previous: form.previous.clone(),
            },
            connected,
        )
    }

    pub(super) fn scheduling_priority_loaded(
        &mut self,
        activity_id: &str,
        response: ActivitySchedulingPriorityResponse,
    ) {
        if self.visible
            && self.selected.as_deref() == Some(activity_id)
            && self.scheduling_priority.form.is_none()
            && response.matches_activity(activity_id)
        {
            self.scheduling_priority.response = Some(response);
        } else {
            self.error = Some(fl!("activity-scheduling-priority-invalid-response"));
        }
    }

    pub(super) fn scheduling_priority_saved(
        &mut self,
        activity_id: &str,
        request: &ActivitySchedulingPrioritySetRequest,
        previous: Option<&ActivitySchedulingPolicy>,
        policy: ActivitySchedulingPolicy,
        connected: bool,
    ) -> Option<Request> {
        let valid = self.visible
            && self.selected.as_deref() == Some(activity_id)
            && self.scheduling_priority.form.as_ref().is_some_and(|form| {
                form.activity_id == activity_id
                    && form.previous.as_deref() == previous
                    && form.request() == *request
            })
            && policy.matches_set(activity_id, request)
            && previous.is_none_or(|previous| {
                request.expected_revision == Some(previous.revision)
                    && policy.preserves_identity(previous)
            });
        if !valid {
            self.error = Some(fl!("activity-scheduling-priority-invalid-response"));
            return None;
        }
        self.scheduling_priority.form = None;
        self.notice = Some(fl!(
            "activity-scheduling-priority-saved",
            priority = priority_label(policy.priority),
            revision = policy.revision.to_string()
        ));
        self.refresh_scheduling_priority(connected)
    }

    pub(super) fn scheduling_priority_view<'a>(
        &'a self,
        detail: &'a ActivityDetailResponse,
        available: bool,
    ) -> Element<'a, AppMessage> {
        let can_configure = editable(detail.activity.state);
        let mut card = Column::new()
            .spacing(10)
            .push(text(fl!("activity-scheduling-priority")).size(18.0))
            .push(text(fl!("activity-scheduling-priority-semantics")).size(12.0))
            .push(text(fl!("activity-scheduling-priority-aging")).size(12.0))
            .push(text(fl!("activity-scheduling-priority-authority")).size(12.0));
        if let Some(form) = &self.scheduling_priority.form {
            card = card.push(self.scheduling_priority_form_view(form, available && can_configure));
        } else {
            card = card.push(
                Row::new()
                    .spacing(8)
                    .push(priority_control(
                        fl!("activity-scheduling-priority-refresh"),
                        Message::Refresh,
                        available,
                    ))
                    .push(priority_control(
                        fl!("activity-scheduling-priority-configure"),
                        Message::Configure,
                        available && can_configure && self.scheduling_priority.response.is_some(),
                    )),
            );
        }
        if !can_configure {
            card = card.push(text(fl!("activity-scheduling-priority-readonly")).size(12.0));
        }
        match self.scheduling_priority.response.as_ref() {
            None => {
                card = card.push(text(fl!("activity-scheduling-priority-load-first")).size(12.0))
            }
            Some(response) => match &response.scheduling_policy {
                None => {
                    card =
                        card.push(text(fl!("activity-scheduling-priority-unconfigured")).size(13.0))
                }
                Some(policy) => {
                    card = card
                        .push(priority_section(
                            fl!("activity-scheduling-priority-current"),
                            priority_label(policy.priority),
                        ))
                        .push(text(revision_label(policy.revision)).size(12.0));
                }
            },
        }
        container(card)
            .padding(12)
            .width(Length::Fill)
            .class(theme::Container::custom(styles::tool_card))
            .into()
    }

    fn scheduling_priority_form_view<'a>(
        &'a self,
        form: &'a Form,
        available: bool,
    ) -> Element<'a, AppMessage> {
        let selected = priority_label(form.priority);
        Column::new()
            .spacing(8)
            .push(priority_section(
                fl!("activity-scheduling-priority-selected"),
                selected,
            ))
            .push(
                Row::new()
                    .spacing(8)
                    .push(priority_control(
                        fl!("activity-scheduling-priority-foreground"),
                        Message::Select(ActivitySchedulingPriority::Foreground),
                        available,
                    ))
                    .push(priority_control(
                        fl!("activity-scheduling-priority-standard"),
                        Message::Select(ActivitySchedulingPriority::Standard),
                        available,
                    ))
                    .push(priority_control(
                        fl!("activity-scheduling-priority-background"),
                        Message::Select(ActivitySchedulingPriority::Background),
                        available,
                    )),
            )
            .push(text(fl!("activity-scheduling-priority-cas")).size(12.0))
            .push(
                text(match &form.previous {
                    Some(previous) => fl!(
                        "activity-scheduling-priority-expected-revision",
                        revision = previous.revision.to_string()
                    ),
                    None => fl!("activity-scheduling-priority-initial"),
                })
                .size(12.0),
            )
            .push(
                Row::new()
                    .spacing(8)
                    .push(priority_control(
                        fl!("activity-scheduling-priority-save"),
                        Message::Save,
                        available,
                    ))
                    .push(priority_control(
                        fl!("activity-scheduling-priority-discard"),
                        Message::Discard,
                        available,
                    )),
            )
            .into()
    }
}

fn priority_control(
    label: String,
    message: Message,
    enabled: bool,
) -> Element<'static, AppMessage> {
    control(label, ActivityMessage::SchedulingPriority(message), enabled)
}

fn priority_label(priority: ActivitySchedulingPriority) -> String {
    match priority {
        ActivitySchedulingPriority::Foreground => fl!("activity-scheduling-priority-foreground"),
        ActivitySchedulingPriority::Standard => fl!("activity-scheduling-priority-standard"),
        ActivitySchedulingPriority::Background => fl!("activity-scheduling-priority-background"),
    }
}

fn revision_label(revision: u64) -> String {
    fl!(
        "activity-scheduling-priority-revision",
        revision = revision.to_string()
    )
}

fn priority_section(label: String, value: impl Into<String>) -> Element<'static, AppMessage> {
    Column::new()
        .spacing(4)
        .push(text(label).size(14.0))
        .push(text(value.into()).size(13.0))
        .into()
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/scheduling_priority.rs"
    ));
}
