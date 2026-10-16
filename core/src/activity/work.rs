//! Human controls for the shared, explicitly enabled Activity work loop.

use crate::clawd::routes::Command;
use serde_json::{json, Value};

pub(super) fn parse(command: &str, args: &[String]) -> Result<(Command, Value), String> {
    let id = args.first().ok_or("an Activity ID is required")?;
    crate::activities::validate_id(id).map_err(|error| error.to_string())?;
    let mut params = json!({"id":id});
    let mut draft = json!({"check_interval_secs":3600,"wake_event":null});
    let mut source = None;
    let mut event_type = None;
    let mut seen = std::collections::BTreeSet::new();
    let mut index = 1;
    while let Some(flag) = args.get(index) {
        if !seen.insert(flag.as_str()) {
            return Err(format!("repeated work flag: {flag}"));
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match (command, flag.as_str()) {
            ("set-work", "--task") => draft["task"] = json!(value),
            ("set-work", "--check-interval-secs") => {
                draft["check_interval_secs"] =
                    json!(value.parse::<u32>().map_err(|_| "invalid work interval")?);
            }
            ("set-work", "--wake-source") => source = Some(value.clone()),
            ("set-work", "--wake-event") => event_type = Some(value.clone()),
            ("start-work", "--next-task") => params["next_task"] = json!(value),
            ("start-work", "--review-note") => params["review_note"] = json!(value),
            ("set-work" | "start-work" | "stop-work", "--expected-revision") => {
                let revision = value.parse::<u64>().map_err(|_| "invalid work revision")?;
                if revision == 0 {
                    return Err("work revision must be positive".into());
                }
                params["expected_revision"] = json!(revision);
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
        "work" => Command::ActivityWorkGet,
        "set-work" => {
            match (source, event_type) {
                (Some(source), Some(event_type)) => {
                    draft["wake_event"] = json!({"source":source,"event_type":event_type})
                }
                (None, None) => {}
                _ => return Err("supply both --wake-source and --wake-event".into()),
            }
            let work: crate::activities::ActivityWorkDraft =
                serde_json::from_value(draft).map_err(|error| error.to_string())?;
            work.validate().map_err(|error| error.to_string())?;
            params["work"] = serde_json::to_value(work).map_err(|error| error.to_string())?;
            Command::ActivityWorkSet
        }
        "start-work" | "stop-work" => {
            if params.get("expected_revision").is_none() {
                return Err("starting or stopping work requires --expected-revision".into());
            }
            params["enabled"] = json!(command == "start-work");
            Command::ActivityWorkEnabled
        }
        _ => return Err("unknown Activity work command".into()),
    };
    let params = (route.route().decode)(params)
        .map_err(|error| format!("invalid Activity work request: {}", error.message()))?;
    Ok((route, params))
}
