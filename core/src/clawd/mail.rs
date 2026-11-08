//! Account-scoped mail access through the authenticated owner's Thunderbird.

use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use super::authority::Decision;
use super::client_identity::{ClientIdentity, FsIdentityGuard};
use super::protocol::BrokerError;
use super::wire::requests::MailControl;
use crate::caps::{Cap, Scope, Verb};
use crate::mail_native::{MAX_MAIL_FRAME, SOCKET_NAME};

const MAX_ATTACHMENT: u64 = 16 * 1024 * 1024;
const BODY_BYTES: usize = 256 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BridgeResponse {
    id: String,
    ok: bool,
    dispatched: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}

fn account(raw: &str) -> Result<(), String> {
    let (profile, native) = raw
        .split_once(':')
        .ok_or("Mail account must include its profile id")?;
    let parsed = uuid::Uuid::parse_str(profile).map_err(|_| "invalid Mail profile id")?;
    if parsed.to_string() != profile
        || native.is_empty()
        || native.len() > 64
        || !native
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
    {
        return Err("invalid Thunderbird account identifier".into());
    }
    Ok(())
}

fn text<'a>(args: &'a Map<String, Value>, key: &str, limit: usize) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= limit && !value.contains('\0'))
        .ok_or_else(|| format!("Mail requires bounded nonempty `{key}`"))
}

fn message_account(id: &str) -> Result<String, String> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(id)
        .map_err(|_| "Mail message id is not a canonical reference")?;
    let parts: Vec<String> = serde_json::from_slice(&bytes)
        .map_err(|_| "Mail message id is not a canonical reference")?;
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || part.len() > 4096)
    {
        return Err("Mail message reference is invalid".into());
    }
    account(&parts[0])?;
    Ok(parts[0].clone())
}

fn prepare(request: &MailControl) -> Result<Vec<Cap>, String> {
    let args = &request.args;
    let action = request.action.as_str();
    let allowed: &[&str] = match action {
        "accounts" | "folders" => &[],
        "list" => &["folder", "max_results", "unread"],
        "search" => &["query", "max_results", "folder"],
        "read" | "native_draft" => &["id"],
        "sync" => &["cursor", "reset"],
        "send" => &["to", "cc", "subject", "body"],
        "draft_save" => &["id", "source_account", "to", "cc", "subject", "body"],
        "draft_send" => &["id", "sender_account", "review", "confirm"],
        "attachment_download" => &["id", "attachment", "output", "max_bytes"],
        _ => return Err("unknown Mail action".into()),
    };
    if args.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("Mail action has unknown arguments".into());
    }
    if request.deadline_unix_ms == 0 {
        return Err("Mail deadline must be a positive integer".into());
    }
    if action == "accounts" {
        if request.account.is_some() {
            return Err("account enumeration cannot select an account".into());
        }
        return Ok(vec![Cap::new(Verb::MAIL_ACCOUNTS, Scope::Wild)]);
    }
    let selected = request
        .account
        .as_ref()
        .ok_or("Mail requires an explicit account")?
        .as_str();
    account(selected)?;
    let verb = match action {
        "send" => Verb::MAIL_SEND,
        "draft_save" => Verb::MAIL_DRAFT,
        _ => Verb::MAIL_READ,
    };
    let mut caps = vec![Cap::new(verb, Scope::name(selected))];
    if let Some(value) = args.get("max_results") {
        if !value
            .as_u64()
            .is_some_and(|value| (1..=100).contains(&value))
        {
            return Err("Mail max_results must be between 1 and 100".into());
        }
    }
    for key in ["unread", "reset"] {
        if args.get(key).is_some_and(|value| !value.is_boolean()) {
            return Err(format!("Mail `{key}` must be boolean"));
        }
    }
    if args
        .get("cursor")
        .is_some_and(|value| value.as_u64().is_none())
    {
        return Err("Mail cursor must be a nonnegative integer".into());
    }
    if args.contains_key("folder") {
        text(args, "folder", 4096)?;
    }
    if action == "search" {
        text(args, "query", 4096)?;
    }
    if matches!(action, "send" | "draft_save") {
        for key in ["to", "subject", "body"] {
            let value = args
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| value.len() <= if key == "body" { BODY_BYTES } else { 4096 })
                .filter(|value| !value.contains('\0'))
                .ok_or_else(|| format!("Mail requires bounded string `{key}`"))?;
            if key == "to" && value.trim().is_empty() {
                return Err("Mail recipient list cannot be empty".into());
            }
            if key != "body" && value.contains(['\r', '\n']) {
                return Err("Mail recipient and subject headers cannot contain newlines".into());
            }
        }
        if let Some(cc) = args.get("cc") {
            if !cc
                .as_str()
                .is_some_and(|value| value.len() <= 4096 && !value.contains(['\r', '\n', '\0']))
            {
                return Err("Mail cc must be a bounded header without newlines".into());
            }
        }
    }
    if matches!(
        action,
        "read" | "native_draft" | "draft_send" | "attachment_download"
    ) || (action == "draft_save" && args.contains_key("id"))
    {
        let source = message_account(text(args, "id", 8192)?)?;
        if action == "draft_save" {
            let declared = text(args, "source_account", 128)?;
            account(declared)?;
            if declared != source {
                return Err("Draft replacement requires its matching source account".into());
            }
            if declared.split_once(':').unwrap().0 != selected.split_once(':').unwrap().0 {
                return Err("Draft and sender must use the same Thunderbird profile".into());
            }
            caps.push(Cap::new(Verb::MAIL_READ, Scope::name(&source)));
            caps.push(Cap::new(Verb::MAIL_DRAFT, Scope::name(source)));
        } else if source != selected {
            return Err("Mail message reference belongs to a different account".into());
        }
        if action == "draft_save" && args.contains_key("source_account") && !args.contains_key("id")
        {
            return Err("A draft source account requires the stored draft id".into());
        }
    }
    if action == "draft_send" {
        let sender = text(args, "sender_account", 128)?;
        account(sender)?;
        if sender.split_once(':').unwrap().0 != selected.split_once(':').unwrap().0 {
            return Err("Mail sender and draft must belong to the same Thunderbird profile".into());
        }
        if args.get("confirm") != Some(&Value::Bool(true)) {
            return Err("draft send requires explicit confirmation".into());
        }
        let review = text(args, "review", 71)?;
        if review.len() != 71
            || !review.starts_with("sha256:")
            || !review[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("draft send requires a SHA-256 review fingerprint".into());
        }
        caps.push(Cap::new(Verb::MAIL_DRAFT, Scope::name(selected)));
        caps.push(Cap::new(Verb::MAIL_SEND, Scope::name(sender)));
    }
    if action == "attachment_download" {
        let attachment = text(args, "attachment", 128)?;
        if !attachment
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err("Mail attachment must be a native MIME part identifier".into());
        }
        let output = text(args, "output", 4096)?;
        if !Path::new(output).is_absolute() {
            return Err("Mail attachment output must be absolute".into());
        }
        if !args
            .get("max_bytes")
            .and_then(Value::as_u64)
            .is_some_and(|value| (1..=MAX_ATTACHMENT).contains(&value))
        {
            return Err("Mail attachment limit must be between 1 and 16777216".into());
        }
        caps.push(Cap::new(Verb::FS_WRITE, Scope::path(output)));
    }
    Ok(caps)
}

fn authorize(
    request: &MailControl,
    authority: &Decision,
    owner: u32,
) -> Result<super::authority::Authorized, BrokerError> {
    let caps = prepare(request).map_err(BrokerError::execution)?;
    if owner != authority.owner_uid() {
        return Err(BrokerError::authorization("Mail request owner mismatch"));
    }
    authority
        .require_all(&caps)
        .map_err(BrokerError::authorization)
}

fn now_ms() -> Result<u64, BrokerError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .map_err(|_| BrokerError::unavailable("system clock precedes the Unix epoch"))
}

fn socket_path(owner: u32) -> Result<PathBuf, BrokerError> {
    let runtime = PathBuf::from(format!("/run/user/{owner}"));
    let metadata = fs::symlink_metadata(&runtime).map_err(|error| {
        BrokerError::unavailable(format!("Mail owner runtime is unavailable: {error}"))
    })?;
    if !metadata.is_dir() || metadata.uid() != owner || metadata.mode() & 0o077 != 0 {
        return Err(BrokerError::unavailable(
            "Mail owner runtime must be a private owner directory",
        ));
    }
    Ok(runtime.join(SOCKET_NAME))
}

async fn connect(path: &Path, owner: u32) -> Result<Option<UnixStream>, BrokerError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(BrokerError::unavailable(format!(
                "Thunderbird mailbox socket is unavailable: {error}"
            )))
        }
    };
    if !metadata.file_type().is_socket() || metadata.uid() != owner || metadata.mode() & 0o077 != 0
    {
        return Err(BrokerError::unavailable(
            "Thunderbird mailbox socket must be private and owner-owned",
        ));
    }
    let stream = match UnixStream::connect(path).await {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None)
        }
        Err(error) => {
            return Err(BrokerError::unavailable(format!(
                "connect Thunderbird mailbox bridge: {error}"
            )))
        }
    };
    let peer = stream
        .peer_cred()
        .map_err(|_| BrokerError::unavailable("Mail peer credentials unavailable"))?;
    let pid = peer
        .pid()
        .and_then(|pid| u32::try_from(pid).ok())
        .ok_or_else(|| BrokerError::unavailable("Mail peer pid unavailable"))?;
    if peer.uid() != owner {
        return Err(BrokerError::unavailable(
            "Mail bridge belongs to another owner",
        ));
    }
    super::app_sessions::validate_native_mail_launcher(pid).map_err(BrokerError::unavailable)?;
    Ok(Some(stream))
}

pub async fn control(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, BrokerError> {
    let request: MailControl = serde_json::from_value(params)
        .map_err(|error| BrokerError::execution(format!("invalid Mail request: {error}")))?;
    prepare(&request).map_err(BrokerError::execution)?;
    let deadline = request
        .deadline_unix_ms
        .min(now_ms()?.saturating_add(55_000));
    if deadline <= now_ms()? {
        return Err(BrokerError::unavailable(
            "Mail request deadline has expired",
        ));
    }
    if unsafe { libc::geteuid() } != 0
        || client.require_uid().map_err(BrokerError::authorization)? != authority.owner_uid()
    {
        return Err(BrokerError::authorization(
            "Mail requires the authenticated owner and root clawd",
        ));
    }
    let _authorized = authorize(
        &request,
        authority,
        client.require_uid().map_err(BrokerError::authorization)?,
    )?;
    let path = socket_path(authority.owner_uid())?;
    let mut stream = match connect(&path, authority.owner_uid()).await? {
        Some(stream) => stream,
        None => {
            super::desktop::start_thunderbird(client)
                .await
                .map_err(BrokerError::unavailable)?;
            loop {
                if now_ms()? >= deadline {
                    return Err(BrokerError::unavailable(
                        "Thunderbird mailbox startup deadline elapsed",
                    ));
                }
                if let Some(stream) = connect(&path, authority.owner_uid()).await? {
                    break stream;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    };
    let mut args = request.args.clone();
    let output = args.remove("output");
    if let Some(account) = request.account.as_ref() {
        args.insert("account".into(), json!(account.as_str()));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let payload = serde_json::to_vec(&json!({"id":id,"verb":request.action.as_str(),"args":args}))
        .map_err(|error| BrokerError::execution(error.to_string()))?;
    if payload.len() > 1024 * 1024 {
        return Err(BrokerError::execution(
            "Mail request exceeds its frame limit",
        ));
    }
    let duration = Duration::from_millis(deadline.saturating_sub(now_ms()?).min(35_000));
    if duration.is_zero() {
        return Err(BrokerError::unavailable(
            "Mail request expired before dispatch",
        ));
    }
    let body = tokio::time::timeout(duration, async {
        stream
            .write_all(&(payload.len() as u32).to_le_bytes())
            .await?;
        stream.write_all(&payload).await?;
        let mut header = [0; 4];
        stream.read_exact(&mut header).await?;
        let length = u32::from_le_bytes(header) as usize;
        if length == 0 || length > MAX_MAIL_FRAME {
            return Err(std::io::Error::other(
                "Mail response frame exceeds its limit",
            ));
        }
        let mut body = vec![0; length];
        stream.read_exact(&mut body).await?;
        Ok::<_, std::io::Error>(body)
    })
    .await
    .map_err(|_| BrokerError::indeterminate("Thunderbird Mail response deadline elapsed"))
    .and_then(|result| {
        result.map_err(|error| BrokerError::indeterminate(format!("Mail exchange failed: {error}")))
    })?;
    let response: BridgeResponse = serde_json::from_slice(&body)
        .map_err(|_| BrokerError::indeterminate("Thunderbird Mail returned an invalid envelope"))?;
    if response.id != id {
        return Err(BrokerError::indeterminate(
            "Thunderbird Mail response did not correlate",
        ));
    }
    let mut result = match (response.ok, response.result, response.error) {
        (true, Some(result), None) if result.is_object() && response.dispatched => result,
        (false, None, Some(error)) if !error.is_empty() && error.len() <= 4096 => {
            return Err(if response.dispatched {
                BrokerError::indeterminate(error)
            } else {
                BrokerError::unavailable(error)
            });
        }
        _ => {
            return Err(BrokerError::indeterminate(
                "Thunderbird Mail returned an invalid result",
            ))
        }
    };
    if request
        .account
        .as_ref()
        .is_some_and(|account| result["account"].as_str() != Some(account.as_str()))
    {
        return Err(BrokerError::indeterminate(
            "Thunderbird returned a different Mail account",
        ));
    }
    if request.action.as_str() == "attachment_download" && result.get("error").is_none() {
        let encoded = result["data"]
            .as_str()
            .ok_or_else(|| BrokerError::execution("Mail attachment content is missing"))?;
        let limit = request.args["max_bytes"].as_u64().unwrap();
        if encoded.len() as u64 > limit.div_ceil(3) * 4 {
            return Err(BrokerError::execution(
                "Mail attachment exceeds the requested byte limit",
            ));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| BrokerError::execution("Mail attachment encoding is invalid"))?;
        if bytes.len() as u64 > limit {
            return Err(BrokerError::execution(
                "Mail attachment exceeds the requested byte limit",
            ));
        }
        let output = output.as_ref().and_then(Value::as_str).unwrap();
        let path = {
            let _identity =
                FsIdentityGuard::enter(authority.owner_uid()).map_err(BrokerError::execution)?;
            super::filesystem::resolve_path(output).map_err(BrokerError::execution)?
        };
        if path != Path::new(output) {
            return Err(BrokerError::execution(
                "Mail attachment output must be canonical",
            ));
        }
        crate::worker::derive::reject_forbidden(&path).map_err(BrokerError::execution)?;
        let target = super::filesystem::Target::open(&path, authority.owner_uid())
            .map_err(BrokerError::execution)?;
        target.write_new(&bytes, authority).map_err(|error| {
            BrokerError::indeterminate(format!("Mail attachment persistence failed: {error}"))
        })?;
        let object = result.as_object_mut().unwrap();
        object.remove("data");
        object.insert("path".into(), json!(output));
        object.insert("bytes".into(), json!(bytes.len()));
        object.insert(
            "sha256".into(),
            json!(format!("sha256:{:x}", Sha256::digest(&bytes))),
        );
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/mail.rs"
    ));
}
