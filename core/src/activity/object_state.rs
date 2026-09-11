//! Terminal metadata submission uses the same bounded object-state contract.

use std::io::{IsTerminal, Read};

use serde_json::{json, Value};

use crate::activities::ObjectStateDraft;
use crate::clawd::routes::Command;

pub(super) fn parse(command: &str, args: &[String]) -> Result<(Command, Value), String> {
    let activity = args.first().ok_or("an Activity ID is required")?;
    crate::activities::validate_id(activity).map_err(|error| error.to_string())?;
    let mut fields = json!({});
    let mut index = 1;
    while index < args.len() {
        let (flag, inline) = args[index]
            .split_once('=')
            .map_or((args[index].as_str(), None), |(flag, value)| {
                (flag, Some(value))
            });
        let key = match (command, flag) {
            (_, "--id") => "id",
            (_, "--reference") => "reference",
            (_, "--supersedes") => "supersedes",
            ("observe", "--source") => "source",
            ("observe", "--text") => "text",
            ("observe", "--receipt") => "receipt_id",
            ("observe", "--observed-at") => "observed_at",
            ("observe", "--valid-until") => "valid_until",
            ("relate", "--target") => "target",
            ("relate", "--relation") => "relation",
            ("relate", "--note") => "note",
            ("retract-object-state", "--reason") => "reason",
            _ => {
                return Err(format!(
                    "unsupported argument for cos activity {command}: {flag}"
                ))
            }
        };
        let value = inline
            .or_else(|| args.get(index + 1).map(String::as_str))
            .ok_or_else(|| format!("{flag} requires a value"))?;
        super::set(&mut fields, key, json!(value))?;
        index += if inline.is_some() { 1 } else { 2 };
    }
    let content = match command {
        "observe" if fields.get("receipt_id").is_some() => {
            if fields.get("text").is_some() || fields.get("source").is_some() {
                return Err("--receipt cannot be combined with --text or --source".into());
            }
            json!({"kind":"app_report","receipt_id":fields["receipt_id"]})
        }
        "observe" => {
            let source = fields
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("user_statement");
            if !matches!(source, "user_statement" | "agent_inference") {
                return Err(
                    "--source must be user_statement or agent_inference; App reports use --receipt"
                        .into(),
                );
            }
            json!({"kind":source,"text":fields["text"]})
        }
        "relate" => json!({
            "kind":"relation","relation":fields["relation"],"target":fields["target"],
            "note":fields.get("note").cloned().unwrap_or_else(|| json!("")),
        }),
        "retract-object-state" => json!({"kind":"retracted","reason":fields["reason"]}),
        _ => return Err("unknown object-state mutation".into()),
    };
    let entry = json!({
        "id":fields.get("id").cloned().unwrap_or_else(|| json!(uuid::Uuid::new_v4().to_string())),
        "reference":fields["reference"],"content":content,
        "observed_at":fields["observed_at"],"valid_until":fields["valid_until"],
        "supersedes":fields["supersedes"],
    });
    let route = Command::ActivityObjectStateRecord;
    let params = (route.route().decode)(json!({"id":activity,"entry":entry}))
        .map_err(|error| format!("invalid object-state entry: {}", error.message()))?;
    Ok((route, params))
}

pub(super) fn submit(params: Value) -> Result<Value, String> {
    let id = params
        .pointer("/entry/id")
        .and_then(Value::as_str)
        .ok_or("object-state entry ID is missing")?
        .to_string();
    crate::operations::cli::request(Command::ActivityObjectStateRecord, params)
        .map_err(|error| format!("{error}\nIf retrying this metadata submission, keep entry ID {id}; do not create another entry."))
}

pub(super) fn from_stdin(args: &[String]) -> Result<Value, String> {
    if args.len() != 2 || args[1] != "--stdin" || std::io::stdin().is_terminal() {
        return Err("usage: pipe entry JSON to cos activity record-object-state ID --stdin".into());
    }
    crate::activities::validate_id(&args[0]).map_err(|error| error.to_string())?;
    let entry = read(std::io::stdin().lock())?;
    submit(json!({"id":args[0],"entry":entry}))
}

fn read(reader: impl Read) -> Result<ObjectStateDraft, String> {
    const MAX: u64 = 16 * 1024;
    let mut bytes = Vec::new();
    reader
        .take(MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read object-state entry: {error}"))?;
    if bytes.len() as u64 > MAX {
        return Err("object-state entry exceeds 16 KiB".into());
    }
    let draft: ObjectStateDraft = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid object-state JSON: {error}"))?;
    draft.validate().map_err(|error| error.to_string())?;
    Ok(draft)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activity/object_state.rs"
    ));
}
