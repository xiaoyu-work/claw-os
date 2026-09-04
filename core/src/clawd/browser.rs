use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::caps::{Cap, Scope, Verb};

use super::authority::Decision;
use super::protocol::BrokerError;
use super::wire::requests::BrowserControl;

const BRIDGE_TIMEOUT: Duration = Duration::from_secs(35);
const MAX_BRIDGE_FRAME_BYTES: usize = 8 * 1024 * 1024;
const MAX_BRIDGE_ERROR_BYTES: usize = 4096;
const MAX_TAB_ID: u64 = i32::MAX as u64;

#[derive(Debug)]
struct PreparedAction {
    capability: Cap,
    verb: &'static str,
    args: Value,
}

#[derive(Serialize)]
struct BridgeRequest<'a> {
    id: &'a str,
    verb: &'a str,
    args: &'a Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BridgeResponse {
    id: String,
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}

pub async fn control(params: Value, authority: &Decision) -> Result<Value, BrokerError> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, authority);
        return Err(BrokerError::unavailable(
            "attached browser control requires Linux",
        ));
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err(BrokerError::unavailable(
                "attached browser control requires root clawd",
            ));
        }
        authority
            .require_app("browser-attached")
            .map_err(BrokerError::authorization)?;
        let request: BrowserControl = serde_json::from_value(params).map_err(|error| {
            BrokerError::execution(format!("invalid browser control request: {error}"))
        })?;
        let prepared = prepare_action(&request).map_err(BrokerError::execution)?;
        let _authorized = authority
            .require(prepared.capability)
            .map_err(BrokerError::authorization)?;
        bridge_call(authority.owner_uid(), prepared.verb, &prepared.args).await
    }
}

fn prepare_action(request: &BrowserControl) -> Result<PreparedAction, String> {
    let action = request.action.as_str();
    match action {
        "tabs.list" => {
            validate_fields(request, &[], &[])?;
            Ok(PreparedAction {
                capability: Cap::new(Verb::BROWSER_TABS_READ, Scope::Wild),
                verb: "tabs.list",
                args: json!({}),
            })
        }
        "tabs.activate" => {
            validate_fields(request, &["tab_id"], &["tab_id"])?;
            let tab_id = validated_tab_id(request.tab_id)?;
            Ok(PreparedAction {
                capability: Cap::new(Verb::BROWSER_TABS_READ, Scope::Wild),
                verb: "tabs.activate",
                args: json!({"id": tab_id}),
            })
        }
        "nav.go" => {
            validate_fields(request, &["tab_id", "url"], &["tab_id", "url"])?;
            let tab_id = validated_tab_id(request.tab_id)?;
            let (url, scope) = browser_url(required_text(request.url.as_ref(), "url")?)?;
            Ok(PreparedAction {
                capability: Cap::new(Verb::BROWSER_NAV, Scope::host(scope)),
                verb: "nav.go",
                args: json!({"id": tab_id, "url": url}),
            })
        }
        "dom.query" => {
            validate_fields(
                request,
                &["tab_id", "page_url", "selector"],
                &["tab_id", "page_url", "selector"],
            )?;
            host_bound(
                request,
                Verb::BROWSER_DOM_READ,
                "dom.query",
                json!({
                    "id": validated_tab_id(request.tab_id)?,
                    "selector": required_nonempty(request.selector.as_ref(), "selector")?,
                }),
            )
        }
        "dom.click" => {
            validate_fields(
                request,
                &["tab_id", "page_url", "reference"],
                &["tab_id", "page_url", "reference"],
            )?;
            host_bound(
                request,
                Verb::BROWSER_DOM_WRITE,
                "dom.click",
                json!({
                    "id": validated_tab_id(request.tab_id)?,
                    "ref": required_nonempty(request.reference.as_ref(), "reference")?,
                }),
            )
        }
        "dom.fill" => {
            validate_fields(
                request,
                &["tab_id", "page_url", "reference", "value"],
                &["tab_id", "page_url", "reference", "value"],
            )?;
            host_bound(
                request,
                Verb::BROWSER_DOM_WRITE,
                "dom.fill",
                json!({
                    "id": validated_tab_id(request.tab_id)?,
                    "ref": required_nonempty(request.reference.as_ref(), "reference")?,
                    "value": required_text(request.value.as_ref(), "value")?,
                }),
            )
        }
        "dom.fill_secret" => {
            validate_fields(
                request,
                &["tab_id", "page_url", "reference", "value"],
                &["tab_id", "page_url", "reference", "value"],
            )?;
            host_bound(
                request,
                Verb::BROWSER_INPUT_SECRET,
                "dom.fill",
                json!({
                    "id": validated_tab_id(request.tab_id)?,
                    "ref": required_nonempty(request.reference.as_ref(), "reference")?,
                    "value": required_text(request.value.as_ref(), "value")?,
                    "allow_secret": true,
                }),
            )
        }
        "page.snapshot" => {
            validate_fields(
                request,
                &["tab_id", "page_url"],
                &["tab_id", "page_url", "kind"],
            )?;
            let kind = request.kind.as_ref().map_or("ax", |value| value.as_str());
            if !matches!(kind, "ax" | "text") {
                return Err("browser snapshot kind must be `ax` or `text`".to_string());
            }
            host_bound(
                request,
                Verb::BROWSER_DOM_READ,
                "page.snapshot",
                json!({
                    "id": validated_tab_id(request.tab_id)?,
                    "kind": kind,
                }),
            )
        }
        "page.screenshot" => {
            validate_fields(request, &["tab_id", "page_url"], &["tab_id", "page_url"])?;
            host_bound(
                request,
                Verb::BROWSER_DOM_READ,
                "page.screenshot",
                json!({"id": validated_tab_id(request.tab_id)?}),
            )
        }
        "eval" => {
            validate_fields(
                request,
                &["tab_id", "page_url", "expr"],
                &["tab_id", "page_url", "expr"],
            )?;
            host_bound(
                request,
                Verb::BROWSER_EVAL,
                "eval",
                json!({
                    "id": validated_tab_id(request.tab_id)?,
                    "expr": required_nonempty(request.expr.as_ref(), "expr")?,
                    "allow_eval": true,
                }),
            )
        }
        _ => Err(format!("unknown browser action: {action}")),
    }
}

fn host_bound(
    request: &BrowserControl,
    verb: Verb,
    bridge_verb: &'static str,
    mut args: Value,
) -> Result<PreparedAction, String> {
    let (canonical_url, scope) =
        browser_url(required_text(request.page_url.as_ref(), "page_url")?)?;
    let scheme = url::Url::parse(&canonical_url)
        .map_err(|_| "browser URL is invalid".to_string())?
        .scheme()
        .to_string();
    args.as_object_mut()
        .expect("browser bridge arguments are objects")
        .insert(
            "expected_origin".to_string(),
            Value::String(format!("{scheme}://{scope}")),
        );
    Ok(PreparedAction {
        capability: Cap::new(verb, Scope::host(scope)),
        verb: bridge_verb,
        args,
    })
}

fn browser_url(raw: &str) -> Result<(String, String), String> {
    let normalized;
    let parsed_raw = if raw.contains("://") {
        raw
    } else {
        normalized = format!("https://{raw}");
        &normalized
    };
    let parsed = url::Url::parse(parsed_raw).map_err(|_| "browser URL is invalid".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("browser URL must use http or https".to_string());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("browser URL must not contain credentials".to_string());
    }
    let (canonical, scope) = crate::caps::manifest::canonical_url_and_scope(raw)
        .ok_or_else(|| "browser URL must name a host".to_string())?;
    let Scope::Host(scope) = scope else {
        return Err("browser URL did not produce a host scope".to_string());
    };
    Ok((canonical, scope))
}

fn validated_tab_id(tab_id: Option<u64>) -> Result<u64, String> {
    match tab_id {
        Some(id) if (1..=MAX_TAB_ID).contains(&id) => Ok(id),
        _ => Err(format!("browser tab_id must be between 1 and {MAX_TAB_ID}")),
    }
}

fn required_text<'a, const MAX: usize>(
    value: Option<&'a super::wire::bounded::Text<MAX>>,
    name: &str,
) -> Result<&'a str, String> {
    value
        .map(|value| value.as_str())
        .ok_or_else(|| format!("browser action requires `{name}`"))
}

fn required_nonempty<'a, const MAX: usize>(
    value: Option<&'a super::wire::bounded::Text<MAX>>,
    name: &str,
) -> Result<&'a str, String> {
    let value = required_text(value, name)?;
    if value.trim().is_empty() {
        return Err(format!("browser field `{name}` must not be empty"));
    }
    Ok(value)
}

fn validate_fields(
    request: &BrowserControl,
    required: &[&str],
    allowed: &[&str],
) -> Result<(), String> {
    let fields = [
        ("tab_id", request.tab_id.is_some()),
        ("page_url", request.page_url.is_some()),
        ("url", request.url.is_some()),
        ("selector", request.selector.is_some()),
        ("reference", request.reference.is_some()),
        ("value", request.value.is_some()),
        ("expr", request.expr.is_some()),
        ("kind", request.kind.is_some()),
    ];
    for field in required {
        if !fields
            .iter()
            .any(|(name, present)| name == field && *present)
        {
            return Err(format!("browser action requires `{field}`"));
        }
    }
    for (field, present) in fields {
        if present && !allowed.contains(&field) {
            return Err(format!("browser action does not accept `{field}`"));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
async fn bridge_call(owner_uid: u32, verb: &str, args: &Value) -> Result<Value, BrokerError> {
    let path = validated_socket_path(owner_uid).map_err(BrokerError::unavailable)?;
    let mut stream = tokio::time::timeout(BRIDGE_TIMEOUT, UnixStream::connect(&path))
        .await
        .map_err(|_| BrokerError::unavailable("attached browser bridge connection timed out"))?
        .map_err(|error| {
            BrokerError::unavailable(format!("connect attached browser bridge: {error}"))
        })?;
    verify_peer_uid(&stream, owner_uid).map_err(BrokerError::unavailable)?;

    let request_id = uuid::Uuid::new_v4().to_string();
    let payload = serde_json::to_vec(&BridgeRequest {
        id: &request_id,
        verb,
        args,
    })
    .map_err(|error| BrokerError::execution(format!("encode attached browser request: {error}")))?;
    if payload.len() > MAX_BRIDGE_FRAME_BYTES {
        return Err(BrokerError::execution(
            "attached browser request exceeds the bridge frame limit",
        ));
    }

    let response = tokio::time::timeout(BRIDGE_TIMEOUT, async {
        stream
            .write_all(&(payload.len() as u32).to_le_bytes())
            .await?;
        stream.write_all(&payload).await?;
        stream.flush().await?;

        let mut length = [0u8; 4];
        stream.read_exact(&mut length).await?;
        let length = u32::from_le_bytes(length) as usize;
        if length == 0 || length > MAX_BRIDGE_FRAME_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "attached browser response has an invalid frame length",
            ));
        }
        let mut body = vec![0u8; length];
        stream.read_exact(&mut body).await?;
        Ok::<Vec<u8>, std::io::Error>(body)
    })
    .await
    .map_err(|_| {
        BrokerError::indeterminate("attached browser bridge request timed out after dispatch")
    })?
    .map_err(|error| {
        BrokerError::indeterminate(format!(
            "exchange with attached browser bridge after dispatch: {error}"
        ))
    })?;

    let response: BridgeResponse = serde_json::from_slice(&response).map_err(|error| {
        BrokerError::indeterminate(format!("decode attached browser response: {error}"))
    })?;
    if response.id != request_id {
        return Err(BrokerError::indeterminate(
            "attached browser response id does not match the request",
        ));
    }
    match (response.ok, response.result, response.error) {
        (true, Some(result), None) if result.is_object() => Ok(result),
        (false, None, Some(error))
            if !error.is_empty() && error.len() <= MAX_BRIDGE_ERROR_BYTES =>
        {
            Err(BrokerError::execution(format!(
                "attached browser rejected `{verb}`"
            )))
        }
        _ => Err(BrokerError::indeterminate(
            "attached browser returned an invalid response envelope",
        )),
    }
}

#[cfg(target_os = "linux")]
fn validated_socket_path(owner_uid: u32) -> Result<PathBuf, String> {
    let runtime_dir = PathBuf::from(format!("/run/user/{owner_uid}"));
    let runtime_meta = fs::symlink_metadata(&runtime_dir)
        .map_err(|error| format!("inspect browser runtime directory: {error}"))?;
    if !runtime_meta.file_type().is_dir()
        || runtime_meta.uid() != owner_uid
        || runtime_meta.permissions().mode() & 0o077 != 0
    {
        return Err(
            "browser runtime directory must be owner-only and owned by the session user"
                .to_string(),
        );
    }

    let socket = runtime_dir.join("claw-browser.sock");
    let socket_meta = fs::symlink_metadata(&socket)
        .map_err(|error| format!("inspect attached browser socket: {error}"))?;
    if !socket_meta.file_type().is_socket()
        || socket_meta.uid() != owner_uid
        || socket_meta.permissions().mode() & 0o077 != 0
    {
        return Err(
            "attached browser socket must be owner-only and owned by the session user".to_string(),
        );
    }
    Ok(socket)
}

#[cfg(target_os = "linux")]
fn verify_peer_uid(stream: &UnixStream, expected_uid: u32) -> Result<(), String> {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(credentials).cast::<libc::c_void>(),
            std::ptr::addr_of_mut!(length),
        )
    };
    if result != 0 {
        return Err(format!(
            "inspect attached browser peer: {}",
            std::io::Error::last_os_error()
        ));
    }
    if length as usize != std::mem::size_of::<libc::ucred>() || credentials.uid != expected_uid {
        return Err("attached browser peer is not the authorized session user".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/browser.rs"
    ));
}
