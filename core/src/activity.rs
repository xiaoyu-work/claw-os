//! Terminal presentation of the same Activity broker used by graphical clients.

use serde_json::{json, Value};

use crate::activities::{ActivityDraft, ActivityPatch};
use crate::clawd::{client, config, protocol::Request, routes::Command};

pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    if command == "record-receipt" {
        use std::io::IsTerminal;
        if args.len() != 2 || args[1] != "--stdin" || std::io::stdin().is_terminal() {
            return Err("usage: pipe report JSON to cos activity record-receipt ID --stdin".into());
        }
        crate::activities::validate_id(&args[0]).map_err(|error| error.to_string())?;
        let report = read_receipt(std::io::stdin().lock())?;
        return crate::operations::cli::request(
            Command::ActivityReceiptRecord,
            json!({"id":args[0],"report":report}),
        );
    }
    let (route, params) = parse(command, args)?;
    let response = client::request_blocking(config::socket_path(), Request::build(route, params))?;
    if !response.ok {
        return Err(match response.error {
            Some(error) => format!("{}: {}", error.code, error.message),
            None => "clawd refused the Activity request without an error".to_string(),
        });
    }
    response
        .result
        .ok_or_else(|| "clawd returned no Activity result".to_string())
}

fn parse(command: &str, args: &[String]) -> Result<(Command, Value), String> {
    if command == "attach-object" {
        return parse_object_attachment(args);
    }
    let mut params = json!({});
    let route = match command {
        "create" => Command::ActivityCreate,
        "list" => Command::ActivityList,
        "show" => Command::ActivityGet,
        "update" => Command::ActivityUpdate,
        "run" => Command::ActivityRun,
        "objects" => Command::ActivityObjects,
        "receipts" => Command::ActivityReceipts,
        "pause" | "resume" | "complete" | "cancel" => Command::ActivityTransition,
        other => {
            return Err(format!(
                "unknown Activity command: {other}; try cos activity --help"
            ))
        }
    };
    let mut index = 0;
    if command != "list" {
        let positional = args
            .first()
            .filter(|value| !value.trim().is_empty() && !value.starts_with("--"))
            .ok_or_else(|| {
                format!(
                    "cos activity {command} requires a {}",
                    if command == "create" {
                        "title"
                    } else {
                        "valid Activity ID"
                    }
                )
            })?;
        if command == "create" {
            params["title"] = json!(positional);
        } else {
            crate::activities::validate_id(positional).map_err(|error| error.to_string())?;
            params["id"] = json!(positional);
        }
        index = 1;
    }
    match command {
        "pause" => params["state"] = json!("paused"),
        "resume" => params["state"] = json!("active"),
        "complete" => params["state"] = json!("completed"),
        "cancel" => params["state"] = json!("cancelled"),
        _ => {}
    }

    let mut resources = Vec::new();
    let mut clear_resources = false;
    while index < args.len() {
        let flag = args[index].as_str();
        if command == "run" && !flag.starts_with("--") {
            set(&mut params, "prompt", json!(flag))?;
            index += 1;
            continue;
        }
        if flag == "--clear-resources" && command == "update" {
            if clear_resources || !resources.is_empty() {
                return Err(
                    "--clear-resources cannot be repeated or combined with --resource".into(),
                );
            }
            clear_resources = true;
            index += 1;
            continue;
        }
        let key = match (command, flag) {
            ("create" | "update", "--goal") => "goal",
            ("create" | "update", "--criteria") => "completion_criteria",
            ("create" | "update", "--boundaries") => "boundaries",
            ("create" | "update", "--resource") => "resource",
            ("update", "--title") => "title",
            ("list", "--state") => "state",
            ("list" | "show" | "receipts", "--limit") => "limit",
            ("run", "--session") => "session_id",
            ("run", "--max-turns") => "max_turns",
            ("complete", "--note") => "completion_note",
            _ => {
                return Err(format!(
                    "unsupported argument for cos activity {command}: {flag}"
                ))
            }
        };
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?;
        if key == "resource" {
            if clear_resources {
                return Err("--resource cannot be combined with --clear-resources".into());
            }
            let (label, reference) = value
                .split_once('=')
                .ok_or("--resource requires LABEL=REFERENCE")?;
            resources.push(json!({ "label": label, "reference": reference }));
        } else if matches!(key, "limit" | "max_turns") {
            let count = value
                .parse::<u64>()
                .map_err(|_| format!("{flag} requires a positive integer"))?;
            if count == 0 || (key == "limit" && count > crate::activities::MAX_LIST_LIMIT as u64) {
                return Err(format!(
                    "{flag} must be positive{}",
                    if key == "limit" {
                        " and at most 100"
                    } else {
                        ""
                    }
                ));
            }
            set(&mut params, key, json!(count))?;
        } else {
            set(&mut params, key, json!(value))?;
        }
        index += 2;
    }
    if clear_resources || !resources.is_empty() {
        params["resources"] = json!(resources);
    }
    if command == "complete"
        && !params["completion_note"]
            .as_str()
            .is_some_and(|note| !note.trim().is_empty())
    {
        return Err(
            "cos activity complete requires --note with an explicit completion confirmation".into(),
        );
    }

    // Use the broker's decoder even for local CLI input. Terminal commands
    // cannot accidentally introduce a more permissive request shape.
    let params = (route.route().decode)(params)
        .map_err(|fault| format!("invalid Activity request: {}", fault.message()))?;
    match command {
        "create" => {
            let draft: ActivityDraft =
                serde_json::from_value(params.clone()).map_err(|error| error.to_string())?;
            draft.validate().map_err(|error| error.to_string())?;
        }
        "update" => {
            let mut fields = params.clone();
            fields
                .as_object_mut()
                .ok_or("Activity update must be an object")?
                .remove("id");
            let patch: ActivityPatch =
                serde_json::from_value(fields).map_err(|error| error.to_string())?;
            patch.validate().map_err(|error| error.to_string())?;
        }
        _ => {}
    }
    Ok((route, params))
}

fn set(params: &mut Value, key: &str, value: Value) -> Result<(), String> {
    if params.get(key).is_some() {
        return Err(format!("Activity option {key} may only be specified once"));
    }
    params[key] = value;
    Ok(())
}

fn read_receipt(reader: impl std::io::Read) -> Result<crate::activities::ReceiptReport, String> {
    use std::io::Read;
    const MAX_REPORT_BYTES: u64 = 16 * 1024;
    let mut data = Vec::new();
    reader.take(MAX_REPORT_BYTES + 1).read_to_end(&mut data)
        .map_err(|error| format!("read receipt report: {error}"))?;
    if data.len() as u64 > MAX_REPORT_BYTES {
        return Err("receipt report input exceeds 16 KiB".into());
    }
    let report: crate::activities::ReceiptReport = serde_json::from_slice(&data)
        .map_err(|error| format!("invalid receipt report JSON: {error}"))?;
    report.validate().map_err(|error| error.to_string())?;
    Ok(report)
}

fn parse_object_attachment(args: &[String]) -> Result<(Command, Value), String> {
    let id = args.first().ok_or("attach-object requires an Activity ID")?;
    crate::activities::validate_id(id).map_err(|error| error.to_string())?;
    let mut params = json!({"id": id, "object": {}});
    let mut index = 1;
    while index < args.len() {
        let (flag, inline) = match args[index].split_once('=') {
            Some((flag, value)) => (flag, Some(value)),
            None => (args[index].as_str(), None),
        };
        let key = match flag {
            "--label" => "label",
            "--app" => "app_id",
            "--type" => "object_type",
            "--object-id" => "object_id",
            "--revision" => "revision",
            _ => return Err(format!("unknown attach-object flag: {flag}")),
        };
        let value = match inline {
            Some(value) => value,
            None => args
                .get(index + 1)
                .map(String::as_str)
                .ok_or_else(|| format!("{flag} requires a value"))?,
        };
        if key == "label" {
            set(&mut params, key, json!(value))?;
        } else {
            set(&mut params["object"], key, json!(value))?;
        }
        index += if inline.is_some() { 1 } else { 2 };
    }
    let route = Command::ActivityObjectAttach;
    let params = (route.route().decode)(params)
        .map_err(|error| format!("invalid object attachment: {}", error.message()))?;
    Ok((route, params))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activity.rs"
    ));
}
