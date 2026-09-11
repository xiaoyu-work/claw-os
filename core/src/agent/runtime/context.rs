//! Composition of one request snapshot. No system/App operations are executed
//! here: current observations still go through the guarded tool trajectory.

use crate::agent::context::budget::request_tokens;
use crate::agent::context::budget::{ContextBudget, ContextError};
use crate::agent::context::packet::{ContextBuilder, ContextKind, ContextPacket, ContextSection};
use crate::agent::llm::{Message, Tool as LlmTool};
use crate::agent::memory::notes::{NotesStore, USER_FILE};
use crate::agent::memory::sqlite_fts::MemoryDb;
use crate::agent::prompt;
use crate::agent::safety::redact::Redactor;
use crate::agent::tools::exposure::ToolExposureContext;
use crate::agent::tools::registry::ToolRegistry;
use crate::agent::trust::{LabeledSegment, PromptProjection, SourceKind};

use super::deps::RuntimeDeps;

pub struct ContextRequest<'a> {
    pub budget: ContextBudget,
    pub system: &'a str,
    pub prelude: &'a [Message],
    pub user_prompt: &'a str,
    pub transient_context: Option<&'a str>,
    pub tools: &'a ToolRegistry,
    pub exposure: Option<&'a ToolExposureContext>,
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
        prelude,
        user_prompt,
        transient_context,
        tools,
        exposure,
        llm_tools,
        redactor,
    } = request;
    let mut required_messages = prelude.to_vec();
    let instruction =
        PromptProjection::new().with(LabeledSegment::of(SourceKind::UserMessage, user_prompt));
    required_messages.extend(instruction.instruction_message());
    budget.check(
        "system, tools and original request",
        system,
        &required_messages,
        llm_tools,
    )?;
    let fixed = request_tokens(system, &required_messages, llm_tools);
    let mut builder = ContextBuilder::new(budget, budget.input_tokens.saturating_sub(fixed));
    if let Some(context) = transient_context.filter(|value| !value.trim().is_empty()) {
        let content = match redactor {
            Some(redactor) => redactor.redact(context.trim()),
            None => context.trim().to_string(),
        };
        builder.required(ContextSection::new(
            ContextKind::Request,
            "request surface context",
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
            Some(redactor) => redactor.redact(&reminder.raw),
            None => reminder.raw.clone(),
        };
        builder.required(ContextSection::new(
            ContextKind::Reminder,
            reminder.source(),
            content,
        ))?;
    }
    let fallback = ToolExposureContext::isolated(tools.guardrails().clone());
    let exposure = exposure.unwrap_or(&fallback);
    let memory_tools: Vec<_> = [
        "cos_memory",
        "cos_recall",
        "cos_recall_semantic",
        "cos_app_memory",
    ]
    .into_iter()
    .filter(|name| {
        tools.get_for(exposure, name).is_some() && llm_tools.iter().any(|tool| tool.name == *name)
    })
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
    let profile_read = crate::caps::Cap::new(
        crate::caps::Verb::MEMORY_READ,
        crate::caps::Scope::self_ref(crate::agent::tools::SYSTEM_AGENT_MEMORY_SCOPE),
    );
    if tools.get_for(exposure, "cos_memory").is_some()
        && exposure.capabilities().covers(&profile_read)
    {
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
    for segment in packet.segments() {
        let rendered = segment.render_fenced(crate::agent::trust::envelope::process_seal());
        db.record_labeled_message(
            session_id,
            crate::agent::memory::sqlite_fts::INJECTED_ROLE,
            &segment,
            &rendered,
        )
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
        let segment = LabeledSegment::from_locator(
            SourceKind::SessionExtras,
            "context_selection",
            manifest.to_string(),
        );
        db.record_labeled_message(
            session_id,
            crate::agent::memory::sqlite_fts::INJECTED_ROLE,
            &segment,
            &segment.render_fenced(crate::agent::trust::envelope::process_seal()),
        )
        .map_err(|error| ContextError::Source {
            origin: "context selection recording".into(),
            detail: error.to_string(),
        })?;
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
