use std::sync::Arc;

use tokio::sync::mpsc;

use super::backend::{Backend, ReviewDecision};
use super::commands;
use super::requests::Request;
use super::state::{App, NotificationPreferenceAction, PickerSelection, RunStatus};
use super::stream::{self, RuntimeEvent};
use super::InputAction;

pub(super) fn prepare(
    action: InputAction,
    app: &mut App,
    backend: Arc<dyn Backend>,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
) -> Result<Option<Request>, String> {
    let request = match action {
        InputAction::None => return Ok(None),
        InputAction::Quit => {
            app.should_quit = true;
            return Ok(None);
        }
        InputAction::Cancel => return Ok(None),
        InputAction::OpenAgentHooks => Request::new(
            "Loading hooks",
            async move { Ok(backend.agent_hooks().await) },
            |app, result| {
                match result {
                    Ok(settings) => app.open_agent_hooks(settings),
                    Err(error) => {
                        app.close_platform_overview();
                        return Err(error);
                    }
                }
                Ok(())
            },
        ),
        InputAction::ToggleAgentHook(kind) => {
            let enabled = app
                .agent_hook_enabled(kind)
                .ok_or("Agent hook settings changed; reopen the panel.")?;
            Request::new(
                "Saving hook setting",
                async move { Ok(backend.set_agent_hook(kind, !enabled).await) },
                |app, result| {
                    match result {
                        Ok(settings) => app.open_agent_hooks(settings),
                        Err(error) => app.set_agent_hook_error(&error),
                    }
                    Ok(())
                },
            )
            .mutation()
        }
        InputAction::OpenMcpOverview => platform_request(
            "Loading MCP inventory",
            async move { backend.mcp_overview().await },
            |app, overview| {
                app.open_mcp_overview(overview);
                Ok(())
            },
        ),
        InputAction::OpenExtensionsOverview => platform_request(
            "Loading extensions",
            async move { backend.extensions_overview().await },
            |app, overview| {
                app.open_extensions_overview(overview);
                Ok(())
            },
        ),
        InputAction::OpenUsageOverview(period) => platform_request(
            "Loading usage",
            async move { backend.usage_overview(period).await },
            |app, overview| {
                app.open_usage_overview(overview);
                Ok(())
            },
        ),
        InputAction::OpenDebugOverview => platform_request(
            "Loading diagnostics",
            async move { backend.debug_overview().await },
            |app, overview| {
                app.open_debug_overview(overview);
                Ok(())
            },
        ),
        InputAction::OpenAccountOverview => platform_request(
            "Loading account",
            async move { backend.account_overview().await },
            |app, overview| {
                app.open_account_overview(overview);
                Ok(())
            },
        ),
        InputAction::OpenVoiceOverview => Request::new(
            "Loading voice settings",
            async move { backend.voice_overview().await },
            |app, overview| {
                app.open_voice_overview(overview);
                Ok(())
            },
        ),
        InputAction::Review(decision) => {
            let Some(approval) = app.current_approval() else {
                return Ok(None);
            };
            review_request(backend, approval.id.clone(), decision, false)
        }
        InputAction::ApprovalReview(id, decision) => review_request(backend, id, decision, true),
        InputAction::Picker(selection) => match selection {
            PickerSelection::Model(model) => {
                app.selected_model = model.clone();
                app.push_system(&format!("Future tasks will use {model}."));
                return Ok(None);
            }
            PickerSelection::File(path) => {
                app.insert_file_mention(&path);
                return Ok(None);
            }
            PickerSelection::Approval(approval) => {
                app.open_approval_detail(approval);
                return Ok(None);
            }
            PickerSelection::Notification(notification) => {
                app.open_notification_detail(notification);
                return Ok(None);
            }
            PickerSelection::Session(id) => Request::new(
                "Opening conversation",
                async move { backend.get_conversation(&id).await },
                |app, conversation| {
                    app.replace_conversation(conversation);
                    Ok(())
                },
            ),
            PickerSelection::History {
                before_user_turn,
                prompt,
            } => {
                let id = app.conversation.id.clone();
                Request::new(
                    "Forking conversation history",
                    async move { backend.fork_conversation(&id, Some(before_user_turn)).await },
                    move |app, conversation| {
                        app.replace_conversation(conversation);
                        app.prefill_input(prompt);
                        app.push_system(
                            "Forked before the selected prompt. Files, processes, approvals, \
                             and other admitted effects were not rolled back.",
                        );
                        Ok(())
                    },
                )
                .mutation()
            }
            PickerSelection::Task(id) => Request::new(
                "Loading task",
                async move { backend.get_task(&id).await },
                |app, task| {
                    app.open_task_detail(task);
                    Ok(())
                },
            ),
            PickerSelection::Activity(id) => Request::new(
                "Loading Activity",
                async move { backend.get_activity(&id).await },
                |app, detail| {
                    app.open_activity_detail(detail);
                    Ok(())
                },
            ),
            PickerSelection::ActivityReview(id) => Request::new(
                "Loading staged-file review",
                async move { backend.activity_review(&id).await },
                |app, review| {
                    app.open_activity_review(review);
                    Ok(())
                },
            ),
        },
        InputAction::Confirm(action) => commands::confirm(app, backend, action),
        InputAction::TaskCancel(id) => {
            if app.active_task.as_deref() == Some(id.as_str()) {
                return Ok(None);
            }
            Request::new(
                "Stopping selected task",
                async move {
                    backend.cancel(&id).await?;
                    backend.get_task(&id).await
                },
                |app, task| {
                    app.open_task_detail(task);
                    Ok(())
                },
            )
            .mutation()
        }
        InputAction::TaskRetry(id) => Request::new(
            "Retrying selected task",
            async move { backend.retry_task(&id).await },
            |app, task| {
                let id = task.id.clone();
                app.open_task_detail(task);
                app.push_system(&format!("Retried as durable task {id}."));
                Ok(())
            },
        )
        .mutation(),
        InputAction::FileMentions => {
            let workspace = std::path::PathBuf::from(&app.selected_workspace);
            Request::new(
                "Loading workspace file names",
                async move {
                    tokio::task::spawn_blocking(move || commands::list_workspace_files(&workspace))
                        .await
                        .map_err(|_| "workspace file mention reader failed".to_string())?
                },
                |app, files| {
                    app.open_file_picker(files);
                    Ok(())
                },
            )
        }
        InputAction::MutateNotification(id, mutation) => Request::new(
            "Updating notification",
            async move { backend.mutate_notification(&id, mutation).await },
            |app, notification| {
                app.open_notification_detail(notification);
                Ok(())
            },
        )
        .mutation(),
        InputAction::NotificationPreference(action) => {
            let Some(mut preferences) = app.notification_preferences.clone() else {
                return Ok(None);
            };
            match action {
                NotificationPreferenceAction::ToggleWeb => {
                    preferences.web_enabled = !preferences.web_enabled
                }
                NotificationPreferenceAction::ToggleDesktop => {
                    preferences.desktop_enabled = !preferences.desktop_enabled
                }
                NotificationPreferenceAction::ToggleNtfy => {
                    preferences.ntfy_enabled = !preferences.ntfy_enabled
                }
                NotificationPreferenceAction::ToggleDnd => {
                    if preferences.dnd_start_minute_utc.is_some()
                        && preferences.dnd_end_minute_utc.is_some()
                    {
                        preferences.dnd_start_minute_utc = None;
                        preferences.dnd_end_minute_utc = None;
                    } else {
                        preferences.dnd_start_minute_utc = Some(0);
                        preferences.dnd_end_minute_utc = Some(0);
                    }
                }
            }
            Request::new(
                "Saving notification settings",
                async move { backend.set_notification_preferences(&preferences).await },
                |app, preferences| {
                    app.open_notification_preferences(preferences);
                    Ok(())
                },
            )
            .mutation()
        }
        InputAction::ActivityRun(id) => {
            let workspace = app.selected_workspace.clone();
            Request::new(
                "Submitting Activity task",
                async move { backend.run_activity(&id, None, &workspace).await },
                |app, job| {
                    let id = job.id.clone();
                    app.open_task_detail(job);
                    app.push_system(&format!(
                        "Activity work was submitted as durable task {id}."
                    ));
                    Ok(())
                },
            )
            .mutation()
        }
        InputAction::ActivityTransition(id, state) => Request::new(
            "Updating Activity",
            async move { backend.transition_activity(&id, state, None).await },
            move |app, activity| {
                app.update_activity(activity);
                app.push_system(if state == "paused" {
                    "Activity paused. In-flight tasks were not cancelled."
                } else {
                    "Activity is active."
                });
                Ok(())
            },
        )
        .mutation(),
        InputAction::ActivityAttention(id) => Request::new(
            "Loading Activity attention",
            async move { backend.activity_attention(&id).await },
            |app, attention| {
                app.open_activity_attention(attention);
                Ok(())
            },
        ),
        InputAction::ActivityControls(id) => Request::new(
            "Loading Activity controls",
            async move { backend.activity_controls(&id).await },
            |app, controls| {
                app.open_activity_controls(controls);
                Ok(())
            },
        ),
        InputAction::ActivityEvidence(id) => Request::new(
            "Loading Activity evidence",
            async move { backend.activity_evidence(&id).await },
            |app, evidence| {
                app.open_activity_evidence(evidence);
                Ok(())
            },
        ),
        InputAction::ActivityReview(id) => Request::new(
            "Loading staged-file review",
            async move { backend.activity_review(&id).await },
            |app, review| {
                app.open_activity_review(review);
                Ok(())
            },
        ),
        InputAction::Submit(input) => {
            if let Some(command) = commands::parse(&input) {
                return commands::execute(app, backend, command);
            }
            if app.active_task.is_some() {
                stream::queue_prompt(
                    app,
                    backend,
                    runtime_tx,
                    app.task_use_memory,
                    app.task_max_turns,
                    input,
                )?
            } else {
                stream::start_prompt(
                    app,
                    backend,
                    runtime_tx,
                    app.task_use_memory,
                    app.task_max_turns,
                    input,
                    app.selected_workspace.clone(),
                )?
            }
        }
    };
    Ok(Some(request))
}

fn review_request(
    backend: Arc<dyn Backend>,
    id: String,
    decision: ReviewDecision,
    detail: bool,
) -> Request {
    let request_id = id.clone();
    Request::new(
        "Confirming permission decision",
        async move { Ok(backend.review(&request_id, decision).await) },
        move |app, result| {
            if let Err(error) = result {
                if detail {
                    app.close_approval_detail();
                }
                return Err(error);
            }
            let previous = app.status;
            let approved = decision == ReviewDecision::ApproveOnce;
            app.resolve_approval(&id, approved);
            if matches!(previous, RunStatus::Cancelling | RunStatus::Reconnecting) {
                app.status = previous;
            }
            if detail {
                if let Some(approval) = &mut app.approval_detail {
                    if approval.id == id {
                        approval.status = if approved { "approved" } else { "denied" }.into();
                    }
                }
                app.push_system(if approved {
                    "Approval granted once from this attended terminal."
                } else {
                    "Approval denied; no capability was granted."
                });
            }
            Ok(())
        },
    )
    .mutation()
}

fn platform_request<T, F, A>(label: &'static str, work: F, apply: A) -> Request
where
    T: Send + 'static,
    F: std::future::Future<Output = Result<T, String>> + Send + 'static,
    A: FnOnce(&mut App, T) -> Result<(), String> + Send + 'static,
{
    Request::new(
        label,
        async move { Ok(work.await) },
        move |app, result| match result {
            Ok(value) => apply(app, value),
            Err(error) => {
                app.close_usage_overview();
                app.close_platform_overview();
                Err(error)
            }
        },
    )
}
