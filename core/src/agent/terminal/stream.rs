use std::sync::Arc;

use serde_json::Value;
use tokio::sync::mpsc;

use super::backend::{Backend, Job};
use super::state::{App, MAX_PROMPT_BYTES};

pub(super) enum RuntimeEvent {
    Record(Value),
    Finished(Job),
    Failed(String),
}

pub(super) async fn start_prompt(
    app: &mut App,
    backend: Arc<dyn Backend>,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
    use_memory: bool,
    max_turns: Option<u32>,
    prompt: String,
    workspace: String,
) {
    if app.conversation.archived {
        app.push_error("This conversation is archived. Use /new or /resume another session.");
        return;
    }
    if prompt.len() > MAX_PROMPT_BYTES {
        app.push_error("Prompt exceeds the 64 KiB terminal limit.");
        return;
    }
    match backend
        .submit(
            &prompt,
            &app.conversation.id,
            &workspace,
            use_memory,
            max_turns,
            &app.selected_model,
        )
        .await
    {
        Ok(job)
            if job.session_id == app.conversation.id
                && job.requested_model.as_deref() == Some(app.selected_model.as_str())
                && job.workspace.as_deref() == Some(workspace.as_str()) =>
        {
            app.begin_task(&job);
            spawn(backend, runtime_tx, job);
        }
        Ok(job) if job.session_id != app.conversation.id => {
            let _ = backend.cancel(&job.id).await;
            app.push_error("Claw submitted the task under another conversation.");
        }
        Ok(job) if job.workspace.as_deref() != Some(workspace.as_str()) => {
            let _ = backend.cancel(&job.id).await;
            app.push_error("Claw did not acknowledge the broker-validated task workspace.");
        }
        Ok(job) => {
            let _ = backend.cancel(&job.id).await;
            app.push_error("Claw did not acknowledge the selected per-task model.");
        }
        Err(error) => app.push_error(&error),
    }
}

fn spawn(backend: Arc<dyn Backend>, runtime_tx: mpsc::UnboundedSender<RuntimeEvent>, job: Job) {
    tokio::spawn(async move {
        let mut cursor = 0;
        loop {
            let frame = match backend.stream(&job.id, cursor).await {
                Ok(frame) => frame,
                Err(error) => {
                    let _ = runtime_tx.send(RuntimeEvent::Failed(error));
                    return;
                }
            };
            cursor = frame.cursor;
            for record in frame.records {
                let _ = runtime_tx.send(RuntimeEvent::Record(record));
            }
            if frame.terminal {
                let _ = runtime_tx.send(RuntimeEvent::Finished(frame.job));
                return;
            }
        }
    });
}
