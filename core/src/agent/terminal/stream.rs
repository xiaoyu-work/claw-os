use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc;

use super::backend::{Backend, Job, StreamError};
use super::requests::Request;
use super::state::{App, MAX_PROMPT_BYTES};

const MAX_QUEUED_TASKS: usize = 64;
const MAX_STREAM_RECONNECT_ATTEMPTS: u32 = 60;
const STREAM_RECONNECT_DELAY: Duration = Duration::from_secs(1);

pub(super) struct RuntimeEvent {
    pub task_id: String,
    pub kind: RuntimeEventKind,
}

pub(super) enum RuntimeEventKind {
    Record(Value),
    Finished(Box<Job>),
    Reconnecting(String),
    Reconnected,
    Failed(String),
}

pub(super) fn start_prompt(
    app: &mut App,
    backend: Arc<dyn Backend>,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
    use_memory: bool,
    max_turns: Option<u32>,
    prompt: String,
    workspace: String,
) -> Result<Request, String> {
    if app.conversation.archived {
        return Err("This conversation is archived. Use /new or /resume another session.".into());
    }
    if prompt.len() > MAX_PROMPT_BYTES {
        return Err("Prompt exceeds the 64 KiB terminal limit.".into());
    }
    let session_id = app.conversation.id.clone();
    let model = app.selected_model.clone();
    let effort = app.task_reasoning_effort.clone();
    let plan_only = app.task_plan_only;
    let attachments = app.pending_attachments().to_vec();
    let worker_backend = backend.clone();
    Ok(Request::new(
        "Submitting task",
        async move {
            let job = worker_backend
                .submit(
                    &prompt,
                    &attachments,
                    &session_id,
                    &workspace,
                    None,
                    use_memory,
                    max_turns,
                    &model,
                    effort.as_deref(),
                    plan_only,
                )
                .await?;
            if job.session_id != session_id
                || job.requested_model.as_deref() != Some(model.as_str())
                || job.workspace.as_deref() != Some(workspace.as_str())
            {
                return reject_submission(worker_backend.as_ref(), &job).await;
            }
            Ok(job)
        },
        move |app, job| {
            app.begin_task(&job);
            app.consume_attachments();
            spawn(backend, runtime_tx, job);
            Ok(())
        },
    )
    .submission())
}

pub(super) fn queue_prompt(
    app: &mut App,
    backend: Arc<dyn Backend>,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
    use_memory: bool,
    max_turns: Option<u32>,
    prompt: String,
) -> Result<Request, String> {
    if prompt.len() > MAX_PROMPT_BYTES {
        return Err("Prompt exceeds the 64 KiB terminal limit.".into());
    }
    if app.queued_tasks.len() >= MAX_QUEUED_TASKS {
        return Err("The terminal durable queue is limited to 64 tasks.".into());
    }
    let predecessor = app
        .queued_tasks
        .back()
        .map(|job| job.id.clone())
        .or_else(|| app.active_task.clone());
    let Some(predecessor) = predecessor else {
        return Err("No active task is available to anchor queued work.".into());
    };
    let workspace = app.selected_workspace.clone();
    let session_id = app.conversation.id.clone();
    let model = app.selected_model.clone();
    let effort = app.task_reasoning_effort.clone();
    let plan_only = app.task_plan_only;
    let attachments = app.pending_attachments().to_vec();
    let worker_backend = backend.clone();
    Ok(Request::new(
        "Queuing follow-up task",
        async move {
            let job = worker_backend
                .submit(
                    &prompt,
                    &attachments,
                    &session_id,
                    &workspace,
                    Some(&predecessor),
                    use_memory,
                    max_turns,
                    &model,
                    effort.as_deref(),
                    plan_only,
                )
                .await?;
            if job.session_id != session_id
                || job.workspace.as_deref() != Some(workspace.as_str())
                || job.after_task_id.as_deref() != Some(predecessor.as_str())
                || job.requested_model.as_deref() != Some(model.as_str())
                || job.status != "pending"
            {
                return reject_submission(worker_backend.as_ref(), &job).await;
            }
            Ok(job)
        },
        move |app, job| {
            if app.active_task.is_none() && app.queued_tasks.is_empty() {
                attach_queued(app, backend, runtime_tx, job);
            } else {
                app.queue_task(job);
            }
            app.consume_attachments();
            Ok(())
        },
    )
    .mutation())
}

async fn reject_submission(backend: &dyn Backend, job: &Job) -> Result<Job, String> {
    let error = "Claw did not acknowledge the task's conversation, model, workspace or dependency";
    backend
        .cancel(&job.id)
        .await
        .map_err(|cleanup| format!("{error}; stop mismatched task {}: {cleanup}", job.id))?;
    Err(error.into())
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

pub(super) fn restore_conversation(
    app: &mut App,
    backend: Arc<dyn Backend>,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
) -> Option<Request> {
    let conversation_id = app.conversation.id.clone();
    let references = app.take_conversation_jobs();
    if references.is_empty() {
        return None;
    }
    let loader = backend.clone();
    Some(Request::new(
        "Restoring conversation tasks",
        async move { Ok(load_conversation(loader, conversation_id, references).await) },
        move |app, restored| {
            for error in restored.errors {
                app.push_error(&error);
            }
            app.restore_selected_model(
                restored
                    .latest_job
                    .as_ref()
                    .and_then(|job| job.requested_model.as_deref()),
            );
            app.restore_reasoning_effort(
                restored
                    .latest_job
                    .as_ref()
                    .and_then(|job| job.requested_reasoning_effort.as_deref()),
            );
            app.restore_plan_mode(
                restored
                    .latest_job
                    .as_ref()
                    .is_some_and(|job| job.plan_only),
            );
            let mut jobs = restored.jobs;
            if jobs.is_empty() {
                return Ok(());
            }
            if jobs.len() > MAX_QUEUED_TASKS {
                return Err(
                    "Conversation has too many active durable tasks to restore in the terminal."
                        .into(),
                );
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
            app.resume_task(&head, queued.len());
            for job in queued {
                app.restore_queued_task(job);
            }
            spawn(backend, runtime_tx, head);
            Ok(())
        },
    ))
}

struct RestoredConversation {
    latest_job: Option<Job>,
    jobs: Vec<Job>,
    errors: Vec<String>,
}

async fn load_conversation(
    backend: Arc<dyn Backend>,
    conversation_id: String,
    references: Vec<super::backend::ConversationJob>,
) -> RestoredConversation {
    let latest_id = references
        .iter()
        .rev()
        .find(|job| job.session_id == conversation_id)
        .map(|job| job.id.clone());
    let mut latest_job = None;
    let mut jobs = Vec::new();
    let mut errors = Vec::new();
    for reference in references
        .into_iter()
        .filter(|job| {
            job.session_id == conversation_id
                && !matches!(job.status.as_str(), "ok" | "error" | "cancelled")
        })
        .take(MAX_QUEUED_TASKS + 1)
    {
        match backend.get_task(&reference.id).await {
            Ok(job) if job.session_id == conversation_id && !job.is_terminal() => {
                if latest_id.as_deref() == Some(job.id.as_str()) {
                    latest_job = Some(job.clone());
                }
                jobs.push(job);
            }
            Ok(job) if job.session_id != conversation_id => {
                errors.push(format!(
                    "Durable task {} belongs to another conversation; it was not restored.",
                    job.id
                ));
            }
            Ok(_) => {}
            Err(error) => {
                errors.push(format!(
                    "Could not restore durable task {}: {error}",
                    reference.id
                ));
            }
        }
    }
    if latest_job.is_none() {
        if let Some(latest_id) = latest_id {
            match backend.get_task(&latest_id).await {
                Ok(job) if job.session_id == conversation_id => latest_job = Some(job),
                Ok(job) => {
                    errors.push(format!(
                        "Latest durable task {} belongs to another conversation; \
                         its model selection was not restored.",
                        job.id
                    ));
                }
                Err(error) => {
                    errors.push(format!(
                        "Could not restore the conversation's latest model selection: {error}"
                    ));
                }
            }
        }
    }
    RestoredConversation {
        latest_job,
        jobs,
        errors,
    }
}

fn spawn(backend: Arc<dyn Backend>, runtime_tx: mpsc::UnboundedSender<RuntimeEvent>, job: Job) {
    tokio::spawn(async move {
        let send = |kind| {
            runtime_tx.send(RuntimeEvent {
                task_id: job.id.clone(),
                kind,
            })
        };
        let mut cursor = 0;
        let mut reconnect_attempts = 0;
        loop {
            let frame = match backend.stream(&job.id, cursor).await {
                Ok(frame) => frame,
                Err(StreamError::Retryable(error))
                    if reconnect_attempts < MAX_STREAM_RECONNECT_ATTEMPTS =>
                {
                    if reconnect_attempts == 0 {
                        let _ = send(RuntimeEventKind::Reconnecting(error));
                    }
                    reconnect_attempts += 1;
                    tokio::time::sleep(STREAM_RECONNECT_DELAY).await;
                    continue;
                }
                Err(StreamError::Retryable(error)) => {
                    let _ = send(RuntimeEventKind::Failed(format!(
                        "Claw broker task stream did not recover after \
                         {MAX_STREAM_RECONNECT_ATTEMPTS} attempts: {error}"
                    )));
                    return;
                }
                Err(StreamError::Fatal(error)) => {
                    let _ = send(RuntimeEventKind::Failed(error));
                    return;
                }
            };
            if reconnect_attempts > 0 {
                reconnect_attempts = 0;
                let _ = send(RuntimeEventKind::Reconnected);
            }
            cursor = frame.cursor;
            for record in frame.records {
                let _ = send(RuntimeEventKind::Record(record));
            }
            if frame.terminal {
                let _ = send(RuntimeEventKind::Finished(Box::new(frame.job)));
                return;
            }
        }
    });
}
