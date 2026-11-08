//! Root-owned provider verification and delivery evidence, not an Agent queue.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::caps::{Cap, Scope, Verb};

use super::authority::Decision;
use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;
use super::wire::requests::{ChannelConfigure, ChannelControl, ChannelManage};

mod providers;

const MAX_PROFILES: usize = 32;
const MAX_STATE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_REPLIES: usize = 4096;
const FRESH_MS: u64 = 300_000;
const REPLY_MS: u64 = 24 * 60 * 60 * 1000;
static POLLING: AtomicBool = AtomicBool::new(false);
static WEBHOOKS: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Platform {
    Discord,
    Telegram,
    Slack,
    Whatsapp,
}

impl Platform {
    const ALL: [Self; 4] = [Self::Discord, Self::Telegram, Self::Slack, Self::Whatsapp];

    fn parse(value: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|p| p.name() == value)
            .ok_or_else(|| "channel must be discord, telegram, slack or whatsapp".into())
    }

    fn name(self) -> &'static str {
        match self {
            Self::Discord => "discord",
            Self::Telegram => "telegram",
            Self::Slack => "slack",
            Self::Whatsapp => "whatsapp",
        }
    }

    fn app_id(self) -> String {
        format!("gateway-{}", self.name())
    }
    fn source(self) -> String {
        format!("channel:{}", self.app_id())
    }
    fn polling(self) -> bool {
        matches!(self, Self::Discord | Self::Telegram)
    }
    fn host(self) -> &'static str {
        match self {
            Self::Discord => "discord.com",
            Self::Telegram => "api.telegram.org",
            Self::Slack => "slack.com",
            Self::Whatsapp => "graph.facebook.com",
        }
    }
    fn credentials(self) -> &'static [&'static str] {
        match self {
            Self::Discord => &["discord_bot_token"],
            Self::Telegram => &["telegram_bot_token"],
            Self::Slack => &["slack_bot_token", "slack_signing_secret"],
            Self::Whatsapp => &[
                "whatsapp_access_token",
                "whatsapp_phone_number_id",
                "whatsapp_app_secret",
                "whatsapp_verify_token",
            ],
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Profile {
    schema: u8,
    owner_uid: u32,
    platform: Platform,
    account_id: String,
    channel_id: String,
    sender_id: String,
    trigger_id: String,
    trigger_generation: String,
    credential_tier: u8,
    credential_fingerprints: BTreeMap<String, String>,
    revision: u64,
    enabled: bool,
    cursor: Option<String>,
    window_ms: u64,
    window_count: u8,
    recent: BTreeMap<String, Seen>,
    pending: Option<Value>,
    replies: BTreeMap<String, Reply>,
    last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Seen {
    fingerprint: String,
    at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    fingerprint: String,
    result: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Incoming {
    message_id: String,
    sender_id: String,
    channel_id: String,
    thread_id: Option<String>,
    timestamp_ms: u64,
    text: String,
    truncated: bool,
}

fn state_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn directory() -> PathBuf {
    crate::paths::data_dir().join("channel-ingress")
}
fn profile_path(uid: u32, platform: Platform) -> PathBuf {
    directory().join(format!("{uid}-{}.json", platform.name()))
}

fn check_metadata(metadata: &fs::Metadata, directory: bool) -> Result<(), String> {
    if metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file() || metadata.nlink() != 1
        }
    {
        return Err("channel state must be a private Root-owned regular file or directory".into());
    }
    Ok(())
}

fn ensure_directory() -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;
    match fs::DirBuilder::new().mode(0o700).create(directory()) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("create channel state: {error}")),
    }
    check_metadata(
        &fs::symlink_metadata(directory()).map_err(|e| e.to_string())?,
        true,
    )
}

fn read_private(path: &Path, maximum: u64) -> Result<Option<Vec<u8>>, String> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("open channel state: {error}")),
    };
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    check_metadata(&metadata, false)?;
    if metadata.len() > maximum {
        return Err("channel state exceeds its retained bound".into());
    }
    let mut bytes = Vec::new();
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > maximum {
        return Err("channel state exceeds its retained bound".into());
    }
    Ok(Some(bytes))
}

fn load(uid: u32, platform: Platform) -> Result<Option<Profile>, String> {
    if !directory().try_exists().map_err(|e| e.to_string())? {
        return Ok(None);
    }
    check_metadata(
        &fs::symlink_metadata(directory()).map_err(|e| e.to_string())?,
        true,
    )?;
    let Some(bytes) = read_private(&profile_path(uid, platform), MAX_STATE_BYTES)? else {
        return Ok(None);
    };
    let profile: Profile = serde_json::from_slice(&bytes)
        .map_err(|_| "channel state is corrupt; automatic reset is refused")?;
    if profile.schema != 1
        || profile.owner_uid != uid
        || profile.platform != platform
        || profile.replies.len() > MAX_REPLIES
        || profile.recent.len() > 512
    {
        return Err("channel state identity or retained bounds do not match".into());
    }
    Ok(Some(profile))
}

fn store(profile: &Profile) -> Result<(), String> {
    ensure_directory()?;
    let bytes = serde_json::to_vec(profile).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err("channel state exceeds 2 MiB".into());
    }
    let path = profile_path(profile.owner_uid, profile.platform);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => check_metadata(&metadata, false)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    let temporary = directory().join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(&temporary, &path).map_err(|e| e.to_string())?;
        File::open(directory())
            .and_then(|d| d.sync_all())
            .map_err(|e| format!("channel state persistence outcome is indeterminate: {e}"))
    })();
    let _ = fs::remove_file(temporary);
    result
}

fn profiles() -> Result<Vec<Profile>, String> {
    if !directory().try_exists().map_err(|e| e.to_string())? {
        return Ok(Vec::new());
    }
    check_metadata(
        &fs::symlink_metadata(directory()).map_err(|e| e.to_string())?,
        true,
    )?;
    let mut profiles = Vec::new();
    for entry in fs::read_dir(directory()).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let Some((uid, platform)) = name
            .to_str()
            .and_then(|n| n.strip_suffix(".json"))
            .and_then(|n| n.split_once('-'))
        else {
            continue;
        };
        let uid: u32 = uid.parse().map_err(|_| "invalid channel state filename")?;
        let platform = Platform::parse(platform)?;
        profiles.push(load(uid, platform)?.ok_or("channel state disappeared")?);
        if profiles.len() > MAX_PROFILES {
            return Err("channel profile capacity is exhausted".into());
        }
    }
    Ok(profiles)
}

fn bound_rule(profile: &Profile, active: bool) -> Result<crate::triggers::TriggerRule, String> {
    let rule = crate::triggers::load_rule(&profile.trigger_id)?;
    if rule.owner_uid != Some(profile.owner_uid)
        || rule.generation.as_deref() != Some(profile.trigger_generation.as_str())
        || rule.source.as_deref() != Some(profile.platform.source().as_str())
        || rule.event_type.as_deref() != Some("message.received")
        || rule.activity_id.is_none()
    {
        return Err("channel requires its exact owned Activity trigger generation".into());
    }
    if active {
        if !rule.enabled {
            return Err("channel Activity trigger is disabled".into());
        }
        crate::triggers::validate_activity_trigger(
            profile.owner_uid,
            rule.activity_id.as_deref().unwrap(),
        )?;
    }
    Ok(rule)
}

pub async fn configure(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let request: ChannelConfigure =
        serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;
    let platform = Platform::parse(request.platform.as_str())?;
    providers::validate_identifiers(
        platform,
        request.account_id.as_str(),
        request.channel_id.as_str(),
        request.sender_id.as_str(),
    )?;
    let uid = client.require_uid()?;
    let rule = crate::triggers::load_rule(request.trigger_id.as_str())?;
    let generation = rule.generation.clone().ok_or_else(|| {
        BrokerError::authorization("channel requires a generation-bound Activity trigger")
    })?;
    let mut profile = Profile {
        schema: 1,
        owner_uid: uid,
        platform,
        account_id: request.account_id.as_str().into(),
        channel_id: request.channel_id.as_str().into(),
        sender_id: request.sender_id.as_str().into(),
        trigger_id: request.trigger_id.as_str().into(),
        trigger_generation: generation,
        credential_tier: 0,
        credential_fingerprints: BTreeMap::new(),
        revision: 1,
        enabled: false,
        cursor: None,
        window_ms: 0,
        window_count: 0,
        recent: BTreeMap::new(),
        pending: None,
        replies: BTreeMap::new(),
        last_error: None,
    };
    bound_rule(&profile, true)?;
    let mut required = vec![
        Cap::new(Verb::IPC_PUBLISH, Scope::name(platform.source())),
        Cap::new(Verb::NET_DIAL, Scope::host(platform.host())),
    ];
    required.extend(
        platform
            .credentials()
            .iter()
            .map(|name| Cap::new(Verb::SECRET_READ, Scope::name(format!("default/{name}")))),
    );
    let operation = crate::crypto::sha256_hex(params.to_string().as_bytes());
    let label = format!(
        "Configure persistent {} ingress: account {}, channel {}, sender {}, Activity trigger {}",
        platform.name(),
        profile.account_id,
        profile.channel_id,
        profile.sender_id,
        profile.trigger_id,
    );
    let owner =
        super::scheduler::authorize_channel_effect(client, &required, &operation, &label).await?;
    profile.credential_tier = owner.tier;
    providers::bind_credentials(&mut profile, &owner.home)?;
    providers::verify_account(&profile, &owner.home).await?;
    let _lock = state_lock().lock().await;
    if let Some(previous) = load(uid, platform)? {
        if previous.enabled
            || previous.pending.is_some()
            || previous.replies.values().any(|r| r.result.is_none())
        {
            return Err(BrokerError::authorization(
                "stop the channel and resolve uncertain delivery before replacing its binding",
            ));
        }
        profile.revision = previous
            .revision
            .checked_add(1)
            .ok_or_else(|| BrokerError::unavailable("channel revision exhausted"))?;
        if previous.account_id == profile.account_id && previous.channel_id == profile.channel_id {
            profile.cursor = previous.cursor;
        }
        profile.window_ms = previous.window_ms;
        profile.window_count = previous.window_count;
        profile.recent = previous.recent;
        profile.replies = previous.replies;
    } else if profiles()?.len() >= MAX_PROFILES {
        return Err(BrokerError::unavailable(
            "channel profile capacity is exhausted",
        ));
    }
    // One Telegram bot has one destructive vendor cursor, not one per owner.
    if platform == Platform::Telegram
        && profiles()?.iter().any(|p| {
            p.platform == platform && p.account_id == profile.account_id && p.owner_uid != owner.uid
        })
    {
        return Err(BrokerError::authorization(
            "this Telegram bot is already bound to another owner",
        ));
    }
    bound_rule(&profile, true)?;
    store(&profile)?;
    Ok(status(Some(&profile), platform))
}

fn status(profile: Option<&Profile>, platform: Platform) -> Value {
    let receiver = if platform.polling() {
        POLLING.load(Ordering::Relaxed)
    } else {
        WEBHOOKS.load(Ordering::Relaxed)
    };
    json!({
        "ok":true,"platform":platform.name(),"configured":profile.is_some(),
        "running":profile.is_some_and(|p| p.enabled) && receiver,
        "enabled":profile.is_some_and(|p| p.enabled),
        "source":platform.source(),"mode":if platform.polling() {"poll"} else {"signed_webhook"},
        "trigger_id":profile.map(|p| &p.trigger_id),
        "account_id":profile.map(|p| &p.account_id),"channel_id":profile.map(|p| &p.channel_id),
        "sender_id":profile.map(|p| &p.sender_id),
        "last_error":profile.and_then(|p| p.last_error.as_ref()),
        "pending_event":profile.is_some_and(|p| p.pending.is_some()),
        "pending_replies":profile.map(|p| p.replies.values().filter(|r| r.result.is_none()).count()).unwrap_or(0),
        "webhook_path":(!platform.polling()).then(|| profile.map(|p| format!("/channels/{}/{}",p.owner_uid,platform.name()))).flatten(),
    })
}

pub async fn manage(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let request: ChannelManage = serde_json::from_value(params).map_err(|e| e.to_string())?;
    let platform = Platform::parse(request.platform.as_str())?;
    if !matches!(request.action.as_str(), "status" | "stop") {
        return Err(BrokerError::execution(
            "human channel management supports status and stop only",
        ));
    }
    let owner = super::scheduler::authenticate_channel_owner(client).await?;
    let _lock = state_lock().lock().await;
    let mut profile = load(owner.uid, platform)?;
    if request.action.as_str() == "stop" {
        if let Some(profile) = &mut profile {
            profile.enabled = false;
            profile.revision = profile
                .revision
                .checked_add(1)
                .ok_or_else(|| BrokerError::unavailable("channel revision exhausted"))?;
            store(profile)?;
        }
    }
    Ok(status(profile.as_ref(), platform))
}

pub async fn control(params: Value, authority: &Decision) -> Result<Value, BrokerError> {
    let request: ChannelControl = serde_json::from_value(params).map_err(|e| e.to_string())?;
    let app = authority.app_id().ok_or_else(|| {
        BrokerError::authorization("channel control requires an authenticated connector App")
    })?;
    let platform = Platform::parse(
        app.strip_prefix("gateway-")
            .ok_or_else(|| BrokerError::authorization("not a channel connector App"))?,
    )?;
    let action = request.action.as_str();
    if !matches!(action, "status" | "start" | "stop" | "reply")
        || (action == "reply") != (request.event_id.is_some() && request.text.is_some())
        || (action != "reply" && (request.event_id.is_some() || request.text.is_some()))
        || (action == "reply" && request.confirm != Some(true))
        || (action != "reply" && request.confirm.is_some())
    {
        return Err(BrokerError::execution("invalid channel action or fields"));
    }
    let owner = authority.owner_uid();
    if owner == 0 {
        return Err(BrokerError::authorization(
            "channel requires a non-root owner",
        ));
    }
    let _authorized =
        authority.require(Cap::new(Verb::IPC_PUBLISH, Scope::name(platform.source())))?;
    if action == "reply" {
        return reply(
            owner,
            platform,
            request.event_id.as_ref().unwrap().as_str(),
            request.text.as_ref().unwrap().as_str(),
        )
        .await
        .map_err(BrokerError::from);
    }
    let _lock = state_lock().lock().await;
    let Some(mut profile) = load(owner, platform)? else {
        return if action == "status" {
            Ok(status(None, platform))
        } else {
            Err(BrokerError::authorization(
                "configure this channel through the human OS channel service first",
            ))
        };
    };
    if action == "start" {
        bound_rule(&profile, true)?;
        if (platform.polling() && !POLLING.load(Ordering::Relaxed))
            || (!platform.polling() && !WEBHOOKS.load(Ordering::Relaxed))
        {
            return Err(BrokerError::unavailable(
                "the managed channel receiver is unavailable",
            ));
        }
        profile.enabled = true;
        profile.last_error = None;
    } else if action == "stop" {
        profile.enabled = false;
    }
    if action != "status" {
        profile.revision = profile
            .revision
            .checked_add(1)
            .ok_or_else(|| BrokerError::unavailable("channel revision exhausted"))?;
        store(&profile)?;
    }
    Ok(status(Some(&profile), platform))
}

fn event_record(profile: &Profile, message: &Incoming, now: u64) -> Value {
    let id = format!(
        "channel-event-{}",
        crate::crypto::sha256_hex(
            format!(
                "{}\0{}\0{}\0{}\0{}",
                profile.owner_uid,
                profile.platform.name(),
                profile.account_id,
                profile.channel_id,
                message.message_id
            )
            .as_bytes(),
        )
    );
    json!({
        "schema":1,"event":"context.event","id":id,"ts":Utc::now(),"received_at":Utc::now(),
        "source":profile.platform.source(),"app_id":profile.platform.app_id(),"event_type":"message.received",
        "entity_id":message.message_id,"session_id":null,"task_id":null,
        "client":{"uid":profile.owner_uid,"pid":null},
        "metadata":{"source":"provider_verified","received_at_ms":now},
        "payload":{"platform":profile.platform.name(),"account_id":profile.account_id,"message":message},
    })
}

fn flush_pending(profile: &mut Profile) -> Result<(), String> {
    let Some(record) = profile.pending.as_ref() else {
        return Ok(());
    };
    bound_rule(profile, true)?;
    super::app_events::append_once(record)?;
    let id = record["id"]
        .as_str()
        .ok_or("invalid pending channel event")?
        .to_string();
    let fingerprint = crate::crypto::sha256_hex(record["payload"].to_string().as_bytes());
    let at_ms = record["metadata"]["received_at_ms"]
        .as_u64()
        .ok_or("invalid channel receipt timestamp")?;
    profile.recent.insert(id, Seen { fingerprint, at_ms });
    profile.pending = None;
    store(profile)?;
    super::heartbeat::wake_activity_work();
    Ok(())
}

fn admit(profile: &mut Profile, incoming: &Incoming, now: u64) -> Result<Value, String> {
    if !profile.enabled {
        return Err("channel is stopped".into());
    }
    bound_rule(profile, true)?;
    flush_pending(profile)?;
    if incoming.sender_id != profile.sender_id || incoming.channel_id != profile.channel_id {
        return Ok(json!({"accepted":false,"reason":"sender_or_channel_mismatch"}));
    }
    let record = event_record(profile, incoming, now);
    let id = record["id"].as_str().unwrap();
    let fingerprint = crate::crypto::sha256_hex(record["payload"].to_string().as_bytes());
    if let Some(seen) = profile.recent.get(id) {
        if seen.fingerprint != fingerprint {
            return Err("channel event ID conflicts with retained authenticated data".into());
        }
        return Ok(json!({"accepted":true,"deduplicated":true,"event_id":id}));
    }
    if incoming.timestamp_ms.abs_diff(now) > FRESH_MS {
        return Ok(json!({"accepted":false,"reason":"stale_message"}));
    }
    if now < profile.window_ms || now.saturating_sub(profile.window_ms) >= 60_000 {
        if now < profile.window_ms {
            return Err("channel clock moved backwards".into());
        }
        profile.window_ms = now;
        profile.window_count = 0;
    }
    if profile.window_count >= 5 {
        return Err("channel rate limit: five new messages per minute".into());
    }
    profile
        .recent
        .retain(|_, seen| now.saturating_sub(seen.at_ms) <= FRESH_MS);
    profile.window_count += 1;
    profile.pending = Some(record.clone());
    // A failed append or lost reply cannot make the next request spend authority twice.
    store(profile)?;
    flush_pending(profile)?;
    Ok(json!({"accepted":true,"deduplicated":false,"event_id":id}))
}

fn received_event(profile: &Profile, id: &str, now: u64) -> Result<Incoming, String> {
    let path = crate::paths::context_events_log_path();
    let bytes = read_private(&path, 32 * 1024 * 1024)?.ok_or("channel event journal is absent")?;
    if !bytes.ends_with(b"\n") {
        return Err("channel event progress is indeterminate".into());
    }
    for line in bytes.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
        let event: Value =
            serde_json::from_slice(line).map_err(|_| "channel event journal is corrupt")?;
        if event["id"].as_str() != Some(id) {
            continue;
        }
        if event["client"]["uid"].as_u64() != Some(profile.owner_uid as u64)
            || event["source"].as_str() != Some(profile.platform.source().as_str())
            || event["metadata"]["source"] != "provider_verified"
            || event["payload"]["account_id"] != profile.account_id
        {
            return Err("channel reply does not belong to this owner, provider and account".into());
        }
        let message: Incoming = serde_json::from_value(event["payload"]["message"].clone())
            .map_err(|_| "invalid channel message receipt")?;
        if message.sender_id != profile.sender_id
            || message.channel_id != profile.channel_id
            || message.timestamp_ms > now
            || now - message.timestamp_ms > REPLY_MS
        {
            return Err("channel reply binding changed or its 24-hour window expired".into());
        }
        return Ok(message);
    }
    Err("unknown authenticated channel event".into())
}

async fn reply(uid: u32, platform: Platform, id: &str, text: &str) -> Result<Value, String> {
    let maximum = if platform == Platform::Discord {
        2000
    } else {
        4000
    };
    if text.trim().is_empty() || text.chars().count() > maximum {
        return Err(format!(
            "channel reply must contain 1..{maximum} characters"
        ));
    }
    let fingerprint = crate::crypto::sha256_hex(text.as_bytes());
    let (profile, incoming) = {
        let _lock = state_lock().lock().await;
        let mut profile = load(uid, platform)?.ok_or("channel is not configured")?;
        if let Some(previous) = profile.replies.get(id) {
            if previous.fingerprint != fingerprint {
                return Err("this event is already bound to different reply text".into());
            }
            return previous.result.clone().ok_or_else(|| {
                "reply submission is indeterminate; automatic resend is refused".into()
            });
        }
        if !profile.enabled {
            return Err("channel is stopped".into());
        }
        bound_rule(&profile, true)?;
        let incoming = received_event(&profile, id, Utc::now().timestamp_millis() as u64)?;
        if profile.replies.len() >= MAX_REPLIES {
            return Err("channel reply receipts reached their retained bound".into());
        }
        profile.replies.insert(
            id.into(),
            Reply {
                fingerprint,
                result: None,
            },
        );
        store(&profile)?;
        (profile, incoming)
    };
    let result = providers::send_reply(&profile, &incoming, text)
        .await
        .map_err(|_| {
            "reply submission is indeterminate; inspect the provider before any manual resend"
        })?;
    let _lock = state_lock().lock().await;
    let mut latest = load(uid, platform)?.ok_or("channel state disappeared after reply")?;
    let receipt = latest
        .replies
        .get_mut(id)
        .ok_or("channel reply receipt disappeared")?;
    receipt.result = Some(
        json!({"ok":true,"platform":platform.name(),"event_id":id,"submitted":true,"delivery_confirmed":false,"provider_result":result}),
    );
    let response = receipt.result.clone().unwrap();
    store(&latest)?;
    Ok(response)
}

pub fn spawn() {
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    POLLING.store(true, Ordering::Relaxed);
    for platform in [Platform::Discord, Platform::Telegram] {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(3));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                let snapshots = {
                    let _lock = state_lock().lock().await;
                    match profiles() {
                        Ok(profiles) => profiles,
                        Err(_) => {
                            tracing::error!(
                                "channel state is unavailable; managed polling is held"
                            );
                            Vec::new()
                        }
                    }
                };
                for profile in snapshots
                    .into_iter()
                    .filter(|p| p.enabled && p.platform == platform)
                {
                    let result = providers::poll(&profile).await;
                    let _lock = state_lock().lock().await;
                    let Ok(Some(mut latest)) = load(profile.owner_uid, platform) else {
                        continue;
                    };
                    if !latest.enabled || latest.revision != profile.revision {
                        continue;
                    }
                    let applied = (|| -> Result<(), String> {
                        bound_rule(&latest, true)?;
                        flush_pending(&mut latest)?;
                        let (messages, baseline) = result?;
                        if let Some(cursor) = baseline {
                            latest.cursor = Some(cursor);
                            store(&latest)?;
                        }
                        for (cursor, message) in messages {
                            if let Some(message) = message {
                                admit(&mut latest, &message, Utc::now().timestamp_millis() as u64)?;
                            }
                            latest.cursor = Some(cursor);
                            store(&latest)?;
                        }
                        Ok(())
                    })();
                    latest.last_error = applied.err();
                    let _ = store(&latest);
                }
            }
        });
    }
    providers::spawn_webhooks();
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/channels.rs"
    ));
}
