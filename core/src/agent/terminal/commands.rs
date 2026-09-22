use std::sync::Arc;

use super::backend::Backend;
use super::state::{App, ConfirmationAction, RunStatus};

pub(super) const COMMANDS: &[(&str, &str)] = &[
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
    ("/activities", "browse persistent Activity goals"),
    ("/activity", "open an Activity by id"),
    ("/activity-create", "create TITLE | GOAL"),
    ("/activity-run", "run an active Activity"),
    ("/activity-pause", "pause future Activity work"),
    ("/activity-resume", "resume or reopen an Activity"),
    ("/activity-complete", "complete ID | CONFIRMATION NOTE"),
    ("/activity-cancel", "cancel an Activity goal"),
    ("/activity-attention", "show Activity attention"),
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
    ActivityCreate { title: String, goal: String },
    ActivityRun { id: String, prompt: Option<String> },
    ActivityPause(String),
    ActivityResume(String),
    ActivityComplete { id: String, note: String },
    ActivityCancel(String),
    ActivityAttention(String),
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
        "activity" => Command::Activity(rest.to_string()),
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
    COMMANDS
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
             /fork  /rewind N  /models  /model ID  /skills\n\
             /tasks  /task ID  /approvals  /approval ID\n\
             /inbox [all]  /notification ID  /notify-settings\n\
             /notify-channel CHANNEL on|off  /notify-severity CHANNEL LEVEL\n\
             /dnd off|HH:MM-HH:MM  /session\n\
             /activities [STATE]  /activity ID  /activity-create TITLE | GOAL\n\
             /activity-run ID [PROMPT]  /activity-pause ID  /activity-resume ID\n\
             /activity-complete ID | NOTE  /activity-cancel ID\n\
             /activity-attention ID\n\
             /clear  /cancel  /quit\n\
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
            let conversation = backend.fork_conversation(&app.conversation.id).await?;
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
            let job = backend.run_activity(&id, prompt.as_deref()).await?;
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
        Command::Session => app.push_system(&format!(
            "session: {}\nmodel: {}\nprovider: {}",
            app.conversation.id, app.selected_model, app.info.provider
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
    }
    Ok(())
}
