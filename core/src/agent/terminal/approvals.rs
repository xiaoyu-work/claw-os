use std::sync::Arc;

use crate::approvals::RequestStatus;

use super::backend::{ApprovalSnapshot, Backend, ReviewDecision};
use super::requests::Request;
use super::state::{App, ApprovalStatus, EntryKind, RunStatus};

#[derive(Default)]
pub(super) struct ApprovalWatch {
    ids: Vec<String>,
    generation: u64,
    error: Option<String>,
}

impl ApprovalWatch {
    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.error = None;
    }

    pub fn resolve(&mut self, id: &str) {
        self.ids.retain(|pending| pending != id);
        self.invalidate();
    }
}

impl App {
    pub fn awaiting_approval(&self) -> bool {
        !self.approval_watch.ids.is_empty() || self.current_approval().is_some()
    }

    pub fn wait_for_approvals(&mut self, ids: Vec<String>) {
        if self.approval_watch.ids != ids {
            self.clear_approval_ui();
            self.approval_watch.ids = ids;
        }
        if !matches!(self.status, RunStatus::Cancelling | RunStatus::Reconnecting) {
            self.status = RunStatus::WaitingApproval;
        }
    }

    pub fn clear_approval_ui(&mut self) {
        self.approval_watch.ids.clear();
        self.approval_watch.invalidate();
        self.pending_approvals.clear();
        self.approval_choice = ReviewDecision::ApproveOnce;
        self.approval_scroll = 0;
        for entry in &mut self.entries {
            if let EntryKind::Approval { status, .. } = &mut entry.kind {
                if *status == ApprovalStatus::Pending {
                    *status = ApprovalStatus::Closed;
                }
            }
        }
        if self
            .approval_detail
            .as_ref()
            .is_some_and(|request| matches!(request.status.as_str(), "pending" | "resolving"))
        {
            self.close_approval_detail();
        }
        if self.status == RunStatus::WaitingApproval {
            self.status = if self.active_task.is_some() {
                RunStatus::Working
            } else {
                RunStatus::Ready
            };
        }
    }

    pub fn reconcile_approvals(&mut self, snapshot: ApprovalSnapshot) {
        let previous = self.status;
        let current_id = self.current_approval().map(|request| request.id.clone());
        self.approval_watch.error = None;
        for state in snapshot.states {
            if let Some(detail) = &mut self.approval_detail {
                if detail.id == state.id {
                    detail.status = state.status.as_str().to_string();
                }
            }
            let status = match state.status {
                RequestStatus::Pending => continue,
                RequestStatus::Resolving => {
                    self.pending_approvals
                        .retain(|request| request.id != state.id);
                    continue;
                }
                RequestStatus::Approved | RequestStatus::Consumed => ApprovalStatus::Approved,
                RequestStatus::Denied => ApprovalStatus::Denied,
                RequestStatus::Unknown => ApprovalStatus::Closed,
            };
            self.pending_approvals
                .retain(|request| request.id != state.id);
            self.approval_watch.resolve(&state.id);
            for entry in &mut self.entries {
                if let EntryKind::Approval {
                    id,
                    status: entry_status,
                } = &mut entry.kind
                {
                    if id == &state.id {
                        *entry_status = status;
                    }
                }
            }
        }
        let pending = snapshot
            .pending
            .into_iter()
            .filter(|request| self.approval_watch.ids.contains(&request.id))
            .collect();
        self.add_approvals(pending);
        if current_id.as_deref() != self.current_approval().map(|request| request.id.as_str()) {
            self.approval_choice = ReviewDecision::ApproveOnce;
            self.approval_scroll = 0;
        }
        self.status = match previous {
            RunStatus::Cancelling | RunStatus::Reconnecting => previous,
            _ if self.awaiting_approval() => RunStatus::WaitingApproval,
            _ if self.active_task.is_some() => RunStatus::Working,
            _ => RunStatus::Ready,
        };
    }
}

pub(super) fn refresh(app: &App, backend: Arc<dyn Backend>) -> Option<Request> {
    let mut ids = app.approval_watch.ids.clone();
    if let Some(detail) = &app.approval_detail {
        if matches!(detail.status.as_str(), "pending" | "resolving")
            && !ids.contains(&detail.id)
            && ids.len() < 64
        {
            ids.push(detail.id.clone());
        }
    }
    if ids.is_empty() {
        return None;
    }
    let task_id = app.active_task.clone();
    let generation = app.approval_watch.generation;
    Some(Request::new(
        "Refreshing permissions",
        async move { Ok(backend.approvals(&ids).await) },
        move |app, result| {
            if app.active_task != task_id || app.approval_watch.generation != generation {
                return Ok(());
            }
            match result {
                Ok(snapshot) => app.reconcile_approvals(snapshot),
                Err(error) => {
                    if app.approval_watch.error.as_ref() != Some(&error) {
                        app.push_error(&error);
                        app.approval_watch.error = Some(error);
                    }
                }
            }
            Ok(())
        },
    ))
}
