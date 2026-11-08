use serde_json::{json, Value};

use crate::BridgeError;

/// Report App-owned data; the live OS session supplies identity and authority.
pub fn publish(
    event_type: &str,
    key: &str,
    payload: Option<Value>,
    entity_id: Option<&str>,
) -> Result<Value, BridgeError> {
    let input = json!({
        "event_type":event_type, "key":key, "payload":payload, "entity_id":entity_id,
    })
    .to_string();
    if input.len() > 20 * 1024 {
        return Err(BridgeError::Decode {
            app: "__app_event".into(),
            verb: "publish".into(),
            message: "App event request exceeds 20 KiB".into(),
        });
    }
    crate::cos_call_json_with_stdin(
        "__app_event",
        "publish",
        std::iter::empty::<&str>(),
        input.as_bytes(),
    )
}
