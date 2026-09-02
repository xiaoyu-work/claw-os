//! Anti-corruption translation from clawd's generic JSON envelopes into the
//! stable desktop presentation protocol.

use cos_agent_protocol::{
    CancelResponse, DeltaPayload, DonePayload, HistoryResponse, SessionSummary, StreamEvent,
    TaskStarted, ToolResultPayload, ToolStartPayload, ToolUsePayload, ToolUseStartPayload,
    TurnDonePayload, Usage, WarningPayload,
};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct CoreTaskStream {
    pub cursor: u64,
    #[serde(default)]
    pub events: Vec<CoreStreamRecord>,
    pub job: CoreJob,
    pub terminal: bool,
}

#[derive(Debug, Deserialize)]
pub struct CoreJob {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub response: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub turns_used: Option<u32>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub cancelled: bool,
    #[serde(default)]
    pub cancel_requested: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

impl CoreJob {
    pub fn into_done(self) -> Result<DonePayload, String> {
        if self.status == "error" {
            return Err(self
                .error
                .unwrap_or_else(|| "agent task failed".to_string()));
        }
        let answer = self.response.clone();
        Ok(DonePayload {
            event_type: "done".to_string(),
            task_id: self.id,
            session_id: self.session_id,
            answer,
            response: self.response,
            turns_used: self.turns_used,
            provider: self.provider,
            model: self.model,
        })
    }

    pub fn into_cancel(self) -> CancelResponse {
        CancelResponse {
            id: self.id,
            status: self.status,
            cancelled: self.cancelled,
            cancel_requested: self.cancel_requested,
            reason: self.reason,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CoreStreamRecord {
    #[serde(default)]
    event: Option<CoreAgentEvent>,
    #[serde(default)]
    progress: Option<CoreProgress>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CoreAgentEvent {
    TextDelta {
        #[serde(default)]
        text: String,
    },
    ToolUseStart {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
    },
    ToolInputDelta,
    ToolUse(CoreToolCall),
    Message {
        #[serde(default)]
        content: Vec<CoreContentBlock>,
        #[serde(default)]
        tool_calls: Vec<CoreToolCall>,
    },
    Done {
        #[serde(default)]
        finish: Option<String>,
        #[serde(default)]
        usage: Usage,
    },
    Warning {
        #[serde(default)]
        message: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct CoreToolCall {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct CoreContentBlock {
    #[serde(rename = "type", alias = "kind")]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize)]
struct CoreProgress {
    kind: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    ok: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CoreSessions {
    sessions: Vec<SessionSummary>,
}

pub fn task_started(value: Value) -> Result<TaskStarted, String> {
    #[derive(Deserialize)]
    struct Submission {
        id: String,
        #[serde(default)]
        session_id: Option<String>,
    }

    let submission: Submission = serde_json::from_value(value)
        .map_err(|error| format!("invalid task.submit result: {error}"))?;
    if submission.id.is_empty() {
        return Err("clawd task.submit returned an empty task id".to_string());
    }
    Ok(TaskStarted {
        task_id: submission.id,
        session_id: submission.session_id,
    })
}

pub fn task_stream(value: Value) -> Result<CoreTaskStream, String> {
    serde_json::from_value(value).map_err(|error| format!("invalid task.stream result: {error}"))
}

pub fn cancel_response(value: Value) -> Result<CancelResponse, String> {
    serde_json::from_value::<CoreJob>(value)
        .map(CoreJob::into_cancel)
        .map_err(|error| format!("invalid task.cancel result: {error}"))
}

pub fn sessions(value: Value) -> Result<Vec<SessionSummary>, String> {
    let mut sessions = serde_json::from_value::<CoreSessions>(value)
        .map(|response| response.sessions)
        .map_err(|error| format!("invalid memory.sessions result: {error}"))?;
    for session in &mut sessions {
        let compact = session
            .title
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        session.title = if compact.is_empty() {
            format!("Session {}", session.id.chars().take(8).collect::<String>())
        } else if compact.chars().count() > 80 {
            format!("{}...", compact.chars().take(80).collect::<String>())
        } else {
            compact
        };
    }
    Ok(sessions)
}

pub fn history(value: Value) -> Result<HistoryResponse, String> {
    let mut response: HistoryResponse = serde_json::from_value(value)
        .map_err(|error| format!("invalid memory.history result: {error}"))?;
    response.messages.retain(|message| message.role != "system");
    response.n = response.messages.len();
    Ok(response)
}

pub fn stream_events(
    record: CoreStreamRecord,
    turn_emitted_text: &mut bool,
    emitted_any_text: &mut bool,
) -> Vec<StreamEvent> {
    if let Some(progress) = record.progress {
        return progress_event(progress).into_iter().collect();
    }
    let Some(event) = record.event else {
        return Vec::new();
    };
    match event {
        CoreAgentEvent::TextDelta { text } if !text.is_empty() => {
            *turn_emitted_text = true;
            *emitted_any_text = true;
            vec![StreamEvent::Delta(DeltaPayload::new(text))]
        }
        CoreAgentEvent::ToolUseStart { id, name } => {
            vec![StreamEvent::ToolUseStart(ToolUseStartPayload { id, name })]
        }
        CoreAgentEvent::ToolInputDelta => Vec::new(),
        CoreAgentEvent::ToolUse(call) => vec![tool_use_event(call)],
        CoreAgentEvent::Message {
            content,
            tool_calls,
        } => {
            let mut events = Vec::new();
            if !*turn_emitted_text {
                let text = content
                    .into_iter()
                    .filter(|block| block.kind == "text")
                    .map(|block| block.text)
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.is_empty() {
                    *emitted_any_text = true;
                    events.push(StreamEvent::Delta(DeltaPayload::new(text)));
                }
            }
            events.extend(tool_calls.into_iter().map(tool_use_event));
            events
        }
        CoreAgentEvent::Done { finish, usage } => {
            *turn_emitted_text = false;
            vec![StreamEvent::TurnDone(TurnDonePayload { finish, usage })]
        }
        CoreAgentEvent::Warning { message } => {
            vec![StreamEvent::Warning(WarningPayload { message })]
        }
        CoreAgentEvent::TextDelta { .. } | CoreAgentEvent::Other => Vec::new(),
    }
}

fn tool_use_event(call: CoreToolCall) -> StreamEvent {
    StreamEvent::ToolUse(ToolUsePayload {
        id: call.id,
        name: call.name,
        input: None,
    })
}

fn progress_event(progress: CoreProgress) -> Option<StreamEvent> {
    match progress.kind.as_str() {
        "tool_start" => Some(StreamEvent::ToolStart(ToolStartPayload {
            kind: Some(progress.kind),
            id: progress.id,
            name: progress.name,
            input: None,
        })),
        "tool_result" => Some(StreamEvent::ToolResult(ToolResultPayload {
            kind: Some(progress.kind),
            id: progress.id,
            name: progress.name,
            ok: progress.ok,
            preview: None,
            output: None,
            content: None,
            text: None,
            is_error: None,
        })),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/translation.rs"
    ));
}
