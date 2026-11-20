use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use super::{Tool, ToolResult};

const MAX_BRIEF_BYTES: usize = 8 * 1024;
pub(crate) const MAX_SUBMISSIONS: u32 = 2;

pub(crate) fn validate_brief(brief: &str) -> Result<&str, String> {
    let brief = brief.trim();
    if brief.is_empty() || brief.len() > MAX_BRIEF_BYTES {
        return Err("background task brief must contain 1 to 8192 UTF-8 bytes".into());
    }
    Ok(brief)
}

#[async_trait]
pub(crate) trait BackgroundSubmitter: Send + Sync {
    async fn submit(&self, brief: String) -> Result<String, String>;
}

pub(crate) struct BackgroundDelegate(pub Arc<dyn BackgroundSubmitter>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    brief: String,
}

#[async_trait]
impl Tool for BackgroundDelegate {
    fn name(&self) -> &str {
        "cos_delegate_background"
    }

    fn description(&self) -> &str {
        "Queue a substantial, self-contained part of the current user's request and return \
         immediately. Choose this when a long investigation, multi-file task, or deliverable \
         would keep the user waiting. Answer quick questions and one-step lookups yourself. \
         Split only independent work; keep dependent steps or conflicting writes together. \
         Give the goal, necessary context, input references, and expected result, not the whole \
         conversation. Queue at most two jobs. On success say the job was queued and end \
         the turn when no other immediate work remains. Do not wait, duplicate the work, \
         or bypass approvals."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["brief"],
            "properties": {
                "brief": {"type": "string", "minLength": 1, "maxLength": MAX_BRIEF_BYTES}
            }
        })
    }

    fn exposure(&self) -> super::exposure::ToolExposure {
        super::exposure::ToolExposure::always()
            .from_sources([crate::session::SessionSource::BrokerTask])
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let Ok(input) = serde_json::from_value::<Input>(input) else {
            return ToolResult::err("expected a background task brief");
        };
        let brief = match validate_brief(&input.brief) {
            Ok(brief) => brief,
            Err(error) => return ToolResult::err(error),
        };
        match self.0.submit(brief.to_string()).await {
            Ok(job_id) => ToolResult::ok(json!({"job_id": job_id, "status": "queued"}).to_string()),
            Err(error) => ToolResult::err(error),
        }
    }
}
