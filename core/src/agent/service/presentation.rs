//! Bounded retained task presentation; never exports provider state or tool bodies.

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Job, Store};
use crate::agent::attachments::{AttachmentMetadata, ImageAttachment};
use crate::agent::llm::{ContentBlock, StreamEvent, Usage};
use crate::agent::safety::redact::Redactor;

pub(crate) const MAX_SCAN_BYTES: usize = 32 * 1024 * 1024;
const MAX_TASK_BYTES: usize = 8 * 1024 * 1024;
const MAX_SUMMARY_BYTES: usize = 16 * 1024;
const MAX_TOOLS: usize = 128;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct UsageTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolPresentation {
    pub id: String,
    pub name: String,
    pub started: bool,
    pub ok: Option<bool>,
    pub latency_ms: Option<u64>,
    pub bytes_returned: Option<u64>,
    pub error_preview: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct TaskPresentation {
    pub available: bool,
    pub reasoning: Vec<String>,
    pub usage: Option<UsageTotals>,
    pub tools: Vec<ToolPresentation>,
    pub attachments: Vec<AttachmentMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
}

impl TaskPresentation {
    pub(crate) fn unavailable(job: &Job, diagnostic: Option<String>) -> Self {
        Self {
            attachments: job
                .attachments
                .iter()
                .map(ImageAttachment::metadata)
                .collect(),
            diagnostic,
            ..Self::default()
        }
    }
}

pub(crate) fn read(store: &Store, job: &Job, remaining: &mut usize) -> TaskPresentation {
    match read_checked(store, job, remaining) {
        Ok(presentation) => presentation,
        Err(error) => {
            tracing::warn!(task_id = %job.id, %error, "retained task presentation unavailable");
            TaskPresentation::unavailable(job, Some(error))
        }
    }
}

fn read_checked(
    store: &Store,
    job: &Job,
    remaining: &mut usize,
) -> Result<TaskPresentation, String> {
    super::validate_job_id(&job.id).map_err(|error| error.to_string())?;
    let path = store.stream_path(&job.id);
    match std::fs::metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(TaskPresentation::unavailable(job, None));
        }
        Err(error) => return Err(format!("inspect retained task stream: {error}")),
    }
    crate::filelock::with_exclusive_path_lock(&path, || {
        let file =
            File::open(&path).map_err(|error| format!("read retained task stream: {error}"))?;
        let size = usize::try_from(file.metadata().map_err(|error| error.to_string())?.len())
            .map_err(|_| "retained task stream size is invalid".to_string())?;
        if size > MAX_TASK_BYTES || size > *remaining {
            return Err("retained task presentation exceeds its bounded read budget".to_string());
        }
        *remaining -= size;
        let mut presentation = TaskPresentation {
            available: true,
            attachments: job
                .attachments
                .iter()
                .map(ImageAttachment::metadata)
                .collect(),
            ..TaskPresentation::default()
        };
        static REDACTOR: OnceLock<Redactor> = OnceLock::new();
        let redactor = REDACTOR.get_or_init(Redactor::default_set);
        for line in BufReader::new(file.take(size as u64)).lines() {
            let line = line.map_err(|error| format!("read retained task record: {error}"))?;
            let record: Value = serde_json::from_str(&line)
                .map_err(|error| format!("invalid retained task record: {error}"))?;
            project_record(&mut presentation, record, redactor)?;
        }
        Ok(presentation)
    })
}

fn project_record(
    presentation: &mut TaskPresentation,
    record: Value,
    redactor: &Redactor,
) -> Result<(), String> {
    if let Some(event) = record.get("event") {
        let event: StreamEvent = serde_json::from_value(event.clone())
            .map_err(|error| format!("invalid retained provider event: {error}"))?;
        match event {
            StreamEvent::Reasoning { summary, .. } => {
                append_summary(presentation, summary, redactor)?
            }
            StreamEvent::Message(message) => {
                for call in message.tool_calls {
                    tool(presentation, call.id, call.name)?;
                }
                for block in message.content {
                    if let ContentBlock::Reasoning { summary, .. } = block {
                        append_summary(presentation, summary, redactor)?;
                    }
                }
            }
            StreamEvent::Done { usage, .. } => add_usage(presentation, &usage)?,
            StreamEvent::ToolUseStart { id, name } => {
                tool(presentation, id, name)?;
            }
            StreamEvent::ToolUse(call) => {
                tool(presentation, call.id, call.name)?;
            }
            _ => {}
        }
    }
    if let Some(progress) = record.get("progress") {
        match progress.get("kind").and_then(Value::as_str) {
            Some("tool_start" | "tool_result") => {
                let id = progress
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or("retained tool progress omitted its id")?
                    .to_string();
                let name = progress
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("retained tool progress omitted its name")?
                    .to_string();
                let result = tool(presentation, id, name)?;
                result.started = true;
                if progress.get("kind").and_then(Value::as_str) == Some("tool_result") {
                    let ok = progress
                        .get("ok")
                        .and_then(Value::as_bool)
                        .ok_or("retained tool progress omitted its outcome")?;
                    result.ok = Some(ok);
                    result.latency_ms = progress.get("latency_ms").and_then(Value::as_u64);
                    result.bytes_returned = progress.get("bytes_returned").and_then(Value::as_u64);
                    result.error_preview = if ok {
                        None
                    } else {
                        progress
                            .get("error_preview")
                            .and_then(Value::as_str)
                            .map(|preview| {
                                crate::agent::runtime::progress::preview_with_limit(
                                    &redactor.redact(preview),
                                    crate::agent::runtime::progress::DEFAULT_PREVIEW_BYTES,
                                )
                            })
                    };
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn tool(
    presentation: &mut TaskPresentation,
    id: String,
    name: String,
) -> Result<&mut ToolPresentation, String> {
    if id.is_empty() || id.len() > 256 || name.len() > 256 {
        return Err("invalid retained tool identity".to_string());
    }
    let index = if let Some(index) = presentation.tools.iter().position(|tool| tool.id == id) {
        index
    } else {
        if presentation.tools.len() >= MAX_TOOLS {
            return Err("retained tool presentation exceeds its item limit".to_string());
        }
        presentation.tools.push(ToolPresentation {
            id,
            name: name.clone(),
            started: false,
            ok: None,
            latency_ms: None,
            bytes_returned: None,
            error_preview: None,
        });
        presentation.tools.len() - 1
    };
    let result = &mut presentation.tools[index];
    result.name = name;
    Ok(result)
}

fn append_summary(
    presentation: &mut TaskPresentation,
    summaries: Vec<String>,
    redactor: &Redactor,
) -> Result<(), String> {
    for summary in summaries {
        let summary = redactor.redact(summary.trim());
        if summary.is_empty() || presentation.reasoning.contains(&summary) {
            continue;
        }
        if presentation
            .reasoning
            .iter()
            .map(String::len)
            .sum::<usize>()
            + summary.len()
            > MAX_SUMMARY_BYTES
        {
            return Err("retained reasoning summaries exceed their byte limit".to_string());
        }
        presentation.reasoning.push(summary);
    }
    Ok(())
}

fn add_usage(presentation: &mut TaskPresentation, usage: &Usage) -> Result<(), String> {
    let totals = presentation.usage.get_or_insert_with(UsageTotals::default);
    for (total, count) in [
        (&mut totals.input_tokens, usage.input_tokens),
        (&mut totals.output_tokens, usage.output_tokens),
        (&mut totals.cache_read_tokens, usage.cache_read_tokens),
        (&mut totals.cache_write_tokens, usage.cache_write_tokens),
    ] {
        *total = total
            .checked_add(u64::from(count))
            .ok_or("retained usage overflow")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/service/presentation.rs"
    ));
}
