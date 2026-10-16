use crate::{Message as AppMessage, bridge, fl};
use cos_agent_protocol::{BridgeEndpoint, FollowUp, FollowUpQueue};
use cosmic::{
    Element,
    app::Task,
    widget::{Column, Row, button, text},
};

#[derive(Clone, Debug)]
pub enum Message {
    Poll(Option<String>),
    Loaded(u64, Result<FollowUpQueue, String>),
    Cancel(String),
    AskAll,
    KeepAll,
    CancelAll,
    Cancelled(u64, Result<(), String>),
}
#[derive(Default)]
pub struct QueuePanel {
    task_id: Option<String>,
    generation: u64,
    queue: Option<FollowUpQueue>,
    loading: bool,
    cancelling: bool,
    selection: Option<Vec<FollowUp>>,
    error: Option<String>,
}
impl QueuePanel {
    pub fn update(
        &mut self,
        message: Message,
        endpoint: Option<BridgeEndpoint>,
    ) -> Task<AppMessage> {
        match message {
            Message::Poll(id) => {
                if self.task_id != id {
                    self.task_id = id;
                    self.queue = None;
                    self.loading = false;
                    self.cancelling = false;
                    self.error = None;
                    self.selection = None;
                    self.generation = self.generation.wrapping_add(1);
                }
                if self.loading || self.cancelling {
                    return Task::none();
                }
                let (Some(id), Some(endpoint)) = (self.task_id.clone(), endpoint) else {
                    return Task::none();
                };
                self.loading = true;
                self.generation = self.generation.wrapping_add(1);
                let generation = self.generation;
                Task::perform(
                    async move {
                        bridge::fetch_followups(endpoint, &id)
                            .await
                            .map_err(|error| format!("{error:#}"))
                    },
                    move |result| {
                        cosmic::Action::App(AppMessage::Queue(Message::Loaded(generation, result)))
                    },
                )
            }
            Message::Loaded(generation, result) if generation == self.generation => {
                self.loading = false;
                match result {
                    Ok(queue) => {
                        self.queue = Some(queue);
                        self.error = None
                    }
                    Err(error) => {
                        self.queue = None;
                        self.error = Some(error)
                    }
                }
                Task::none()
            }
            Message::AskAll => {
                self.selection = self.queue.as_ref().map(|queue| queue.tasks.clone());
                Task::none()
            }
            Message::KeepAll => {
                self.selection = None;
                Task::none()
            }
            Message::Cancel(id) => self.cancel(vec![id], endpoint),
            Message::CancelAll if self.selection.is_some() => {
                let ids = self
                    .selection
                    .as_ref()
                    .map(|tasks| tasks.iter().map(|task| task.id.clone()).collect())
                    .unwrap_or_default();
                self.cancel(ids, endpoint)
            }
            Message::Cancelled(generation, result) if generation == self.generation => {
                self.cancelling = false;
                match result {
                    Ok(()) => {
                        self.selection = None;
                        self.loading = false;
                        self.update(Message::Poll(self.task_id.clone()), endpoint)
                    }
                    Err(error) => {
                        self.error = Some(error);
                        Task::none()
                    }
                }
            }
            _ => Task::none(),
        }
    }
    fn cancel(&mut self, ids: Vec<String>, endpoint: Option<BridgeEndpoint>) -> Task<AppMessage> {
        if self.cancelling {
            return Task::none();
        }
        let Some(endpoint) = endpoint else {
            self.error = Some(fl!("bridge-offline"));
            return Task::none();
        };
        if ids.iter().any(|id| Some(id) == self.task_id.as_ref()) {
            self.error = Some(fl!("queue-current-excluded"));
            return Task::none();
        }
        if ids.iter().any(|id| {
            !self
                .queue
                .as_ref()
                .is_some_and(|queue| queue.tasks.iter().any(|task| &task.id == id))
        }) {
            self.error = Some(fl!("queue-invalid-selection"));
            return Task::none();
        }
        self.cancelling = true;
        self.loading = false;
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        Task::perform(
            async move {
                for id in ids.into_iter().rev() {
                    bridge::cancel_followup(endpoint.clone(), &id)
                        .await
                        .map_err(|error| format!("{error:#}"))?;
                }
                Ok(())
            },
            move |result| {
                cosmic::Action::App(AppMessage::Queue(Message::Cancelled(generation, result)))
            },
        )
    }
    pub fn view(&self) -> Element<'_, AppMessage> {
        let mut content = Column::new().spacing(6);
        if let Some(error) = &self.error {
            content = content.push(text(error).size(11.0)).push(
                button::text(fl!("queue-refresh"))
                    .on_press(AppMessage::Queue(Message::Poll(self.task_id.clone()))),
            );
        }
        let Some(queue) = &self.queue else {
            return content.into();
        };
        if queue.tasks.is_empty() && !queue.truncated {
            return content.into();
        }
        content = content
            .push(text(fl!("queue-title")).size(12.0))
            .push(text(fl!("queue-caveat")).size(10.0));
        for task in self.selection.as_ref().unwrap_or(&queue.tasks) {
            let mut cancel = button::text(fl!("queue-cancel-one"));
            if !self.cancelling && !queue.tasks.is_empty() {
                cancel = cancel.on_press(AppMessage::Queue(Message::Cancel(task.id.clone())))
            }
            content = content.push(
                Row::new()
                    .spacing(8)
                    .push(text(format!("{}: {}", task.status, task.prompt)).size(11.0))
                    .push(cancel),
            );
        }
        if queue.truncated {
            content = content.push(text(fl!("queue-truncated")).size(10.0))
        }
        if !self.cancelling {
            content = content.push(
                button::text(if self.selection.is_some() {
                    fl!("queue-confirm-all")
                } else {
                    fl!("queue-cancel-all")
                })
                .on_press(AppMessage::Queue(if self.selection.is_some() {
                    Message::CancelAll
                } else {
                    Message::AskAll
                })),
            );
            if self.selection.is_some() {
                content = content.push(
                    button::text(fl!("queue-keep")).on_press(AppMessage::Queue(Message::KeepAll)),
                )
            }
        }
        content.into()
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/queue_panel.rs"
    ));
}
