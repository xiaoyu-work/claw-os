//! Server-Sent Events helpers.

use serde::Serialize;

pub fn encode_event<T: Serialize>(name: &str, payload: &T) -> String {
    let json = serde_json::to_string(payload).unwrap_or_else(|_| "{}".into());
    let safe = json.replace('\n', "\\n");
    format!("event: {name}\ndata: {safe}\n\n")
}

pub fn encode_comment(text: &str) -> String {
    format!(": {text}\n\n")
}
