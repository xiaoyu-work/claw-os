//! Ephemeral copy/paste continuity presentation with explicit local placement.

use cos_agent_protocol::{
    ActivityContinuityDocument, ActivityContinuityImportAcknowledgement,
    ActivityContinuityImportRequest, ActivityExecutionPlacement,
    MAX_ACTIVITY_CONTINUITY_DOCUMENT_BYTES,
};

use super::{
    Action, Activities, ActivityDetailResponse, AppMessage, Column, Element, Length,
    Message as ActivityMessage, Request, Row, container, control, fl, styles, text, theme, widget,
};

#[derive(Debug, Clone)]
pub enum Message {
    Export,
    ImportText(String),
    SelectLocalPlacement,
    ClearPlacement,
    Import,
    ClearImport,
}

#[derive(Debug, Default)]
pub(super) struct State {
    exported: Option<Exported>,
    import_text: String,
    placement: Option<ActivityExecutionPlacement>,
    input_error: Option<String>,
}

#[derive(Debug)]
struct Exported {
    activity_id: String,
    document: ActivityContinuityDocument,
}

impl State {
    pub(super) fn is_editing(&self) -> bool {
        !self.import_text.is_empty() || self.placement.is_some()
    }
}

impl Activities {
    pub(super) fn update_continuity(
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
        {
            return None;
        }
        match message {
            Message::Export => {
                let id = self.selected.clone()?;
                let detail = self.detail.as_ref()?;
                if detail.activity.id != id {
                    self.error = Some(fl!("activity-continuity-invalid-response"));
                    return None;
                }
                self.continuity.exported = None;
                self.begin(Action::ExportContinuity(id), connected)
            }
            Message::ImportText(value) => {
                self.continuity.placement = None;
                if value.len() > MAX_ACTIVITY_CONTINUITY_DOCUMENT_BYTES {
                    self.continuity.input_error = Some(fl!(
                        "activity-continuity-too-large",
                        bytes = MAX_ACTIVITY_CONTINUITY_DOCUMENT_BYTES.to_string()
                    ));
                } else {
                    self.continuity.import_text = value;
                    self.continuity.input_error = None;
                }
                None
            }
            Message::SelectLocalPlacement => {
                if self.selected.is_none() && !self.continuity.import_text.trim().is_empty() {
                    self.continuity.placement = Some(ActivityExecutionPlacement::Local);
                    self.continuity.input_error = None;
                }
                None
            }
            Message::ClearPlacement => {
                self.continuity.placement = None;
                None
            }
            Message::Import => self.import_continuity(connected),
            Message::ClearImport => {
                self.continuity = State::default();
                self.error = None;
                None
            }
        }
    }

    fn import_continuity(&mut self, connected: bool) -> Option<Request> {
        if self.selected.is_some()
            || self.continuity.placement != Some(ActivityExecutionPlacement::Local)
        {
            self.continuity.input_error = Some(fl!("activity-continuity-placement-required"));
            return None;
        }
        let document =
            match ActivityContinuityDocument::from_json(self.continuity.import_text.as_bytes()) {
                Ok(document) => document,
                Err(error) => {
                    self.continuity.input_error = Some(error);
                    return None;
                }
            };
        let canonical = match document.to_json() {
            Ok(canonical) => canonical,
            Err(error) => {
                self.continuity.input_error = Some(error);
                return None;
            }
        };
        let request = ActivityContinuityImportRequest {
            placement: ActivityExecutionPlacement::Local,
            document: canonical,
        };
        self.continuity.input_error = None;
        self.begin(
            Action::ImportContinuity {
                request,
                document: Box::new(document),
            },
            connected,
        )
    }

    pub(super) fn continuity_exported(
        &mut self,
        activity_id: &str,
        document: ActivityContinuityDocument,
    ) {
        if self.visible
            && self.selected.as_deref() == Some(activity_id)
            && document.validate().is_ok()
        {
            self.continuity.exported = Some(Exported {
                activity_id: activity_id.into(),
                document,
            });
            self.notice = Some(fl!("activity-continuity-exported"));
        } else {
            self.error = Some(fl!("activity-continuity-invalid-response"));
        }
    }

    pub(crate) fn continuity_copy_text(&self) -> Option<String> {
        let exported = self.continuity.exported.as_ref()?;
        if !self.visible || self.selected.as_deref() != Some(exported.activity_id.as_str()) {
            return None;
        }
        exported.document.to_json().ok()
    }

    pub(super) fn continuity_imported(
        &mut self,
        request: &ActivityContinuityImportRequest,
        document: &ActivityContinuityDocument,
        acknowledgement: ActivityContinuityImportAcknowledgement,
        connected: bool,
    ) -> Option<Request> {
        let current = ActivityContinuityDocument::from_json(self.continuity.import_text.as_bytes());
        if !self.visible
            || self.selected.is_some()
            || self.continuity.placement != Some(ActivityExecutionPlacement::Local)
            || request.placement != ActivityExecutionPlacement::Local
            || request.validated_document().as_ref() != Ok(document)
            || current.as_ref() != Ok(document)
            || !acknowledgement.matches(document, request.placement)
            || self
                .list
                .iter()
                .any(|activity| activity.id == acknowledgement.activity.id)
        {
            self.error = Some(fl!("activity-continuity-invalid-ack"));
            return None;
        }
        let activity_id = acknowledgement.activity.id;
        self.continuity = State::default();
        self.selected = Some(activity_id.clone());
        self.detail = None;
        self.notice = Some(fl!(
            "activity-continuity-imported",
            id = activity_id.clone(),
            revision = acknowledgement.continuity_revision.to_string()
        ));
        self.begin(Action::Get(activity_id), connected)
    }

    pub(super) fn continuity_export_view<'a>(
        &'a self,
        detail: &'a ActivityDetailResponse,
        available: bool,
    ) -> Element<'a, AppMessage> {
        let mut card =
            Column::new()
                .spacing(8)
                .push(text(fl!("activity-continuity")).size(18.0))
                .push(text(fl!("activity-continuity-safe-contents")).size(12.0))
                .push(text(fl!("activity-continuity-non-goals")).size(12.0))
                .push(text(fl!("activity-continuity-copy-paste-only")).size(12.0))
                .push(
                    Row::new()
                        .spacing(8)
                        .push(continuity_control(
                            fl!("activity-continuity-export"),
                            Message::Export,
                            available,
                        ))
                        .push(copy_control(
                            fl!("activity-continuity-copy"),
                            available
                                && self.continuity.exported.as_ref().is_some_and(|exported| {
                                    exported.activity_id == detail.activity.id
                                }),
                        )),
                );
        if let Some(exported) = self
            .continuity
            .exported
            .as_ref()
            .filter(|exported| exported.activity_id == detail.activity.id)
        {
            card = card
                .push(
                    text(fl!(
                        "activity-continuity-lineage",
                        id = exported.document.lineage.id.clone(),
                        revision = exported.document.lineage.revision.to_string()
                    ))
                    .size(12.0),
                )
                .push(
                    text(fl!(
                        "activity-continuity-snapshot",
                        digest = exported.document.snapshot.clone()
                    ))
                    .size(12.0),
                );
        }
        container(card)
            .padding(12)
            .width(Length::Fill)
            .class(theme::Container::custom(styles::tool_card))
            .into()
    }

    pub(super) fn continuity_import_view(&self, available: bool) -> Element<'_, AppMessage> {
        let placement_selected =
            self.continuity.placement == Some(ActivityExecutionPlacement::Local);
        let mut card = Column::new()
            .spacing(8)
            .push(text(fl!("activity-continuity-import")).size(18.0))
            .push(text(fl!("activity-continuity-safe-contents")).size(12.0))
            .push(text(fl!("activity-continuity-non-goals")).size(12.0))
            .push(text(fl!("activity-continuity-copy-paste-only")).size(12.0))
            .push(
                widget::text_input(
                    fl!("activity-continuity-json"),
                    &self.continuity.import_text,
                )
                .on_input(|value| {
                    AppMessage::Activities(ActivityMessage::Continuity(Message::ImportText(value)))
                }),
            )
            .push(
                text(fl!(
                    "activity-continuity-input-bound",
                    bytes = MAX_ACTIVITY_CONTINUITY_DOCUMENT_BYTES.to_string()
                ))
                .size(12.0),
            );
        if let Some(error) = &self.continuity.input_error {
            card = card.push(text(error).size(12.0));
        }
        card = card
            .push(
                text(if placement_selected {
                    fl!("activity-continuity-local-selected")
                } else {
                    fl!("activity-continuity-placement-unselected")
                })
                .size(12.0),
            )
            .push(
                Row::new()
                    .spacing(8)
                    .push(continuity_control(
                        if placement_selected {
                            fl!("activity-continuity-clear-placement")
                        } else {
                            fl!("activity-continuity-select-local")
                        },
                        if placement_selected {
                            Message::ClearPlacement
                        } else {
                            Message::SelectLocalPlacement
                        },
                        available && !self.continuity.import_text.trim().is_empty(),
                    ))
                    .push(continuity_control(
                        fl!("activity-continuity-import-paused"),
                        Message::Import,
                        available
                            && placement_selected
                            && !self.continuity.import_text.trim().is_empty(),
                    ))
                    .push(continuity_control(
                        fl!("activity-continuity-clear"),
                        Message::ClearImport,
                        self.pending.is_none() && self.continuity.is_editing(),
                    )),
            );
        container(card)
            .padding(12)
            .width(Length::Fill)
            .class(theme::Container::custom(styles::tool_card))
            .into()
    }
}

fn continuity_control(
    label: String,
    message: Message,
    enabled: bool,
) -> Element<'static, AppMessage> {
    control(label, ActivityMessage::Continuity(message), enabled)
}

fn copy_control(label: String, enabled: bool) -> Element<'static, AppMessage> {
    let mut button = widget::button::text(label);
    if enabled {
        button = button.on_press(AppMessage::CopyActivityContinuity);
    }
    button.into()
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities/continuity.rs"
    ));
}
