// SPDX-License-Identifier: GPL-3.0-only

//! Ephemeral presentation state. Only OS snapshots determine review status.
//!
//! Events carry the revision actually rendered. A busy request cannot emit a
//! second decision; polling cannot overwrite it while its helper/show is in
//! flight. A handled request remains visible until the popup closes. Nothing
//! here persists grants, interprets a digest as authority, or retries a decision.

use std::collections::{BTreeMap, BTreeSet};

use clawd_client::system_review::{
    PendingReviews, PermissionChoice, PermissionSelection, ReviewAction, ReviewDecision,
    ReviewError, ReviewStatus, SystemReview,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardPhase {
    Ready,
    Deciding,
    Refreshing,
    Unavailable,
}

#[derive(Clone, Debug)]
pub struct ReviewCardState {
    pub review: SystemReview,
    pub phase: CardPhase,
    pub notice: Option<String>,
    selected: BTreeMap<String, PermissionChoice>,
}

impl ReviewCardState {
    fn new(review: SystemReview) -> Self {
        Self {
            review,
            phase: CardPhase::Ready,
            notice: None,
            selected: BTreeMap::new(),
        }
    }

    pub fn selected(&self, permission: &str) -> Option<PermissionChoice> {
        self.selected.get(permission).copied()
    }

    pub fn busy(&self) -> bool {
        matches!(self.phase, CardPhase::Deciding | CardPhase::Refreshing)
    }

    pub fn can_choose(&self) -> bool {
        self.phase == CardPhase::Ready
            && self.review.status == ReviewStatus::Pending
            && self.review.actions.contains(&ReviewAction::ApplyChoices)
    }

    pub fn can_act(&self, action: ReviewAction) -> bool {
        self.phase == CardPhase::Ready
            && self.review.status == ReviewStatus::Pending
            && self.review.actions.contains(&action)
            && match action {
                ReviewAction::Cancel => true,
                ReviewAction::ApplyChoices => !self.selected.is_empty(),
                _ => self.selected.is_empty(),
            }
    }

    pub fn has_choices(&self) -> bool {
        !self.selected.is_empty()
    }

    fn replace(&mut self, review: SystemReview) -> Result<(), ReviewError> {
        review.validate()?;
        if review.revision < self.review.revision {
            return Ok(());
        }
        if review.revision == self.review.revision && review != self.review {
            return Err(ReviewError(
                "OS reused a revision for a different review; refresh required".into(),
            ));
        }
        if review.revision != self.review.revision {
            self.selected.clear();
        }
        self.review = review;
        self.phase = CardPhase::Ready;
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct ReviewModel {
    pub cards: Vec<ReviewCardState>,
    pub error: Option<String>,
}

impl ReviewModel {
    pub fn pending_count(&self) -> usize {
        self.cards
            .iter()
            .filter(|card| card.review.status == ReviewStatus::Pending)
            .count()
    }

    /// Missing pending rows are shown again before their controls can be reused.
    pub fn receive_pending(&mut self, pending: PendingReviews) -> Result<Vec<String>, ReviewError> {
        pending.validate()?;
        let mut next = self.cards.clone();
        let mut seen = BTreeSet::new();
        for review in pending.reviews {
            seen.insert(review.id.clone());
            if let Some(card) = next.iter_mut().find(|card| card.review.id == review.id) {
                if !card.busy() {
                    card.replace(review)?;
                }
            } else {
                next.push(ReviewCardState::new(review));
            }
        }
        let mut refresh = Vec::new();
        for card in &mut next {
            if !seen.contains(&card.review.id)
                && card.review.status == ReviewStatus::Pending
                && !card.busy()
            {
                card.phase = CardPhase::Refreshing;
                refresh.push(card.review.id.clone());
            }
        }
        self.cards = next;
        self.error = None;
        Ok(refresh)
    }

    pub fn load_failed(&mut self, message: String) {
        self.error = Some(message);
        for card in &mut self.cards {
            if !card.busy() && card.review.status == ReviewStatus::Pending {
                card.phase = CardPhase::Unavailable;
            }
        }
    }

    pub fn select(
        &mut self,
        id: &str,
        displayed_revision: u64,
        permission_id: &str,
        choice: Option<PermissionChoice>,
    ) -> Result<(), ReviewError> {
        if self.error.is_some() {
            return Err(ReviewError(
                "refresh the OS review queue before choosing".into(),
            ));
        }
        let card = self.card_mut(id)?;
        if card.review.revision != displayed_revision {
            return Err(ReviewError(
                "the displayed review changed before selection".into(),
            ));
        }
        if !card.can_choose() {
            return Err(ReviewError(
                "this review is not accepting permission choices".into(),
            ));
        }
        let permission = card
            .review
            .permissions
            .iter()
            .find(|permission| permission.id == permission_id)
            .ok_or_else(|| ReviewError("unknown permission id".into()))?;
        if let Some(choice) = choice {
            if !permission.supported_choices.contains(&choice) {
                return Err(ReviewError(
                    "the OS does not support this permission choice".into(),
                ));
            }
            card.selected.insert(permission_id.into(), choice);
        } else {
            card.selected.remove(permission_id);
        }
        Ok(())
    }

    pub fn begin(
        &mut self,
        id: &str,
        displayed_revision: u64,
        action: ReviewAction,
    ) -> Result<ReviewDecision, ReviewError> {
        if self.error.is_some() {
            return Err(ReviewError(
                "refresh the OS review queue before deciding".into(),
            ));
        }
        let card = self.card_mut(id)?;
        if card.review.revision != displayed_revision {
            return Err(ReviewError(
                "the displayed review changed before the decision".into(),
            ));
        }
        if !card.can_act(action) {
            return Err(ReviewError(
                "decision is busy, unsupported, or needs an explicit selection".into(),
            ));
        }
        let choices = if action == ReviewAction::ApplyChoices {
            card.selected
                .iter()
                .map(|(permission_id, choice)| PermissionSelection {
                    permission_id: permission_id.clone(),
                    choice: *choice,
                })
                .collect()
        } else {
            Vec::new()
        };
        let decision = ReviewDecision {
            id: card.review.id.clone(),
            revision: card.review.revision,
            action,
            choices,
        };
        decision.encode_for(&card.review)?;
        card.phase = CardPhase::Deciding;
        card.notice = None;
        Ok(decision)
    }

    /// Even a successful decision reply is followed by a root-broker show request.
    pub fn decision_returned(
        &mut self,
        id: &str,
        revision: u64,
        result: Result<SystemReview, String>,
    ) -> Result<(), ReviewError> {
        let card = self.card_mut(id)?;
        if card.phase != CardPhase::Deciding || card.review.revision != revision {
            return Err(ReviewError("outdated decision completion".into()));
        }
        card.phase = CardPhase::Refreshing;
        card.notice = result.err();
        Ok(())
    }

    pub fn request_refresh(&mut self, id: &str) -> Result<(), ReviewError> {
        let card = self.card_mut(id)?;
        if card.busy() {
            return Err(ReviewError(
                "request already has an operation in flight".into(),
            ));
        }
        card.phase = CardPhase::Refreshing;
        Ok(())
    }

    pub fn receive_show(&mut self, review: SystemReview) -> Result<(), ReviewError> {
        review.validate()?;
        let card = self.card_mut(&review.id)?;
        if card.phase == CardPhase::Deciding {
            return Err(ReviewError("a decision is still in flight".into()));
        }
        if review.revision < card.review.revision {
            return Err(ReviewError("OS returned an older review revision".into()));
        }
        card.replace(review)
    }

    pub fn show_failed(&mut self, id: &str, message: String) -> Result<(), ReviewError> {
        let card = self.card_mut(id)?;
        card.phase = CardPhase::Unavailable;
        card.notice = Some(message);
        Ok(())
    }

    pub fn closed(&mut self) {
        self.cards
            .retain(|card| card.review.status == ReviewStatus::Pending || card.busy());
        for card in &mut self.cards {
            if !card.busy() {
                card.selected.clear();
            }
        }
    }

    fn card_mut(&mut self, id: &str) -> Result<&mut ReviewCardState, ReviewError> {
        self.cards
            .iter_mut()
            .find(|card| card.review.id == id)
            .ok_or_else(|| ReviewError("unknown review id".into()))
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/review.rs"));
}
