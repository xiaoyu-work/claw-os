//! A task-local handoff slot. Only the supervisor can persist it or dispatch work.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{Tool, ToolResult};
use crate::activities::ActivityWorkCheckpoint;

pub(crate) const CHECKPOINT_TOOL: &str = "cos_activity_checkpoint";
pub(crate) const WORK_INSTRUCTIONS: &str = "\
This task is one bounded segment of an explicitly enabled Activity. Work on the \
current next task, using ordinary authorized tools. Before ending, call \
cos_activity_checkpoint as the only tool call in its response. Report completed \
steps and attached App object artifact references accurately. A handoff is a \
caller report, not verified effects, permission or goal completion. Use continue \
with a concrete different next task when useful work remains, wait when an \
external condition must change, and review when the goal appears ready for owner \
verification. Use blocked when human input is needed. Do not repeat uncertain \
side effects. The handoff ends this segment; no further tools may run.";
pub(crate) const WORK_FINALIZATION: &str = "\
This is the last allowed turn of this Activity work segment. Do not perform \
further operational tools. Call cos_activity_checkpoint alone to hand off the \
results already collected and a concrete next task, wait, or review decision. \
Do not claim goal completion. If safe continuation is unclear, request review.";

#[derive(Default)]
pub(crate) struct WorkCheckpointSlot {
    checkpoint: Mutex<Option<ActivityWorkCheckpoint>>,
    closing: AtomicBool,
}

impl WorkCheckpointSlot {
    pub(crate) fn checkpoint(&self) -> Result<Option<ActivityWorkCheckpoint>, String> {
        self.checkpoint
            .lock()
            .map(|checkpoint| checkpoint.clone())
            .map_err(|_| "Activity handoff slot is poisoned".into())
    }

    pub(crate) fn begin_finalization(&self) {
        self.closing.store(true, Ordering::Release);
    }

    pub(crate) fn permits(&self, tool: &str) -> Result<bool, String> {
        Ok(self.checkpoint()?.is_none()
            && (!self.closing.load(Ordering::Acquire) || tool == CHECKPOINT_TOOL))
    }
}

pub(crate) struct ActivityCheckpoint(pub Arc<WorkCheckpointSlot>);

#[async_trait]
impl Tool for ActivityCheckpoint {
    fn name(&self) -> &str {
        CHECKPOINT_TOOL
    }

    fn description(&self) -> &str {
        "End this Activity work segment with a structured progress report and next action. Call alone. This neither verifies effects nor completes the goal."
    }

    fn input_schema(&self) -> Value {
        let task = json!({"type":"string","minLength":1,"maxLength":8192});
        let reason = json!({"type":"string","minLength":1,"maxLength":2048});
        json!({
            "type":"object","additionalProperties":false,
            "required":["summary","made_progress","completed_steps","artifacts","next"],
            "properties":{
                "summary":{"type":"string","minLength":1,"maxLength":8192},
                "made_progress":{"type":"boolean"},
                "completed_steps":{"type":"array","maxItems":32,"items":{"type":"string","minLength":1,"maxLength":512}},
                "artifacts":{"type":"array","maxItems":32,"items":{"type":"string","description":"Canonical app:// reference already attached to this Activity"}},
                "next":{"oneOf":[
                    {"type":"object","additionalProperties":false,"required":["kind","task"],"properties":{"kind":{"const":"continue"},"task":task}},
                    {"type":"object","additionalProperties":false,"required":["kind","task","reason"],"properties":{"kind":{"const":"wait"},"task":task,"reason":reason}},
                    {"type":"object","additionalProperties":false,"required":["kind","reason"],"properties":{"kind":{"const":"review"},"reason":reason}},
                    {"type":"object","additionalProperties":false,"required":["kind","reason"],"properties":{"kind":{"const":"blocked"},"reason":reason}}
                ]}
            }
        })
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let checkpoint: ActivityWorkCheckpoint = match serde_json::from_value(input) {
            Ok(checkpoint) => checkpoint,
            Err(error) => return ToolResult::err(format!("invalid Activity handoff: {error}")),
        };
        if let Err(error) = checkpoint.validate() {
            return ToolResult::err(error.to_string());
        }
        match self.0.checkpoint.lock() {
            Ok(mut slot) if slot.is_none() => {
                *slot = Some(checkpoint);
                ToolResult::ok("Handoff prepared; this segment ends now. The supervisor must durably record it and the Job result before any continuation. No goal completion or effects are verified.")
            }
            Ok(_) => {
                ToolResult::err("This segment already has a handoff; no further work is admitted.")
            }
            Err(_) => ToolResult::err("Activity handoff slot is poisoned."),
        }
    }
}
