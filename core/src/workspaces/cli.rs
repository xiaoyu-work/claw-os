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
                "--revision"
                    | "--name"
                    | "--description"
                    | "--context"
                    | "--content"
                    | "--note"
                    | "--limit"
                    | "--offset"
                    | "--max-chars"
                    | "--source-revision"
                    | "--link"
                    | "--source-space"
                    | "--job"
                    | "--conversation"
                    | "--notes"
                    | "--sources"
                    | "--expires"
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
    if matches!(
        command,
        "context" | "read" | "search" | "index" | "read-note" | "index-source"
    ) {
        let second = positional.get(1);
        let request = match command {
            "context" => WorkspaceRead::Context,
            "index" => WorkspaceRead::Index,
            "read-note" => WorkspaceRead::Note {
                note_id: second.ok_or("read-note requires a note UUID")?.clone(),
                link_id: flags.remove("--link"),
            },
            "read" => WorkspaceRead::Source {
                path: second
                    .ok_or("read requires an absolute tagged path")?
                    .clone(),
                link_id: flags.remove("--link"),
                offset: flags
                    .remove("--offset")
                    .unwrap_or_else(|| "0".into())
                    .parse()
                    .map_err(|_| "invalid --offset")?,
                max_chars: flags
                    .remove("--max-chars")
                    .unwrap_or_else(|| "4096".into())
                    .parse()
                    .map_err(|_| "invalid --max-chars")?,
                revision: flags.remove("--source-revision"),
            },
            "search" => WorkspaceRead::Search {
                query: second.ok_or("search requires one quoted query")?.clone(),
                limit: flags
                    .remove("--limit")
                    .unwrap_or_else(|| "10".into())
                    .parse()
                    .map_err(|_| "invalid --limit")?,
            },
            "index-source" => {
                let tag_id = second.ok_or("index-source requires a tag UUID")?;
                let revision: u64 = flags
                    .remove("--revision")
                    .ok_or("index-source requires the displayed --revision")?
                    .parse()
                    .map_err(|_| "invalid --revision")?;
                if positional.len() != 2 || !flags.is_empty() {
                    return Err("unexpected index-source arguments".into());
                }
                return call(
                    Command::WorkspaceIndexSource,
                    json!({"id":id,"tag_id":tag_id,"expected_revision":revision}),
                );
            }
            _ => return Err("unsupported Workspace read".into()),
        };
        let count = if matches!(command, "context" | "index") {
            1
        } else {
            2
        };
        let target = match (flags.remove("--job"), flags.remove("--conversation")) {
            (None, None) => None,
            (Some(id), None) => Some(ReadTarget::Job { id }),
            (None, Some(id)) => Some(ReadTarget::Conversation { id }),
            _ => return Err("select exactly one --job or --conversation".into()),
        };
        if positional.len() != count || !flags.is_empty() {
            return Err("unexpected Workspace read arguments".into());
        }
        let request = ReadRequest {
            id,
            request,
            target,
        };
        request.validate()?;
        return call(
            Command::WorkspaceRead,
            serde_json::to_value(request).map_err(|error| error.to_string())?,
        );
    }
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
        "read-link" => {
            let source_workspace_id = flags
                .remove("--source-space")
                .ok_or("read-link requires --source-space")?;
            let target = match (flags.remove("--job"), flags.remove("--conversation")) {
                (Some(id), None) => ReadTarget::Job { id },
                (None, Some(id)) => ReadTarget::Conversation { id },
                _ => return Err("select exactly one --job or --conversation".into()),
            };
            WorkspaceAction::ReadLink {
                id,
                expected_revision,
                source_workspace_id,
                target,
                note_ids: serde_json::from_str(
                    &flags.remove("--notes").unwrap_or_else(|| "[]".into()),
                )
                .map_err(|e| e.to_string())?,
                sources: serde_json::from_str(
                    &flags.remove("--sources").unwrap_or_else(|| "[]".into()),
                )
                .map_err(|e| e.to_string())?,
                expires_at: flags
                    .remove("--expires")
                    .ok_or("read-link requires --expires RFC3339, within 24 hours")?,
            }
        }
        "revoke" => WorkspaceAction::RevokeLink {
            id,
            expected_revision,
            link_id: second()?,
        },
        _ => {
            return Err(format!(
                "unknown Workspace command: {command}; try cos workspace --help"
            ))
        }
    };
    let expected_positionals = if matches!(
        command,
        "tag" | "untag" | "note" | "forget-note" | "attach" | "detach" | "revoke"
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
