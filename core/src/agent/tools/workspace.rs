//! Task-local Workspace data and reporting, never an owner management tool.

use super::exposure::{MemoryExposure, ToolExposure};
use super::{Tool, ToolResult};
use crate::agent::trust::{SourceKind, TrustClass};
use crate::workspaces::gateway::{self, ContextOperation};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub(crate) struct WorkspaceTool;

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum Request {
    Read(crate::workspaces::WorkspaceRead),
    Report(Report),
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Report {
    command: ReportCommand,
    title: String,
    content: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum ReportCommand {
    Report,
}

#[async_trait]
impl Tool for WorkspaceTool {
    fn name(&self) -> &str {
        "cos_workspace"
    }
    fn description(&self) -> &str {
        "Read the current Root-selected Workspace background, sourced shared notes, live read-only files \
         and local semantic file search. No owner, Workspace, database or authority selector exists. \
         Read source pages with revision and next_offset; indexed snapshots are rechecked against live \
         sharing and file bytes, not proof of factual correctness. Unsupported binary formats are errors. \
         Explicit temporary reads are limited to the human-selected notes/files and exact task/conversation. \
         Report writes a new immutable AgentReport with Root-bound Job provenance, never owner-confirmed \
         rules. It cannot create/delete Workspaces, tag files, grant permissions or import transcripts."
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{
            "command":{"type":"string","enum":["context","notes","resources","note","source","search","index","report"]},
            "note_id":{"type":"string"},"link_id":{"type":"string"},
            "path":{"type":"string"},"query":{"type":"string","maxLength":2048},
            "limit":{"type":"integer","minimum":1,"maximum":50,"default":10},
            "offset":{"type":"integer","minimum":0,"default":0},
            "max_chars":{"type":"integer","minimum":1,"maximum":16384,"default":4096},
            "revision":{"type":"string"},"title":{"type":"string","maxLength":240},
            "content":{"type":"string","maxLength":8192}
        },"required":["command"],"additionalProperties":false})
    }
    fn exposure(&self) -> ToolExposure {
        ToolExposure::always().requiring_memory(
            [crate::caps::Verb::MEMORY_READ],
            MemoryExposure::SystemAgent,
        )
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let request: Request = match serde_json::from_value(input) {
            Ok(request) => request,
            Err(error) => {
                return ToolResult::err(format!("invalid current-domain request: {error}"))
            }
        };
        let operation = match request {
            Request::Read(request) => ContextOperation::Workspace { request },
            Request::Report(note) => ContextOperation::Report {
                note: crate::workspaces::NoteDraft {
                    title: note.title,
                    content: note.content,
                },
            },
        };
        if let Err(error) = operation.validate() {
            return ToolResult::err(error);
        }
        if let Err(error) = super::require_memory(
            crate::caps::Verb::MEMORY_READ,
            super::MemoryScope::SystemAgent,
        ) {
            return ToolResult::err(error.to_string());
        }
        match gateway::request(operation).await {
            Ok(value) => read_result(&value),
            Err(error) => ToolResult::err(error),
        }
    }
}

pub(crate) fn read_result(value: &Value) -> ToolResult {
    let kind = match value
        .get("note")
        .and_then(|note| note.get("source"))
        .and_then(Value::as_str)
    {
        Some("owner_note") if value.get("imported") == Some(&json!(true)) => {
            SourceKind::WorkspaceImportedNote
        }
        Some("owner_note") => SourceKind::WorkspaceOwnerNote,
        Some("agent_report") => SourceKind::WorkspaceAgentReport,
        _ => SourceKind::WorkspaceSharedSource,
    };
    super::cos_proxy::memory::memory_result_with_class(
        kind,
        kind.class().least(TrustClass::UserControlledContext),
        value,
    )
}
