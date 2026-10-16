//! Fetched work state and explicit, revision-bound owner controls only.

use super::{
    Action, Activities, ActivityDetailResponse, AppMessage, Column, Element, Length,
    Message as ActivityMessage, Request, Row, container, control, editable, fl, styles, text,
    theme, widget,
};
use cos_agent_protocol::{
    ActivityAutoWork, ActivityAutoWorkResponse, AutoWorkDraft, AutoWorkEnabledRequest,
    AutoWorkEvent, AutoWorkPhase, AutoWorkSetRequest,
};

#[derive(Debug, Clone, Copy)]
pub enum Field {
    Task,
    Interval,
    Source,
    EventType,
    ReviewNote,
}

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    Configure,
    Review,
    Start,
    Stop,
    Field(Field, String),
    Save,
    Discard,
}

#[derive(Debug, Default)]
pub(super) struct State {
    pub(super) response: Option<ActivityAutoWorkResponse>,
    pub(super) form: Option<Form>,
}

impl State {
    pub(super) fn is_editing(&self) -> bool {
        self.form.is_some()
    }
}

#[derive(Debug)]
pub(super) struct Form {
    id: String,
    revision: Option<u64>,
    review: bool,
    task: String,
    interval: String,
    source: String,
    event_type: String,
    note: String,
}

impl Activities {
    pub(super) fn refresh_automatic_work(&mut self, connected: bool) -> Option<Request> {
        let id = self.selected.clone()?;
        self.automatic_work.response = None;
        self.begin(Action::GetAutomaticWork(id), connected)
    }

    pub(super) fn update_automatic_work(
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
            || self.scheduling_priority.form.is_some()
            || self.capability_policy.form.is_some()
            || self.continuity.is_editing()
        {
            return None;
        }
        match message {
            Message::Refresh if !self.automatic_work.is_editing() => {
                return self.refresh_automatic_work(connected);
            }
            Message::Configure | Message::Review => {
                let detail = self.detail.as_ref()?;
                if !editable(detail.activity.state) {
                    self.error = Some(fl!("activity-auto-work-readonly"));
                    return None;
                }
                let view = self.automatic_work.response.as_ref()?;
                if !view.matches_activity(&detail.activity.id) {
                    return None;
                }
                let work = view.work.as_ref();
                if work.is_some_and(|work| work.current.is_some()) {
                    return None;
                }
                let review = matches!(message, Message::Review);
                if review
                    && work.is_none_or(|work| {
                        !matches!(work.phase, AutoWorkPhase::Review | AutoWorkPhase::Held)
                    })
                {
                    return None;
                }
                if !review
                    && work.is_some_and(|work| {
                        matches!(work.phase, AutoWorkPhase::Review | AutoWorkPhase::Held)
                    })
                {
                    self.error = Some(fl!("activity-auto-work-review-required"));
                    return None;
                }
                self.automatic_work.form = Some(Form {
                    id: detail.activity.id.clone(),
                    revision: work.map(|work| work.revision),
                    review,
                    task: if review {
                        String::new()
                    } else {
                        work.map_or(String::new(), |work| work.config.task.clone())
                    },
                    interval: work
                        .map_or(3600, |work| work.config.check_interval_secs)
                        .to_string(),
                    source: work
                        .and_then(|work| work.config.wake_event.as_ref())
                        .map_or(String::new(), |event| event.source.clone()),
                    event_type: work
                        .and_then(|work| work.config.wake_event.as_ref())
                        .map_or(String::new(), |event| event.event_type.clone()),
                    note: String::new(),
                });
                self.error = None;
                self.notice = None;
                self.invalidate_operation_preview();
            }
            Message::Field(field, value) => {
                if let Some(form) = &mut self.automatic_work.form {
                    match field {
                        Field::Task => form.task = value,
                        Field::Interval => form.interval = value,
                        Field::Source => form.source = value,
                        Field::EventType => form.event_type = value,
                        Field::ReviewNote => form.note = value,
                    }
                }
            }
            Message::Discard => {
                self.automatic_work.form = None;
                return self.refresh_automatic_work(connected);
            }
            Message::Save => {
                let form = self.automatic_work.form.as_ref()?;
                if self.selected.as_deref() != Some(form.id.as_str()) {
                    return None;
                }
                let action = if form.review {
                    let request = AutoWorkEnabledRequest {
                        expected_revision: form.revision?,
                        enabled: true,
                        next_task: Some(form.task.clone()),
                        review_note: Some(form.note.clone()),
                    };
                    if let Err(error) = request.validate() {
                        self.error = Some(error.into());
                        return None;
                    }
                    Action::EnableAutomaticWork {
                        activity_id: form.id.clone(),
                        request,
                    }
                } else {
                    let interval = match form.interval.parse::<u32>() {
                        Ok(value) => value,
                        Err(_) => {
                            self.error = Some(fl!("activity-auto-work-invalid"));
                            return None;
                        }
                    };
                    let request = AutoWorkSetRequest {
                        expected_revision: form.revision,
                        work: AutoWorkDraft {
                            task: form.task.clone(),
                            check_interval_secs: interval,
                            wake_event: if form.source.is_empty() && form.event_type.is_empty() {
                                None
                            } else {
                                Some(AutoWorkEvent {
                                    source: form.source.clone(),
                                    event_type: form.event_type.clone(),
                                })
                            },
                        },
                    };
                    if let Err(error) = request.validate() {
                        self.error = Some(error.into());
                        return None;
                    }
                    Action::SetAutomaticWork {
                        activity_id: form.id.clone(),
                        request,
                    }
                };
                return self.begin(action, connected);
            }
            Message::Start | Message::Stop if !self.automatic_work.is_editing() => {
                let detail = self.detail.as_ref()?;
                let work = self.automatic_work.response.as_ref()?.work.as_ref()?;
                let enabled = matches!(message, Message::Start);
                if enabled
                    && (detail.activity.state != super::ActivityState::Active
                        || work.current.is_some()
                        || matches!(work.phase, AutoWorkPhase::Review | AutoWorkPhase::Held))
                {
                    self.error = Some(fl!("activity-auto-work-review-required"));
                    return None;
                }
                return self.begin(
                    Action::EnableAutomaticWork {
                        activity_id: detail.activity.id.clone(),
                        request: AutoWorkEnabledRequest {
                            expected_revision: work.revision,
                            enabled,
                            next_task: None,
                            review_note: None,
                        },
                    },
                    connected,
                );
            }
            _ => {}
        }
        None
    }

    pub(super) fn automatic_work_loaded(&mut self, id: &str, response: ActivityAutoWorkResponse) {
        let valid = self.selected.as_deref() == Some(id)
            && self
                .detail
                .as_ref()
                .is_some_and(|detail| detail.activity.id == id && response.matches_activity(id));
        if valid && !self.automatic_work.is_editing() {
            self.automatic_work.response = Some(response);
        } else {
            self.error = Some(fl!("activity-auto-work-invalid"));
        }
    }

    pub(super) fn automatic_work_configured(
        &mut self,
        id: &str,
        request: &AutoWorkSetRequest,
        work: ActivityAutoWork,
        connected: bool,
    ) -> Option<Request> {
        if !self.work_ack_matches(id, &work)
            || work.enabled
            || work.config != request.work
            || request.expected_revision.unwrap_or(0).checked_add(1) != Some(work.revision)
        {
            self.error = Some(fl!("activity-auto-work-invalid"));
            return None;
        }
        self.automatic_work.form = None;
        self.notice = Some(fl!("activity-auto-work-configured"));
        self.refresh_automatic_work(connected)
    }

    pub(super) fn automatic_work_enabled(
        &mut self,
        id: &str,
        request: &AutoWorkEnabledRequest,
        work: ActivityAutoWork,
        connected: bool,
    ) -> Option<Request> {
        if !self.work_ack_matches(id, &work)
            || work.enabled != request.enabled
            || request.expected_revision.checked_add(1) != Some(work.revision)
        {
            self.error = Some(fl!("activity-auto-work-invalid"));
            return None;
        }
        self.automatic_work.form = None;
        self.notice = Some(if request.enabled {
            fl!("activity-auto-work-started")
        } else {
            fl!("activity-auto-work-stopped")
        });
        self.refresh_automatic_work(connected)
    }

    fn work_ack_matches(&self, id: &str, work: &ActivityAutoWork) -> bool {
        self.visible
            && self.selected.as_deref() == Some(id)
            && self
                .detail
                .as_ref()
                .is_some_and(|detail| detail.activity.id == id && work.matches_activity(id))
            && self
                .automatic_work
                .response
                .as_ref()
                .and_then(|view| view.work.as_ref())
                .is_none_or(|previous| {
                    work.owner_uid == previous.owner_uid && work.created_at == previous.created_at
                })
    }

    pub(super) fn automatic_work_view<'a>(
        &'a self,
        detail: &'a ActivityDetailResponse,
        available: bool,
    ) -> Element<'a, AppMessage> {
        let mut card = Column::new()
            .spacing(8)
            .push(text(fl!("activity-auto-work")).size(18.0))
            .push(text(fl!("activity-auto-work-semantics")).size(12.0))
            .push(text(fl!("activity-auto-work-reports")).size(12.0));
        if let Some(form) = &self.automatic_work.form {
            card = card
                .push(
                    text(fl!(
                        "activity-auto-work-editing",
                        revision = form.revision.map_or("new".into(), |r| r.to_string())
                    ))
                    .size(12.0),
                )
                .push(work_input(
                    fl!("activity-auto-work-task"),
                    &form.task,
                    Field::Task,
                    available,
                ));
            if form.review {
                card = card
                    .push(text(fl!("activity-auto-work-review-required")).size(12.0))
                    .push(work_input(
                        fl!("activity-auto-work-note"),
                        &form.note,
                        Field::ReviewNote,
                        available,
                    ));
            } else {
                card = card
                    .push(work_input(
                        fl!("activity-auto-work-interval"),
                        &form.interval,
                        Field::Interval,
                        available,
                    ))
                    .push(work_input(
                        fl!("activity-auto-work-source"),
                        &form.source,
                        Field::Source,
                        available,
                    ))
                    .push(work_input(
                        fl!("activity-auto-work-event"),
                        &form.event_type,
                        Field::EventType,
                        available,
                    ));
            }
            card = card.push(
                Row::new()
                    .spacing(8)
                    .push(work_control(
                        fl!("activity-auto-work-save"),
                        Message::Save,
                        available,
                    ))
                    .push(work_control(fl!("cancel"), Message::Discard, available)),
            );
        } else {
            card = card.push(work_control(
                fl!("activity-auto-work-refresh"),
                Message::Refresh,
                available,
            ));
            match self.automatic_work.response.as_ref() {
                None => card = card.push(text(fl!("activity-auto-work-load")).size(12.0)),
                Some(response) => {
                    let current = response.work.as_ref();
                    if let Some(work) = current {
                        card = card
                            .push(
                                text(fl!(
                                    "activity-auto-work-status",
                                    phase = if work.phase == AutoWorkPhase::Running {
                                        fl!("activity-auto-work-inflight")
                                    } else {
                                        format!("{:?}", work.phase)
                                    },
                                    enabled = work.enabled.to_string(),
                                    revision = work.revision.to_string()
                                ))
                                .size(13.0),
                            )
                            .push(
                                text(fl!(
                                    "activity-auto-work-next",
                                    task = work.next_task.clone()
                                ))
                                .size(13.0),
                            );
                        if let Some(message) = &work.message {
                            card = card.push(text(message).size(13.0));
                        }
                        if let Some(checkpoint) = work
                            .last_checkpoint
                            .as_ref()
                            .and_then(|record| record.checkpoint.as_ref())
                        {
                            card = card.push(
                                text(fl!(
                                    "activity-auto-work-handoff",
                                    summary = checkpoint.summary.clone()
                                ))
                                .size(13.0),
                            );
                        }
                        let needs_review =
                            matches!(work.phase, AutoWorkPhase::Review | AutoWorkPhase::Held);
                        card = card.push(work_control(
                            if work.enabled {
                                fl!("activity-auto-work-stop")
                            } else if needs_review {
                                fl!("activity-auto-work-review")
                            } else {
                                fl!("activity-auto-work-start")
                            },
                            if work.enabled {
                                Message::Stop
                            } else if needs_review {
                                Message::Review
                            } else {
                                Message::Start
                            },
                            available
                                && (work.enabled
                                    || detail.activity.state == super::ActivityState::Active
                                        && work.current.is_none()),
                        ));
                    } else {
                        card = card.push(text(fl!("activity-auto-work-unconfigured")).size(13.0));
                    }
                    card = card.push(work_control(
                        fl!("activity-auto-work-configure"),
                        Message::Configure,
                        available
                            && editable(detail.activity.state)
                            && current.is_none_or(|work| {
                                work.current.is_none()
                                    && !matches!(
                                        work.phase,
                                        AutoWorkPhase::Review | AutoWorkPhase::Held
                                    )
                            }),
                    ));
                }
            }
        }
        container(card)
            .padding(12)
            .width(Length::Fill)
            .class(theme::Container::custom(styles::tool_card))
            .into()
    }
}

fn work_control(label: String, message: Message, enabled: bool) -> Element<'static, AppMessage> {
    control(label, ActivityMessage::AutomaticWork(message), enabled)
}

fn work_input<'a>(
    label: String,
    value: &'a str,
    field: Field,
    enabled: bool,
) -> Element<'a, AppMessage> {
    let input = widget::text_input(label, value);
    if enabled {
        input
            .on_input(move |value| {
                AppMessage::Activities(ActivityMessage::AutomaticWork(Message::Field(field, value)))
            })
            .into()
    } else {
        input.into()
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/automatic_work.rs"
    ));
}
