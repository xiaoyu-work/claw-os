//! Fixed capability-rule controls and revision-bound forms, never approval decisions.

use cos_agent_protocol::{
    CapabilityPolicyDraft, CapabilityPolicyMode, CapabilityPolicyRule, CapabilityPolicyScope,
    MAX_CAPABILITY_POLICY_RULES, MAX_CAPABILITY_POLICY_SCOPES,
};

use super::{
    Action, Activities, ActivityCapabilityPolicy, ActivityCapabilityPolicyEnabledRequest,
    ActivityCapabilityPolicyResponse, ActivityCapabilityPolicySetRequest, ActivityDetailResponse,
    AppMessage, Column, Element, Length, Message as ActivityMessage, Request, Row, container,
    control, destructive, editable, fl, section, styles, text, theme, widget,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Path,
    Host,
    Name,
    SelfRef,
    Wild,
}

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    Configure,
    AddRule,
    RemoveRule(usize),
    Verb(usize, String),
    Mode(usize, CapabilityPolicyMode),
    AddScope(usize),
    RemoveScope(usize, usize),
    ScopeKind(usize, usize, ScopeKind),
    ScopeValue(usize, usize, String),
    Save,
    Discard,
    SetEnabled(bool),
}

#[derive(Debug, Default)]
pub(super) struct State {
    pub(super) response: Option<ActivityCapabilityPolicyResponse>,
    pub(super) form: Option<Form>,
}

#[derive(Debug)]
pub(super) struct Form {
    activity_id: String,
    previous: Option<Box<ActivityCapabilityPolicy>>,
    draft: CapabilityPolicyDraft,
}

impl Form {
    fn new(activity_id: String, previous: Option<&ActivityCapabilityPolicy>) -> Self {
        Self {
            activity_id,
            previous: previous.cloned().map(Box::new),
            draft: CapabilityPolicyDraft {
                rules: previous.map_or_else(Vec::new, |policy| policy.rules.clone()),
            },
        }
    }

    fn request(&self) -> Result<ActivityCapabilityPolicySetRequest, String> {
        let request = ActivityCapabilityPolicySetRequest {
            expected_revision: self.previous.as_ref().map(|policy| policy.revision),
            policy: self.draft.clone(),
        };
        request.validate_shape().map_err(str::to_owned)?;
        Ok(request)
    }
}

fn empty_scope() -> CapabilityPolicyScope {
    CapabilityPolicyScope::Path {
        value: String::new(),
    }
}

fn scope_kind(scope: &CapabilityPolicyScope) -> ScopeKind {
    match scope {
        CapabilityPolicyScope::Path { .. } => ScopeKind::Path,
        CapabilityPolicyScope::Host { .. } => ScopeKind::Host,
        CapabilityPolicyScope::Name { .. } => ScopeKind::Name,
        CapabilityPolicyScope::SelfRef { .. } => ScopeKind::SelfRef,
        CapabilityPolicyScope::Wild {} => ScopeKind::Wild,
    }
}

fn change_scope_kind(scope: &mut CapabilityPolicyScope, kind: ScopeKind) {
    let value = scope.value().unwrap_or_default().to_owned();
    *scope = match kind {
        ScopeKind::Path => CapabilityPolicyScope::Path { value },
        ScopeKind::Host => CapabilityPolicyScope::Host { value },
        ScopeKind::Name => CapabilityPolicyScope::Name { value },
        ScopeKind::SelfRef => CapabilityPolicyScope::SelfRef { value },
        ScopeKind::Wild => CapabilityPolicyScope::Wild {},
    };
}

impl Activities {
    pub(super) fn update_capability_policy(
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
        {
            return None;
        }
        match message {
            Message::Refresh if self.capability_policy.form.is_none() => {
                return self.refresh_capability_policy(connected);
            }
            Message::Configure if self.capability_policy.form.is_none() => {
                let detail = self.detail.as_ref()?;
                if !editable(detail.activity.state) {
                    self.error = Some(fl!("activity-capability-policy-readonly"));
                    return None;
                }
                let Some(response) = &self.capability_policy.response else {
                    self.error = Some(fl!("activity-capability-policy-load-first"));
                    return None;
                };
                if self.selected.as_deref() != Some(detail.activity.id.as_str())
                    || !response.matches_activity(&detail.activity.id)
                {
                    self.error = Some(fl!("activity-capability-policy-invalid-response"));
                    return None;
                }
                self.capability_policy.form = Some(Form::new(
                    detail.activity.id.clone(),
                    response.capability_policy.as_ref(),
                ));
                self.invalidate_operation_preview();
                self.error = None;
                self.notice = None;
            }
            Message::Save => return self.save_capability_policy(connected),
            Message::Discard => {
                self.capability_policy.form = None;
                self.error = None;
                return self.refresh_capability_policy(connected);
            }
            Message::SetEnabled(enabled) if self.capability_policy.form.is_none() => {
                return self.set_capability_policy_enabled(enabled, connected);
            }
            Message::AddRule => {
                let form = self.capability_policy.form.as_mut()?;
                if form.draft.rules.len() >= MAX_CAPABILITY_POLICY_RULES {
                    self.error = Some(fl!("activity-capability-policy-rule-limit"));
                    return None;
                }
                form.draft.rules.push(CapabilityPolicyRule {
                    verb: String::new(),
                    mode: CapabilityPolicyMode::Normal,
                    scopes: vec![empty_scope()],
                });
            }
            Message::RemoveRule(index) => {
                if let Some(form) = &mut self.capability_policy.form
                    && index < form.draft.rules.len()
                {
                    form.draft.rules.remove(index);
                }
            }
            Message::Verb(index, verb) => {
                if let Some(rule) = self.capability_rule_mut(index) {
                    rule.verb = verb;
                }
            }
            Message::Mode(index, mode) => {
                if let Some(rule) = self.capability_rule_mut(index) {
                    rule.mode = mode;
                    if mode == CapabilityPolicyMode::Deny {
                        rule.scopes.clear();
                    } else if rule.scopes.is_empty() {
                        rule.scopes.push(empty_scope());
                    }
                }
            }
            Message::AddScope(index) => {
                if let Some(rule) = self.capability_rule_mut(index)
                    && rule.mode != CapabilityPolicyMode::Deny
                {
                    if rule.scopes.len() >= MAX_CAPABILITY_POLICY_SCOPES {
                        self.error = Some(fl!("activity-capability-policy-scope-limit"));
                    } else {
                        rule.scopes.push(empty_scope());
                    }
                }
            }
            Message::RemoveScope(rule_index, scope_index) => {
                if let Some(rule) = self.capability_rule_mut(rule_index)
                    && scope_index < rule.scopes.len()
                {
                    rule.scopes.remove(scope_index);
                }
            }
            Message::ScopeKind(rule_index, scope_index, kind) => {
                if let Some(scope) = self
                    .capability_rule_mut(rule_index)
                    .and_then(|rule| rule.scopes.get_mut(scope_index))
                {
                    change_scope_kind(scope, kind);
                }
            }
            Message::ScopeValue(rule_index, scope_index, value) => {
                if let Some(scope) = self
                    .capability_rule_mut(rule_index)
                    .and_then(|rule| rule.scopes.get_mut(scope_index))
                {
                    match scope {
                        CapabilityPolicyScope::Path { value: current }
                        | CapabilityPolicyScope::Host { value: current }
                        | CapabilityPolicyScope::Name { value: current }
                        | CapabilityPolicyScope::SelfRef { value: current } => *current = value,
                        CapabilityPolicyScope::Wild {} => {}
                    }
                }
            }
            _ => {}
        }
        None
    }

    fn capability_rule_mut(&mut self, index: usize) -> Option<&mut CapabilityPolicyRule> {
        self.capability_policy
            .form
            .as_mut()?
            .draft
            .rules
            .get_mut(index)
    }

    fn refresh_capability_policy(&mut self, connected: bool) -> Option<Request> {
        let id = self.selected.clone()?;
        self.capability_policy.response = None;
        self.begin(Action::GetCapabilityPolicy(id), connected)
    }

    fn save_capability_policy(&mut self, connected: bool) -> Option<Request> {
        let form = self.capability_policy.form.as_ref()?;
        let detail = self.detail.as_ref()?;
        if !editable(detail.activity.state) {
            self.error = Some(fl!("activity-capability-policy-readonly"));
            return None;
        }
        if self.selected.as_deref() != Some(form.activity_id.as_str())
            || detail.activity.id != form.activity_id
        {
            self.error = Some(fl!("activity-capability-policy-invalid-response"));
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
            Action::SetCapabilityPolicy {
                activity_id: form.activity_id.clone(),
                request,
                previous: form.previous.clone(),
            },
            connected,
        )
    }

    fn set_capability_policy_enabled(&mut self, enabled: bool, connected: bool) -> Option<Request> {
        let detail = self.detail.as_ref()?;
        if enabled && !editable(detail.activity.state) {
            self.error = Some(fl!("activity-capability-policy-readonly"));
            return None;
        }
        let Some(previous) = self
            .capability_policy
            .response
            .as_ref()
            .and_then(|response| response.capability_policy.as_ref())
        else {
            self.error = Some(fl!("activity-capability-policy-load-first"));
            return None;
        };
        if self.selected.as_deref() != Some(detail.activity.id.as_str())
            || !previous.matches_activity(&detail.activity.id)
        {
            self.error = Some(fl!("activity-capability-policy-invalid-response"));
            return None;
        }
        if previous.enabled == enabled {
            return None;
        }
        let request = ActivityCapabilityPolicyEnabledRequest {
            expected_revision: previous.revision,
            enabled,
        };
        if let Err(error) = request.validate_shape() {
            self.error = Some(error.into());
            return None;
        }
        self.begin(
            Action::EnableCapabilityPolicy {
                activity_id: detail.activity.id.clone(),
                request,
                previous: Box::new(previous.clone()),
            },
            connected,
        )
    }

    pub(super) fn capability_policy_loaded(
        &mut self,
        activity_id: &str,
        response: ActivityCapabilityPolicyResponse,
    ) {
        if self.visible
            && self.selected.as_deref() == Some(activity_id)
            && self.capability_policy.form.is_none()
            && response.matches_activity(activity_id)
        {
            self.capability_policy.response = Some(response);
        } else {
            self.error = Some(fl!("activity-capability-policy-invalid-response"));
        }
    }

    pub(super) fn capability_policy_saved(
        &mut self,
        activity_id: &str,
        request: &ActivityCapabilityPolicySetRequest,
        previous: Option<&ActivityCapabilityPolicy>,
        policy: ActivityCapabilityPolicy,
        connected: bool,
    ) -> Option<Request> {
        let valid = self.visible
            && self.selected.as_deref() == Some(activity_id)
            && self.capability_policy.form.as_ref().is_some_and(|form| {
                form.activity_id == activity_id
                    && form.previous.as_deref() == previous
                    && form.request().is_ok_and(|current| current == *request)
            })
            && policy.matches_set(activity_id, request)
            && previous.is_none_or(|previous| {
                request.expected_revision == Some(previous.revision)
                    && policy.enabled == previous.enabled
                    && policy.preserves_identity(previous)
            });
        if !valid {
            self.error = Some(fl!("activity-capability-policy-invalid-response"));
            return None;
        }
        self.capability_policy.form = None;
        self.notice = Some(fl!(
            "activity-capability-policy-saved",
            revision = policy.revision.to_string()
        ));
        self.refresh_capability_policy(connected)
    }

    pub(super) fn capability_policy_enabled(
        &mut self,
        activity_id: &str,
        request: &ActivityCapabilityPolicyEnabledRequest,
        previous: &ActivityCapabilityPolicy,
        policy: ActivityCapabilityPolicy,
        connected: bool,
    ) -> Option<Request> {
        let current = self
            .capability_policy
            .response
            .as_ref()
            .and_then(|response| response.capability_policy.as_ref());
        if !self.visible
            || self.selected.as_deref() != Some(activity_id)
            || current != Some(previous)
            || request.expected_revision != previous.revision
            || !policy.matches_enabled(activity_id, request)
            || !policy.preserves_identity(previous)
            || !policy.rules_match(&previous.rules)
        {
            self.error = Some(fl!("activity-capability-policy-invalid-response"));
            return None;
        }
        self.notice = Some(if policy.enabled {
            fl!(
                "activity-capability-policy-enabled-notice",
                revision = policy.revision.to_string()
            )
        } else {
            fl!(
                "activity-capability-policy-disabled-notice",
                revision = policy.revision.to_string()
            )
        });
        self.refresh_capability_policy(connected)
    }

    pub(super) fn capability_policy_view<'a>(
        &'a self,
        detail: &'a ActivityDetailResponse,
        available: bool,
    ) -> Element<'a, AppMessage> {
        let can_configure = editable(detail.activity.state);
        let mut card = Column::new()
            .spacing(10)
            .push(text(fl!("activity-capability-policy")).size(18.0))
            .push(text(fl!("activity-capability-policy-authority-hint")).size(12.0))
            .push(text(fl!("activity-capability-policy-approval-hint")).size(12.0))
            .push(text(fl!("activity-capability-policy-denial-hint")).size(12.0))
            .push(text(fl!("activity-capability-policy-revocation-hint")).size(12.0))
            .push(text(fl!("activity-capability-policy-disable-hint")).size(12.0));
        if let Some(form) = &self.capability_policy.form {
            card = card.push(self.capability_policy_form_view(form, available && can_configure));
        } else {
            card = card.push(
                Row::new()
                    .spacing(8)
                    .push(policy_control(
                        fl!("activity-capability-policy-refresh"),
                        Message::Refresh,
                        available,
                    ))
                    .push(policy_control(
                        fl!("activity-capability-policy-configure"),
                        Message::Configure,
                        available && can_configure && self.capability_policy.response.is_some(),
                    )),
            );
        }
        if !can_configure {
            card = card.push(text(fl!("activity-capability-policy-readonly")).size(12.0));
        }
        match self.capability_policy.response.as_ref() {
            None => card = card.push(text(fl!("activity-capability-policy-load-first")).size(12.0)),
            Some(response) => match &response.capability_policy {
                None => {
                    card =
                        card.push(text(fl!("activity-capability-policy-unconfigured")).size(13.0))
                }
                Some(policy) => {
                    card = card
                        .push(text(enabled_label(policy.enabled)).size(15.0))
                        .push(
                            text(fl!(
                                "activity-capability-policy-revision",
                                revision = policy.revision.to_string()
                            ))
                            .size(12.0),
                        )
                        .push(section(
                            fl!("activity-capability-policy-created"),
                            &policy.created_at,
                        ))
                        .push(section(
                            fl!("activity-capability-policy-updated"),
                            &policy.updated_at,
                        ))
                        .push(text(fl!("activity-capability-policy-snapshot-hint")).size(12.0));
                    if policy.rules.is_empty() {
                        card = card.push(text(fl!("activity-capability-policy-empty")).size(12.0));
                    }
                    for rule in &policy.rules {
                        card = card.push(rule_view(rule));
                    }
                    card = card.push(policy_control(
                        if policy.enabled {
                            fl!("activity-capability-policy-disable")
                        } else {
                            fl!("activity-capability-policy-enable")
                        },
                        Message::SetEnabled(!policy.enabled),
                        available
                            && self.capability_policy.form.is_none()
                            && (policy.enabled || can_configure),
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

    fn capability_policy_form_view<'a>(
        &'a self,
        form: &'a Form,
        available: bool,
    ) -> Element<'a, AppMessage> {
        let editable = self.pending.is_none();
        let mut content = Column::new()
            .spacing(10)
            .push(text(fl!("activity-capability-policy-catalogue-hint")).size(12.0))
            .push(text(fl!("activity-capability-policy-cas-hint")).size(12.0))
            .push(text(fl!("activity-capability-policy-bounds-hint")).size(12.0))
            .push(
                text(match &form.previous {
                    Some(previous) => fl!(
                        "activity-capability-policy-expected-revision",
                        revision = previous.revision.to_string()
                    ),
                    None => fl!("activity-capability-policy-initial"),
                })
                .size(12.0),
            );
        if form.draft.rules.is_empty() {
            content = content.push(text(fl!("activity-capability-policy-empty")).size(12.0));
        }
        for (rule_index, rule) in form.draft.rules.iter().enumerate() {
            let mut row = Column::new()
                .spacing(6)
                .push(
                    widget::text_input(fl!("activity-capability-policy-verb"), &rule.verb)
                        .on_input(move |value| app_message(Message::Verb(rule_index, value))),
                )
                .push(mode_controls(rule_index, rule.mode, editable));
            if rule.mode == CapabilityPolicyMode::Deny {
                row = row.push(text(fl!("activity-capability-policy-deny-whole-verb")).size(12.0));
            } else {
                for (scope_index, scope) in rule.scopes.iter().enumerate() {
                    row = row.push(scope_editor(rule_index, scope_index, scope, editable));
                }
                row = row.push(policy_control(
                    fl!("activity-capability-policy-add-scope"),
                    Message::AddScope(rule_index),
                    editable && rule.scopes.len() < MAX_CAPABILITY_POLICY_SCOPES,
                ));
            }
            row = row.push(policy_control(
                fl!("activity-capability-policy-remove-rule"),
                Message::RemoveRule(rule_index),
                editable,
            ));
            content = content.push(
                container(row)
                    .padding(8)
                    .class(theme::Container::custom(styles::tool_card)),
            );
        }
        content
            .push(policy_control(
                fl!("activity-capability-policy-add-rule"),
                Message::AddRule,
                editable && form.draft.rules.len() < MAX_CAPABILITY_POLICY_RULES,
            ))
            .push(
                Row::new()
                    .spacing(8)
                    .push(policy_control(
                        fl!("activity-capability-policy-save"),
                        Message::Save,
                        available,
                    ))
                    .push(policy_control(
                        fl!("activity-capability-policy-discard"),
                        Message::Discard,
                        editable,
                    )),
            )
            .into()
    }
}

fn app_message(message: Message) -> AppMessage {
    AppMessage::Activities(ActivityMessage::CapabilityPolicy(message))
}

fn policy_control(label: String, message: Message, enabled: bool) -> Element<'static, AppMessage> {
    if matches!(message, Message::SetEnabled(false)) {
        destructive(label, ActivityMessage::CapabilityPolicy(message), enabled)
    } else {
        control(label, ActivityMessage::CapabilityPolicy(message), enabled)
    }
}

fn mode_label(mode: CapabilityPolicyMode) -> String {
    match mode {
        CapabilityPolicyMode::Normal => fl!("activity-capability-policy-mode-normal"),
        CapabilityPolicyMode::RequireApproval => fl!("activity-capability-policy-mode-ask"),
        CapabilityPolicyMode::Deny => fl!("activity-capability-policy-mode-deny"),
    }
}

fn enabled_label(enabled: bool) -> String {
    if enabled {
        fl!("activity-capability-policy-enabled")
    } else {
        fl!("activity-capability-policy-disabled")
    }
}

fn kind_label(kind: ScopeKind) -> String {
    match kind {
        ScopeKind::Path => fl!("activity-capability-policy-scope-path"),
        ScopeKind::Host => fl!("activity-capability-policy-scope-host"),
        ScopeKind::Name => fl!("activity-capability-policy-scope-name"),
        ScopeKind::SelfRef => fl!("activity-capability-policy-scope-self"),
        ScopeKind::Wild => fl!("activity-capability-policy-scope-wild"),
    }
}

fn mode_controls(
    index: usize,
    selected: CapabilityPolicyMode,
    editable: bool,
) -> Element<'static, AppMessage> {
    let mut row = Row::new().spacing(6);
    for mode in [
        CapabilityPolicyMode::Normal,
        CapabilityPolicyMode::RequireApproval,
        CapabilityPolicyMode::Deny,
    ] {
        row = row.push(policy_control(
            mode_label(mode),
            Message::Mode(index, mode),
            editable && mode != selected,
        ));
    }
    Column::new()
        .spacing(4)
        .push(text(mode_label(selected)).size(13.0))
        .push(row)
        .into()
}

fn scope_editor(
    rule_index: usize,
    scope_index: usize,
    scope: &CapabilityPolicyScope,
    editable: bool,
) -> Element<'_, AppMessage> {
    let mut kinds = Row::new().spacing(4);
    for kind in [
        ScopeKind::Path,
        ScopeKind::Host,
        ScopeKind::Name,
        ScopeKind::SelfRef,
        ScopeKind::Wild,
    ] {
        kinds = kinds.push(policy_control(
            kind_label(kind),
            Message::ScopeKind(rule_index, scope_index, kind),
            editable && scope_kind(scope) != kind,
        ));
    }
    let mut content = Column::new()
        .spacing(4)
        .push(text(kind_label(scope_kind(scope))).size(12.0))
        .push(kinds);
    if let Some(value) = scope.value() {
        content = content.push(
            widget::text_input(fl!("activity-capability-policy-scope-value"), value).on_input(
                move |value| app_message(Message::ScopeValue(rule_index, scope_index, value)),
            ),
        );
    } else {
        content = content.push(text(fl!("activity-capability-policy-wild-hint")).size(12.0));
    }
    content
        .push(policy_control(
            fl!("activity-capability-policy-remove-scope"),
            Message::RemoveScope(rule_index, scope_index),
            editable,
        ))
        .into()
}

fn rule_view(rule: &CapabilityPolicyRule) -> Element<'_, AppMessage> {
    let mut content = Column::new()
        .spacing(4)
        .push(text(&rule.verb).size(14.0))
        .push(text(mode_label(rule.mode)).size(13.0));
    if rule.mode == CapabilityPolicyMode::Deny {
        content = content.push(text(fl!("activity-capability-policy-deny-whole-verb")).size(12.0));
    } else {
        for scope in &rule.scopes {
            content = content.push(text(kind_label(scope_kind(scope))).size(12.0));
            content = content.push(match scope.value() {
                Some(value) => text(value).size(12.0),
                None => text(fl!("activity-capability-policy-wild-hint")).size(12.0),
            });
        }
    }
    content.into()
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/capability_policy.rs"
    ));
}
