use serde_json::{json, Value};

/// `cos agent todo [list <session_id>|add <session_id> <id> <title> [--note <text>]|set-status <session_id> <id> <pending|in_progress|completed|cancelled>|remove <session_id> <id>|clear <session_id> --yes|path]`
///
/// Surface for the per-session `TodoStore` (the same store the
/// `cos_todo` LLM tool writes to). Lets operators inspect or
/// hand-edit a session's todo list out-of-band — useful when a
/// long-running session has accumulated state and you want to
/// see/correct it without re-running the agent.
///
/// `clear` requires `--yes` so a typo can't wipe a session's todos.
/// `add` and `remove` are convenience wrappers over read+write
/// (whole-list semantics; concurrent writers will race, just like
/// the on-disk format expects).
pub(super) fn todo_cmd(args: &[String]) -> Result<Value, String> {
    todo_cmd_at(args, &crate::agent::tools::todo::TodoStore::default_store())
}

/// Inner implementation taking an explicit store, so unit tests can
/// point at a tempdir without trampling the live `<data_dir>/agent/todos/`.
fn todo_cmd_at(
    args: &[String],
    store: &crate::agent::tools::todo::TodoStore,
) -> Result<Value, String> {
    use crate::agent::tools::todo::{TodoItem, TodoStatus};

    let sub = args.first().map(|s| s.as_str()).unwrap_or("path");
    match sub {
        "path" => Ok(json!({
            "path": crate::paths::agent_todos_dir().display().to_string(),
        })),
        "list" => {
            let session = args
                .get(1)
                .cloned()
                .ok_or_else(|| "usage: cos agent todo list <session_id>".to_string())?;
            let list = store.read(&session)?;
            let counts = todo_status_counts(&list);
            Ok(json!({
                "session_id": session,
                "count": list.items.len(),
                "by_status": counts,
                "items": list.items,
            }))
        }
        "add" => {
            let session = args
                .get(1)
                .cloned()
                .ok_or_else(|| "usage: cos agent todo add <session_id> <id> <title> [--note <text>]".to_string())?;
            let id = args
                .get(2)
                .cloned()
                .ok_or_else(|| "todo add: id required".to_string())?;
            // Title can have spaces; collect non-flag positionals after id and join.
            let mut note: Option<String> = None;
            let mut positional: Vec<String> = Vec::new();
            let mut i = 3usize;
            while i < args.len() {
                if args[i].as_str() == "--note" {
                    note = Some(
                        args.get(i + 1)
                            .cloned()
                            .ok_or_else(|| "--note needs a value".to_string())?,
                    );
                    i += 2;
                } else {
                    positional.push(args[i].clone());
                    i += 1;
                }
            }
            if positional.is_empty() {
                return Err("todo add: title required".into());
            }
            let title = positional.join(" ");

            let mut list = store.read(&session)?;
            if list.items.iter().any(|item| item.id == id) {
                return Err(format!("todo id already exists: {id}"));
            }
            list.items.push(TodoItem {
                id: id.clone(),
                title,
                status: TodoStatus::default(),
                note,
            });
            store.write(&session, &list)?;
            Ok(json!({
                "session_id": session,
                "added": id,
                "count": list.items.len(),
            }))
        }
        "set-status" | "set_status" => {
            let session = args
                .get(1)
                .cloned()
                .ok_or_else(|| "usage: cos agent todo set-status <session_id> <id> <status>".to_string())?;
            let id = args
                .get(2)
                .cloned()
                .ok_or_else(|| "todo set-status: id required".to_string())?;
            let status_raw = args
                .get(3)
                .cloned()
                .ok_or_else(|| "todo set-status: status required".to_string())?;
            let status = parse_todo_status(&status_raw)?;
            let updated = store.set_status(&session, &id, status)?;
            Ok(json!({
                "session_id": session,
                "id": id,
                "status": status.as_str(),
                "items": updated.items,
            }))
        }
        "remove" => {
            let session = args
                .get(1)
                .cloned()
                .ok_or_else(|| "usage: cos agent todo remove <session_id> <id>".to_string())?;
            let id = args
                .get(2)
                .cloned()
                .ok_or_else(|| "todo remove: id required".to_string())?;
            let mut list = store.read(&session)?;
            let before = list.items.len();
            list.items.retain(|item| item.id != id);
            if list.items.len() == before {
                return Err(format!("todo id not found: {id}"));
            }
            store.write(&session, &list)?;
            Ok(json!({
                "session_id": session,
                "removed": id,
                "count": list.items.len(),
            }))
        }
        "clear" => {
            let session = args
                .get(1)
                .cloned()
                .ok_or_else(|| "usage: cos agent todo clear <session_id> --yes".to_string())?;
            let confirmed = args.iter().skip(2).any(|a| a == "--yes");
            if !confirmed {
                return Err("refusing to clear without --yes".into());
            }
            store.clear(&session)?;
            Ok(json!({
                "session_id": session,
                "cleared": true,
            }))
        }
        other => Err(format!(
            "unknown todo subcommand: {other}. try: list | add | set-status | remove | clear --yes | path"
        )),
    }
}

fn todo_status_counts(list: &crate::agent::tools::todo::TodoList) -> serde_json::Value {
    use crate::agent::tools::todo::TodoStatus;
    let mut pending = 0u64;
    let mut in_progress = 0u64;
    let mut completed = 0u64;
    let mut cancelled = 0u64;
    for item in &list.items {
        match item.status {
            TodoStatus::Pending => pending += 1,
            TodoStatus::InProgress => in_progress += 1,
            TodoStatus::Completed => completed += 1,
            TodoStatus::Cancelled => cancelled += 1,
        }
    }
    json!({
        "pending": pending,
        "in_progress": in_progress,
        "completed": completed,
        "cancelled": cancelled,
    })
}

fn parse_todo_status(raw: &str) -> Result<crate::agent::tools::todo::TodoStatus, String> {
    use crate::agent::tools::todo::TodoStatus;
    match raw {
        "pending" => Ok(TodoStatus::Pending),
        "in_progress" | "in-progress" => Ok(TodoStatus::InProgress),
        "completed" | "done" => Ok(TodoStatus::Completed),
        "cancelled" | "canceled" => Ok(TodoStatus::Cancelled),
        other => Err(format!(
            "unknown todo status: {other}. try: pending | in_progress | completed | cancelled"
        )),
    }
}

/// `cos agent nudge [list|due|add <due_in_secs> <message> [--repeat <secs>] [--tag <tag>]|fire <id>|remove <id>|path]`
/// — managed periodic-nudge store. `list` shows all nudges; `due`
/// shows only those with `due_at_epoch_s <= now`. `add` parses a
/// relative offset in seconds (the most common case for "remind me
/// in 30 minutes"); `fire` advances repeating nudges or deletes
/// one-shots.
pub(super) fn nudge_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::nudge::{now_epoch_s, Nudge, NudgeStore};
    let store = NudgeStore::new(crate::paths::agent_nudges_path());
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "path" => Ok(json!({
            "path": crate::paths::agent_nudges_path().display().to_string(),
        })),
        "list" | "" => {
            let mut all = store.list();
            all.sort_by_key(|n| n.due_at_epoch_s);
            Ok(json!({
                "path": crate::paths::agent_nudges_path().display().to_string(),
                "n": all.len(),
                "nudges": all,
            }))
        }
        "due" => {
            let now = now_epoch_s();
            let mut due = store.due(now);
            due.sort_by_key(|n| n.due_at_epoch_s);
            Ok(json!({
                "now": now,
                "n": due.len(),
                "nudges": due,
            }))
        }
        "add" => {
            let due_in: i64 = args
                .get(1)
                .ok_or_else(|| "usage: cos agent nudge add <due_in_secs> <message> [--repeat <secs>] [--tag <tag>]".to_string())?
                .parse()
                .map_err(|e| format!("due_in_secs must be integer: {e}"))?;
            let message = args
                .get(2)
                .cloned()
                .filter(|m| !m.is_empty())
                .ok_or_else(|| "usage: cos agent nudge add <due_in_secs> <message> [--repeat <secs>] [--tag <tag>]".to_string())?;
            let mut repeat_secs: Option<u64> = None;
            let mut tag: Option<String> = None;
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--repeat" => {
                        repeat_secs = Some(
                            args.get(i + 1)
                                .ok_or_else(|| "--repeat needs <secs>".to_string())?
                                .parse()
                                .map_err(|e| format!("--repeat secs invalid: {e}"))?,
                        );
                        i += 2;
                    }
                    "--tag" => {
                        tag = Some(
                            args.get(i + 1)
                                .cloned()
                                .ok_or_else(|| "--tag needs <value>".to_string())?,
                        );
                        i += 2;
                    }
                    other => return Err(format!("unknown flag: {other}")),
                }
            }
            let now = now_epoch_s();
            let due_at = if due_in >= 0 {
                now.saturating_add(due_in as u64)
            } else {
                now.saturating_sub((-due_in) as u64)
            };
            let nudge = Nudge {
                id: String::new(),
                message,
                due_at_epoch_s: due_at,
                repeat_secs,
                tag,
                last_fired_epoch_s: None,
            };
            let id = store
                .add(nudge)
                .map_err(|e| format!("add failed: {e}"))?;
            Ok(json!({
                "id": id,
                "due_at_epoch_s": due_at,
            }))
        }
        "fire" => {
            let id = args
                .get(1)
                .cloned()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "usage: cos agent nudge fire <id>".to_string())?;
            let updated = store
                .fire(&id, now_epoch_s())
                .map_err(|e| format!("fire failed: {e}"))?;
            Ok(json!({ "id": id, "updated": updated }))
        }
        "remove" => {
            let id = args
                .get(1)
                .cloned()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "usage: cos agent nudge remove <id>".to_string())?;
            let removed = store
                .remove(&id)
                .map_err(|e| format!("remove failed: {e}"))?;
            Ok(json!({ "id": id, "removed": removed }))
        }
        other => Err(format!(
            "unknown nudge subcommand: {other}. try: list | due | add <due_in_secs> <message> [--repeat <secs>] [--tag <tag>] | fire <id> | remove <id> | path"
        )),
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/task_commands.rs"
    ));
}
