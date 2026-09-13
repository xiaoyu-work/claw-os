//! Bounded terminal presentation of the shared Activity capability policy.

use serde_json::{json, Value};

use crate::clawd::routes::Command;

pub(super) fn parse(command: &str, args: &[String]) -> Result<(Command, Value), String> {
    let id = args.first().ok_or("an Activity ID is required")?;
    crate::activities::validate_id(id).map_err(|error| error.to_string())?;
    let mut params = json!({"id": id});
    let route = match command {
        "capability-policy" => Command::ActivityCapabilityPolicyGet,
        "set-capability-policy" => Command::ActivityCapabilityPolicySet,
        "enable-capability-policy" | "disable-capability-policy" => {
            params["enabled"] = json!(command == "enable-capability-policy");
            Command::ActivityCapabilityPolicyEnabled
        }
        _ => return Err("unknown Activity capability-policy command".into()),
    };
    let mut index = 1;
    while index < args.len() {
        let flag = args[index].as_str();
        match flag {
            "--expected-revision" if command != "capability-policy" => {
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
            "--policy" if command == "set-capability-policy" => {
                let value = args.get(index + 1).ok_or("--policy requires draft JSON")?;
                if value.len() > 16 * 1024 {
                    return Err("Activity capability-policy draft exceeds 16 KiB".into());
                }
                let policy: Value = serde_json::from_str(value)
                    .map_err(|error| format!("invalid capability-policy JSON: {error}"))?;
                super::set(&mut params, "policy", policy)?;
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
        .map_err(|error| format!("invalid Activity capability policy: {}", error.message()))?;
    Ok((route, params))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activity/capability_policy.rs"
    ));
}
