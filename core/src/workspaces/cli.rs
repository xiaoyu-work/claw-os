//! Human CLI controls for the shared Workspace service.

use super::*;
use crate::clawd::{client, config, protocol::Request, routes::Command};
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn call(route: Command, params: Value) -> Result<Value, String> {
    let response = client::request_blocking(config::socket_path(), Request::build(route, params))?;
    if !response.ok {
        return Err(response
            .error
            .map(|e| format!("{}: {}", e.code, e.message))
            .ok_or("Workspace broker refused the request without an error")?);
    }
    response
        .result
        .ok_or_else(|| "Workspace broker returned no result".into())
}

pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    if command == "list" {
        if !args.is_empty() {
            return Err("usage: cos workspace list".into());
        }
        return call(Command::WorkspaceList, json!({}));
    }
    if command == "show" {
        if args.len() != 1 {
            return Err("usage: cos workspace show ID".into());
        }
        validate_id(&args[0])?;
        return call(Command::WorkspaceGet, json!({"id":args[0]}));
    }
    let mut positional = Vec::new();
    let mut flags = BTreeMap::<String, String>::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--confirm" {
            if flags.insert(arg.clone(), "true".into()).is_some() {
                return Err("duplicate --confirm".into());
            }
            index += 1;
        } else if arg.starts_with("--") {
            if !matches!(
                arg.as_str(),
                "--revision" | "--name" | "--description" | "--context" | "--content" | "--note"
            ) {
                return Err(format!("unknown Workspace flag: {arg}"));
            }
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("{arg} requires a value"))?;
            if flags.insert(arg.clone(), value.clone()).is_some() {
                return Err(format!("duplicate {arg}"));
            }
            index += 2;
        } else {
            positional.push(arg.clone());
            index += 1;
        }
    }
    let first = positional
        .first()
        .ok_or("Workspace command requires a name or identifier")?;
    if command == "create" {
        if positional.len() != 1
            || flags
                .keys()
                .any(|k| !matches!(k.as_str(), "--description" | "--context"))
        {
            return Err(
                "usage: cos workspace create NAME [--description TEXT] [--context TEXT]".into(),
            );
        }
        let action = WorkspaceAction::Create {
            draft: WorkspaceDraft {
                name: first.clone(),
                description: flags.remove("--description").unwrap_or_default(),
                context: flags.remove("--context").unwrap_or_default(),
            },
        };
        action.validate()?;
        return call(Command::WorkspaceControl, json!({"request":action}));
    }
    validate_id(first)?;
    let id = first.clone();
    let expected_revision: u64 = flags
        .remove("--revision")
        .ok_or("supply --revision from cos workspace show ID")?
        .parse()
        .map_err(|_| "--revision must be a positive integer")?;
    let second = || {
        positional
            .get(1)
            .cloned()
            .ok_or("Workspace command requires a second argument")
    };
    let action = match command {
        "update" => {
            let current: WorkspaceView =
                serde_json::from_value(call(Command::WorkspaceGet, json!({"id":id}))?)
                    .map_err(|e| e.to_string())?;
            WorkspaceAction::Update {
                id,
                expected_revision,
                draft: WorkspaceDraft {
                    name: flags.remove("--name").unwrap_or(current.workspace.name),
                    description: flags
                        .remove("--description")
                        .unwrap_or(current.workspace.description),
                    context: flags
                        .remove("--context")
                        .unwrap_or(current.workspace.context),
                },
            }
        }
        "archive" | "restore" => WorkspaceAction::Archive {
            id,
            expected_revision,
            archived: command == "archive",
        },
        "delete" => WorkspaceAction::Delete {
            id,
            expected_revision,
            confirm: flags.remove("--confirm").is_some(),
        },
        "tag" => WorkspaceAction::Tag {
            id,
            expected_revision,
            path: second()?,
            kind: None,
        },
        "untag" => WorkspaceAction::Untag {
            id,
            expected_revision,
            tag_id: second()?,
        },
        "note" => WorkspaceAction::PutNote {
            id,
            expected_revision,
            note_id: flags.remove("--note"),
            note: NoteDraft {
                title: second()?,
                content: flags.remove("--content").ok_or("note requires --content")?,
            },
        },
        "forget-note" => WorkspaceAction::ForgetNote {
            id,
            expected_revision,
            note_id: second()?,
        },
        "attach" => WorkspaceAction::BindActivity {
            id,
            expected_revision,
            activity_id: second()?,
        },
        "detach" => WorkspaceAction::UnbindActivity {
            id,
            expected_revision,
            activity_id: second()?,
        },
        _ => {
            return Err(format!(
                "unknown Workspace command: {command}; try cos workspace --help"
            ))
        }
    };
    let expected_positionals = if matches!(
        command,
        "tag" | "untag" | "note" | "forget-note" | "attach" | "detach"
    ) {
        2
    } else {
        1
    };
    if positional.len() != expected_positionals || !flags.is_empty() {
        return Err("unexpected Workspace arguments".into());
    }
    action.validate()?;
    call(Command::WorkspaceControl, json!({"request":action}))
}
