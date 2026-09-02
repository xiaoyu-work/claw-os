use futures::future::AbortHandle;

use crate::bridge::{StreamEvent, ToolCallView, ToolResultView};
use crate::fl;
use crate::session::SessionState;

struct ActiveStream {
    generation: u64,
    session_index: usize,
    task_id: Option<String>,
    abort: Option<AbortHandle>,
}

struct PendingCancel {
    generation: u64,
    session_index: usize,
    message_index: usize,
    abort: Option<AbortHandle>,
}

enum StreamPhase {
    Idle,
    Active(ActiveStream),
    Cancelling(PendingCancel),
    Terminal,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StreamReduction {
    Applied,
    Terminal,
    Failed {
        session_index: usize,
    },
    CancelRemote {
        task_id: String,
        session_index: usize,
        message_index: usize,
    },
    Cancelled,
    Stale,
}

pub(crate) enum CancelRequest {
    AwaitTask,
    Remote {
        task_id: String,
        session_index: usize,
        message_index: usize,
    },
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
        matches!(self.phase, StreamPhase::Active(_))
    }

    pub(crate) fn is_cancelling(&self) -> bool {
        matches!(self.phase, StreamPhase::Cancelling(_))
    }

    pub(crate) fn session_index(&self) -> Option<usize> {
        match &self.phase {
            StreamPhase::Active(active) => Some(active.session_index),
            StreamPhase::Cancelling(cancel) => Some(cancel.session_index),
            StreamPhase::Idle | StreamPhase::Terminal | StreamPhase::Cancelled => None,
        }
    }

    pub(crate) fn start(&mut self, session_index: usize, abort: AbortHandle) -> u64 {
        self.next_generation = self.next_generation.wrapping_add(1);
        let generation = self.next_generation;
        self.phase = StreamPhase::Active(ActiveStream {
            generation,
            session_index,
            task_id: None,
            abort: Some(abort),
        });
        generation
    }

    pub(crate) fn reduce(
        &mut self,
        generation: u64,
        event: StreamEvent,
        sessions: &mut SessionState,
    ) -> StreamReduction {
        if let StreamPhase::Cancelling(cancel) = &mut self.phase
            && cancel.generation == generation
        {
            if let StreamEvent::TaskStarted(started) = event {
                sessions.capture_provisional(cancel.session_index, started.session_id.as_deref());
                if let Some(abort) = cancel.abort.take() {
                    abort.abort();
                }
                self.next_generation = self.next_generation.wrapping_add(1);
                return StreamReduction::CancelRemote {
                    task_id: started.task_id,
                    session_index: cancel.session_index,
                    message_index: cancel.message_index,
                };
            }
            if matches!(event, StreamEvent::Error(_) | StreamEvent::Done(_)) {
                self.phase = StreamPhase::Cancelled;
                self.next_generation = self.next_generation.wrapping_add(1);
                return StreamReduction::Cancelled;
            }
            return StreamReduction::Stale;
        }

        let StreamPhase::Active(active) = &mut self.phase else {
            return StreamReduction::Stale;
        };
        if active.generation != generation {
            return StreamReduction::Stale;
        }
        let session_index = active.session_index;

        match event {
            StreamEvent::TaskStarted(started) => {
                active.task_id = (!started.task_id.is_empty()).then_some(started.task_id);
                sessions.capture_provisional(session_index, started.session_id.as_deref());
                StreamReduction::Applied
            }
            StreamEvent::Delta(delta) => {
                if let Some(message) = sessions.streaming_assistant_mut(session_index) {
                    message.content.push_str(&delta.text);
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
                    } else {
                        message.upsert_tool_call(ToolCallView {
                            id: payload.id,
                            name: fl!("tool-running"),
                            input: serde_json::Value::Null,
                            partial_json: payload.delta,
                            in_progress: true,
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
                    });
                }
                StreamReduction::Applied
            }
            StreamEvent::ToolStart(payload) => {
                if let Some(message) = sessions.streaming_assistant_mut(session_index) {
                    message.upsert_tool_call(ToolCallView {
                        id: payload.id,
                        name: payload.name,
                        input: payload.input.unwrap_or(serde_json::Value::Null),
                        partial_json: String::new(),
                        in_progress: true,
                    });
                }
                StreamReduction::Applied
            }
            StreamEvent::ToolResult(payload) => {
                let text = payload.presented_text();
                let is_error = payload.presented_is_error();
                if let Some(message) = sessions.streaming_assistant_mut(session_index) {
                    if let Some(call) = message
                        .tool_calls
                        .iter_mut()
                        .find(|call| !payload.id.is_empty() && call.id == payload.id)
                    {
                        call.in_progress = false;
                    }
                    message.upsert_tool_result(ToolResultView {
                        id: payload.id,
                        name: payload.name,
                        text,
                        is_error,
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
            StreamEvent::TurnDone(_) => StreamReduction::Applied,
            StreamEvent::Done(done) => {
                sessions.capture_remote(session_index, done.session_id.as_deref());
                sessions.finalize_stream(session_index, done.presented_answer(), false);
                self.phase = StreamPhase::Terminal;
                StreamReduction::Terminal
            }
            StreamEvent::Error(error) => {
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
        if matches!(
            &self.phase,
            StreamPhase::Cancelling(cancel) if cancel.generation == generation
        ) {
            self.phase = StreamPhase::Cancelled;
            self.next_generation = self.next_generation.wrapping_add(1);
            return StreamReduction::Cancelled;
        }
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

    pub(crate) fn request_cancel(&mut self, sessions: &mut SessionState) -> Option<CancelRequest> {
        if !matches!(self.phase, StreamPhase::Active(_)) {
            return None;
        }
        let StreamPhase::Active(active) = std::mem::replace(&mut self.phase, StreamPhase::Idle)
        else {
            unreachable!("active stream phase checked before extraction");
        };
        let message_index = sessions
            .get(active.session_index)
            .and_then(|session| session.messages.len().checked_sub(1))
            .unwrap_or(0);
        sessions.finalize_stream(active.session_index, None, true);
        let pending = PendingCancel {
            generation: active.generation,
            session_index: active.session_index,
            message_index,
            abort: active.abort,
        };
        if let Some(task_id) = active.task_id {
            if let Some(abort) = pending.abort.as_ref() {
                abort.abort();
            }
            self.next_generation = self.next_generation.wrapping_add(1);
            let request = CancelRequest::Remote {
                task_id,
                session_index: pending.session_index,
                message_index,
            };
            self.phase = StreamPhase::Cancelling(PendingCancel {
                abort: None,
                ..pending
            });
            Some(request)
        } else {
            self.phase = StreamPhase::Cancelling(pending);
            Some(CancelRequest::AwaitTask)
        }
    }

    pub(crate) fn cancel_finished(
        &mut self,
        session_index: usize,
        message_index: usize,
        result: Result<(), String>,
        sessions: &mut SessionState,
    ) -> Option<usize> {
        let matches = matches!(
            &self.phase,
            StreamPhase::Cancelling(cancel)
                if cancel.session_index == session_index
                    && cancel.message_index == message_index
        );
        if !matches {
            return None;
        }
        if let Err(error) = result
            && let Some(message) = sessions
                .get_mut(session_index)
                .and_then(|session| session.messages.get_mut(message_index))
            && message.role() == crate::session::ChatRole::Assistant
        {
            message.error = Some(error);
        }
        self.phase = StreamPhase::Cancelled;
        Some(session_index)
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/stream_state.rs"
    ));
}
