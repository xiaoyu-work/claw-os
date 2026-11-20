use cosmic::widget::markdown;
use futures::future::AbortHandle;

use crate::bridge::{StreamEvent, ToolCallView, ToolPhase, ToolResultView};
use crate::fl;
use crate::session::{ChatMessage, SessionState};

struct ActiveStream {
    generation: u64,
    session_index: usize,
    task_id: Option<String>,
    abort: Option<AbortHandle>,
    markdown: markdown::Content,
    pending_stop: Option<String>,
}

impl ActiveStream {
    fn append_text(&mut self, message: &mut ChatMessage, delta: &str) {
        message.content.push_str(delta);
        self.markdown.push_str(delta);
        let items = self.markdown.items();
        message.parsed_markdown = (!items.is_empty()).then(|| items.to_vec());
    }
}

enum StreamPhase {
    Idle,
    Active(ActiveStream),
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StreamReduction {
    Applied,
    Terminal,
    Failed { session_index: usize },
    CancelRemote { task_id: String },
    Stale,
}

pub(crate) enum CancelRequest {
    AwaitTask,
    Remote { task_id: String, generation: u64 },
}

pub(crate) struct StreamState {
    next_generation: u64,
    phase: StreamPhase,
}

impl Default for StreamState {
    fn default() -> Self {
        Self {
            next_generation: 0,
            phase: StreamPhase::Idle,
        }
    }
}

impl StreamState {
    pub(crate) fn is_active(&self) -> bool {
        matches!(&self.phase, StreamPhase::Active(active) if active.pending_stop.is_none())
    }

    pub(crate) fn is_cancelling(&self) -> bool {
        matches!(&self.phase, StreamPhase::Active(active) if active.pending_stop.is_some())
    }

    pub(crate) fn session_index(&self) -> Option<usize> {
        match &self.phase {
            StreamPhase::Active(active) => Some(active.session_index),
            StreamPhase::Idle | StreamPhase::Terminal => None,
        }
    }

    pub(crate) fn task_id(&self) -> Option<&str> {
        match &self.phase {
            StreamPhase::Active(active) => active.task_id.as_deref(),
            StreamPhase::Idle | StreamPhase::Terminal => None,
        }
    }

    pub(crate) fn detach(&mut self, sessions: &mut SessionState) -> Option<usize> {
        let StreamPhase::Active(mut active) = std::mem::replace(&mut self.phase, StreamPhase::Idle)
        else {
            return None;
        };
        if let Some(abort) = active.abort.take() {
            abort.abort();
        }
        sessions.finalize_stream(active.session_index, None, false);
        self.next_generation = self.next_generation.wrapping_add(1);
        Some(active.session_index)
    }

    pub(crate) fn start(&mut self, session_index: usize, abort: AbortHandle) -> u64 {
        self.next_generation = self.next_generation.wrapping_add(1);
        let generation = self.next_generation;
        self.phase = StreamPhase::Active(ActiveStream {
            generation,
            session_index,
            task_id: None,
            abort: Some(abort),
            markdown: markdown::Content::new(),
            pending_stop: None,
        });
        generation
    }

    pub(crate) fn reduce(
        &mut self,
        generation: u64,
        event: StreamEvent,
        sessions: &mut SessionState,
    ) -> StreamReduction {
        let StreamPhase::Active(active) = &mut self.phase else {
            return StreamReduction::Stale;
        };
        if active.generation != generation {
            return StreamReduction::Stale;
        }
        let session_index = active.session_index;

        match event {
            StreamEvent::TaskStarted(started) => {
                let needs_cancel = active.pending_stop.is_some() && active.task_id.is_none();
                active.task_id = (!started.task_id.is_empty()).then_some(started.task_id);
                if let Some(message) = sessions.streaming_assistant_mut(session_index) {
                    message.task_id = active.task_id.clone();
                }
                sessions.capture_provisional(session_index, started.session_id.as_deref());
                if needs_cancel
                    && let Some(task_id) = active.task_id.clone()
                {
                    return StreamReduction::CancelRemote { task_id };
                }
                StreamReduction::Applied
            }
            StreamEvent::Delta(delta) => {
                if let Some(tail) = active.pending_stop.as_mut() {
                    tail.push_str(&delta.text);
                } else if let Some(message) = sessions.streaming_assistant_mut(session_index) {
                    active.append_text(message, &delta.text);
                }
                StreamReduction::Applied
            }
            StreamEvent::Reasoning(payload) => {
                if let Some(message) = sessions.streaming_assistant_mut(session_index) {
                    message.append_reasoning(payload.summary);
                }
                StreamReduction::Applied
            }
            StreamEvent::Evidence(report) => {
                if let Some(message) = sessions.streaming_assistant_mut(session_index) {
                    message.evidence = Some(report);
                }
                StreamReduction::Applied
            }
            StreamEvent::MemorySources(payload) => {
                if let Some(message) = sessions.streaming_assistant_mut(session_index) {
                    for source in payload.sources {
                        if !message.memory_sources.iter().any(|existing| existing.source == source.source && existing.revision == source.revision) {
                            message.memory_sources.push(source);
                        }
                    }
                }
                StreamReduction::Applied
            }
            StreamEvent::ToolUseStart(payload) => {
                if let Some(message) = sessions.streaming_assistant_mut(session_index) {
                    message.upsert_tool_call(ToolCallView {
                        id: payload.id,
                        name: payload.name,
                        input: serde_json::Value::Null,
                        partial_json: String::new(),
                        in_progress: true,
                        phase: Some(ToolPhase::Preparing),
                    });
                }
                StreamReduction::Applied
            }
            StreamEvent::ToolInputDelta(payload) => {
                if let Some(message) = sessions.streaming_assistant_mut(session_index) {
                    if let Some(call) = message
                        .tool_calls
                        .iter_mut()
                        .find(|call| call.id == payload.id)
                    {
                        call.partial_json.push_str(&payload.delta);
                        call.in_progress = true;
                        call.phase = Some(ToolPhase::Preparing);
                    } else {
                        message.upsert_tool_call(ToolCallView {
                            id: payload.id,
                            name: fl!("tool-unnamed"),
                            input: serde_json::Value::Null,
                            partial_json: payload.delta,
                            in_progress: true,
                            phase: Some(ToolPhase::Preparing),
                        });
                    }
                }
                StreamReduction::Applied
            }
            StreamEvent::ToolUse(payload) => {
                if let Some(message) = sessions.streaming_assistant_mut(session_index) {
                    message.upsert_tool_call(ToolCallView {
                        id: payload.id,
                        name: payload.name,
                        input: payload.input.unwrap_or(serde_json::Value::Null),
                        partial_json: String::new(),
                        in_progress: false,
                        phase: Some(ToolPhase::Preparing),
                    });
                }
                StreamReduction::Applied
            }
            StreamEvent::ToolStart(payload) => {
                if let Some(message) = sessions.streaming_assistant_mut(session_index) {
                    message
                        .tool_results
                        .retain(|result| result.id != payload.id);
                    message.upsert_tool_call(ToolCallView {
                        id: payload.id,
                        name: payload.name,
                        input: payload.input.unwrap_or(serde_json::Value::Null),
                        partial_json: String::new(),
                        in_progress: true,
                        phase: Some(ToolPhase::Running),
                    });
                }
                StreamReduction::Applied
            }
            StreamEvent::ToolResult(payload) => {
                let Some(ok) = payload.presented_outcome() else {
                    if let Some(message) = sessions.streaming_assistant_mut(session_index) {
                        message
                            .tool_results
                            .retain(|result| result.id != payload.id);
                        if let Some(call) = message
                            .tool_calls
                            .iter_mut()
                            .find(|call| call.id == payload.id)
                        {
                            call.in_progress = false;
                            call.phase = Some(ToolPhase::Unconfirmed);
                        }
                        message.warnings.push(fl!("tool-outcome-unknown"));
                    }
                    return StreamReduction::Applied;
                };
                let is_error = payload.presented_is_error();
                let error_preview = payload.presented_error_preview();
                if let Some(message) = sessions.streaming_assistant_mut(session_index) {
                    if let Some(call) = message
                        .tool_calls
                        .iter_mut()
                        .find(|call| !payload.id.is_empty() && call.id == payload.id)
                    {
                        call.in_progress = false;
                        call.phase = Some(if ok {
                            ToolPhase::Completed
                        } else {
                            ToolPhase::Failed
                        });
                    }
                    message.upsert_tool_result(ToolResultView {
                        id: payload.id,
                        name: payload.name,
                        text: String::new(),
                        is_error,
                        latency_ms: payload.latency_ms,
                        bytes_returned: payload.bytes_returned,
                        error_preview,
                    });
                }
                StreamReduction::Applied
            }
            StreamEvent::Warning(warning) => {
                if let Some(message) = sessions.streaming_assistant_mut(session_index)
                    && !message.warnings.contains(&warning.message)
                {
                    message.warnings.push(warning.message);
                }
                StreamReduction::Applied
            }
            StreamEvent::TurnDone(turn) => {
                if let Some(message) = sessions.streaming_assistant_mut(session_index) {
                    message.accumulate_usage(&turn.usage);
                }
                StreamReduction::Applied
            }
            StreamEvent::Done(done) => {
                if let Some(message) = sessions.streaming_assistant_mut(session_index) {
                    if let Some(tail) = active.pending_stop.take() {
                        message.content.push_str(&tail);
                    }
                    if done.evidence.is_some() {
                        message.evidence = done.evidence.clone();
                    }
                }
                sessions.capture_remote(session_index, done.session_id.as_deref());
                sessions.finalize_stream(session_index, done.presented_answer(), false);
                self.phase = StreamPhase::Terminal;
                StreamReduction::Terminal
            }
            StreamEvent::Error(error) => {
                if error.task_status.as_deref() == Some("cancelled") {
                    sessions.finalize_stream(session_index, None, true);
                    self.phase = StreamPhase::Terminal;
                    return StreamReduction::Terminal;
                }
                if let Some(message) = sessions.streaming_assistant_mut(session_index) {
                    message.error = Some(error.presented_message());
                }
                sessions.finalize_stream(session_index, None, false);
                self.phase = StreamPhase::Terminal;
                StreamReduction::Failed { session_index }
            }
        }
    }

    pub(crate) fn transport_failed(
        &mut self,
        generation: u64,
        error: String,
        sessions: &mut SessionState,
    ) -> StreamReduction {
        let StreamPhase::Active(active) = &self.phase else {
            return StreamReduction::Stale;
        };
        if active.generation != generation {
            return StreamReduction::Stale;
        }
        let session_index = active.session_index;
        if let Some(message) = sessions.streaming_assistant_mut(session_index) {
            message.error = Some(error);
        }
        sessions.finalize_stream(session_index, None, false);
        self.phase = StreamPhase::Terminal;
        StreamReduction::Failed { session_index }
    }

    pub(crate) fn request_cancel(&mut self) -> Option<CancelRequest> {
        let StreamPhase::Active(active) = &mut self.phase else {
            return None;
        };
        if active.pending_stop.is_some() {
            return None;
        }
        active.pending_stop = Some(String::new());
        Some(match active.task_id.clone() {
            Some(task_id) => CancelRequest::Remote {
                task_id,
                generation: active.generation,
            },
            None => CancelRequest::AwaitTask,
        })
    }

    pub(crate) fn cancel_finished(
        &mut self,
        generation: u64,
        result: Result<(), String>,
        sessions: &mut SessionState,
    ) -> bool {
        let (StreamPhase::Active(active), Err(error)) = (&mut self.phase, result) else {
            return false;
        };
        if active.generation != generation {
            return false;
        }
        let Some(tail) = active.pending_stop.take() else {
            return false;
        };
        if let Some(message) = sessions.streaming_assistant_mut(active.session_index) {
            active.append_text(message, &tail);
            message.warnings.push(error);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/stream_state.rs"
    ));
}
