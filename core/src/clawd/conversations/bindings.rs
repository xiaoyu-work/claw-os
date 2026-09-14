//! @agent-file
//! Responsibility: verify retained task/message membership before exposing history annotations.
//! Key dependencies: canonical memory bindings and owner-scoped Job projections.
//! Constraints: incomplete, interleaved, foreign, or legacy history fails closed.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::agent::memory::conversation_bindings::{
    validate_task_id, BindingState, MessageTaskBinding,
};
use crate::agent::memory::conversations::ConversationHistoryPage;
use crate::agent::service::Store;
use crate::session::{SessionId, SessionMeta};

use super::jobs::ConversationJobs;
use super::{ConversationId, Presentation};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskMembership {
    pub(super) task_id: String,
    pub(super) source_session_id: String,
    pub(super) message_ids: Vec<i64>,
}

#[derive(Debug)]
pub(super) struct VerifiedHistory {
    pub(super) jobs: ConversationJobs,
    pub(super) memberships: Vec<TaskMembership>,
}

pub(super) fn verify(
    meta: &SessionMeta,
    presentation: &Presentation,
    state: &BindingState,
    visible_count: u64,
    owner_uid: u32,
) -> Result<VerifiedHistory, String> {
    verify_with_scope(meta, presentation, state, visible_count, owner_uid, true)
}

pub(super) fn verify_retained(
    meta: &SessionMeta,
    presentation: &Presentation,
    state: &BindingState,
    visible_count: u64,
    owner_uid: u32,
) -> Result<VerifiedHistory, String> {
    verify_with_scope(meta, presentation, state, visible_count, owner_uid, false)
}

fn verify_with_scope(
    meta: &SessionMeta,
    presentation: &Presentation,
    state: &BindingState,
    visible_count: u64,
    owner_uid: u32,
    require_exact_current_jobs: bool,
) -> Result<VerifiedHistory, String> {
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

    let ancestors = if state
        .members
        .iter()
        .any(|binding| binding.source_session_id != meta.id.as_str())
        || !presentation.inherited_tasks.is_empty()
    {
        ancestors(presentation, owner_uid)?
    } else {
        BTreeSet::new()
    };

    let mut expected = BTreeMap::<(String, String), BTreeSet<i64>>::new();
    for binding in &state.originals {
        validate_original(&meta.id, binding)?;
        if !expected
            .entry(key(binding))
            .or_default()
            .insert(binding.source_message_id)
        {
            return Err("duplicate original message identity".to_string());
        }
    }

    let mut inherited_rows = 0usize;
    for inherited in &presentation.inherited_tasks {
        validate_task_id(&inherited.task_id).map_err(|error| error.to_string())?;
        let source = ConversationId::parse(&inherited.source_session_id)?;
        if source.session_id() == &meta.id
            || !ancestors.contains(inherited.source_session_id.as_str())
            || inherited.message_ids.is_empty()
        {
            return Err(
                "inherited task source is outside the owner-scoped session lineage".to_string(),
            );
        }
        inherited_rows = inherited_rows.saturating_add(inherited.message_ids.len());
        if inherited_rows > crate::agent::memory::conversation_bindings::MAX_BINDING_ROWS {
            return Err("inherited task history exceeds the bounded verification size".to_string());
        }
        let ids = inherited
            .message_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        if ids.len() != inherited.message_ids.len() || ids.iter().any(|id| *id <= 0) {
            return Err("invalid inherited task message identity".to_string());
        }
        if expected
            .insert(
                (
                    inherited.source_session_id.clone(),
                    inherited.task_id.clone(),
                ),
                ids,
            )
            .is_some()
        {
            return Err("duplicate inherited task membership".to_string());
        }
    }

    let mut groups = BTreeMap::<(String, String), BTreeSet<i64>>::new();
    let mut task_order = Vec::new();
    for binding in &state.members {
        validate_member(&meta.id, &ancestors, binding)?;
        let outer = by_id
            .get(&binding.user_message_id)
            .ok_or_else(|| "task membership has no retained outer user message".to_string())?;
        if !outer.is_user_prompt
            || key(outer) != key(binding)
            || outer.source_message_id != binding.source_user_message_id
            || outer.source_user_message_id != binding.source_user_message_id
            || outer.message_id != outer.user_message_id
        {
            return Err(
                "task membership does not match its explicit outer user message".to_string(),
            );
        }
        let key = key(binding);
        let starts_group = !groups.contains_key(&key);
        if starts_group && !binding.is_user_prompt {
            return Err("task membership does not start with its outer user message".to_string());
        }
        if !starts_group && binding.is_user_prompt {
            return Err("task membership contains more than one outer user message".to_string());
        }
        if starts_group {
            task_order.push(key.clone());
        } else if task_order.last() != Some(&key) {
            return Err(
                "interleaved task memberships cannot be projected as complete turns".to_string(),
            );
        }
        if !groups
            .entry(key)
            .or_default()
            .insert(binding.source_message_id)
        {
            return Err("duplicate inherited message identity".to_string());
        }
    }
    if expected != groups {
        return Err(
            "partial task history: retained rows do not contain each complete task".to_string(),
        );
    }

    if require_exact_current_jobs {
        let current_order = task_order
            .iter()
            .filter(|(source, _)| source == meta.id.as_str())
            .map(|(_, task_id)| task_id.clone())
            .collect::<Vec<_>>();
        let mut current = super::jobs::load(&meta.id, owner_uid)?;
        current.mark_verified(&current_order)?;
    }

    let mut jobs = Vec::with_capacity(task_order.len());
    if !task_order.is_empty() {
        let store = Store::open_default().map_err(|error| error.to_string())?;
        for (source_session_id, task_id) in &task_order {
            let source = ConversationId::parse(source_session_id)?;
            super::owned_meta(&source, owner_uid)?;
            let Some((_, job)) = store
                .locate_for_owner(task_id, Some(owner_uid))
                .map_err(|error| error.to_string())?
            else {
                return Err("bound task evidence is unavailable for this owner".to_string());
            };
            if job.id != *task_id || job.session_id.as_deref() != Some(source_session_id) {
                return Err("bound task is not owned by its recorded source session".to_string());
            }
            jobs.push(job);
        }
    }
    let jobs = super::jobs::project_retained(jobs)?;
    let memberships = task_order
        .into_iter()
        .map(|(source_session_id, task_id)| TaskMembership {
            message_ids: groups[&(source_session_id.clone(), task_id.clone())]
                .iter()
                .copied()
                .collect(),
            task_id,
            source_session_id,
        })
        .collect();
    Ok(VerifiedHistory { jobs, memberships })
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
        || binding.is_user_prompt != (binding.message_id == binding.user_message_id)
    {
        return Err("task membership names another canonical session".to_string());
    }
    Ok(())
}

fn validate_member(
    session_id: &SessionId,
    ancestors: &BTreeSet<String>,
    binding: &MessageTaskBinding,
) -> Result<(), String> {
    validate_task_id(&binding.task_id).map_err(|error| error.to_string())?;
    if binding.session_id != session_id.as_str()
        || binding.message_id <= 0
        || binding.user_message_id <= 0
        || binding.source_message_id <= 0
        || binding.source_user_message_id <= 0
        || (binding.source_session_id != session_id.as_str()
            && !ancestors.contains(&binding.source_session_id))
        || binding.is_user_prompt != (binding.message_id == binding.user_message_id)
    {
        return Err("task membership names another canonical session".to_string());
    }
    if binding.source_session_id == session_id.as_str()
        && (binding.message_id != binding.source_message_id
            || binding.user_message_id != binding.source_user_message_id)
    {
        return Err("invalid original task membership".to_string());
    }
    Ok(())
}

fn key(binding: &MessageTaskBinding) -> (String, String) {
    (binding.source_session_id.clone(), binding.task_id.clone())
}

fn ancestors(presentation: &Presentation, owner_uid: u32) -> Result<BTreeSet<String>, String> {
    let mut ancestors = BTreeSet::new();
    let mut next = presentation.parent_id.clone();
    for _ in 0..64 {
        let Some(parent_id) = next else {
            return Ok(ancestors);
        };
        let parent = ConversationId::parse(&parent_id)?;
        if !ancestors.insert(parent_id) {
            return Err("cyclic conversation lineage".to_string());
        }
        let meta = super::owned_meta(&parent, owner_uid)?;
        next = super::read_presentation(&meta.id)?.parent_id;
    }
    Err("conversation lineage exceeds the bounded verification depth".to_string())
}

#[cfg(test)]
mod tests {
    include!("../../../test/unit/clawd/conversations/bindings.rs");
}
