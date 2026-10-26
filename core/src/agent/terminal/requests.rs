use std::collections::VecDeque;
use std::future::{pending, Future};
use std::pin::Pin;
use std::sync::Arc;

use super::backend::Backend;
use super::state::{App, RunStatus};
use super::InputAction;

type Update = Box<dyn FnOnce(&mut App) -> Result<(), String> + Send>;
type Work = Pin<Box<dyn Future<Output = Result<Update, String>> + Send>>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Read,
    Mutation,
    Submission,
}

pub(super) struct Request {
    label: &'static str,
    kind: Kind,
    work: Work,
}

impl Request {
    pub fn new<T, F, A>(label: &'static str, work: F, apply: A) -> Self
    where
        T: Send + 'static,
        F: Future<Output = Result<T, String>> + Send + 'static,
        A: FnOnce(&mut App, T) -> Result<(), String> + Send + 'static,
    {
        Self {
            label,
            kind: Kind::Read,
            work: Box::pin(async move {
                let value = work.await?;
                Ok(Box::new(move |app: &mut App| apply(app, value)) as Update)
            }),
        }
    }

    pub fn mutation(mut self) -> Self {
        self.kind = Kind::Mutation;
        self
    }

    pub fn submission(mut self) -> Self {
        self.kind = Kind::Submission;
        self
    }
}

struct Pending {
    request: Request,
    conversation_id: String,
    stop_after_submit: bool,
}

pub(super) struct Completion {
    pending: Pending,
    result: Result<Update, String>,
    cancellation: bool,
    observation: bool,
}

#[derive(Default)]
pub(super) struct Requests {
    foreground: Option<Pending>,
    cancellation: Option<Pending>,
    observation: Option<Pending>,
    deferred: VecDeque<InputAction>,
}

impl Requests {
    pub fn busy(&self) -> bool {
        self.foreground.is_some()
    }

    pub fn must_settle(&self) -> bool {
        self.cancellation.is_some()
            || self
                .foreground
                .as_ref()
                .is_some_and(|pending| pending.request.kind != Kind::Read)
    }

    pub fn defer(&mut self, app: &mut App, action: InputAction) {
        match action {
            InputAction::Review(_) | InputAction::ApprovalReview(_, _) => {}
            InputAction::Submit(input) => {
                app.prefill_input(input);
                app.push_system("A request is pending. Your input was retained in the composer.");
            }
            action if self.deferred.len() < 16 => self.deferred.push_back(action),
            _ => {
                app.push_error("Too many pending interface actions; wait for the current request.")
            }
        }
    }

    pub fn take_deferred(&mut self) -> Option<InputAction> {
        self.deferred.pop_front()
    }

    pub fn start(&mut self, app: &mut App, request: Request) {
        assert!(
            self.foreground.is_none(),
            "foreground request already pending"
        );
        app.pending_request = Some(request.label);
        self.foreground = Some(Pending {
            request,
            conversation_id: app.conversation.id.clone(),
            stop_after_submit: false,
        });
    }

    pub fn observe(&mut self, app: &App, request: Request) {
        if self.observation.is_none() {
            self.observation = Some(Pending {
                request,
                conversation_id: app.conversation.id.clone(),
                stop_after_submit: false,
            });
        }
    }

    pub fn stop(&mut self, app: &mut App, backend: Arc<dyn Backend>) {
        self.deferred.clear();
        if self
            .foreground
            .as_ref()
            .is_some_and(|pending| pending.request.kind == Kind::Read)
        {
            self.foreground = None;
            app.pending_request = None;
        }
        let Some(task_id) = app.active_task.clone() else {
            if let Some(pending) = &mut self.foreground {
                if pending.request.kind == Kind::Submission {
                    pending.stop_after_submit = true;
                    app.pending_request = Some("Stopping task after submission is acknowledged");
                } else {
                    app.push_system(
                        "Waiting for broker confirmation. The operation may already have applied.",
                    );
                }
            }
            return;
        };
        if self.cancellation.is_some() || app.status == RunStatus::Cancelling {
            return;
        }
        let previous = app.status;
        app.status = RunStatus::Cancelling;
        let cancel_id = task_id.clone();
        let request = Request::new(
            "Stopping task",
            async move { Ok(backend.cancel(&cancel_id).await) },
            move |app, result| {
                if let Err(error) = result {
                    if app.active_task.as_deref() == Some(task_id.as_str())
                        && app.status == RunStatus::Cancelling
                    {
                        app.status = previous;
                    }
                    return Err(format!("Stop task {task_id}: {error}"));
                }
                Ok(())
            },
        )
        .mutation();
        self.cancellation = Some(Pending {
            request,
            conversation_id: app.conversation.id.clone(),
            stop_after_submit: false,
        });
    }

    pub async fn next(&mut self) -> Completion {
        let (cancellation, observation, result) = tokio::select! {
            result = wait(&mut self.foreground) => (false, false, result),
            result = wait(&mut self.cancellation) => (true, false, result),
            result = wait(&mut self.observation) => (false, true, result),
        };
        let pending = if cancellation {
            self.cancellation.take()
        } else if observation {
            self.observation.take()
        } else {
            self.foreground.take()
        }
        .expect("completed request is present");
        Completion {
            pending,
            result,
            cancellation,
            observation,
        }
    }

    pub fn complete(&mut self, app: &mut App, completion: Completion, backend: Arc<dyn Backend>) {
        if !completion.cancellation && !completion.observation {
            app.pending_request = None;
        }
        if completion.pending.conversation_id != app.conversation.id {
            if let Err(error) = completion.result {
                app.push_error(&error);
            }
            return;
        }
        if let Err(error) = completion.result.and_then(|update| update(app)) {
            app.push_error(&error);
        }
        if completion.pending.conversation_id != app.conversation.id {
            self.deferred.clear();
        }
        if completion.pending.stop_after_submit {
            self.stop(app, backend);
        }
    }
}

async fn wait(slot: &mut Option<Pending>) -> Result<Update, String> {
    match slot {
        Some(pending) => pending.request.work.as_mut().await,
        None => pending().await,
    }
}
