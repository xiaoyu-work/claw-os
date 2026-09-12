//! Typed terminal controls for finite Activity execution, never authority.

use serde_json::{json, Value};

use crate::clawd::routes::Command;

pub(super) fn parse(command: &str, args: &[String]) -> Result<(Command, Value), String> {
    let id = args.first().ok_or("an Activity ID is required")?;
    crate::activities::validate_id(id).map_err(|error| error.to_string())?;
    let mut params = json!({"id":id});
    let setting = command == "set-execution-limits";
    if setting {
        params["limits"] = json!({});
    }
    let mut index = 1;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag {
            "--revision" if command != "execution-limits" => {
                let revision = value
                    .parse::<u64>()
                    .map_err(|_| "--revision requires a positive integer")?;
                if revision == 0 {
                    return Err("--revision must be positive".into());
                }
                super::set(&mut params, "expected_revision", json!(revision))?;
            }
            "--max-attempts" | "--max-turns" if setting => {
                let count = value
                    .parse::<u32>()
                    .map_err(|_| format!("{flag} requires a positive integer"))?;
                let key = if flag == "--max-attempts" {
                    "max_attempts"
                } else {
                    "max_turns_per_attempt"
                };
                super::set(&mut params["limits"], key, json!(count))?;
            }
            "--expires-at" if setting => {
                super::set(&mut params["limits"], "expires_at", json!(value))?;
            }
            _ => {
                return Err(format!(
                    "unsupported argument for cos activity {command}: {flag}"
                ))
            }
        }
        index += 2;
    }
    let route = match command {
        "execution-limits" => Command::ActivityExecutionLimitsGet,
        "set-execution-limits" => Command::ActivityExecutionLimitsSet,
        "enable-execution-limits" | "disable-execution-limits" => {
            params["enabled"] = json!(command == "enable-execution-limits");
            Command::ActivityExecutionLimitsEnabled
        }
        _ => return Err("unknown Activity execution-limit command".into()),
    };
    let params = (route.route().decode)(params)
        .map_err(|error| format!("invalid Activity execution limits: {}", error.message()))?;
    Ok((route, params))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activity/execution_limits.rs"
    ));
}
