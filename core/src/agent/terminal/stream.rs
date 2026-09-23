use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc;

use super::backend::{Backend, Job, StreamError};
use super::state::{App, MAX_PROMPT_BYTES};

const MAX_QUEUED_TASKS: usize = 64;
const MAX_STREAM_RECONNECT_ATTEMPTS: u32 = 60;
const STREAM_RECONNECT_DELAY: Duration = Duration::from_secs(1);

pub(super) enum RuntimeEvent {
    Record(Value),
    Finished(Job),
    Reconnecting(String),
    Reconnected,
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
            None,
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

pub(super) async fn queue_prompt(
    app: &mut App,
    backend: Arc<dyn Backend>,
    use_memory: bool,
    max_turns: Option<u32>,
    prompt: String,
) {
    if prompt.len() > MAX_PROMPT_BYTES {
        app.push_error("Prompt exceeds the 64 KiB terminal limit.");
        return;
    }
    if app.queued_tasks.len() >= MAX_QUEUED_TASKS {
        app.push_error("The terminal durable queue is limited to 64 tasks.");
        return;
    }
    let predecessor = app
        .queued_tasks
        .back()
        .map(|job| job.id.clone())
        .or_else(|| app.active_task.clone());
    let Some(predecessor) = predecessor else {
        app.push_error("No active task is available to anchor queued work.");
        return;
    };
    let workspace = app.selected_workspace.clone();
    match backend
        .submit(
            &prompt,
            &app.conversation.id,
            &workspace,
            Some(&predecessor),
            use_memory,
            max_turns,
            &app.selected_model,
        )
        .await
    {
        Ok(job)
            if job.session_id == app.conversation.id
                && job.workspace.as_deref() == Some(workspace.as_str())
                && job.after_task_id.as_deref() == Some(predecessor.as_str())
                && job.requested_model.as_deref() == Some(app.selected_model.as_str())
                && job.status == "pending" =>
        {
            app.queue_task(job);
        }
        Ok(job) => {
            let _ = backend.cancel(&job.id).await;
            app.push_error("Claw did not acknowledge the durable queue dependency.");
        }
        Err(error) => app.push_error(&error),
    }
}

pub(super) fn attach_queued(
    app: &mut App,
    backend: Arc<dyn Backend>,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
    job: Job,
) {
    app.begin_task(&job);
    spawn(backend, runtime_tx, job);
}

pub(super) async fn restore_conversation(
    app: &mut App,
    backend: Arc<dyn Backend>,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
) {
    let conversation_id = app.conversation.id.clone();
    let references = app.take_conversation_jobs();
    let mut jobs = Vec::new();
    for reference in references
        .into_iter()
        .filter(|job| !matches!(job.status.as_str(), "ok" | "error" | "cancelled"))
        .take(MAX_QUEUED_TASKS + 1)
    {
        match backend.get_task(&reference.id).await {
            Ok(job) if job.session_id == conversation_id && !job.is_terminal() => jobs.push(job),
            Ok(_) => {}
            Err(error) => {
                app.push_error(&format!(
                    "Could not restore durable task {}: {error}",
                    reference.id
                ));
            }
        }
    }
    if jobs.is_empty() {
        return;
    }
    if jobs.len() > MAX_QUEUED_TASKS {
        app.push_error("Conversation has too many active durable tasks to restore in the terminal.");
        return;
    }

    let head = jobs.remove(0);
    let mut tail = head.id.clone();
    let mut queued = Vec::new();
    let mut skipped = 0usize;
    for job in jobs {
        if job.after_task_id.as_deref() == Some(tail.as_str()) {
            tail = job.id.clone();
            queued.push(job);
        } else {
            skipped += 1;
        }
    }
    if skipped > 0 {
        app.push_system(&format!(
            "{skipped} other active conversation task(s) remain available through /tasks."
        ));
    }
    let queued_count = queued.len();
    app.resume_task(&head, queued_count);
    for job in queued {
        app.restore_queued_task(job);
    }
    spawn(backend, runtime_tx, head);
}

fn spawn(backend: Arc<dyn Backend>, runtime_tx: mpsc::UnboundedSender<RuntimeEvent>, job: Job) {
    tokio::spawn(async move {
        let mut cursor = 0;
        let mut reconnect_attempts = 0;
        loop {
            let frame = match backend.stream(&job.id, cursor).await {
                Ok(frame) => frame,
                Err(StreamError::Retryable(error))
                    if reconnect_attempts < MAX_STREAM_RECONNECT_ATTEMPTS =>
                {
                    if reconnect_attempts == 0 {
                        let _ = runtime_tx.send(RuntimeEvent::Reconnecting(error));
                    }
                    reconnect_attempts += 1;
                    tokio::time::sleep(STREAM_RECONNECT_DELAY).await;
                    continue;
                }
                Err(StreamError::Retryable(error)) => {
                    let _ = runtime_tx.send(RuntimeEvent::Failed(format!(
                        "Claw broker task stream did not recover after \
                         {MAX_STREAM_RECONNECT_ATTEMPTS} attempts: {error}"
                    )));
                    return;
                }
                Err(StreamError::Fatal(error)) => {
                    let _ = runtime_tx.send(RuntimeEvent::Failed(error));
                    return;
                }
            };
            if reconnect_attempts > 0 {
                reconnect_attempts = 0;
                let _ = runtime_tx.send(RuntimeEvent::Reconnected);
            }
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
