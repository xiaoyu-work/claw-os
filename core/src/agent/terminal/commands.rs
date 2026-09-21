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
    Session,
    Clear,
    Cancel,
    Quit,
    Unknown(String),
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
        "/resume" | "/rename" | "/rewind" | "/model" | "/task"
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
             /tasks  /task ID  /session\n\
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
    }
    Ok(())
}
