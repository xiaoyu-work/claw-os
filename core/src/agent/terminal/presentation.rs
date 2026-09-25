use serde_json::Value;

use crate::agent::llm::{ContentBlock, StreamEvent};

use super::state::App;

#[derive(Default)]
pub(super) struct RecordOutcome {
    pub approvals: Vec<String>,
    pub resumed: bool,
}

pub(super) fn apply_record(app: &mut App, record: &Value) -> Result<RecordOutcome, String> {
    let mut outcome = RecordOutcome::default();
    if let Some(progress) = record.get("progress") {
        let kind = required_string(progress, "kind")?;
        match kind {
            "tool_start" => app.tool_started(
                required_string(progress, "id")?,
                required_string(progress, "name")?,
            ),
            "tool_result" => app.tool_finished(
                required_string(progress, "id")?,
                required_string(progress, "name")?,
                progress
                    .get("ok")
                    .and_then(Value::as_bool)
                    .ok_or("tool result omitted its outcome")?,
                progress.get("latency_ms").and_then(Value::as_u64),
            ),
            "waiting_approval" => {
                let ids = progress
                    .get("request_ids")
                    .and_then(Value::as_array)
                    .ok_or("approval wait omitted request ids")?;
                if ids.is_empty() || ids.len() > 64 {
                    return Err("invalid approval request count".into());
                }
                outcome.approvals = ids
                    .iter()
                    .map(|id| {
                        id.as_str()
                            .map(str::to_string)
                            .ok_or_else(|| "invalid approval request id".to_string())
                    })
                    .collect::<Result<_, _>>()?;
            }
            "approval_resumed" => outcome.resumed = true,
            "task_workspace" => {
                let workspace = required_string(progress, "workspace")?;
                if app.active_workspace.as_deref() != Some(workspace)
                    || progress.get("grants_capability").and_then(Value::as_bool) != Some(false)
                {
                    return Err("task workspace evidence does not match the submitted task".into());
                }
            }
            _ => app.push_system("Claw reported progress this terminal cannot display."),
        }
        return Ok(outcome);
    }

    let event = record
        .get("event")
        .ok_or("task stream record omitted event and progress")?;
    let event: StreamEvent = serde_json::from_value(event.clone())
        .map_err(|_| "Claw returned an unsupported stream event".to_string())?;
    match event {
        StreamEvent::TextDelta { text } => app.push_assistant_delta(&text),
        StreamEvent::Message(response) => {
            if !app.provider_had_text() {
                let text = response
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>();
                app.push_assistant_delta(&text);
            }
            for block in response.content {
                match block {
                    ContentBlock::Reasoning { id: _, summary, .. } => {
                        app.push_reasoning(&summary);
                    }
                    ContentBlock::ToolUse { id, name, .. } => app.tool_announced(&id, &name),
                    _ => {}
                }
            }
            for call in response.tool_calls {
                app.tool_announced(&call.id, &call.name);
            }
        }
        StreamEvent::ToolUseStart { id, name } => app.tool_announced(&id, &name),
        StreamEvent::ToolUse(call) => app.tool_announced(&call.id, &call.name),
        StreamEvent::Reasoning { summary, .. } => app.push_reasoning(&summary),
        StreamEvent::ToolInputDelta { .. } | StreamEvent::ToolState { .. } => {}
        StreamEvent::Done { usage, .. } => {
            app.usage_input = app
                .usage_input
                .saturating_add(u64::from(usage.input_tokens));
            app.usage_output = app
                .usage_output
                .saturating_add(u64::from(usage.output_tokens));
            app.usage_cached = app
                .usage_cached
                .saturating_add(u64::from(usage.cache_read_tokens));
            app.finish_provider();
        }
        StreamEvent::Warning { message } => app.push_system(&format!("Warning: {message}")),
    }
    Ok(outcome)
}

fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("task stream record omitted {key}"))
}
