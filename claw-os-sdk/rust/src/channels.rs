use serde_json::{json, Value};

use crate::BridgeError;

/// No owner, provider, recipient, credentials or task authority can be supplied.
pub fn control(
    action: &str,
    event_id: Option<&str>,
    text: Option<&str>,
    confirm: Option<bool>,
) -> Result<Value, BridgeError> {
    let input =
        json!({"action":action,"event_id":event_id,"text":text,"confirm":confirm}).to_string();
    if input.len() > 20 * 1024 {
        return Err(BridgeError::Decode {
            app: "__channel".into(),
            verb: "control".into(),
            message: "channel control exceeds 20 KiB".into(),
        });
    }
    crate::cos_call_json_with_stdin(
        "__channel",
        "control",
        std::iter::empty::<&str>(),
        input.as_bytes(),
    )
}
