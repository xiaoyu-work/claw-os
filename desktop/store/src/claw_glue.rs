//! Thin desktop-side glue around [`cos_runtime`] so the COSMIC App
//! Store fork can route user-intent fs / exec calls through
//! `cos app <name> <verb>` without each call-site re-deriving error
//! handling, base64 decoding, or denial-message formatting.
//!
//! Only **user-intent** mutations and process launches belong here:
//!
//! * installing / uninstalling packages (which transitively reads the
//!   `.flatpakref` the user picked, removes per-app data, etc.)
//! * launching the user's chosen executable
//!
//! Internal caches (AppStream metadata, search index, icon cache,
//! `/pkg` directory scans) deliberately bypass this module — they
//! aren't user actions and don't deserve a permission prompt every
//! time the store boots. Those sites are marked `FIXME(claw)` in
//! their respective files.

use std::path::Path;

use base64::Engine as _;
use cos_runtime::{ask_claw, BridgeError, call, exec, fs as bridge_fs};
use serde::Serialize;

/// Format a [`BridgeError`] for end-user display. Permission denials
/// from the kernel get a friendlier blurb that points the user back
/// to the Agent-mediated approval flow; everything else falls back to
/// the bridge's own `Display`.
pub fn user_message(err: &BridgeError) -> String {
    if err.is_denied() {
        format!(
            "This action requires permission ({err}). \
             Ask the Agent to explain and approve the request in context."
        )
    } else {
        err.to_string()
    }
}

/// Read a (possibly binary) file via `cos app fs read_bytes`,
/// base64-decoding the response server-side.
///
/// [`cos_runtime::fs::read`] can't be used here because that verb is
/// UTF-8 only — binary content comes back through `errors=replace`
/// which is lossy. The `read_bytes` verb is base64 over the wire and
/// pages at the kernel's binary read limit, so we loop on
/// `truncated=true` until the whole file has been streamed in. This
/// matches the semantics of `std::fs::read`.
pub fn read_bytes(path: &Path) -> Result<Vec<u8>, BridgeError> {
    let path_str = path.to_string_lossy().into_owned();
    let mut out = Vec::new();
    let mut offset: u64 = 0;
    loop {
        let offset_str = offset.to_string();
        let v = call(
            "fs",
            "read_bytes",
            [path_str.as_str(), "--offset", offset_str.as_str()],
            None,
        )?;

        let b64 =
            v.get("base64")
                .and_then(|x| x.as_str())
                .ok_or_else(|| BridgeError::Decode {
                    app: "fs".into(),
                    verb: "read_bytes".into(),
                    message: format!("missing base64 field in response: {v}"),
                })?;
        let chunk = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| BridgeError::Decode {
                app: "fs".into(),
                verb: "read_bytes".into(),
                message: format!("base64 decode: {e}"),
            })?;
        let bytes_returned = v
            .get("bytes_returned")
            .and_then(|x| x.as_u64())
            .unwrap_or(chunk.len() as u64);
        out.extend_from_slice(&chunk);
        offset = offset.saturating_add(bytes_returned);

        let truncated = v
            .get("truncated")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        if !truncated {
            break;
        }
        // Guard against a buggy/empty page: if the server still claims
        // `truncated` but returned no bytes, the offset never advances and
        // this loop would spin forever. Bail out instead of hanging.
        if bytes_returned == 0 {
            return Err(BridgeError::Decode {
                app: "fs".into(),
                verb: "read_bytes".into(),
                message: format!(
                    "server reported truncated read but returned 0 bytes at offset {offset}"
                ),
            });
        }
    }
    Ok(out)
}

/// Remove a file or directory tree through `cos app fs rm`. The
/// kernel-side handler already recurses for directories (see
/// `apps/fs/main.py::cmd_rm`), so no extra flag is needed.
pub fn fs_rm(path: &Path) -> Result<(), BridgeError> {
    bridge_fs::rm(path.to_string_lossy().as_ref()).map(|_| ())
}

/// Spawn `argv` via `cos app exec start`. The returned opaque launch id,
/// PID, start time, and command identify the child tracked in the registry.
pub fn exec_start(argv: &[&str]) -> Result<exec::LaunchHandle, BridgeError> {
    exec::start(argv)
}

#[derive(Serialize)]
struct StoreViewContext<'a> {
    view: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    app_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
}

impl ask_claw::Context for StoreViewContext<'_> {
    const APP_ID: &'static str = "cosmic-store";
}

#[derive(Serialize)]
struct StoreSearchContext<'a> {
    mode: &'static str,
    query: &'a str,
}

impl ask_claw::Context for StoreSearchContext<'_> {
    const APP_ID: &'static str = "cosmic-store";
}

pub fn ask_claw_home() -> Result<(), ask_claw::LaunchError> {
    ask_claw::launch(&StoreViewContext {
        view: "home",
        page: None,
        app_id: None,
        name: None,
    })
}

pub fn ask_claw_explore(page: &str) -> Result<(), ask_claw::LaunchError> {
    ask_claw::launch(&StoreViewContext {
        view: "explore",
        page: Some(page),
        app_id: None,
        name: None,
    })
}

pub fn ask_claw_app(app_id: &str, name: &str) -> Result<(), ask_claw::LaunchError> {
    ask_claw::launch(&StoreViewContext {
        view: "app",
        page: None,
        app_id: Some(app_id),
        name: Some(name),
    })
}

pub fn ask_claw_search(query: &str) -> Result<(), ask_claw::LaunchError> {
    ask_claw::launch(&StoreSearchContext {
        mode: "search",
        query,
    })
}
