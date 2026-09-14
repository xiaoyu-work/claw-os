//! @agent-file
//! Responsibility: verify retained task/message membership before exposing history annotations.
//! Key dependencies: canonical memory bindings and owner-scoped Job projections.
//! Constraints: incomplete, interleaved, foreign, or legacy history fails closed.

use std::collections::{BTreeMap, BTreeSet};

use crate::agent::memory::conversation_bindings::{
    validate_task_id, BindingState, MessageTaskBinding,
};
use crate::agent::memory::conversations::ConversationHistoryPage;
use crate::session::SessionId;

use super::jobs::ConversationJobs;

pub(super) fn verify(
    session_id: &SessionId,
    state: &BindingState,
    visible_count: u64,
    jobs: &mut ConversationJobs,
) -> Result<(), String> {
    if state.oversized {
        return Err("task history membership exceeds the bounded verification size".to_string());
    }
    if state.members.len() as u64 != visible_count {
        return Err(
            "legacy or unbound canonical messages have no explicit task identity".to_string(),
        );
    }

    let by_id = state
        .members
        .iter()
        .map(|binding| (binding.message_id, binding))
        .collect::<BTreeMap<_, _>>();
    if by_id.len() != state.members.len() {
        return Err("duplicate canonical message membership".to_string());
    }

    let mut expected = BTreeMap::<String, BTreeSet<i64>>::new();
    for binding in &state.originals {
        validate_original(session_id, binding)?;
        if !expected
            .entry(binding.task_id.clone())
            .or_default()
            .insert(binding.source_message_id)
        {
            return Err("duplicate original message identity".to_string());
        }
    }

    let mut groups = BTreeMap::<String, BTreeSet<i64>>::new();
    let mut task_order = Vec::new();
    for binding in &state.members {
        validate_original(session_id, binding)?;
        let outer = by_id
            .get(&binding.user_message_id)
            .ok_or_else(|| "task membership has no retained outer user message".to_string())?;
        if !outer.is_user_prompt
            || outer.task_id != binding.task_id
            || outer.source_user_message_id != binding.source_user_message_id
            || outer.message_id != outer.user_message_id
        {
            return Err(
                "task membership does not match its explicit outer user message".to_string(),
            );
        }
        if !groups.contains_key(&binding.task_id) {
            task_order.push(binding.task_id.clone());
        } else if task_order.last() != Some(&binding.task_id) {
            return Err(
                "interleaved task memberships cannot be projected as complete turns".to_string(),
            );
        }
        if !groups
            .entry(binding.task_id.clone())
            .or_default()
            .insert(binding.source_message_id)
        {
            return Err("duplicate retained message identity".to_string());
        }
    }
    if expected != groups {
        return Err(
            "partial task history: retained rows do not contain each complete task".to_string(),
        );
    }
    jobs.mark_verified(&task_order)
}

pub(super) fn annotate(page: &mut ConversationHistoryPage) {
    let members = page
        .bindings
        .members
        .iter()
        .map(|binding| (binding.message_id, binding))
        .collect::<BTreeMap<_, _>>();
    for message in &mut page.messages {
        if let Some(binding) = members.get(&message.id) {
            message.task_id = Some(binding.task_id.clone());
            message.source_session_id = Some(binding.source_session_id.clone());
            message.source_message_id = Some(binding.source_message_id);
            message.source_user_message_id = Some(binding.source_user_message_id);
            message.is_user_prompt = Some(binding.is_user_prompt);
        }
    }
}

fn validate_original(session_id: &SessionId, binding: &MessageTaskBinding) -> Result<(), String> {
    validate_task_id(&binding.task_id).map_err(|error| error.to_string())?;
    if binding.session_id != session_id.as_str()
        || binding.source_session_id != session_id.as_str()
        || binding.message_id <= 0
        || binding.user_message_id <= 0
        || binding.message_id != binding.source_message_id
        || binding.user_message_id != binding.source_user_message_id
    {
        return Err("task membership names another canonical session".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!("../../../test/unit/clawd/conversations/bindings.rs");
}
