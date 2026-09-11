//! Composition of one request snapshot. No system/App operations are executed
//! here: current observations still go through the guarded tool trajectory.

use crate::agent::context::budget::{ContextBudget, ContextError};
use crate::agent::context::compressor::{estimate_text_tokens, estimate_tools_tokens};
use crate::agent::context::packet::{ContextBuilder, ContextKind, ContextPacket, ContextSection};
use crate::agent::llm::{Message, Tool as LlmTool};
use crate::agent::memory::notes::{NotesStore, USER_FILE};
use crate::agent::memory::sqlite_fts::MemoryDb;
use crate::agent::prompt;
use crate::agent::safety::redact::Redactor;
use crate::agent::tools::registry::ToolRegistry;

use super::deps::RuntimeDeps;

pub struct ContextRequest<'a> {
    pub budget: ContextBudget,
    pub system: &'a str,
    pub user_prompt: &'a str,
    pub transient_context: Option<&'a str>,
    pub tools: &'a ToolRegistry,
    pub llm_tools: &'a [LlmTool],
    pub redactor: Option<&'a Redactor>,
}

pub fn prepare(
    deps: &RuntimeDeps,
    request: ContextRequest<'_>,
) -> Result<ContextPacket, ContextError> {
    let ContextRequest {
        budget,
        system,
        user_prompt,
        transient_context,
        tools,
        llm_tools,
        redactor,
    } = request;
    budget.check(
        "system, tools and original request",
        system,
        &[Message::user_text(user_prompt)],
        llm_tools,
    )?;
    let fixed = estimate_text_tokens(system)
        .saturating_add(estimate_tools_tokens(llm_tools))
        .saturating_add(estimate_text_tokens(user_prompt))
        .saturating_add(16);
    let mut builder = ContextBuilder::new(budget, budget.input_tokens.saturating_sub(fixed));
    if let Some(context) = transient_context.filter(|value| !value.trim().is_empty()) {
        let content = match redactor {
            Some(redactor) => redactor.redact(context.trim()),
            None => context.trim().to_string(),
        };
        builder.required(ContextSection::new(
            ContextKind::Request,
            "request App/Activity context",
            content,
        ))?;
    }
    let reminders = match deps.paths() {
        Some(paths) => prompt::try_build_turn_context_segments_with(
            &crate::agent::nudge::NudgeStore::new(&paths.nudges_path),
            deps.now_ms() / 1000,
        ),
        None => prompt::try_build_turn_context_segments_with(
            &crate::agent::nudge::NudgeStore::new(crate::paths::agent_nudges_path()),
            deps.now_ms() / 1000,
        ),
    }
    .map_err(|detail| ContextError::Source {
        origin: "due reminders".into(),
        detail,
    })?;
    for reminder in reminders {
        let content = match redactor {
            Some(redactor) => redactor.redact(&reminder.content),
            None => reminder.content,
        };
        builder.required(ContextSection::new(
            ContextKind::Reminder,
            reminder.source,
            content,
        ))?;
    }
    let memory_tools: Vec<_> = [
        "cos_memory",
        "cos_recall",
        "cos_recall_semantic",
        "cos_app_memory",
    ]
    .into_iter()
    .filter(|name| tools.get(name).is_some() && llm_tools.iter().any(|tool| tool.name == *name))
    .collect();
    if !memory_tools.is_empty() {
        builder.required(ContextSection::new(
            ContextKind::Discovery,
            "exposed memory tools",
            format!(
                "Available memory tools: {}. Choose sources and queries according to \
                 the current request. Unprovided memories have not been searched and \
                 must not be assumed absent. Use normal guarded tool calls to read them.",
                memory_tools.join(", "),
            ),
        ))?;
    }
    if tools.guardrails().permits("cos_memory") {
        add_notes(&mut builder, deps.notes(), redactor)?;
    }
    builder.finish()
}

pub fn add_notes(
    builder: &mut ContextBuilder,
    notes: &NotesStore,
    redactor: Option<&Redactor>,
) -> Result<(), ContextError> {
    let entries = notes
        .context_entries()
        .map_err(|detail| ContextError::Source {
            origin: "memory notes".into(),
            detail,
        })?;
    for entry in entries {
        let content = match redactor {
            Some(redactor) => redactor.redact(&entry.content),
            None => entry.content,
        };
        let mut section = ContextSection::new(
            if entry.name == USER_FILE {
                ContextKind::UserNotes
            } else {
                ContextKind::MemoryNotes
            },
            entry.name,
            content,
        );
        section.revision = entry.revision;
        section.read_hint = format!("cos_memory command=read name={}", entry.name);
        builder.optional(section);
    }
    Ok(())
}

pub fn record(
    packet: &ContextPacket,
    recorder: Option<(&MemoryDb, &str)>,
) -> Result<(), ContextError> {
    let Some((db, session_id)) = recorder else {
        return Ok(());
    };
    for segment in packet.injected_segments() {
        db.record_injected(session_id, segment.source, &segment.content)
            .map_err(|error| ContextError::Source {
                origin: "request context recording".into(),
                detail: error.to_string(),
            })?;
    }
    if !packet.sections.is_empty() || packet.omitted_sections != 0 {
        let manifest = serde_json::json!({
            "version": packet.version,
            "budget": packet.budget,
            "selected": packet.sections.iter().map(|section| serde_json::json!({
                "kind": section.kind,
                "reference": section.reference,
                "revision": section.revision,
            })).collect::<Vec<_>>(),
            "omitted_sections": packet.omitted_sections,
        });
        db.record_injected(session_id, "context_selection", &manifest.to_string())
            .map_err(|error| ContextError::Source {
                origin: "context selection recording".into(),
                detail: error.to_string(),
            })?;
    }
    Ok(())
}

pub fn scrub_assistant_history(messages: Vec<Message>) -> Vec<Message> {
    let scrubber = crate::agent::context::think_scrub::ThinkScrubber::new();
    messages
        .into_iter()
        .flat_map(|message| {
            if message.role == crate::agent::llm::Role::Assistant {
                scrubber.scrub_messages(vec![message])
            } else {
                vec![message]
            }
        })
        .collect()
}

/// A compressor may replace older turns, but cannot replace the user's
/// current request, selected objects, or its explicit constraints.
pub fn preserve_request(messages: &mut Vec<Message>, request: &Message) {
    if !messages.contains(request) {
        let after_summary = first_summary(messages).is_some();
        messages.insert(usize::from(after_summary), request.clone());
    }
}

pub fn first_summary(messages: &[Message]) -> Option<&Message> {
    messages.first().filter(|message| {
        message.role == crate::agent::llm::Role::Assistant
            && message.content.iter().any(|block| {
                matches!(
                    block,
                    crate::agent::llm::ContentBlock::Text { text }
                        if text.starts_with(crate::agent::context::compressor::SUMMARY_MARKER)
                )
            })
    })
}

pub fn record_compaction(
    messages: &mut [Message],
    previous: Option<&Message>,
    recorder: Option<(&MemoryDb, &str)>,
    redactor: Option<&Redactor>,
) -> Result<(), ContextError> {
    if first_summary(messages).is_none() || first_summary(messages) == previous {
        return Ok(());
    }
    if let Some(message) = messages.first_mut() {
        for block in &mut message.content {
            let crate::agent::llm::ContentBlock::Text { text } = block else {
                continue;
            };
            if !text.starts_with(crate::agent::context::compressor::SUMMARY_MARKER) {
                continue;
            }
            if let Some(redactor) = redactor {
                *text = redactor.redact(text);
            }
            if let Some((db, session_id)) = recorder {
                db.record_injected(session_id, "context_compaction", text)
                    .map_err(|error| ContextError::Source {
                        origin: "context compaction recording".into(),
                        detail: error.to_string(),
                    })?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/runtime/context.rs"
    ));
}
