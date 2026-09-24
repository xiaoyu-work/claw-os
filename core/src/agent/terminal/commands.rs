use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use serde_json::Value;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use super::backend::{ActivityControlPolicy, Backend};
use super::state::{App, ConfirmationAction, RunStatus};

pub(super) struct WorkspaceFiles {
    pub paths: Vec<String>,
    pub truncated: bool,
}

// Keep object-specific mutations in their panels instead of turning the
// completion palette into a list of backend operations.
pub(super) const PALETTE_COMMANDS: &[(&str, &str)] = &[
    ("/help", "show Claw terminal commands"),
    ("/new", "start a new canonical conversation"),
    ("/sessions", "list recent conversations"),
    ("/resume", "resume a conversation by session id"),
    ("/rename", "rename the current conversation"),
    ("/archive", "archive without deleting history"),
    ("/unarchive", "restore an archived conversation"),
    ("/fork", "fork retained history into a new conversation"),
    (
        "/rewind",
        "rewind retained user turns without undoing effects",
    ),
    ("/models", "list configured-provider models"),
    ("/model", "select the model for future tasks"),
    ("/workspace", "show or select a broker-validated workspace"),
    ("/attach", "attach an image to the next task"),
    ("/review", "review staged file plans and reported diffs"),
    ("/copy", "copy the latest assistant response"),
    ("/export", "export visible conversation text as Markdown"),
    ("/raw", "publish a redacted snapshot to terminal scrollback"),
    ("/appearance", "configure theme, title, and status line"),
    ("/voice", "show Claw voice models and realtime availability"),
    ("/agents", "show scoped delegate calls in this task"),
    ("/side", "start or return from a side conversation"),
    ("/platform", "show verified Skills, MCP, Apps, and usage"),
    ("/skills", "list enabled Claw Skills"),
    ("/tasks", "browse durable Agent tasks"),
    ("/task", "open a durable task by id"),
    ("/approvals", "browse pending and recent approvals"),
    ("/approval", "open an approval by id"),
    ("/inbox", "browse durable notifications"),
    ("/notification", "open a notification by id"),
    ("/notify-settings", "show delivery and DND settings"),
    ("/notify-channel", "enable or disable a delivery channel"),
    ("/notify-severity", "set a channel minimum severity"),
    ("/dnd", "set or disable the UTC DND window"),
    ("/activity", "browse, open, or create an Activity"),
    ("/session", "show current Claw identity and model"),
    ("/clear", "clear only the terminal transcript view"),
    ("/cancel", "cancel the exact current task"),
    ("/quit", "close the terminal without inventing task success"),
];

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Command {
    Help,
    New,
    Sessions,
    Resume(String),
    Rename(String),
    Archive,
    Unarchive,
    Fork,
    Rewind(u32),
    Models,
    Model(String),
    Workspace(Option<String>),
    Attach(Option<String>),
    Review(Option<String>),
    Copy,
    Export(String),
    Raw,
    Appearance,
    Voice,
    Vim,
    Keymap,
    Agents,
    Side(bool),
    Platform,
    Skills,
    Tasks,
    Task(String),
    Approvals,
    Approval(String),
    Notifications(bool),
    Notification(String),
    NotificationSettings,
    NotifyChannel(NotificationChannel, bool),
    NotifySeverity(NotificationChannel, String),
    Dnd(Option<(u16, u16)>),
    Activities(Option<String>),
    Activity(String),
    ActivityCreate {
        title: String,
        goal: String,
    },
    ActivityRun {
        id: String,
        prompt: Option<String>,
    },
    ActivityPause(String),
    ActivityResume(String),
    ActivityComplete {
        id: String,
        note: String,
    },
    ActivityCancel(String),
    ActivityAttention(String),
    ActivityControls(String),
    ActivityControlSet {
        id: String,
        policy: ActivityControlPolicy,
        expected_revision: Option<u64>,
        draft: Value,
    },
    ActivityControlEnabled {
        id: String,
        policy: ActivityControlPolicy,
        revision: u64,
        enabled: bool,
    },
    ActivityPriority {
        id: String,
        priority: String,
        expected_revision: Option<u64>,
    },
    ActivityEvidence(String),
    ActivityPreview {
        id: String,
        app_id: String,
        operation: String,
        args: Vec<String>,
    },
    Session,
    Clear,
    Cancel,
    Quit,
    Unknown(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NotificationChannel {
    Web,
    Desktop,
    Ntfy,
}

pub(super) fn parse(value: &str) -> Option<Command> {
    let value = value.strip_prefix('/')?;
    let (name, rest) = value
        .split_once(char::is_whitespace)
        .map_or((value, ""), |(name, rest)| (name, rest.trim()));
    Some(match name {
        "help" | "?" => Command::Help,
        "new" => Command::New,
        "sessions" => Command::Sessions,
        "resume" if rest.is_empty() => Command::Sessions,
        "resume" => Command::Resume(rest.to_string()),
        "rename" if !rest.is_empty() => Command::Rename(rest.to_string()),
        "archive" => Command::Archive,
        "unarchive" => Command::Unarchive,
        "fork" => Command::Fork,
        "rewind" => rest
            .parse::<u32>()
            .ok()
            .filter(|turns| *turns > 0)
            .map_or_else(|| Command::Unknown(value.to_string()), Command::Rewind),
        "models" => Command::Models,
        "model" if rest.is_empty() => Command::Models,
        "model" => Command::Model(rest.to_string()),
        "workspace" if rest.is_empty() => Command::Workspace(None),
        "workspace" => Command::Workspace(Some(rest.to_string())),
        "attach" if rest.is_empty() => Command::Attach(None),
        "attach" => Command::Attach(Some(rest.to_string())),
        "review" if rest.is_empty() => Command::Review(None),
        "review" => Command::Review(Some(rest.to_string())),
        "copy" if rest.is_empty() => Command::Copy,
        "export" if !rest.is_empty() => Command::Export(rest.to_string()),
        "raw" if rest.is_empty() => Command::Raw,
        "appearance" if rest.is_empty() => Command::Appearance,
        "voice" if rest.is_empty() || rest == "settings" => Command::Voice,
        "vim" if rest.is_empty() => Command::Vim,
        "keymap" if rest.is_empty() => Command::Keymap,
        "agents" if rest.is_empty() => Command::Agents,
        "side" if rest.is_empty() => Command::Side(false),
        "side" if rest == "return" => Command::Side(true),
        "platform" if rest.is_empty() => Command::Platform,
        "skills" => Command::Skills,
        "tasks" => Command::Tasks,
        "task" if rest.is_empty() => Command::Tasks,
        "task" => Command::Task(rest.to_string()),
        "approvals" => Command::Approvals,
        "approval" if rest.is_empty() => Command::Approvals,
        "approval" => Command::Approval(rest.to_string()),
        "inbox" | "notifications" if rest.is_empty() => Command::Notifications(false),
        "inbox" | "notifications" if rest.eq_ignore_ascii_case("all") => {
            Command::Notifications(true)
        }
        "notification" if rest.is_empty() => Command::Notifications(false),
        "notification" => Command::Notification(rest.to_string()),
        "notify-settings" if rest.is_empty() => Command::NotificationSettings,
        "notify-channel" => parse_channel_toggle(rest).map_or_else(
            || Command::Unknown(value.to_string()),
            |(channel, enabled)| Command::NotifyChannel(channel, enabled),
        ),
        "notify-severity" => parse_channel_severity(rest).map_or_else(
            || Command::Unknown(value.to_string()),
            |(channel, severity)| Command::NotifySeverity(channel, severity),
        ),
        "dnd" => parse_dnd(rest)
            .map(Command::Dnd)
            .unwrap_or_else(|| Command::Unknown(value.to_string())),
        "activities" if rest.is_empty() => Command::Activities(None),
        "activities" if matches!(rest, "active" | "paused" | "completed" | "cancelled") => {
            Command::Activities(Some(rest.to_string()))
        }
        "activity" if rest.is_empty() => Command::Activities(None),
        "activity" if rest == "new" => Command::Unknown(value.to_string()),
        "activity" => rest.strip_prefix("new ").map_or_else(
            || Command::Activity(rest.to_string()),
            |draft| {
                parse_required_pair(draft.trim())
                    .map(|(title, goal)| Command::ActivityCreate { title, goal })
                    .unwrap_or_else(|| Command::Unknown(value.to_string()))
            },
        ),
        "activity-create" => parse_required_pair(rest)
            .map(|(title, goal)| Command::ActivityCreate { title, goal })
            .unwrap_or_else(|| Command::Unknown(value.to_string())),
        "activity-run" => parse_id_and_optional_text(rest)
            .map(|(id, prompt)| Command::ActivityRun { id, prompt })
            .unwrap_or_else(|| Command::Unknown(value.to_string())),
        "activity-pause" if !rest.is_empty() => Command::ActivityPause(rest.to_string()),
        "activity-resume" | "activity-reopen" if !rest.is_empty() => {
            Command::ActivityResume(rest.to_string())
        }
        "activity-complete" => parse_required_pair(rest)
            .map(|(id, note)| Command::ActivityComplete { id, note })
            .unwrap_or_else(|| Command::Unknown(value.to_string())),
        "activity-cancel" if !rest.is_empty() => Command::ActivityCancel(rest.to_string()),
        "activity-attention" if !rest.is_empty() => Command::ActivityAttention(rest.to_string()),
        "activity-controls" if !rest.is_empty() => Command::ActivityControls(rest.to_string()),
        "activity-limits-set" => {
            parse_activity_control_set(rest, ActivityControlPolicy::ExecutionLimits)
                .unwrap_or_else(|| Command::Unknown(value.to_string()))
        }
        "activity-budget-set" => {
            parse_activity_control_set(rest, ActivityControlPolicy::MonetaryBudget)
                .unwrap_or_else(|| Command::Unknown(value.to_string()))
        }
        "activity-capability-set" => {
            parse_activity_control_set(rest, ActivityControlPolicy::CapabilityPolicy)
                .unwrap_or_else(|| Command::Unknown(value.to_string()))
        }
        "activity-limits-enable" => {
            parse_activity_control_enabled(rest, ActivityControlPolicy::ExecutionLimits)
                .unwrap_or_else(|| Command::Unknown(value.to_string()))
        }
        "activity-budget-enable" => {
            parse_activity_control_enabled(rest, ActivityControlPolicy::MonetaryBudget)
                .unwrap_or_else(|| Command::Unknown(value.to_string()))
        }
        "activity-capability-enable" => {
            parse_activity_control_enabled(rest, ActivityControlPolicy::CapabilityPolicy)
                .unwrap_or_else(|| Command::Unknown(value.to_string()))
        }
        "activity-priority" => {
            parse_activity_priority(rest).unwrap_or_else(|| Command::Unknown(value.to_string()))
        }
        "activity-evidence" if !rest.is_empty() => Command::ActivityEvidence(rest.to_string()),
        "activity-preview" => {
            parse_activity_preview(rest).unwrap_or_else(|| Command::Unknown(value.to_string()))
        }
        "session" => Command::Session,
        "clear" => Command::Clear,
        "cancel" | "stop" => Command::Cancel,
        "quit" | "exit" | "q" => Command::Quit,
        _ => Command::Unknown(value.to_string()),
    })
}

pub(super) fn suggestions(input: &str) -> Vec<(&'static str, &'static str)> {
    if !input.starts_with('/') || input.chars().any(char::is_whitespace) {
        return Vec::new();
    }
    PALETTE_COMMANDS
        .iter()
        .copied()
        .filter(|(command, _)| command.starts_with(input))
        .collect()
}

pub(super) fn completion(input: &str, selected: usize) -> Option<String> {
    let suggestions = suggestions(input);
    let (command, _) = suggestions.get(selected.min(suggestions.len().saturating_sub(1)))?;
    Some(if takes_argument(command) {
        format!("{command} ")
    } else {
        (*command).to_string()
    })
}

fn takes_argument(command: &str) -> bool {
    matches!(
        command,
        "/resume"
            | "/rename"
            | "/rewind"
            | "/model"
            | "/workspace"
            | "/attach"
            | "/review"
            | "/export"
            | "/side"
            | "/task"
            | "/approval"
            | "/inbox"
            | "/notification"
            | "/notify-channel"
            | "/notify-severity"
            | "/dnd"
            | "/activities"
            | "/activity"
            | "/activity-create"
            | "/activity-run"
            | "/activity-pause"
            | "/activity-resume"
            | "/activity-complete"
            | "/activity-cancel"
            | "/activity-attention"
            | "/activity-controls"
            | "/activity-limits-set"
            | "/activity-limits-enable"
            | "/activity-budget-set"
            | "/activity-budget-enable"
            | "/activity-priority"
            | "/activity-capability-set"
            | "/activity-capability-enable"
            | "/activity-evidence"
            | "/activity-preview"
    )
}

pub(super) async fn execute(
    app: &mut App,
    backend: Arc<dyn Backend>,
    command: Command,
) -> Result<(), String> {
    if app.active_task.is_some()
        && !matches!(
            command,
            Command::Help
                | Command::Tasks
                | Command::Task(_)
                | Command::Workspace(_)
                | Command::Attach(_)
                | Command::Review(_)
                | Command::Copy
                | Command::Export(_)
                | Command::Raw
                | Command::Appearance
                | Command::Vim
                | Command::Keymap
                | Command::Agents
                | Command::Platform
                | Command::Approvals
                | Command::Approval(_)
                | Command::Notifications(_)
                | Command::Notification(_)
                | Command::NotificationSettings
                | Command::NotifyChannel(_, _)
                | Command::NotifySeverity(_, _)
                | Command::Dnd(_)
                | Command::Activities(_)
                | Command::Activity(_)
                | Command::ActivityCreate { .. }
                | Command::ActivityRun { .. }
                | Command::ActivityPause(_)
                | Command::ActivityResume(_)
                | Command::ActivityComplete { .. }
                | Command::ActivityCancel(_)
                | Command::ActivityAttention(_)
                | Command::ActivityControls(_)
                | Command::ActivityControlSet { .. }
                | Command::ActivityControlEnabled { .. }
                | Command::ActivityPriority { .. }
                | Command::ActivityEvidence(_)
                | Command::ActivityPreview { .. }
                | Command::Session
                | Command::Cancel
                | Command::Quit
        )
    {
        app.push_system("That command requires the current task to finish. Use /cancel first.");
        return Ok(());
    }
    match command {
        Command::Help => app.push_system(
            "/new  /sessions  /resume ID  /rename TITLE  /archive  /unarchive\n\
             /fork  /rewind N  /models  /model ID  /workspace [PATH|home]  /skills\n\
             /attach [PATH|clear]\n\
             /review [ACTIVITY_ID]\n\
             /copy  /export PATH\n\
             /raw\n\
             /appearance\n\
             /voice [settings]\n\
             /agents\n\
             /side [return]\n\
             /platform\n\
             /tasks  /task ID  /approvals  /approval ID\n\
             /inbox [all]  /notification ID  /notify-settings\n\
             /notify-channel CHANNEL on|off  /notify-severity CHANNEL LEVEL\n\
             /dnd off|HH:MM-HH:MM  /session\n\
             /activity [ID]  /activity new TITLE | GOAL\n\
             /clear  /cancel  /quit\n\
             Open an Activity to run, pause, complete, configure, or inspect it.\n\
             Enter submits text; Esc cancels the current task; queued text runs next.",
        ),
        Command::New => {
            let conversation = backend.create_conversation().await?;
            app.replace_conversation(conversation);
        }
        Command::Sessions => {
            let page = backend.list_conversations().await?;
            if page.conversations.is_empty() {
                app.push_system("No conversations.");
            } else {
                if page.truncated {
                    app.push_system("Session picker shows only the newest 100 conversations.");
                }
                app.open_session_picker(page.conversations);
            }
        }
        Command::Resume(id) => {
            let conversation = backend.get_conversation(&id).await?;
            app.replace_conversation(conversation);
        }
        Command::Rename(title) => {
            let conversation = backend
                .update_conversation(&app.conversation.id, Some(&title), None)
                .await?;
            app.conversation.title = conversation.title;
            app.push_system("Conversation renamed.");
        }
        Command::Archive => {
            app.confirm_archive();
        }
        Command::Unarchive => {
            let conversation = backend
                .update_conversation(&app.conversation.id, None, Some(false))
                .await?;
            app.conversation.archived = conversation.archived;
            app.push_system("Conversation restored from the archive.");
        }
        Command::Fork => {
            let conversation = backend
                .fork_conversation(&app.conversation.id, None)
                .await?;
            app.replace_conversation(conversation);
            app.push_system("Forked into a new canonical Claw conversation.");
        }
        Command::Rewind(user_turns) => {
            app.confirm_rewind(user_turns);
        }
        Command::Models => app.open_model_picker(),
        Command::Model(model) => {
            if app.info.models.iter().any(|candidate| candidate == &model) {
                app.selected_model = model.clone();
                app.push_system(&format!("Future tasks will use {model}."));
            } else {
                app.push_error("Unknown model. Use /models for the configured catalogue.");
            }
        }
        Command::Workspace(None) => app.push_system(&format!(
            "workspace: {}\nBroker validation is required before this path reaches a task.",
            app.selected_workspace
        )),
        Command::Workspace(Some(path)) => {
            let workspace = backend
                .resolve_workspace((path != "home").then_some(path.as_str()))
                .await?;
            app.set_workspace(workspace);
        }
        Command::Attach(path) => match path.as_deref() {
            None => app.describe_attachments(),
            Some("clear") => app.clear_attachments(),
            Some(path) => {
                let home = app.info.home.clone();
                let workspace = PathBuf::from(&app.selected_workspace);
                let path = path.to_string();
                let attachment = tokio::task::spawn_blocking(move || {
                    load_image_attachment(&path, &home, &workspace)
                })
                .await
                .map_err(|_| "image attachment reader failed".to_string())??;
                app.add_attachment(attachment)?;
            }
        },
        Command::Review(None) => {
            let activities = backend.list_activities(None).await?;
            if activities.is_empty() {
                app.push_system("No Activities are available for staged-file review.");
            } else {
                app.open_activity_review_picker(activities);
            }
        }
        Command::Review(Some(id)) => {
            let review = backend.activity_review(&id).await?;
            app.open_activity_review(review);
        }
        Command::Copy => {
            let text = app
                .last_assistant_text()
                .ok_or("There is no assistant response to copy.")?;
            let sequence = osc52_copy_sequence(text)?;
            let mut stdout = std::io::stdout();
            stdout
                .write_all(sequence.as_bytes())
                .and_then(|_| stdout.flush())
                .map_err(|error| format!("write terminal clipboard sequence: {error}"))?;
            app.push_system("Copied the latest displayed assistant response through OSC 52.");
        }
        Command::Export(path) => {
            let markdown = app.export_markdown()?;
            let home = app.info.home.clone();
            let workspace = PathBuf::from(&app.selected_workspace);
            let path = tokio::task::spawn_blocking(move || {
                write_markdown_export(&path, &home, &workspace, &markdown)
            })
            .await
            .map_err(|_| "conversation export writer failed".to_string())??;
            app.push_system(&format!("Exported visible conversation text to {}.", path.display()));
        }
        Command::Raw => app.request_raw_scrollback(),
        Command::Appearance => app.open_appearance(),
        Command::Voice => {
            let overview = backend.voice_overview().await?;
            app.open_voice_overview(overview);
        }
        Command::Vim => {
            app.toggle_vim_mode();
            app.push_system(&format!("Composer keymap: {}.", app.keymap_name()));
        }
        Command::Keymap => app.open_appearance(),
        Command::Agents => app.open_agents(),
        Command::Side(false) => {
            if app.in_side_conversation() {
                return Err("Already in a side conversation. Use /side return first.".into());
            }
            let parent_id = app.conversation.id.clone();
            let conversation = backend.fork_conversation(&parent_id, None).await?;
            let side_id = conversation.id.clone();
            app.replace_conversation(conversation);
            app.begin_side_conversation(parent_id, side_id);
            app.push_system(
                "Side conversation started as a checked durable fork. \
                 Use /side return to archive it and return to the parent.",
            );
        }
        Command::Side(true) => {
            let (parent_id, side_id) = app
                .side_conversation()
                .map(|(parent, side)| (parent.to_string(), side.to_string()))
                .ok_or("This terminal is not in a side conversation.")?;
            backend
                .update_conversation(&side_id, None, Some(true))
                .await?;
            let parent = backend.get_conversation(&parent_id).await?;
            app.replace_conversation(parent);
            app.push_system("Returned from the archived side conversation.");
        }
        Command::Platform => {
            let overview = backend.platform_overview().await?;
            app.open_platform_overview(overview);
        }
        Command::Skills => match backend.skills().await {
            Ok(skills) if skills.is_empty() => app.push_system("No enabled Claw Skills."),
            Ok(skills) => app.push_system(
                &skills
                    .into_iter()
                    .map(|skill| {
                        format!(
                            "{} [{}]{}",
                            skill.id,
                            skill.origin,
                            if skill.description.is_empty() {
                                String::new()
                            } else {
                                format!(" - {}", skill.description)
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Err(error) => app.push_error(&error),
        },
        Command::Tasks => {
            let tasks = backend.list_tasks().await?;
            if tasks.is_empty() {
                app.push_system("No durable Agent tasks.");
            } else {
                app.open_task_picker(tasks);
            }
        }
        Command::Task(id) => {
            let task = backend.get_task(&id).await?;
            app.open_task_detail(task);
        }
        Command::Approvals => {
            let approvals = backend.list_approvals().await?;
            if approvals.is_empty() {
                app.push_system("No pending or recent approvals.");
            } else {
                app.open_approval_picker(approvals);
            }
        }
        Command::Approval(id) => {
            let approval = backend
                .list_approvals()
                .await?
                .into_iter()
                .find(|approval| approval.id == id)
                .ok_or_else(|| format!("Approval {id} is not pending or recent."))?;
            app.open_approval_detail(approval);
        }
        Command::Notifications(include_dismissed) => {
            let page = backend.list_notifications(include_dismissed).await?;
            if page.notifications.is_empty() {
                app.push_system("Notification Inbox is empty.");
            } else {
                app.open_notification_picker(page);
            }
        }
        Command::Notification(id) => {
            let notification = backend
                .list_notifications(true)
                .await?
                .notifications
                .into_iter()
                .find(|notification| notification.id == id)
                .ok_or_else(|| format!("Notification {id} is not retained."))?;
            app.open_notification_detail(notification);
        }
        Command::NotificationSettings => {
            let preferences = backend.notification_preferences().await?;
            app.open_notification_preferences(preferences);
        }
        Command::NotifyChannel(channel, enabled) => {
            let mut preferences = backend.notification_preferences().await?;
            match channel {
                NotificationChannel::Web => preferences.web_enabled = enabled,
                NotificationChannel::Desktop => preferences.desktop_enabled = enabled,
                NotificationChannel::Ntfy => preferences.ntfy_enabled = enabled,
            }
            let preferences = backend.set_notification_preferences(&preferences).await?;
            app.open_notification_preferences(preferences);
        }
        Command::NotifySeverity(channel, severity) => {
            let mut preferences = backend.notification_preferences().await?;
            match channel {
                NotificationChannel::Web => preferences.web_min_severity = severity,
                NotificationChannel::Desktop => preferences.desktop_min_severity = severity,
                NotificationChannel::Ntfy => preferences.ntfy_min_severity = severity,
            }
            let preferences = backend.set_notification_preferences(&preferences).await?;
            app.open_notification_preferences(preferences);
        }
        Command::Dnd(window) => {
            let mut preferences = backend.notification_preferences().await?;
            (
                preferences.dnd_start_minute_utc,
                preferences.dnd_end_minute_utc,
            ) = window
                .map(|(start, end)| (Some(start), Some(end)))
                .unwrap_or((None, None));
            let preferences = backend.set_notification_preferences(&preferences).await?;
            app.open_notification_preferences(preferences);
        }
        Command::Activities(state) => {
            let activities = backend.list_activities(state.as_deref()).await?;
            if activities.is_empty() {
                app.push_system("No Activities match this view.");
            } else {
                app.open_activity_picker(activities);
            }
        }
        Command::Activity(id) => {
            let detail = backend.get_activity(&id).await?;
            app.open_activity_detail(detail);
        }
        Command::ActivityCreate { title, goal } => {
            let activity = backend.create_activity(&title, &goal).await?;
            let detail = backend.get_activity(&activity.id).await?;
            app.open_activity_detail(detail);
        }
        Command::ActivityRun { id, prompt } => {
            let job = backend
                .run_activity(&id, prompt.as_deref(), &app.selected_workspace)
                .await?;
            let task_id = job.id.clone();
            app.open_task_detail(job);
            app.push_system(&format!(
                "Activity work was submitted as durable task {task_id}."
            ));
        }
        Command::ActivityPause(id) => {
            let activity = backend.transition_activity(&id, "paused", None).await?;
            app.update_activity(activity);
            app.push_system("Activity paused. In-flight tasks were not cancelled.");
        }
        Command::ActivityResume(id) => {
            let activity = backend.transition_activity(&id, "active", None).await?;
            app.update_activity(activity);
            app.push_system("Activity is active.");
        }
        Command::ActivityComplete { id, note } => {
            app.confirm_activity_complete(id, note);
        }
        Command::ActivityCancel(id) => {
            app.confirm_activity_cancel(id);
        }
        Command::ActivityAttention(id) => {
            let attention = backend.activity_attention(&id).await?;
            app.open_activity_attention(attention);
        }
        Command::ActivityControls(id) => {
            let controls = backend.activity_controls(&id).await?;
            app.open_activity_controls(controls);
        }
        Command::ActivityControlSet {
            id,
            policy,
            expected_revision,
            draft,
        } => {
            let controls = backend
                .set_activity_control(&id, policy, expected_revision, draft)
                .await?;
            app.open_activity_controls(controls);
        }
        Command::ActivityControlEnabled {
            id,
            policy,
            revision,
            enabled,
        } => {
            let controls = backend
                .set_activity_control_enabled(&id, policy, revision, enabled)
                .await?;
            app.open_activity_controls(controls);
        }
        Command::ActivityPriority {
            id,
            priority,
            expected_revision,
        } => {
            let controls = backend
                .set_activity_priority(&id, expected_revision, &priority)
                .await?;
            app.open_activity_controls(controls);
        }
        Command::ActivityEvidence(id) => {
            let evidence = backend.activity_evidence(&id).await?;
            app.open_activity_evidence(evidence);
        }
        Command::ActivityPreview {
            id,
            app_id,
            operation,
            args,
        } => {
            let preview = backend
                .activity_operation_preview(&id, &app_id, &operation, &args)
                .await?;
            app.open_activity_operation_preview(preview);
        }
        Command::Session => app.push_system(&format!(
            "session: {}\nmodel: {}\nprovider: {}\nreasoning effort: {}\nmode: {}\nworkspace: {}\nmemory: {}\nmax turns: {}",
            app.conversation.id,
            app.selected_model,
            app.info.provider,
            app.task_reasoning_effort
                .as_deref()
                .unwrap_or("provider default"),
            if app.task_plan_only {
                "plan only"
            } else {
                "execute"
            },
            app.selected_workspace,
            if app.task_use_memory { "on" } else { "off" },
            app.task_max_turns
                .map(|turns| turns.to_string())
                .unwrap_or_else(|| "configured default".into())
        )),
        Command::Clear => app.clear_transcript(),
        Command::Cancel => {
            if let Some(task_id) = app.active_task.clone() {
                app.status = RunStatus::Cancelling;
                if let Err(error) = backend.cancel(&task_id).await {
                    app.push_error(&error);
                    app.status = RunStatus::Working;
                }
            } else {
                app.push_system("No active task.");
            }
        }
        Command::Quit => app.should_quit = true,
        Command::Unknown(command) => {
            app.push_error(&format!("Unknown command /{command}. Type /help."));
        }
    }
    Ok(())
}

pub(super) fn load_image_attachment(
    value: &str,
    home: &Path,
    workspace: &Path,
) -> Result<crate::agent::attachments::AttachmentInput, String> {
    let value = value.trim();
    let value = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(value);
    if value.is_empty() {
        return Err("image attachment path is empty".into());
    }
    let requested = Path::new(value);
    let requested = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        workspace.join(requested)
    };
    let path = requested
        .canonicalize()
        .map_err(|error| format!("resolve image attachment {}: {error}", requested.display()))?;
    if !path.starts_with(home) {
        return Err("image attachments must be inside the verified owner home".into());
    }
    let metadata = path
        .metadata()
        .map_err(|error| format!("inspect image attachment {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err("image attachment must be a regular file".into());
    }
    if metadata.len() > crate::agent::attachments::MAX_TOTAL_BYTES as u64 {
        return Err(format!(
            "image attachment exceeds the {}-byte task limit",
            crate::agent::attachments::MAX_TOTAL_BYTES
        ));
    }
    let media_type = match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => return Err("image attachment must be PNG, JPEG, GIF, or WebP".into()),
    };
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("image attachment has no valid UTF-8 file name")?
        .to_string();
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .map_err(|error| format!("open image attachment {}: {error}", path.display()))?
        .take(crate::agent::attachments::MAX_TOTAL_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read image attachment {}: {error}", path.display()))?;
    if bytes.len() > crate::agent::attachments::MAX_TOTAL_BYTES {
        return Err(format!(
            "image attachment exceeds the {}-byte task limit",
            crate::agent::attachments::MAX_TOTAL_BYTES
        ));
    }
    let input = crate::agent::attachments::AttachmentInput {
        name,
        media_type: media_type.to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
    };
    crate::agent::attachments::normalize(vec![input.clone()])?;
    Ok(input)
}

pub(super) fn list_workspace_files(workspace: &Path) -> Result<WorkspaceFiles, String> {
    const MAX_FILES: usize = 512;
    const MAX_DEPTH: usize = 4;
    let workspace = workspace
        .canonicalize()
        .map_err(|error| format!("resolve workspace for file mentions: {error}"))?;
    let mut paths = Vec::new();
    let mut stack = vec![(workspace.clone(), 0usize)];
    let mut truncated = false;
    while let Some((directory, depth)) = stack.pop() {
        let entries = std::fs::read_dir(&directory)
            .map_err(|error| format!("list workspace {}: {error}", directory.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("read workspace entry: {error}"))?;
            let file_type = entry
                .file_type()
                .map_err(|error| format!("inspect workspace entry: {error}"))?;
            let path = entry.path();
            if file_type.is_dir() && depth < MAX_DEPTH {
                let name = entry.file_name();
                if !matches!(
                    name.to_str(),
                    Some(".git" | "target" | "node_modules" | ".venv")
                ) {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if paths.len() == MAX_FILES {
                truncated = true;
                break;
            }
            let relative = path
                .strip_prefix(&workspace)
                .map_err(|_| "workspace file escaped its canonical root".to_string())?;
            let relative = relative
                .to_str()
                .filter(|value| !value.chars().any(char::is_control))
                .ok_or("workspace file path is not valid UTF-8")?;
            paths.push(relative.replace('\\', "/"));
        }
        if truncated {
            break;
        }
    }
    paths.sort();
    Ok(WorkspaceFiles { paths, truncated })
}

pub(super) fn osc52_copy_sequence(text: &str) -> Result<String, String> {
    const MAX_COPY_BYTES: usize = 64 * 1024;
    if text.is_empty() {
        return Err("There is no assistant response to copy.".into());
    }
    if text.len() > MAX_COPY_BYTES {
        return Err(format!(
            "assistant response exceeds the {MAX_COPY_BYTES}-byte terminal clipboard limit"
        ));
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    Ok(format!("\u{1b}]52;c;{encoded}\u{7}"))
}

pub(super) fn write_markdown_export(
    value: &str,
    home: &Path,
    workspace: &Path,
    markdown: &str,
) -> Result<PathBuf, String> {
    let requested = Path::new(value.trim());
    if requested.as_os_str().is_empty() {
        return Err("conversation export path is empty".into());
    }
    let requested = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        workspace.join(requested)
    };
    let parent = requested
        .parent()
        .ok_or("conversation export path has no parent")?
        .canonicalize()
        .map_err(|error| format!("resolve conversation export parent: {error}"))?;
    if !parent.starts_with(home) {
        return Err("conversation export must stay inside the verified owner home".into());
    }
    let name = requested
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or("conversation export path has no file name")?;
    let target = parent.join(name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&target)
        .map_err(|error| format!("create conversation export {}: {error}", target.display()))?;
    if let Err(error) = file
        .write_all(markdown.as_bytes())
        .and_then(|_| file.sync_all())
    {
        let cleanup = std::fs::remove_file(&target);
        return Err(match cleanup {
            Ok(()) => format!("write conversation export {}: {error}", target.display()),
            Err(cleanup) => format!(
                "write conversation export {}: {error}; cleanup failed: {cleanup}",
                target.display()
            ),
        });
    }
    std::fs::File::open(&parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync conversation export directory: {error}"))?;
    Ok(target)
}

fn parse_channel(value: &str) -> Option<NotificationChannel> {
    match value {
        "web" => Some(NotificationChannel::Web),
        "desktop" => Some(NotificationChannel::Desktop),
        "ntfy" => Some(NotificationChannel::Ntfy),
        _ => None,
    }
}

fn parse_channel_toggle(value: &str) -> Option<(NotificationChannel, bool)> {
    let mut parts = value.split_whitespace();
    let channel = parse_channel(parts.next()?)?;
    let enabled = match parts.next()? {
        "on" | "enable" | "enabled" => true,
        "off" | "disable" | "disabled" => false,
        _ => return None,
    };
    parts.next().is_none().then_some((channel, enabled))
}

fn parse_channel_severity(value: &str) -> Option<(NotificationChannel, String)> {
    let mut parts = value.split_whitespace();
    let channel = parse_channel(parts.next()?)?;
    let severity = match parts.next()? {
        "info" => "info",
        "warning" | "warn" => "warning",
        "error" => "error",
        "critical" => "critical",
        _ => return None,
    };
    parts
        .next()
        .is_none()
        .then_some((channel, severity.to_string()))
}

fn parse_dnd(value: &str) -> Option<Option<(u16, u16)>> {
    if value == "off" {
        return Some(None);
    }
    let (start, end) = value.split_once('-')?;
    Some(Some((parse_utc_minute(start)?, parse_utc_minute(end)?)))
}

fn parse_utc_minute(value: &str) -> Option<u16> {
    let (hour, minute) = value.split_once(':')?;
    let hour = hour.parse::<u16>().ok()?;
    let minute = minute.parse::<u16>().ok()?;
    (hour < 24 && minute < 60).then_some(hour * 60 + minute)
}

fn parse_required_pair(value: &str) -> Option<(String, String)> {
    let (left, right) = value.split_once('|')?;
    let left = left.trim();
    let right = right.trim();
    (!left.is_empty() && !right.is_empty()).then(|| (left.to_string(), right.to_string()))
}

fn parse_id_and_optional_text(value: &str) -> Option<(String, Option<String>)> {
    let mut parts = value.splitn(2, char::is_whitespace);
    let id = parts.next()?.trim();
    if id.is_empty() {
        return None;
    }
    let prompt = parts
        .next()
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .map(str::to_string);
    Some((id.to_string(), prompt))
}

fn parse_activity_control_set(value: &str, policy: ActivityControlPolicy) -> Option<Command> {
    let (identity, draft) = value.split_once('|')?;
    if draft.len() > 16 * 1024 {
        return None;
    }
    let mut identity = identity.split_whitespace();
    let id = identity.next()?.to_string();
    let expected_revision = parse_revision(identity.next()?)?;
    if identity.next().is_some() {
        return None;
    }
    let draft = serde_json::from_str::<Value>(draft.trim()).ok()?;
    draft.is_object().then_some(Command::ActivityControlSet {
        id,
        policy,
        expected_revision,
        draft,
    })
}

fn parse_activity_control_enabled(value: &str, policy: ActivityControlPolicy) -> Option<Command> {
    let mut parts = value.split_whitespace();
    let id = parts.next()?.to_string();
    let enabled = match parts.next()? {
        "on" | "enable" | "enabled" => true,
        "off" | "disable" | "disabled" => false,
        _ => return None,
    };
    let revision = parts.next()?.parse::<u64>().ok()?;
    (revision > 0 && parts.next().is_none()).then_some(Command::ActivityControlEnabled {
        id,
        policy,
        revision,
        enabled,
    })
}

fn parse_activity_priority(value: &str) -> Option<Command> {
    let mut parts = value.split_whitespace();
    let id = parts.next()?.to_string();
    let priority = match parts.next()? {
        priority @ ("foreground" | "standard" | "background") => priority.to_string(),
        _ => return None,
    };
    let expected_revision = parse_revision(parts.next()?)?;
    parts.next().is_none().then_some(Command::ActivityPriority {
        id,
        priority,
        expected_revision,
    })
}

fn parse_revision(value: &str) -> Option<Option<u64>> {
    if value == "new" {
        return Some(None);
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|revision| *revision > 0)
        .map(Some)
}

fn parse_activity_preview(value: &str) -> Option<Command> {
    let (identity, raw_args) = value
        .split_once('|')
        .map_or((value, None), |(identity, args)| {
            (identity, Some(args.trim()))
        });
    let mut identity = identity.split_whitespace();
    let id = identity.next()?.to_string();
    let app_id = identity.next()?.to_string();
    let operation = identity.next()?.to_string();
    if identity.next().is_some() {
        return None;
    }
    let args = match raw_args {
        None | Some("") => Vec::new(),
        Some(raw) if raw.len() <= 64 * 8_192 => serde_json::from_str::<Vec<String>>(raw).ok()?,
        Some(_) => return None,
    };
    (args.len() <= 64).then_some(Command::ActivityPreview {
        id,
        app_id,
        operation,
        args,
    })
}

pub(super) async fn confirm(
    app: &mut App,
    backend: Arc<dyn Backend>,
    action: ConfirmationAction,
) -> Result<(), String> {
    match action {
        ConfirmationAction::Archive => {
            backend
                .update_conversation(&app.conversation.id, None, Some(true))
                .await?;
            app.conversation.archived = true;
            app.push_system(
                "Conversation archived. Canonical history and task evidence were retained.",
            );
        }
        ConfirmationAction::Rewind(user_turns) => {
            let conversation = backend
                .revert_conversation(&app.conversation.id, user_turns)
                .await?;
            app.replace_conversation(conversation);
            app.push_system("Conversation replay was rewound; external effects were not undone.");
        }
        ConfirmationAction::ActivityComplete { id, note } => {
            let activity = backend
                .transition_activity(&id, "completed", Some(&note))
                .await?;
            app.update_activity(activity);
            app.push_system("Activity explicitly completed with the supplied confirmation note.");
        }
        ConfirmationAction::ActivityCancel { id } => {
            let activity = backend.transition_activity(&id, "cancelled", None).await?;
            app.update_activity(activity);
            app.push_system(
                "Activity cancelled. In-flight tasks and admitted effects were not changed.",
            );
        }
        ConfirmationAction::MemoryReset => {
            app.close_memory_center();
            app.close_platform_overview();
            let report = backend.reset_memories().await?;
            app.push_system(&format!(
                "Reset learned memory: {} note(s), {} App memory row(s), and {} semantic row(s) removed. Conversation history and execution evidence were preserved.",
                report.notes_deleted,
                report.app_memories_deleted,
                report.semantic_rows_deleted,
            ));
        }
        ConfirmationAction::AccountLogout => {
            app.close_account_overview();
            app.close_platform_overview();
            let result = backend.account_logout().await?;
            if app.info.provider == result.provider {
                app.info.provider_ready = false;
            }
            app.push_system(if result.was_present {
                "Copilot credential revoked. Provider configuration and conversation history were retained."
            } else {
                "No Copilot credential was present. Provider configuration and conversation history were retained."
            });
        }
    }
    Ok(())
}
