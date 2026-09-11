//! Fetched Activity views and unsaved forms. The broker, not this reducer,
//! owns Activity lifecycle and durable work; leaving this view cancels nothing.

use cos_agent_protocol::{
    ActivityJobView, ActivityObjectStatus, ActivityResource, AppObjectDescription,
};
use cosmic::iced::{Alignment, Length};
use cosmic::widget::{Column, Row, button, container, scrollable, text};
use cosmic::{Element, theme, widget};

use crate::bridge::{
    ActivityCreateRequest, ActivityDetailResponse, ActivityListResponse,
    ActivityObjectAttachRequest, ActivityObjectsResponse, ActivityRunRequest, ActivityState,
    ActivityTransitionRequest, ActivityUpdateRequest, ActivityView, ActivityWorkResponse,
    CancelResponse,
};
use crate::{Message as AppMessage, fl, styles};

#[derive(Debug, Clone, Copy)]
pub enum Field {
    Title,
    Goal,
    Criteria,
    Boundaries,
    CompletionNote,
    Prompt,
}

#[derive(Debug, Clone, Copy)]
pub enum ObjectField {
    Label,
    AppId,
    ObjectType,
    ObjectId,
    Revision,
}

#[derive(Debug, Clone)]
pub enum Message {
    Show,
    Back,
    Refresh,
    Tick,
    Filter(Option<ActivityState>),
    Open(String),
    New,
    Edit,
    Discard,
    Field(Field, String),
    AddResource,
    ResourceLabel(usize, String),
    ResourceReference(usize, String),
    RemoveResource(usize),
    DescribeObjects,
    NewObject,
    ObjectField(ObjectField, String),
    AttachObject,
    DiscardObject,
    Save,
    Transition(ActivityState),
    UseSession(Option<String>),
    Run,
    CancelJob(String),
    RetryJob(String),
    Loaded {
        generation: u64,
        result: Result<Response, String>,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum Action {
    List(Option<ActivityState>),
    Get(String),
    Objects(String),
    AttachObject(String, ActivityObjectAttachRequest),
    Create(ActivityCreateRequest),
    Update(String, ActivityUpdateRequest),
    Transition(String, ActivityTransitionRequest),
    Run(String, ActivityRunRequest),
    CancelJob(String),
    RetryJob(String),
}

#[derive(Debug, Clone)]
pub(crate) struct Request {
    pub(crate) generation: u64,
    pub(crate) action: Action,
}

#[derive(Debug, Clone)]
pub enum Response {
    List(ActivityListResponse),
    Detail(Box<ActivityDetailResponse>),
    Objects(ActivityObjectsResponse),
    Saved(Box<ActivityView>),
    Work(ActivityWorkResponse),
    JobCancellation(CancelResponse),
}

#[derive(Debug, Default)]
pub(crate) struct Activities {
    visible: bool,
    generation: u64,
    pending: Option<Action>,
    list: Vec<ActivityView>,
    filter: Option<ActivityState>,
    selected: Option<String>,
    detail: Option<ActivityDetailResponse>,
    form: Option<ActivityCreateRequest>,
    object_form: Option<ActivityObjectAttachRequest>,
    objects: Option<ActivityObjectsResponse>,
    refresh_objects_after_detail: bool,
    completion_note: String,
    prompt: String,
    continue_session: Option<String>,
    error: Option<String>,
    notice: Option<String>,
}

impl Activities {
    pub(crate) fn is_visible(&self) -> bool {
        self.visible
    }

    pub(crate) fn should_poll(&self) -> bool {
        self.visible
            && self.pending.is_none()
            && self.form.is_none()
            && self.object_form.is_none()
            && self.prompt.is_empty()
            && self.completion_note.is_empty()
            && self.error.is_none()
    }

    pub(crate) fn hide(&mut self) {
        self.visible = false;
        if !self.can_edit_forms() {
            self.form = None;
            self.object_form = None;
        }
        self.invalidate();
    }

    fn can_edit_forms(&self) -> bool {
        self.pending.as_ref().is_none_or(|action| {
            matches!(
                action,
                Action::List(_) | Action::Get(_) | Action::Objects(_)
            )
        })
    }

    pub(crate) fn session_title(&self, id: &str) -> Option<String> {
        self.detail
            .as_ref()
            .filter(|detail| {
                detail.sessions.iter().any(|session| session == id)
                    || detail
                        .jobs
                        .iter()
                        .any(|job| job.session_id.as_deref() == Some(id))
            })
            .map(|detail| detail.activity.title.clone())
    }

    pub(crate) fn object_operation_text(&self, reference: &str) -> Option<String> {
        self.objects
            .as_ref()?
            .objects
            .iter()
            .find(|object| {
                object.reference == reference && object.status == ActivityObjectStatus::Declared
            })?
            .description
            .as_ref()
            .map(object_operation_text)
    }

    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
    }

    fn begin(&mut self, action: Action, connected: bool) -> Option<Request> {
        self.invalidate();
        if !connected {
            self.error = Some(fl!("bridge-offline"));
            return None;
        }
        self.error = None;
        self.pending = Some(action.clone());
        Some(Request {
            generation: self.generation,
            action,
        })
    }

    fn refresh(&mut self, connected: bool) -> Option<Request> {
        if self.pending.is_some() || self.form.is_some() || self.object_form.is_some() {
            return None;
        }
        let action = self
            .selected
            .clone()
            .map(Action::Get)
            .unwrap_or(Action::List(self.filter));
        self.begin(action, connected)
    }

    fn clear_detail(&mut self) {
        self.invalidate();
        self.selected = None;
        self.detail = None;
        self.form = None;
        self.object_form = None;
        self.objects = None;
        self.refresh_objects_after_detail = false;
        self.completion_note.clear();
        self.prompt.clear();
        self.continue_session = None;
        self.error = None;
        self.notice = None;
    }

    pub(crate) fn update(&mut self, message: Message, connected: bool) -> Option<Request> {
        match message {
            Message::Loaded { generation, result } => {
                return self.loaded(generation, result, connected);
            }
            Message::Show => {
                self.visible = true;
                return self.refresh(connected);
            }
            Message::Back => {
                self.clear_detail();
                return self.refresh(connected);
            }
            Message::Open(id) => {
                self.clear_detail();
                self.selected = Some(id.clone());
                return self.begin(Action::Get(id), connected);
            }
            Message::New => {
                self.clear_detail();
                self.form = Some(ActivityCreateRequest::default());
            }
            Message::Tick => {
                if self.should_poll() {
                    return self.refresh(connected);
                }
            }
            Message::Refresh => return self.refresh(connected),
            Message::Field(field, value) => {
                if !self.can_edit_forms() {
                    return None;
                }
                match field {
                    Field::CompletionNote => self.completion_note = value,
                    Field::Prompt => self.prompt = value,
                    _ => {
                        if let Some(form) = &mut self.form {
                            match field {
                                Field::Title => form.title = value,
                                Field::Goal => form.goal = value,
                                Field::Criteria => form.completion_criteria = value,
                                Field::Boundaries => form.boundaries = value,
                                _ => {}
                            }
                        }
                    }
                }
            }
            Message::ObjectField(field, value) => {
                if !self.can_edit_forms() {
                    return None;
                }
                if let Some(form) = &mut self.object_form {
                    match field {
                        ObjectField::Label => form.label = value,
                        ObjectField::AppId => form.object.app_id = value,
                        ObjectField::ObjectType => form.object.object_type = value,
                        ObjectField::ObjectId => form.object.object_id = value,
                        ObjectField::Revision => {
                            form.object.revision = (!value.is_empty()).then_some(value);
                        }
                    }
                }
            }
            _ if self.pending.is_some() => {}
            _ if self.object_form.is_some()
                && !matches!(&message, Message::AttachObject | Message::DiscardObject) => {}
            Message::DescribeObjects => {
                let id = self.selected.clone()?;
                self.objects = None;
                self.refresh_objects_after_detail = true;
                return self.begin(Action::Objects(id), connected);
            }
            Message::NewObject => {
                if self.form.is_some() {
                    return None;
                }
                if self
                    .detail
                    .as_ref()
                    .is_some_and(|detail| editable(detail.activity.state))
                {
                    self.object_form = Some(ActivityObjectAttachRequest::default());
                    self.error = None;
                } else {
                    self.error = Some(fl!("activity-object-readonly"));
                }
            }
            Message::DiscardObject => {
                self.object_form = None;
                self.error = None;
                return self.refresh(connected);
            }
            Message::AttachObject => {
                let form = self.object_form.as_ref()?;
                let detail = self.detail.as_ref()?;
                if !editable(detail.activity.state) {
                    self.error = Some(fl!("activity-object-readonly"));
                    return None;
                }
                if form.label.trim().is_empty()
                    || form.object.app_id.trim().is_empty()
                    || form.object.object_type.trim().is_empty()
                    || form.object.object_id.is_empty()
                {
                    self.error = Some(fl!("activity-object-required"));
                    return None;
                }
                let action = Action::AttachObject(detail.activity.id.clone(), form.clone());
                self.refresh_objects_after_detail = true;
                return self.begin(action, connected);
            }
            Message::Filter(filter) => {
                self.filter = filter;
                return self.refresh(connected);
            }
            Message::Edit => {
                if let Some(detail) = &self.detail
                    && editable(detail.activity.state)
                {
                    let activity = &detail.activity;
                    self.form = Some(ActivityCreateRequest {
                        title: activity.title.clone(),
                        goal: activity.goal.clone(),
                        completion_criteria: activity.completion_criteria.clone(),
                        boundaries: activity.boundaries.clone(),
                        resources: activity.resources.clone(),
                    });
                    self.error = None;
                }
            }
            Message::Discard => {
                self.form = None;
                self.error = None;
                return self.refresh(connected);
            }
            Message::AddResource => {
                if let Some(form) = &mut self.form {
                    form.resources.push(ActivityResource::default());
                }
            }
            Message::ResourceLabel(index, value) => {
                if let Some(resource) = self
                    .form
                    .as_mut()
                    .and_then(|form| form.resources.get_mut(index))
                {
                    resource.label = value;
                }
            }
            Message::ResourceReference(index, value) => {
                if let Some(resource) = self
                    .form
                    .as_mut()
                    .and_then(|form| form.resources.get_mut(index))
                {
                    resource.reference = value;
                }
            }
            Message::RemoveResource(index) => {
                if let Some(form) = &mut self.form
                    && index < form.resources.len()
                {
                    form.resources.remove(index);
                }
            }
            Message::Save => {
                let form = self.form.as_ref()?;
                if form.title.trim().is_empty() || form.goal.trim().is_empty() {
                    self.error = Some(fl!("activity-required"));
                    return None;
                }
                if form.resources.iter().any(|resource| {
                    resource.label.trim().is_empty() || resource.reference.trim().is_empty()
                }) {
                    self.error = Some(fl!("activity-resource-required"));
                    return None;
                }
                let action = match &self.selected {
                    Some(id) => Action::Update(
                        id.clone(),
                        ActivityUpdateRequest {
                            title: Some(form.title.clone()),
                            goal: Some(form.goal.clone()),
                            completion_criteria: Some(form.completion_criteria.clone()),
                            boundaries: Some(form.boundaries.clone()),
                            resources: Some(form.resources.clone()),
                        },
                    ),
                    None => Action::Create(form.clone()),
                };
                return self.begin(action, connected);
            }
            Message::Transition(state) => {
                let id = self.selected.clone()?;
                if state == ActivityState::Completed && self.completion_note.trim().is_empty() {
                    self.error = Some(fl!("activity-completion-required"));
                    return None;
                }
                return self.begin(
                    Action::Transition(
                        id,
                        ActivityTransitionRequest {
                            state,
                            completion_note: (state == ActivityState::Completed)
                                .then(|| self.completion_note.trim().to_string()),
                        },
                    ),
                    connected,
                );
            }
            Message::UseSession(session) => {
                if session
                    .as_deref()
                    .is_none_or(|id| self.session_title(id).is_some())
                {
                    self.continue_session = session;
                }
            }
            Message::Run => {
                let id = self.selected.clone()?;
                return self.begin(
                    Action::Run(
                        id,
                        ActivityRunRequest {
                            prompt: (!self.prompt.trim().is_empty())
                                .then(|| self.prompt.trim().to_string()),
                            session_id: self.continue_session.clone(),
                            ..ActivityRunRequest::default()
                        },
                    ),
                    connected,
                );
            }
            Message::CancelJob(id) | Message::RetryJob(id) if !self.has_job(&id) => {}
            Message::CancelJob(id) => return self.begin(Action::CancelJob(id), connected),
            Message::RetryJob(id) => return self.begin(Action::RetryJob(id), connected),
        }
        None
    }

    fn has_job(&self, id: &str) -> bool {
        self.detail
            .as_ref()
            .is_some_and(|detail| detail.jobs.iter().any(|job| job.id == id))
    }

    fn loaded(
        &mut self,
        generation: u64,
        result: Result<Response, String>,
        connected: bool,
    ) -> Option<Request> {
        if generation != self.generation {
            return None;
        }
        let pending = self.pending.take()?;
        let response = match result {
            Ok(response) => response,
            Err(error) => {
                self.error = Some(error);
                return None;
            }
        };
        match (pending, response) {
            (Action::List(_), Response::List(response)) => self.list = response.activities,
            (Action::Get(id), Response::Detail(detail)) if detail.activity.id == id => {
                if self.objects.is_some()
                    && self.detail.as_ref().is_some_and(|previous| {
                        previous.activity.resources != detail.activity.resources
                    })
                {
                    self.objects = None;
                    self.refresh_objects_after_detail = true;
                }
                self.detail = Some(*detail);
                if self.refresh_objects_after_detail {
                    return self.begin(Action::Objects(id), connected);
                }
            }
            (Action::Objects(id), Response::Objects(objects)) if objects.activity_id == id => {
                self.objects = Some(objects);
                self.refresh_objects_after_detail = false;
            }
            (Action::AttachObject(id, _), Response::Saved(activity)) if activity.id == id => {
                self.object_form = None;
                self.objects = None;
                self.detail = None;
                self.refresh_objects_after_detail = true;
                return self.refresh(connected);
            }
            (Action::Create(_), Response::Saved(activity)) => {
                return self.saved(*activity, connected);
            }
            (Action::Update(id, _) | Action::Transition(id, _), Response::Saved(activity))
                if activity.id == id =>
            {
                return self.saved(*activity, connected);
            }
            (Action::Run(_, _) | Action::RetryJob(_), Response::Work(work))
                if work
                    .activity_id
                    .as_ref()
                    .is_none_or(|id| self.selected.as_ref() == Some(id)) =>
            {
                self.notice = Some(fl!(
                    "activity-work-accepted",
                    id = work.id,
                    status = work.status
                ));
                self.prompt.clear();
                return self.refresh(connected);
            }
            (Action::CancelJob(id), Response::JobCancellation(response)) if response.id == id => {
                let notice = fl!(
                    "activity-job-status",
                    id = response.id,
                    status = response.status
                );
                self.notice = Some(match response.reason {
                    Some(reason) => format!("{notice}: {reason}"),
                    None => notice,
                });
                return self.refresh(connected);
            }
            _ => self.error = Some(fl!("activity-invalid-response")),
        }
        None
    }

    fn saved(&mut self, activity: ActivityView, connected: bool) -> Option<Request> {
        if self.objects.is_some()
            && self
                .detail
                .as_ref()
                .is_none_or(|previous| previous.activity.resources != activity.resources)
        {
            self.objects = None;
            self.refresh_objects_after_detail = true;
        }
        self.selected = Some(activity.id);
        self.detail = None;
        self.form = None;
        self.completion_note.clear();
        self.refresh(connected)
    }

    pub(crate) fn view(&self, connected: bool) -> Element<'_, AppMessage> {
        let spacing = theme::active().cosmic().spacing;
        let available = connected && self.pending.is_none();
        let mut header = Row::new()
            .spacing(spacing.space_xs)
            .align_y(Alignment::Center);
        if self.selected.is_some() || self.form.is_some() {
            header = header.push(control(fl!("activity-back"), Message::Back, true));
        }
        header = header
            .push(text(fl!("activities")).size(24.0))
            .push(widget::space::horizontal());
        if self.form.is_none() && self.object_form.is_none() {
            header = header
                .push(control(
                    fl!("activity-refresh"),
                    Message::Refresh,
                    available,
                ))
                .push(control(
                    fl!("activity-new"),
                    Message::New,
                    self.pending.is_none(),
                ));
        }
        if !connected || self.error.is_some() {
            header = header.push(button::text(fl!("reconnect")).on_press(AppMessage::Reconnect));
        }
        let mut content = Column::new().spacing(spacing.space_s).push(header);
        if !connected {
            content = content.push(text(fl!("bridge-offline")));
        }
        if let Some(error) = &self.error {
            content = content.push(error_card(error));
        }
        if let Some(notice) = &self.notice {
            content = content.push(text(notice).size(12.0));
        }
        if self.pending.is_some() {
            content = content.push(text(fl!("activity-loading")).size(12.0));
        }
        let body = if let Some(form) = &self.form {
            self.form_view(form, available)
        } else if let Some(form) = &self.object_form {
            self.object_form_view(form, available)
        } else if let Some(detail) = &self.detail {
            self.detail_view(detail, available)
        } else if self.selected.is_none() {
            self.list_view(available)
        } else {
            widget::Space::new().into()
        };
        container(content.push(scrollable(body).height(Length::Fill)))
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(spacing.space_l)
            .into()
    }

    fn list_view(&self, available: bool) -> Element<'_, AppMessage> {
        let mut filters = Row::new().spacing(4).push(control(
            fl!("activity-all"),
            Message::Filter(None),
            available && self.filter.is_some(),
        ));
        for state in [
            ActivityState::Active,
            ActivityState::Paused,
            ActivityState::Completed,
            ActivityState::Cancelled,
        ] {
            filters = filters.push(control(
                state_label(state),
                Message::Filter(Some(state)),
                available && self.filter != Some(state),
            ));
        }
        let mut list = Column::new()
            .spacing(12)
            .push(text(fl!("activities-hint")).size(13.0))
            .push(filters);
        if self.list.is_empty() && self.pending.is_none() {
            list = list.push(text(fl!("activities-empty")));
        }
        for activity in &self.list {
            let summary = Column::new()
                .spacing(4)
                .push(text(&activity.title).size(16.0))
                .push(text(state_label(activity.state)).size(12.0))
                .push(text(activity.goal.chars().take(180).collect::<String>()).size(13.0));
            list = list.push(
                button::custom(summary)
                    .width(Length::Fill)
                    .padding(12)
                    .class(theme::Button::Standard)
                    .on_press(AppMessage::Activities(Message::Open(activity.id.clone()))),
            );
        }
        if self.list.len() >= 100 {
            list = list.push(text(fl!("activity-list-limit")).size(12.0));
        }
        list.into()
    }

    fn form_view<'a>(
        &'a self,
        form: &'a ActivityCreateRequest,
        available: bool,
    ) -> Element<'a, AppMessage> {
        let mut content = Column::new()
            .spacing(12)
            .push(
                text(if self.selected.is_some() {
                    fl!("activity-edit")
                } else {
                    fl!("activity-new")
                })
                .size(18.0),
            )
            .push(field(fl!("activity-title"), &form.title, Field::Title))
            .push(field(fl!("activity-goal"), &form.goal, Field::Goal))
            .push(field(
                fl!("activity-criteria"),
                &form.completion_criteria,
                Field::Criteria,
            ))
            .push(field(
                fl!("activity-boundaries"),
                &form.boundaries,
                Field::Boundaries,
            ))
            .push(text(fl!("activity-boundaries-hint")).size(12.0))
            .push(text(fl!("activity-resources")).size(16.0));
        for (index, resource) in form.resources.iter().enumerate() {
            content = content.push(
                Row::new()
                    .spacing(8)
                    .push(
                        widget::text_input(fl!("activity-resource-label"), &resource.label)
                            .on_input(move |value| {
                                AppMessage::Activities(Message::ResourceLabel(index, value))
                            }),
                    )
                    .push(
                        widget::text_input(fl!("activity-resource-reference"), &resource.reference)
                            .on_input(move |value| {
                                AppMessage::Activities(Message::ResourceReference(index, value))
                            }),
                    )
                    .push(control(
                        fl!("activity-remove"),
                        Message::RemoveResource(index),
                        self.pending.is_none(),
                    )),
            );
        }
        content
            .push(text(fl!("activity-resources-hint")).size(12.0))
            .push(control(
                fl!("activity-add-resource"),
                Message::AddResource,
                self.pending.is_none(),
            ))
            .push(
                Row::new()
                    .spacing(8)
                    .push(control(fl!("activity-save"), Message::Save, available))
                    .push(control(
                        fl!("cancel"),
                        Message::Discard,
                        self.pending.is_none(),
                    )),
            )
            .into()
    }

    fn detail_view<'a>(
        &'a self,
        detail: &'a ActivityDetailResponse,
        available: bool,
    ) -> Element<'a, AppMessage> {
        let activity = &detail.activity;
        let mut content = Column::new()
            .spacing(12)
            .push(text(&activity.title).size(22.0))
            .push(text(format!("{} · {}", state_label(activity.state), activity.id)).size(12.0))
            .push(
                text(fl!(
                    "activity-updated",
                    created = activity.created_at.clone(),
                    updated = activity.updated_at.clone()
                ))
                .size(11.0),
            )
            .push(section(fl!("activity-goal"), &activity.goal))
            .push(section(
                fl!("activity-criteria"),
                &activity.completion_criteria,
            ))
            .push(section(fl!("activity-boundaries"), &activity.boundaries))
            .push(text(fl!("activity-boundaries-hint")).size(12.0));
        if let Some(note) = &activity.completion_note {
            content = content.push(section(fl!("activity-completion-note"), note));
        }
        let mut controls = Row::new().spacing(8);
        match activity.state {
            ActivityState::Active => {
                controls = controls.push(control(
                    fl!("activity-pause"),
                    Message::Transition(ActivityState::Paused),
                    available,
                ))
            }
            ActivityState::Paused => {
                controls = controls.push(control(
                    fl!("activity-resume"),
                    Message::Transition(ActivityState::Active),
                    available,
                ))
            }
            ActivityState::Completed | ActivityState::Cancelled => {
                controls = controls.push(control(
                    fl!("activity-reopen"),
                    Message::Transition(ActivityState::Active),
                    available,
                ))
            }
        }
        if editable(activity.state) {
            controls = controls
                .push(control(fl!("activity-edit"), Message::Edit, available))
                .push(destructive(
                    fl!("activity-cancel"),
                    Message::Transition(ActivityState::Cancelled),
                    available,
                ));
        }
        content = content
            .push(controls)
            .push(text(fl!("activity-pause-hint")).size(12.0));
        if editable(activity.state) {
            content = content
                .push(field(
                    fl!("activity-completion-note"),
                    &self.completion_note,
                    Field::CompletionNote,
                ))
                .push(text(fl!("activity-completion-hint")).size(12.0))
                .push(control(
                    fl!("activity-complete"),
                    Message::Transition(ActivityState::Completed),
                    available && !self.completion_note.trim().is_empty(),
                ));
        }
        content = content.push(text(fl!("activity-resources")).size(18.0));
        if activity.resources.is_empty() {
            content = content.push(text(fl!("activity-no-resources")).size(12.0));
        }
        for resource in &activity.resources {
            content = content.push(section(resource.label.clone(), &resource.reference));
        }
        content = content
            .push(text(fl!("activity-resources-hint")).size(12.0))
            .push(self.object_resources_view(editable(activity.state), available))
            .push(text(fl!("activity-work")).size(18.0))
            .push(text(fl!("activity-work-hint")).size(12.0));
        if activity.state == ActivityState::Active {
            content = content
                .push(field(fl!("activity-prompt"), &self.prompt, Field::Prompt))
                .push(
                    text(match &self.continue_session {
                        Some(session) => fl!("activity-continuing-session", id = session.clone()),
                        None => fl!("activity-new-work-session"),
                    })
                    .size(12.0),
                )
                .push(
                    Row::new()
                        .spacing(8)
                        .push(control(fl!("activity-start-work"), Message::Run, available))
                        .push(control(
                            fl!("activity-use-new-session"),
                            Message::UseSession(None),
                            available && self.continue_session.is_some(),
                        )),
                );
        } else {
            content = content.push(text(fl!("activity-work-inactive")).size(12.0));
        }
        content = content
            .push(text(fl!("activity-approvals")).size(18.0))
            .push(text(fl!("activity-approvals-hint")).size(12.0));
        if let Some(error) = &detail.approvals_error {
            content = content.push(error_card(error));
        } else if detail.pending_approvals.is_empty() {
            content = content.push(text(fl!("activity-no-approvals")).size(12.0));
        }
        for approval in &detail.pending_approvals {
            content = content.push(
                Column::new()
                    .spacing(4)
                    .push(text(&approval.label).size(14.0))
                    .push(text(&approval.reason).size(12.0))
                    .push(
                        button::text(fl!("activity-open-session"))
                            .on_press(AppMessage::OpenActivitySession(approval.session_id.clone())),
                    ),
            );
        }
        content = content.push(text(fl!("activity-jobs")).size(18.0));
        if detail.jobs.is_empty() {
            content = content.push(text(fl!("activity-no-jobs")).size(12.0));
        }
        for job in &detail.jobs {
            content = content.push(job_view(
                job,
                available,
                activity.state == ActivityState::Active,
            ));
        }
        content = content.push(text(fl!("sessions")).size(18.0));
        for session in &detail.sessions {
            content = content.push(
                Row::new()
                    .spacing(8)
                    .push(text(session).size(12.0).width(Length::Fill))
                    .push(
                        button::text(fl!("activity-open-session"))
                            .on_press(AppMessage::OpenActivitySession(session.clone())),
                    )
                    .push(control(
                        fl!("activity-use-session"),
                        Message::UseSession(Some(session.clone())),
                        available && activity.state == ActivityState::Active,
                    )),
            );
        }
        content.into()
    }

    fn object_form_view<'a>(
        &'a self,
        form: &'a ActivityObjectAttachRequest,
        available: bool,
    ) -> Element<'a, AppMessage> {
        Column::new()
            .spacing(12)
            .push(text(fl!("activity-object-attach")).size(18.0))
            .push(text(fl!("activity-object-attach-hint")).size(12.0))
            .push(object_field(
                fl!("activity-resource-label"),
                &form.label,
                ObjectField::Label,
            ))
            .push(object_field(
                fl!("activity-object-app-id"),
                &form.object.app_id,
                ObjectField::AppId,
            ))
            .push(object_field(
                fl!("activity-object-type"),
                &form.object.object_type,
                ObjectField::ObjectType,
            ))
            .push(object_field(
                fl!("activity-object-id"),
                &form.object.object_id,
                ObjectField::ObjectId,
            ))
            .push(object_field(
                fl!("activity-object-revision"),
                form.object.revision.as_deref().unwrap_or_default(),
                ObjectField::Revision,
            ))
            .push(text(fl!("activity-object-declaration-hint")).size(12.0))
            .push(
                Row::new()
                    .spacing(8)
                    .push(control(
                        fl!("activity-object-attach"),
                        Message::AttachObject,
                        available,
                    ))
                    .push(control(
                        fl!("cancel"),
                        Message::DiscardObject,
                        self.pending.is_none(),
                    )),
            )
            .into()
    }

    fn object_resources_view(&self, can_attach: bool, available: bool) -> Element<'_, AppMessage> {
        let mut content = Column::new()
            .spacing(12)
            .push(text(fl!("activity-object-declarations")).size(18.0))
            .push(text(fl!("activity-object-declaration-hint")).size(12.0))
            .push(
                Row::new()
                    .spacing(8)
                    .push(control(
                        fl!("activity-object-describe"),
                        Message::DescribeObjects,
                        available,
                    ))
                    .push(control(
                        fl!("activity-object-attach"),
                        Message::NewObject,
                        available && can_attach,
                    )),
            );
        let Some(objects) = &self.objects else {
            return content
                .push(text(fl!("activity-object-describe-hint")).size(12.0))
                .into();
        };
        if objects.objects.is_empty() {
            content = content.push(text(fl!("activity-object-empty")).size(12.0));
        }
        for object in &objects.objects {
            let status = match object.status {
                ActivityObjectStatus::Declared => fl!("activity-object-declared"),
                ActivityObjectStatus::Unavailable => fl!("activity-object-unavailable"),
                ActivityObjectStatus::Invalid => fl!("activity-object-invalid"),
            };
            let mut card = Column::new()
                .spacing(6)
                .push(text(&object.label).size(15.0))
                .push(text(status).size(13.0))
                .push(text(&object.reference).size(12.0));
            if let Some(error) = &object.error {
                card = card.push(error_card(error));
            }
            if object.status == ActivityObjectStatus::Declared
                && let Some(description) = &object.description
            {
                if description.reference != object.reference {
                    card = card.push(section(
                        fl!("activity-object-canonical-reference"),
                        &description.reference,
                    ));
                }
                card = card
                    .push(
                        text(format!(
                            "{} · {}",
                            description.app_name, description.app_version
                        ))
                        .size(14.0),
                    )
                    .push(section(
                        description.object_label.clone(),
                        &description.object_summary,
                    ))
                    .push(section(
                        fl!("activity-object-type"),
                        &description.object.object_type,
                    ))
                    .push(section(
                        fl!("activity-object-id"),
                        &description.object.object_id,
                    ));
                if let Some(revision) = &description.object.revision {
                    card = card.push(section(fl!("activity-object-revision"), revision));
                }
                card = card
                    .push(text(fl!("activity-object-operation-hint")).size(12.0))
                    .push(text(object_operation_text(description)).size(12.0))
                    .push(
                        button::text(fl!("activity-object-copy-operation")).on_press(
                            AppMessage::CopyActivityObjectOperation(object.reference.clone()),
                        ),
                    );
            }
            content = content.push(
                container(card)
                    .padding(12)
                    .width(Length::Fill)
                    .class(theme::Container::custom(styles::tool_card)),
            );
        }
        content.into()
    }
}

fn object_operation_text(description: &AppObjectDescription) -> String {
    let invocation = &description.invocation;
    format!(
        "{}\n{}\n{}\n{}",
        fl!(
            "activity-object-operation-app",
            id = invocation.app_id.clone()
        ),
        fl!(
            "activity-object-operation-name",
            operation = invocation.operation.clone()
        ),
        fl!("activity-object-operation-args"),
        serde_json::Value::from(invocation.args.clone()),
    )
}

fn object_field(label: String, value: &str, field: ObjectField) -> Element<'_, AppMessage> {
    Column::new()
        .spacing(4)
        .push(text(label.clone()).size(13.0))
        .push(
            widget::text_input(label, value)
                .on_input(move |value| AppMessage::Activities(Message::ObjectField(field, value))),
        )
        .into()
}

fn editable(state: ActivityState) -> bool {
    matches!(state, ActivityState::Active | ActivityState::Paused)
}

fn state_label(state: ActivityState) -> String {
    match state {
        ActivityState::Active => fl!("activity-active"),
        ActivityState::Paused => fl!("activity-paused"),
        ActivityState::Completed => fl!("activity-completed"),
        ActivityState::Cancelled => fl!("activity-cancelled"),
    }
}

fn field(label: String, value: &str, field: Field) -> Element<'_, AppMessage> {
    Column::new()
        .spacing(4)
        .push(text(label.clone()).size(13.0))
        .push(
            widget::text_input(label, value)
                .on_input(move |value| AppMessage::Activities(Message::Field(field, value))),
        )
        .into()
}

fn section(label: String, value: &str) -> Element<'_, AppMessage> {
    Column::new()
        .spacing(4)
        .push(text(label).size(14.0))
        .push(text(value).size(13.0))
        .into()
}

fn control(label: String, message: Message, enabled: bool) -> Element<'static, AppMessage> {
    let mut button = button::text(label);
    if enabled {
        button = button.on_press(AppMessage::Activities(message));
    }
    button.into()
}

fn destructive(label: String, message: Message, enabled: bool) -> Element<'static, AppMessage> {
    let mut button = button::text(label).class(theme::Button::Destructive);
    if enabled {
        button = button.on_press(AppMessage::Activities(message));
    }
    button.into()
}

fn error_card(error: &str) -> Element<'_, AppMessage> {
    container(text(error).size(13.0))
        .padding(12)
        .class(theme::Container::custom(styles::tool_error_card))
        .into()
}

fn job_view(job: &ActivityJobView, available: bool, can_retry: bool) -> Element<'_, AppMessage> {
    let mut content = Column::new()
        .spacing(6)
        .push(
            text(if job.title.is_empty() {
                &job.id
            } else {
                &job.title
            })
            .size(15.0),
        )
        .push(text(format!("{} · {} · {}", job.id, job.status, job.created_at)).size(12.0));
    if let Some(finished) = &job.finished_at {
        content =
            content.push(text(fl!("activity-job-finished", time = finished.clone())).size(11.0));
    }
    if let Some(response) = &job.response {
        content = content.push(text(response).size(13.0));
    }
    if let Some(error) = &job.error {
        content = content.push(error_card(error));
    }
    for waiting in &job.waiting_on {
        content =
            content.push(text(fl!("activity-waiting-on", reason = waiting.clone())).size(12.0));
    }
    let mut controls = Row::new().spacing(8);
    if let Some(session) = &job.session_id {
        controls = controls.push(
            button::text(fl!("activity-open-session"))
                .on_press(AppMessage::OpenActivitySession(session.clone())),
        );
    }
    if job.finished_at.is_none() && !matches!(job.status.as_str(), "ok" | "error" | "cancelled") {
        controls = controls.push(destructive(
            fl!("activity-stop-job"),
            Message::CancelJob(job.id.clone()),
            available,
        ));
    }
    if matches!(job.status.as_str(), "error" | "cancelled") {
        controls = controls.push(control(
            fl!("activity-retry-job"),
            Message::RetryJob(job.id.clone()),
            available && can_retry,
        ));
    }
    container(content.push(controls))
        .padding(12)
        .width(Length::Fill)
        .class(theme::Container::custom(styles::tool_card))
        .into()
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities.rs"
    ));
}
