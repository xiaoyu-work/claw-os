//! Fixed native editor activation through the controlled desktop provider.

use crate::BridgeError;
use serde::Deserialize;

pub const EDITOR_ID: &str = "com.clawos.Edit";

/// Open the native editor, optionally with one local file. The broker resolves
/// the path, derives fs.read, and launches in the authenticated owner's desktop.
pub fn open_editor(path: Option<&str>) -> Result<(), BridgeError> {
    let mut args = vec![
        "__desktop".to_string(),
        "launch".into(),
        "--app-id".into(),
        EDITOR_ID.into(),
    ];
    if let Some(path) = path {
        if path.is_empty() || path.contains('\0') {
            return Err(
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid file path").into(),
            );
        }
        let path = std::path::absolute(path)?;
        let path = path.to_str().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "file path must be UTF-8")
        })?;
        // Percent encoding makes spaces, #, %, and non-ASCII filenames data.
        let mut uri = String::from("file://");
        for byte in path.bytes() {
            if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
                uri.push(char::from(byte));
            } else {
                use std::fmt::Write;
                write!(uri, "%{byte:02X}").expect("String formatting cannot fail");
            }
        }
        args.extend(["--uri".into(), uri]);
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Reply {
        launched: bool,
        app_id: String,
        launcher: String,
    }
    let value = claw_os_sdk::cos_call_json("desktop", "launch", args)?;
    let result: Reply = serde_json::from_value(value).map_err(|e| BridgeError::Decode {
        app: "desktop".into(),
        verb: "launch".into(),
        message: e.to_string(),
    })?;
    if !result.launched || result.app_id != EDITOR_ID || result.launcher != "/usr/bin/gtk4-launch" {
        return Err(BridgeError::Decode {
            app: "desktop".into(),
            verb: "launch".into(),
            message: "provider did not confirm native editor launch".into(),
        });
    }
    Ok(())
}
