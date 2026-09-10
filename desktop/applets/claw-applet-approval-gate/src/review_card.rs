// SPDX-License-Identifier: GPL-3.0-only

//! Reusable OS review card. It emits intentions, never capabilities.

use clawd_client::system_review::{
    PermissionChoice, ReviewAction, ReviewKind, ReviewText, ScopeKind, display_text,
};
use cosmic::{
    Element,
    iced::Length,
    theme,
    widget::{button, column, container, divider, row, text},
};

use crate::{
    localize::review_label,
    review::{CardPhase, ReviewCardState},
};

#[derive(Clone, Debug)]
pub enum ReviewMessage {
    Select {
        id: String,
        revision: u64,
        permission: String,
        choice: Option<PermissionChoice>,
    },
    Decide {
        id: String,
        revision: u64,
        action: ReviewAction,
    },
    Refresh(String),
}

pub fn review_card(card: &ReviewCardState) -> Element<'_, ReviewMessage> {
    let spacing = theme::active().cosmic().spacing;
    let review = &card.review;
    let mut content = column::with_capacity(16)
        .spacing(spacing.space_xs)
        .push(text::heading(display_text(&review.subject.name)))
        .push(text(format!(
            "{} - {}",
            review_label(review.kind.text()),
            review_label(review.status.text())
        )))
        .push(detail(ReviewText::Request, &review.id))
        .push(detail(ReviewText::Revision, &review.revision.to_string()));
    for (key, value) in [
        (ReviewText::AppId, &review.subject.app_id),
        (ReviewText::Version, &review.subject.version),
        (ReviewText::Publisher, &review.subject.publisher),
    ] {
        content = content.push(detail(
            key,
            value
                .as_deref()
                .unwrap_or(&review_label(ReviewText::Unreported)),
        ));
    }
    content = content.push(detail(ReviewText::Initiator, &review.initiator));
    for item in &review.context {
        content = content.push(named_detail(&item.label, &item.value));
    }
    content = content.push(text::body(review_label(
        if review.kind == ReviewKind::Capability {
            ReviewText::CapabilityNotice
        } else {
            ReviewText::NoGrantNotice
        },
    )));
    if let Some(message) = &review.status_message {
        content = content.push(text::body(display_text(message)));
    }
    match card.phase {
        CardPhase::Deciding => content = content.push(text::body(review_label(ReviewText::Busy))),
        CardPhase::Refreshing => {
            content = content.push(text::body(review_label(ReviewText::RefreshRequired)))
        }
        CardPhase::Unavailable => {
            content = content.push(text::body(review_label(ReviewText::Error)))
        }
        CardPhase::Ready => {}
    }
    if let Some(notice) = &card.notice {
        content = content.push(text::body(display_text(notice)));
    }
    content = content
        .push(divider::horizontal::default())
        .push(text::heading(review_label(ReviewText::Permissions)));
    if review.permissions.is_empty() {
        content = content.push(text::body(review_label(ReviewText::NoPermissions)));
    }
    for permission in &review.permissions {
        let mut section = column::with_capacity(12)
            .spacing(spacing.space_xs)
            .push(text::heading(display_text(&permission.label)))
            .push(text(format!(
                "{} - {}",
                display_text(&permission.verb),
                review_label(permission.risk.text())
            )))
            .push(text::body(display_text(&permission.blurb)))
            .push(detail(
                if permission.scope.kind == ScopeKind::LateBound {
                    ReviewText::LateBound
                } else {
                    ReviewText::Scope
                },
                &permission.scope.description,
            ));
        if let Some(condition) = &permission.condition {
            section = section.push(detail(ReviewText::Condition, condition));
        }
        for usage in &permission.uses {
            section = section
                .push(detail(ReviewText::AffectedFunction, &usage.function))
                .push(detail(ReviewText::AppPurpose, &usage.purpose));
        }
        section = section.push(detail(
            ReviewText::CurrentChoice,
            &review_label(
                permission
                    .current
                    .map_or(ReviewText::Unreported, PermissionChoice::text),
            ),
        ));
        if review.actions.contains(&ReviewAction::ApplyChoices)
            && !permission.supported_choices.is_empty()
        {
            for choice in
                std::iter::once(None).chain(permission.supported_choices.iter().copied().map(Some))
            {
                let label =
                    review_label(choice.map_or(ReviewText::Unchanged, PermissionChoice::text));
                let selected = card.selected(&permission.id) == choice;
                let control = if selected {
                    button::suggested(format!("[x] {label}"))
                } else {
                    button::standard(format!("[ ] {label}"))
                };
                section = section.push(control.on_press_maybe(card.can_choose().then(|| {
                    ReviewMessage::Select {
                        id: review.id.clone(),
                        revision: review.revision,
                        permission: permission.id.clone(),
                        choice,
                    }
                })));
            }
        }
        if let Some(reason) = &permission.unsupported_reason {
            section = section.push(detail(ReviewText::Unsupported, reason));
        }
        content = content
            .push(container(section).padding(spacing.space_s))
            .push(divider::horizontal::default());
    }
    if !review.disclosures.is_empty() {
        content = content.push(text::heading(review_label(ReviewText::Disclosures)));
        for disclosure in &review.disclosures {
            content = content.push(named_detail(&disclosure.label, &disclosure.value));
        }
    }
    if let Some(changes) = &review.changes {
        content = content
            .push(text::heading(review_label(ReviewText::Changes)))
            .push(text::body(display_text(&changes.summary)));
        for change in &changes.details {
            content = content.push(named_detail(&change.label, &change.value));
        }
    }
    if let Some(digest) = &review.contract_digest {
        content = content
            .push(detail(ReviewText::ContractDigest, digest))
            .push(text::caption(review_label(ReviewText::DigestNotice)));
    }
    if card.has_choices() && review.kind != ReviewKind::Capability {
        content = content.push(text::caption(review_label(ReviewText::SeparateChoices)));
    }
    if review.actions.is_empty() {
        content = content.push(text::body(review_label(ReviewText::NoActions)));
    }
    // Cancellation is first in keyboard order, independent of server list order.
    let actions = std::iter::once(ReviewAction::Cancel)
        .filter(|action| review.actions.contains(action))
        .chain(
            review
                .actions
                .iter()
                .copied()
                .filter(|action| *action != ReviewAction::Cancel),
        );
    for action in actions {
        content = content.push(
            button::standard(review_label(action.text())).on_press_maybe(
                card.can_act(action).then(|| ReviewMessage::Decide {
                    id: review.id.clone(),
                    revision: review.revision,
                    action,
                }),
            ),
        );
    }
    content = content.push(
        button::standard(review_label(ReviewText::Refresh))
            .on_press_maybe((!card.busy()).then(|| ReviewMessage::Refresh(review.id.clone()))),
    );
    container(content)
        .padding(spacing.space_s)
        .width(Length::Fill)
        .into()
}

fn detail(label: ReviewText, value: &str) -> Element<'static, ReviewMessage> {
    named_detail(&review_label(label), value)
}

fn named_detail(label: &str, value: &str) -> Element<'static, ReviewMessage> {
    row![
        text::caption(display_text(label)).width(Length::Fixed(130.0)),
        text::body(display_text(value)).width(Length::Fill),
    ]
    .spacing(8)
    .into()
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/review_card.rs"
    ));
}
