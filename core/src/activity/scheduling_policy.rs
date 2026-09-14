//! Terminal controls for shared Activity pending-admission priority.

use serde_json::{json, Value};

use crate::clawd::routes::Command;

pub(super) fn parse(command: &str, args: &[String]) -> Result<(Command, Value), String> {
    let id = args.first().ok_or("an Activity ID is required")?;
    crate::activities::validate_id(id).map_err(|error| error.to_string())?;
    let mut params = json!({"id": id});
    let route = match command {
        "priority" => Command::ActivitySchedulingPolicyGet,
        "set-priority" => Command::ActivitySchedulingPolicySet,
        _ => return Err("unknown Activity scheduling-priority command".into()),
    };
    let mut index = 1;
    while index < args.len() {
        let flag = args[index].as_str();
        match flag {
            "--priority" if command == "set-priority" => {
                let value = args
                    .get(index + 1)
                    .ok_or("--priority requires foreground, standard, or background")?;
                let priority = match value.as_str() {
                    "foreground" | "standard" | "background" => value,
                    _ => {
                        return Err("--priority must be foreground, standard, or background".into())
                    }
                };
                super::set(&mut params, "priority", json!(priority))?;
            }
            "--expected-revision" if command == "set-priority" => {
                let value = args
                    .get(index + 1)
                    .ok_or("--expected-revision requires a positive integer")?;
                let revision = value
                    .parse::<u64>()
                    .map_err(|_| "--expected-revision requires a positive integer")?;
                if revision == 0 {
                    return Err("--expected-revision must be positive".into());
                }
                super::set(&mut params, "expected_revision", json!(revision))?;
            }
            _ => {
                return Err(format!(
                    "unsupported argument for cos activity {command}: {flag}"
                ))
            }
        }
        index += 2;
    }
    let params = (route.route().decode)(params)
        .map_err(|error| format!("invalid Activity scheduling policy: {}", error.message()))?;
    Ok((route, params))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activity/scheduling_policy.rs"
    ));
}
