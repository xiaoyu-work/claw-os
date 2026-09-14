//! Typed terminal controls for Activity policy-priced monetary budgets.

use serde_json::{json, Value};

use crate::clawd::routes::Command;

pub(super) fn parse(command: &str, args: &[String]) -> Result<(Command, Value), String> {
    let id = args.first().ok_or("an Activity ID is required")?;
    crate::activities::validate_id(id).map_err(|error| error.to_string())?;
    let setting = command == "set-monetary-budget";
    let mut params = json!({"id":id});
    if setting {
        params["budget"] = json!({"currency":"USD"});
    }
    let mut index = 1;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag {
            "--revision" if command != "monetary-budget" => {
                let revision = positive_u64(flag, value)?;
                super::set(&mut params, "expected_revision", json!(revision))?;
            }
            "--max-total-microusd" if setting => {
                super::set(
                    &mut params["budget"],
                    "max_total_microusd",
                    json!(positive_u64(flag, value)?),
                )?;
            }
            "--input-microusd-per-million-tokens" if setting => {
                super::set(
                    &mut params["budget"],
                    "input_microusd_per_million_tokens",
                    json!(positive_u64(flag, value)?),
                )?;
            }
            "--output-microusd-per-million-tokens" if setting => {
                super::set(
                    &mut params["budget"],
                    "output_microusd_per_million_tokens",
                    json!(positive_u64(flag, value)?),
                )?;
            }
            "--max-output-tokens-per-turn" if setting => {
                let tokens = value
                    .parse::<u32>()
                    .map_err(|_| format!("{flag} requires a positive integer"))?;
                if tokens == 0 {
                    return Err(format!("{flag} must be positive"));
                }
                super::set(
                    &mut params["budget"],
                    "max_output_tokens_per_turn",
                    json!(tokens),
                )?;
            }
            "--currency" if setting => {
                super::set(&mut params["budget"], "currency", json!(value))?;
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
        "monetary-budget" => Command::ActivityMonetaryBudgetGet,
        "set-monetary-budget" => Command::ActivityMonetaryBudgetSet,
        "enable-monetary-budget" | "disable-monetary-budget" => {
            params["enabled"] = json!(command == "enable-monetary-budget");
            Command::ActivityMonetaryBudgetEnabled
        }
        _ => return Err("unknown Activity monetary-budget command".into()),
    };
    let params = (route.route().decode)(params)
        .map_err(|error| format!("invalid Activity monetary budget: {}", error.message()))?;
    Ok((route, params))
}

fn positive_u64(flag: &str, value: &str) -> Result<u64, String> {
    let value = value
        .parse::<u64>()
        .map_err(|_| format!("{flag} requires a positive integer"))?;
    if value == 0 {
        return Err(format!("{flag} must be positive"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activity/monetary_budget.rs"
    ));
}
