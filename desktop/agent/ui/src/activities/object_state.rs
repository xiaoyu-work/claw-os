//! Object-state presentation and unsent drafts in the existing Activity reducer.

use cos_agent_protocol::{
    ObjectStateContent, ObjectStateDraft, ObjectStateRelation, ObjectStateSource,
    ObjectStateValidity,
};
use uuid::Uuid;

use super::{
    Action, Activities, ActivityDetailResponse, ActivityObjectStateQuery,
    ActivityObjectStateRecordRequest, ActivityObjectStateResponse, ActivityResource, AppMessage,
    Column, Element, Length, Message as ActivityMessage, ObjectStateEntry, Request, Row, container,
    control, destructive, fl, receipt_outcome_label, receipt_report_view, section, styles, text,
    theme, widget,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    UserStatement,
    AgentInference,
    AppReport,
    Relation,
    Retracted,
}

impl Kind {
    fn has_window(self) -> bool {
        matches!(
            self,
            Self::UserStatement | Self::AgentInference | Self::AppReport
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Field {
    Text,
    ReceiptId,
    ObservedAt,
    ValidUntil,
}

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    Filter(String),
    New,
    Correct(String),
    Retract(String),
    Kind(Kind),
    Subject(String),
    Target(String),
    Relation(ObjectStateRelation),
    Field(Field, String),
    Save,
    Discard,
}

#[derive(Debug, Default)]
pub(super) struct State {
    pub(super) entries: Option<ActivityObjectStateResponse>,
    pub(super) form: Option<Form>,
    filter: String,
}

impl State {
    fn query(&self) -> ActivityObjectStateQuery {
        ActivityObjectStateQuery {
            reference: (!self.filter.is_empty()).then(|| self.filter.clone()),
            limit: Some(100),
        }
    }
}

#[derive(Debug)]
pub(super) struct Form {
    activity_id: String,
    id: String,
    reference: String,
    kind: Kind,
    text: String,
    receipt_id: String,
    relation: ObjectStateRelation,
    target: String,
    observed_at: String,
    valid_until: String,
    supersedes: Option<String>,
    submitted: bool,
}

impl Form {
    fn new(activity_id: String, reference: String) -> Self {
        Self {
            activity_id,
            id: Uuid::new_v4().to_string(),
            reference,
            kind: Kind::UserStatement,
            text: String::new(),
            receipt_id: String::new(),
            relation: ObjectStateRelation::RelatedTo,
            target: String::new(),
            observed_at: String::new(),
            valid_until: String::new(),
            supersedes: None,
            submitted: false,
        }
    }

    fn correction(entry: &ObjectStateEntry, retract: bool) -> Self {
        let mut form = Self::new(entry.activity_id.clone(), entry.draft.reference.clone());
        form.supersedes = Some(entry.id.clone());
        if retract {
            form.kind = Kind::Retracted;
            return form;
        }
        form.observed_at = entry.draft.observed_at.clone().unwrap_or_default();
        form.valid_until = entry.draft.valid_until.clone().unwrap_or_default();
        match &entry.draft.content {
            ObjectStateContent::UserStatement { text } => form.text = text.clone(),
            ObjectStateContent::AgentInference { text } => {
                form.kind = Kind::AgentInference;
                form.text = text.clone();
            }
            ObjectStateContent::AppReport { receipt_id } => {
                form.kind = Kind::AppReport;
                form.receipt_id = receipt_id.clone();
            }
            ObjectStateContent::Relation {
                relation,
                target,
                note,
            } => {
                form.kind = Kind::Relation;
                form.relation = *relation;
                form.target = target.clone();
                form.text = note.clone();
            }
            ObjectStateContent::Retracted { reason } => {
                form.kind = Kind::Retracted;
                form.text = reason.clone();
            }
        }
        form
    }

    fn editing(&mut self) {
        if self.submitted {
            self.id = Uuid::new_v4().to_string();
            self.submitted = false;
        }
    }

    fn set_field(&mut self, field: Field, value: String) {
        let target = match field {
            Field::Text if self.kind != Kind::AppReport => &mut self.text,
            Field::ReceiptId if self.kind == Kind::AppReport => &mut self.receipt_id,
            Field::ObservedAt if self.kind.has_window() => &mut self.observed_at,
            Field::ValidUntil if self.kind.has_window() => &mut self.valid_until,
            _ => return,
        };
        if *target != value {
            *target = value;
            self.editing();
        }
    }

    fn draft(&self) -> Result<ObjectStateDraft, String> {
        let content = match self.kind {
            Kind::UserStatement => ObjectStateContent::UserStatement {
                text: self.text.clone(),
            },
            Kind::AgentInference => ObjectStateContent::AgentInference {
                text: self.text.clone(),
            },
            Kind::AppReport => ObjectStateContent::AppReport {
                receipt_id: Uuid::parse_str(self.receipt_id.trim())
                    .map_err(|_| fl!("activity-object-state-receipt-required"))?
                    .to_string(),
            },
            Kind::Relation => ObjectStateContent::Relation {
                relation: self.relation,
                target: self.target.clone(),
                note: self.text.clone(),
            },
            Kind::Retracted => ObjectStateContent::Retracted {
                reason: self.text.clone(),
            },
        };
        let draft = ObjectStateDraft {
            id: self.id.clone(),
            reference: self.reference.clone(),
            content,
            observed_at: optional_time(&self.observed_at),
            valid_until: optional_time(&self.valid_until),
            supersedes: self.supersedes.clone(),
        };
        draft.validate_shape().map_err(str::to_owned)?;
        Ok(draft)
    }
}

fn optional_time(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn app_resources(resources: &[ActivityResource]) -> impl Iterator<Item = &ActivityResource> {
    // Select existing strings only; canonical URI semantics stay in the broker.
    resources
        .iter()
        .filter(|resource| resource.reference.starts_with("app://"))
}

fn attached(resources: &[ActivityResource], reference: &str) -> bool {
    app_resources(resources).any(|resource| resource.reference == reference)
}

fn can_supersede(entry: &ObjectStateEntry, resources: &[ActivityResource]) -> bool {
    entry.superseded_by.is_none()
        && !matches!(entry.draft.content, ObjectStateContent::Retracted { .. })
        && attached(resources, &entry.draft.reference)
}

impl Activities {
    pub(super) fn update_object_state(
        &mut self,
        message: Message,
        connected: bool,
    ) -> Option<Request> {
        if !self.visible || self.form.is_some() || self.object_form.is_some() {
            return None;
        }
        if let Message::Filter(value) = &message
            && self.object_state.form.is_none()
            && self
                .pending
                .as_ref()
                .is_none_or(|action| matches!(action, Action::ObjectStateList { .. }))
        {
            self.invalidate();
            self.object_state.filter = value.clone();
            self.object_state.entries = None;
            return None;
        }
        if self.pending.is_some() {
            return None;
        }
        match message {
            Message::Refresh if self.object_state.form.is_none() => {
                return self.refresh_object_state(connected);
            }
            Message::New if self.object_state.form.is_none() => {
                let detail = self.detail.as_ref()?;
                let Some(resource) = app_resources(&detail.activity.resources).next() else {
                    self.error = Some(fl!("activity-object-state-no-resources"));
                    return None;
                };
                self.object_state.form = Some(Form::new(
                    detail.activity.id.clone(),
                    resource.reference.clone(),
                ));
                self.invalidate_operation_preview();
                self.error = None;
                self.notice = None;
            }
            Message::Correct(id) if self.object_state.form.is_none() => {
                return self.begin_object_state_correction(id, false);
            }
            Message::Retract(id) if self.object_state.form.is_none() => {
                return self.begin_object_state_correction(id, true);
            }
            Message::Discard => {
                self.object_state.form = None;
                self.error = None;
                return self.refresh_object_state(connected);
            }
            Message::Save => return self.record_object_state(connected),
            Message::Kind(kind) => {
                if let Some(form) = &mut self.object_state.form
                    && form.kind != Kind::Retracted
                    && kind != Kind::Retracted
                    && form.kind != kind
                {
                    form.editing();
                    form.kind = kind;
                    if !kind.has_window() {
                        form.observed_at.clear();
                        form.valid_until.clear();
                    }
                }
            }
            Message::Subject(reference) => {
                let form = self.object_state.form.as_mut()?;
                if form.supersedes.is_some() || form.reference == reference {
                    return None;
                }
                if !self
                    .detail
                    .as_ref()
                    .is_some_and(|detail| attached(&detail.activity.resources, &reference))
                {
                    self.error = Some(fl!("activity-object-state-resource-required"));
                    return None;
                }
                form.editing();
                form.reference = reference;
            }
            Message::Target(reference) => {
                let form = self.object_state.form.as_mut()?;
                if form.kind != Kind::Relation || form.target == reference {
                    return None;
                }
                if !self
                    .detail
                    .as_ref()
                    .is_some_and(|detail| attached(&detail.activity.resources, &reference))
                {
                    self.error = Some(fl!("activity-object-state-resource-required"));
                    return None;
                }
                form.editing();
                form.target = reference;
            }
            Message::Relation(relation) => {
                if let Some(form) = &mut self.object_state.form
                    && form.kind == Kind::Relation
                    && form.relation != relation
                {
                    form.editing();
                    form.relation = relation;
                }
            }
            Message::Field(field, value) => {
                if let Some(form) = &mut self.object_state.form {
                    form.set_field(field, value);
                }
            }
            _ => {}
        }
        None
    }

    fn begin_object_state_correction(&mut self, id: String, retract: bool) -> Option<Request> {
        let detail = self.detail.as_ref()?;
        let entry = self
            .object_state
            .entries
            .as_ref()?
            .entries
            .iter()
            .find(|entry| entry.id == id);
        let Some(entry) = entry.filter(|entry| {
            self.selected.as_deref() == Some(entry.activity_id.as_str())
                && can_supersede(entry, &detail.activity.resources)
        }) else {
            self.error = Some(fl!("activity-object-state-correction-unavailable"));
            return None;
        };
        self.object_state.form = Some(Form::correction(entry, retract));
        self.invalidate_operation_preview();
        self.error = None;
        self.notice = None;
        None
    }

    fn refresh_object_state(&mut self, connected: bool) -> Option<Request> {
        let activity_id = self.selected.clone()?;
        self.object_state.entries = None;
        self.begin(
            Action::ObjectStateList {
                activity_id,
                query: self.object_state.query(),
            },
            connected,
        )
    }

    fn record_object_state(&mut self, connected: bool) -> Option<Request> {
        let form = self.object_state.form.as_ref()?;
        let detail = self.detail.as_ref()?;
        if self.selected.as_deref() != Some(form.activity_id.as_str())
            || detail.activity.id != form.activity_id
        {
            self.error = Some(fl!("activity-invalid-response"));
            return None;
        }
        let draft = match form.draft() {
            Ok(draft) => draft,
            Err(error) => {
                self.error = Some(error);
                return None;
            }
        };
        if !attached(&detail.activity.resources, &draft.reference)
            || matches!(&draft.content, ObjectStateContent::Relation { target, .. }
                if !attached(&detail.activity.resources, target))
        {
            self.error = Some(fl!("activity-object-state-resource-required"));
            return None;
        }
        let action = Action::RecordObjectState {
            activity_id: form.activity_id.clone(),
            request: ActivityObjectStateRecordRequest { entry: draft },
        };
        let request = self.begin(action, connected);
        if request.is_some()
            && let Some(form) = &mut self.object_state.form
        {
            form.submitted = true;
        }
        request
    }

    pub(super) fn object_state_loaded(
        &mut self,
        activity_id: &str,
        query: &ActivityObjectStateQuery,
        response: ActivityObjectStateResponse,
    ) {
        if self.visible
            && self.selected.as_deref() == Some(activity_id)
            && self.object_state.form.is_none()
            && *query == self.object_state.query()
            && response.matches_query(activity_id, query)
        {
            self.object_state.entries = Some(response);
        } else {
            self.error = Some(fl!("activity-invalid-response"));
        }
    }

    pub(super) fn object_state_recorded(
        &mut self,
        activity_id: &str,
        request: &ActivityObjectStateRecordRequest,
        entry: ObjectStateEntry,
        connected: bool,
    ) -> Option<Request> {
        if !self.visible
            || self.selected.as_deref() != Some(activity_id)
            || !self
                .object_state
                .form
                .as_ref()
                .is_some_and(|form| form.activity_id == activity_id && form.id == request.entry.id)
            || !entry.matches_submission(activity_id, &request.entry)
        {
            self.error = Some(fl!("activity-invalid-response"));
            return None;
        }
        self.object_state.form = None;
        self.notice = Some(fl!("activity-object-state-recorded", id = entry.id));
        self.refresh_object_state(connected)
    }

    pub(super) fn object_state_view<'a>(
        &'a self,
        detail: &'a ActivityDetailResponse,
        available: bool,
    ) -> Element<'a, AppMessage> {
        let mut content = Column::new()
            .spacing(12)
            .push(text(fl!("activity-object-state")).size(18.0))
            .push(text(fl!("activity-object-state-caveat")).size(12.0))
            .push(text(fl!("activity-object-state-window-caveat")).size(12.0))
            .push(text(fl!("activity-object-state-snapshot-hint")).size(12.0));
        if let Some(form) = &self.object_state.form {
            content = content.push(self.object_state_form_view(
                form,
                &detail.activity.resources,
                available,
            ));
        } else {
            content = content
                .push(
                    Row::new()
                        .spacing(8)
                        .push(object_control(
                            fl!("activity-object-state-refresh"),
                            Message::Refresh,
                            available,
                        ))
                        .push(object_control(
                            fl!("activity-object-state-new"),
                            Message::New,
                            available && app_resources(&detail.activity.resources).next().is_some(),
                        )),
                )
                .push(text(fl!("activity-object-state-filter-hint")).size(12.0))
                .push(
                    widget::text_input(
                        fl!("activity-object-state-filter"),
                        &self.object_state.filter,
                    )
                    .on_input(|value| app_message(Message::Filter(value))),
                );
            if app_resources(&detail.activity.resources).next().is_none() {
                content = content.push(text(fl!("activity-object-state-no-resources")).size(12.0));
            }
        }
        let Some(response) = &self.object_state.entries else {
            return content
                .push(text(fl!("activity-object-state-load-hint")).size(12.0))
                .into();
        };
        if response.entries.is_empty() {
            content = content.push(text(fl!("activity-object-state-empty")).size(12.0));
        }
        for entry in &response.entries {
            content = content.push(entry_view(
                entry,
                &detail.activity.resources,
                available && self.object_state.form.is_none(),
            ));
        }
        content
            .push(text(fl!("activity-object-state-limit")).size(12.0))
            .into()
    }

    fn object_state_form_view<'a>(
        &'a self,
        form: &'a Form,
        resources: &'a [ActivityResource],
        available: bool,
    ) -> Element<'a, AppMessage> {
        let title = if form.kind == Kind::Retracted {
            fl!("activity-object-state-retract")
        } else if form.supersedes.is_some() {
            fl!("activity-object-state-correct")
        } else {
            fl!("activity-object-state-new")
        };
        let mut content = Column::new().spacing(12).push(text(title).size(16.0));
        if let Some(predecessor) = &form.supersedes {
            content = content
                .push(section(
                    fl!("activity-object-state-supersedes"),
                    predecessor,
                ))
                .push(section(
                    fl!("activity-object-state-subject"),
                    &form.reference,
                ))
                .push(text(fl!("activity-object-state-correction-hint")).size(12.0));
        } else {
            content = content.push(resource_picker(
                fl!("activity-object-state-subject"),
                resources,
                &form.reference,
                Message::Subject,
                self.pending.is_none(),
            ));
        }
        if form.kind != Kind::Retracted {
            let mut kinds = Column::new().spacing(4);
            for kind in [
                Kind::UserStatement,
                Kind::AgentInference,
                Kind::AppReport,
                Kind::Relation,
            ] {
                kinds = kinds.push(object_control(
                    kind_label(kind),
                    Message::Kind(kind),
                    self.pending.is_none() && form.kind != kind,
                ));
            }
            content = content
                .push(kinds)
                .push(text(kind_label(form.kind)).size(14.0));
        }
        if form.kind == Kind::AppReport {
            content = content
                .push(input(
                    fl!("activity-object-state-receipt-id"),
                    &form.receipt_id,
                    Field::ReceiptId,
                ))
                .push(text(fl!("activity-object-state-app-caveat")).size(12.0));
            if let Some(receipts) = &self.receipts {
                for receipt in &receipts.receipts {
                    content = content.push(object_control(
                        format!(
                            "{}: {} - {} - {}",
                            receipt.id,
                            receipt.report.app_id,
                            receipt.report.operation,
                            receipt_outcome_label(receipt.report.outcome)
                        ),
                        Message::Field(Field::ReceiptId, receipt.id.clone()),
                        self.pending.is_none(),
                    ));
                }
            }
        } else {
            if form.kind == Kind::Relation {
                let mut relations = Row::new().spacing(8);
                for relation in [
                    ObjectStateRelation::RelatedTo,
                    ObjectStateRelation::DependsOn,
                    ObjectStateRelation::DerivedFrom,
                ] {
                    relations = relations.push(object_control(
                        relation_label(relation),
                        Message::Relation(relation),
                        self.pending.is_none() && form.relation != relation,
                    ));
                }
                content = content
                    .push(relations)
                    .push(text(relation_label(form.relation)).size(13.0))
                    .push(resource_picker(
                        fl!("activity-object-state-target"),
                        resources,
                        &form.target,
                        Message::Target,
                        self.pending.is_none(),
                    ));
            }
            let label = match form.kind {
                Kind::Relation => fl!("activity-object-state-note"),
                Kind::Retracted => fl!("activity-object-state-reason"),
                _ => fl!("activity-object-state-text"),
            };
            content = content.push(input(label, &form.text, Field::Text));
        }
        if form.kind.has_window() {
            content = content
                .push(input(
                    fl!("activity-object-state-observed-at"),
                    &form.observed_at,
                    Field::ObservedAt,
                ))
                .push(input(
                    fl!("activity-object-state-valid-until"),
                    &form.valid_until,
                    Field::ValidUntil,
                ))
                .push(text(fl!("activity-object-state-window-hint")).size(12.0));
        }
        content
            .push(section(fl!("activity-object-state-draft-id"), &form.id))
            .push(text(fl!("activity-object-state-retry-hint")).size(12.0))
            .push(
                Row::new()
                    .spacing(8)
                    .push(object_control(
                        fl!("activity-object-state-save"),
                        Message::Save,
                        available,
                    ))
                    .push(object_control(
                        fl!("activity-object-state-discard"),
                        Message::Discard,
                        self.pending.is_none(),
                    )),
            )
            .into()
    }
}

fn app_message(message: Message) -> AppMessage {
    AppMessage::Activities(ActivityMessage::ObjectState(message))
}

fn object_control(label: String, message: Message, enabled: bool) -> Element<'static, AppMessage> {
    control(label, ActivityMessage::ObjectState(message), enabled)
}

fn input(label: String, value: &str, field: Field) -> Element<'_, AppMessage> {
    Column::new()
        .spacing(4)
        .push(text(label.clone()).size(13.0))
        .push(
            widget::text_input(label, value)
                .on_input(move |value| app_message(Message::Field(field, value))),
        )
        .into()
}

fn resource_picker<'a>(
    label: String,
    resources: &'a [ActivityResource],
    selected: &'a str,
    choose: fn(String) -> Message,
    enabled: bool,
) -> Element<'a, AppMessage> {
    let mut content = Column::new().spacing(4).push(section(label, selected));
    for resource in app_resources(resources) {
        content = content.push(object_control(
            format!("{}: {}", resource.label, resource.reference),
            choose(resource.reference.clone()),
            enabled && selected != resource.reference,
        ));
    }
    content.into()
}

fn kind_label(kind: Kind) -> String {
    match kind {
        Kind::UserStatement => fl!("activity-object-state-user-statement"),
        Kind::AgentInference => fl!("activity-object-state-agent-inference"),
        Kind::AppReport => fl!("activity-object-state-app-report"),
        Kind::Relation => fl!("activity-object-state-relation"),
        Kind::Retracted => fl!("activity-object-state-retracted"),
    }
}

fn content_kind(content: &ObjectStateContent) -> Kind {
    match content {
        ObjectStateContent::UserStatement { .. } => Kind::UserStatement,
        ObjectStateContent::AgentInference { .. } => Kind::AgentInference,
        ObjectStateContent::AppReport { .. } => Kind::AppReport,
        ObjectStateContent::Relation { .. } => Kind::Relation,
        ObjectStateContent::Retracted { .. } => Kind::Retracted,
    }
}

fn relation_label(relation: ObjectStateRelation) -> String {
    match relation {
        ObjectStateRelation::RelatedTo => fl!("activity-object-state-related-to"),
        ObjectStateRelation::DependsOn => fl!("activity-object-state-depends-on"),
        ObjectStateRelation::DerivedFrom => fl!("activity-object-state-derived-from"),
    }
}

fn validity_label(validity: ObjectStateValidity) -> String {
    match validity {
        ObjectStateValidity::Unknown => fl!("activity-object-state-unknown"),
        ObjectStateValidity::NotYetApplicable => fl!("activity-object-state-not-yet-applicable"),
        ObjectStateValidity::WithinReportedWindow => fl!("activity-object-state-within-window"),
        ObjectStateValidity::Expired => fl!("activity-object-state-expired"),
    }
}

fn history_label(entry: &ObjectStateEntry) -> String {
    if let Some(successor) = &entry.superseded_by {
        fl!("activity-object-state-superseded", id = successor.clone())
    } else if matches!(entry.draft.content, ObjectStateContent::Retracted { .. }) {
        fl!("activity-object-state-retracted")
    } else {
        fl!("activity-object-state-no-successor")
    }
}

fn entry_view<'a>(
    entry: &'a ObjectStateEntry,
    resources: &[ActivityResource],
    available: bool,
) -> Element<'a, AppMessage> {
    let source = match entry.source {
        ObjectStateSource::CallerReported => fl!("activity-receipt-caller-reported"),
    };
    let mut content = Column::new()
        .spacing(8)
        .push(text(source).size(15.0))
        .push(text(kind_label(content_kind(&entry.draft.content))).size(16.0))
        .push(section(fl!("activity-object-state-entry-id"), &entry.id))
        .push(section(
            fl!("activity-object-state-subject"),
            &entry.draft.reference,
        ))
        .push(section(
            fl!("activity-receipt-recorded"),
            &entry.recorded_at,
        ))
        .push(text(fl!("activity-object-state-recorded-hint")).size(12.0))
        .push(text(validity_label(entry.validity)).size(13.0));
    if let (Some(start), Some(end)) = (&entry.draft.observed_at, &entry.draft.valid_until) {
        content = content
            .push(section(fl!("activity-object-state-observed-at"), start))
            .push(section(fl!("activity-object-state-valid-until"), end));
    }
    match &entry.draft.content {
        ObjectStateContent::UserStatement { text: statement }
        | ObjectStateContent::AgentInference { text: statement } => {
            content = content.push(text(statement).size(13.0));
        }
        ObjectStateContent::AppReport { receipt_id } => {
            content = content
                .push(section(fl!("activity-object-state-receipt-id"), receipt_id))
                .push(text(fl!("activity-object-state-app-caveat")).size(12.0));
            if let Some(receipt) = &entry.receipt {
                content = content.push(receipt_report_view(receipt));
            }
        }
        ObjectStateContent::Relation {
            relation,
            target,
            note,
        } => {
            content = content
                .push(text(relation_label(*relation)).size(14.0))
                .push(section(fl!("activity-object-state-target"), target))
                .push(section(fl!("activity-object-state-note"), note));
        }
        ObjectStateContent::Retracted { reason } => {
            content = content.push(section(fl!("activity-object-state-reason"), reason));
        }
    }
    content = content.push(text(history_label(entry)).size(13.0));
    if let Some(predecessor) = &entry.draft.supersedes {
        content = content.push(section(
            fl!("activity-object-state-supersedes"),
            predecessor,
        ));
    }
    if !attached(resources, &entry.draft.reference) {
        content = content.push(text(fl!("activity-object-state-detached")).size(12.0));
    }
    if entry.superseded_by.is_none()
        && !matches!(entry.draft.content, ObjectStateContent::Retracted { .. })
    {
        let enabled = available && can_supersede(entry, resources);
        content = content.push(
            Row::new()
                .spacing(8)
                .push(object_control(
                    fl!("activity-object-state-correct"),
                    Message::Correct(entry.id.clone()),
                    enabled,
                ))
                .push(destructive(
                    fl!("activity-object-state-retract"),
                    ActivityMessage::ObjectState(Message::Retract(entry.id.clone())),
                    enabled,
                )),
        );
    }
    container(content)
        .padding(12)
        .width(Length::Fill)
        .class(theme::Container::custom(styles::tool_card))
        .into()
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/object_state.rs"
    ));
}
