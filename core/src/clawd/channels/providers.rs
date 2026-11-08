use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path as HttpPath, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use hmac::{Hmac, Mac};
use reqwest::{Method, RequestBuilder};
use sha2::Sha256;

use super::*;

const MAX_HTTP_BYTES: usize = 64 * 1024;
const MAX_PROVIDER_BYTES: usize = 1024 * 1024;

fn decimal(value: &str, signed: bool) -> bool {
    let digits = if signed {
        value.strip_prefix('-').unwrap_or(value)
    } else {
        value
    };
    !digits.is_empty() && digits.len() <= 20 && digits.bytes().all(|b| b.is_ascii_digit())
}

fn slack_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64 && value.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn bsuid(value: &str) -> bool {
    let Some((country, value)) = value.split_once('.') else {
        return false;
    };
    let value = value.strip_prefix("ENT.").unwrap_or(value);
    country.len() == 2
        && country.bytes().all(|b| b.is_ascii_uppercase())
        && !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|b| b.is_ascii_alphanumeric())
}

pub(super) fn validate_identifiers(
    platform: Platform,
    account: &str,
    channel: &str,
    sender: &str,
) -> Result<(), String> {
    let valid = match platform {
        Platform::Discord => [account, channel, sender]
            .into_iter()
            .all(|v| decimal(v, false)),
        Platform::Telegram => {
            decimal(account, false) && decimal(channel, true) && decimal(sender, false)
        }
        Platform::Slack => {
            account
                .split_once('/')
                .is_some_and(|(team, app)| slack_id(team) && slack_id(app))
                && slack_id(channel)
                && slack_id(sender)
        }
        Platform::Whatsapp => {
            decimal(account, false)
                && decimal(channel, false)
                && (decimal(sender, false) || bsuid(sender))
        }
    };
    if !valid {
        return Err("invalid exact provider account, channel or sender identifier".into());
    }
    Ok(())
}

fn load_credential(profile: &Profile, home: &Path, name: &str) -> Result<String, String> {
    if !profile.platform.credentials().contains(&name) {
        return Err("credential is not declared by this channel".into());
    }
    let value = crate::credential::load_for_scheduler(
        name,
        "default",
        home,
        profile.owner_uid,
        profile.credential_tier,
    )
    .map_err(|_| format!("owner channel credential {name} is unavailable"))?;
    if value.is_empty() || value.len() > 4096 {
        return Err("channel credential has an invalid length".into());
    }
    Ok(value)
}

pub(super) fn bind_credentials(profile: &mut Profile, home: &Path) -> Result<(), String> {
    for name in profile.platform.credentials() {
        let value = load_credential(profile, home, name)?;
        profile
            .credential_fingerprints
            .insert((*name).into(), crate::crypto::sha256_hex(value.as_bytes()));
    }
    Ok(())
}

fn credential(profile: &Profile, home: &Path, name: &str) -> Result<String, String> {
    let value = load_credential(profile, home, name)?;
    if profile.credential_fingerprints.get(name)
        != Some(&crate::crypto::sha256_hex(value.as_bytes()))
    {
        return Err(
            "channel credential changed; stop and explicitly reconfigure the account binding"
                .into(),
        );
    }
    Ok(value)
}

fn home(profile: &Profile) -> Result<PathBuf, String> {
    super::super::system_caps::verified_owner_home(profile.owner_uid).map_err(|e| e.to_string())
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .user_agent("ClawOS-channel-ingress/0.1")
        .build()
        .map_err(|_| "channel transport is unavailable".into())
}

async fn request(request: RequestBuilder) -> Result<Value, String> {
    // Telegram embeds its token in the URL. Never expose reqwest's URL-bearing errors.
    let mut response = request
        .send()
        .await
        .map_err(|_| "channel provider request failed")?;
    if !response.status().is_success() {
        return Err("channel provider refused the request".into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "channel provider response failed")?
    {
        if body.len() + chunk.len() > MAX_PROVIDER_BYTES {
            return Err("channel provider response exceeds 1 MiB".into());
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| "channel provider returned invalid JSON".into())
}

async fn discord(
    profile: &Profile,
    method: Method,
    route: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    let token = credential(profile, &home(profile)?, "discord_bot_token")?;
    let request = client()?
        .request(method, format!("https://discord.com/api/v10{route}"))
        .header("Authorization", format!("Bot {}", token.trim()));
    request_json(request, body).await
}

async fn request_json(
    request_builder: RequestBuilder,
    body: Option<Value>,
) -> Result<Value, String> {
    request(if let Some(body) = body {
        request_builder.json(&body)
    } else {
        request_builder
    })
    .await
}

async fn telegram(profile: &Profile, method: &str, body: Value) -> Result<Value, String> {
    let token = credential(profile, &home(profile)?, "telegram_bot_token")?;
    let mut url = reqwest::Url::parse("https://api.telegram.org")
        .map_err(|_| "invalid Telegram API origin")?;
    url.path_segments_mut()
        .map_err(|_| "invalid Telegram API origin")?
        .push(&format!("bot{}", token.trim()))
        .push(method);
    let result = request(client()?.post(url).json(&body)).await?;
    if result["ok"] != true {
        return Err("Telegram refused the channel operation".into());
    }
    Ok(result["result"].clone())
}

async fn slack(profile: &Profile, method: &str, body: Value) -> Result<Value, String> {
    let token = credential(profile, &home(profile)?, "slack_bot_token")?;
    let result = request(
        client()?
            .post(format!("https://slack.com/api/{method}"))
            .bearer_auth(token.trim())
            .json(&body),
    )
    .await?;
    if result["ok"] != true {
        return Err("Slack refused the channel operation".into());
    }
    Ok(result)
}

async fn whatsapp(
    profile: &Profile,
    method: Method,
    route: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    let token = credential(profile, &home(profile)?, "whatsapp_access_token")?;
    request_json(
        client()?
            .request(method, format!("https://graph.facebook.com/v21.0/{route}"))
            .bearer_auth(token.trim()),
        body,
    )
    .await
}

pub(super) async fn verify_account(profile: &Profile, verified_home: &Path) -> Result<(), String> {
    for name in profile.platform.credentials() {
        credential(profile, verified_home, name)?;
    }
    let matches = match profile.platform {
        Platform::Discord => {
            let bot = discord(profile, Method::GET, "/users/@me", None).await?;
            let channel = discord(
                profile,
                Method::GET,
                &format!("/channels/{}", profile.channel_id),
                None,
            )
            .await?;
            bot["bot"] == true
                && bot["id"] == profile.account_id
                && channel["id"] == profile.channel_id
        }
        Platform::Telegram => {
            let bot = telegram(profile, "getMe", json!({})).await?;
            bot["is_bot"] == true
                && bot["id"]
                    .as_u64()
                    .is_some_and(|id| id.to_string() == profile.account_id)
        }
        Platform::Slack => {
            let auth = slack(profile, "auth.test", json!({})).await?;
            auth["team_id"].as_str() == profile.account_id.split_once('/').map(|(team, _)| team)
        }
        Platform::Whatsapp => {
            let number = credential(profile, verified_home, "whatsapp_phone_number_id")?;
            if number.trim() != profile.channel_id {
                return Err(
                    "WhatsApp credential does not match the configured sending number ID".into(),
                );
            }
            let account = whatsapp(
                profile,
                Method::GET,
                &format!("{}/phone_numbers?limit=100", profile.account_id),
                None,
            )
            .await?;
            account["data"]
                .as_array()
                .is_some_and(|numbers| numbers.iter().any(|n| n["id"] == profile.channel_id))
        }
    };
    if !matches {
        return Err(
            "authenticated channel credentials do not match the configured provider account".into(),
        );
    }
    Ok(())
}

fn bounded_text(value: &Value) -> Result<(String, bool), String> {
    let text = value
        .as_str()
        .ok_or("incoming channel message must contain text")?;
    if text.trim().is_empty() {
        return Err("incoming channel text is empty".into());
    }
    let end = text
        .char_indices()
        .take_while(|(i, c)| *i + c.len_utf8() <= 8192)
        .map(|(i, c)| i + c.len_utf8())
        .last()
        .unwrap_or(0);
    Ok((text[..end].to_string(), end < text.len()))
}

fn string(value: &Value, field: &str) -> Result<String, String> {
    let value = value
        .as_str()
        .ok_or_else(|| format!("invalid channel {field}"))?;
    if value.is_empty() || value.len() > 192 || value.chars().any(char::is_control) {
        return Err(format!("invalid channel {field}"));
    }
    Ok(value.to_string())
}

fn timestamp(value: &Value) -> Result<u64, String> {
    let seconds = value
        .as_u64()
        .or_else(|| value.as_str().and_then(|v| v.parse().ok()))
        .ok_or("invalid channel message timestamp")?;
    seconds
        .checked_mul(1000)
        .ok_or_else(|| "invalid channel timestamp".into())
}

fn discord_message(value: &Value) -> Result<Option<Incoming>, String> {
    if value["author"]["bot"] == true
        || !value["content"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty())
    {
        return Ok(None);
    }
    let (text, truncated) = bounded_text(&value["content"])?;
    let message_id = string(&value["id"], "message ID")?;
    if !decimal(&message_id, false) {
        return Err("invalid Discord message ID".into());
    }
    let observed = value["timestamp"]
        .as_str()
        .ok_or("missing Discord timestamp")?;
    let timestamp_ms = chrono::DateTime::parse_from_rfc3339(observed)
        .map_err(|_| "invalid Discord timestamp")?
        .timestamp_millis()
        .try_into()
        .map_err(|_| "invalid Discord timestamp")?;
    Ok(Some(Incoming {
        message_id,
        sender_id: string(&value["author"]["id"], "sender ID")?,
        channel_id: string(&value["channel_id"], "channel ID")?,
        thread_id: None,
        timestamp_ms,
        text,
        truncated,
    }))
}

fn telegram_message(value: &Value) -> Result<Option<Incoming>, String> {
    let message = &value["message"];
    if message.is_null()
        || message["from"]["is_bot"] == true
        || message["sender_chat"].is_object()
        || !message["text"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty())
    {
        return Ok(None);
    }
    let (text, truncated) = bounded_text(&message["text"])?;
    Ok(Some(Incoming {
        message_id: message["message_id"]
            .as_u64()
            .ok_or("invalid Telegram message ID")?
            .to_string(),
        sender_id: message["from"]["id"]
            .as_u64()
            .ok_or("invalid Telegram sender ID")?
            .to_string(),
        channel_id: message["chat"]["id"]
            .as_i64()
            .ok_or("invalid Telegram chat ID")?
            .to_string(),
        thread_id: message["message_thread_id"].as_u64().map(|n| n.to_string()),
        timestamp_ms: timestamp(&message["date"])?,
        text,
        truncated,
    }))
}

type PollPage = (Vec<(String, Option<Incoming>)>, Option<String>);

pub(super) async fn poll(profile: &Profile) -> Result<PollPage, String> {
    match profile.platform {
        Platform::Discord => {
            let query = profile
                .cursor
                .as_ref()
                .map(|cursor| format!("after={cursor}&limit=100"))
                .unwrap_or_else(|| "limit=1".into());
            let result = discord(
                profile,
                Method::GET,
                &format!("/channels/{}/messages?{query}", profile.channel_id),
                None,
            )
            .await?;
            let messages = result.as_array().ok_or("invalid Discord poll response")?;
            if messages.len() > 100 {
                return Err("Discord poll page exceeds 100 messages".into());
            }
            let mut sorted = Vec::new();
            for message in messages {
                let id = message["id"]
                    .as_str()
                    .and_then(|v| v.parse::<u64>().ok())
                    .ok_or("invalid Discord poll cursor")?;
                sorted.push((id, message));
            }
            sorted.sort_by_key(|(id, _)| *id);
            if profile.cursor.is_none() {
                return Ok((
                    Vec::new(),
                    Some(
                        sorted
                            .last()
                            .map(|(id, _)| id.to_string())
                            .unwrap_or_else(|| "0".into()),
                    ),
                ));
            }
            let cursor = profile
                .cursor
                .as_ref()
                .unwrap()
                .parse::<u64>()
                .map_err(|_| "invalid Discord cursor; reset is refused")?;
            sorted
                .into_iter()
                .filter(|(id, _)| *id > cursor)
                .map(|(id, message)| Ok((id.to_string(), discord_message(message)?)))
                .collect::<Result<Vec<_>, String>>()
                .map(|messages| (messages, None))
        }
        Platform::Telegram => {
            // A first activation deliberately establishes an inert latest-update baseline.
            let offset = profile
                .cursor
                .as_deref()
                .unwrap_or("-1")
                .parse::<i64>()
                .map_err(|_| "invalid Telegram cursor")?;
            let result = telegram(
                profile,
                "getUpdates",
                json!({"offset":offset,"limit":100,"timeout":0,"allowed_updates":["message"]}),
            )
            .await?;
            let updates = result.as_array().ok_or("invalid Telegram poll response")?;
            if updates.len() > 100 {
                return Err("Telegram poll page exceeds 100 updates".into());
            }
            let mut sorted = updates
                .iter()
                .map(|value| {
                    let id = value["update_id"]
                        .as_u64()
                        .filter(|id| *id < i64::MAX as u64)
                        .ok_or("invalid Telegram poll cursor")?;
                    Ok((id, value))
                })
                .collect::<Result<Vec<_>, String>>()?;
            sorted.sort_by_key(|(id, _)| *id);
            if profile.cursor.is_none() {
                return Ok((
                    Vec::new(),
                    Some(sorted.last().map(|(id, _)| id + 1).unwrap_or(0).to_string()),
                ));
            }
            sorted
                .into_iter()
                .filter(|(id, _)| *id >= offset as u64)
                .map(|(id, update)| Ok(((id + 1).to_string(), telegram_message(update)?)))
                .collect::<Result<Vec<_>, String>>()
                .map(|messages| (messages, None))
        }
        _ => Err("this channel uses authenticated webhooks, not polling".into()),
    }
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, String> {
    if headers.get_all(name).iter().count() != 1 {
        return Err("missing or ambiguous channel signature header".into());
    }
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| "invalid channel signature header".into())
}

fn verify_hmac(secret: &[u8], signature: &str, prefix: &str, input: &[u8]) -> Result<(), String> {
    let signature = signature
        .strip_prefix(prefix)
        .ok_or("invalid channel signature version")?;
    if signature.len() != 64 {
        return Err("invalid channel signature length".into());
    }
    let signature = hex::decode(signature).map_err(|_| "invalid channel signature encoding")?;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret).map_err(|_| "invalid channel signing key")?;
    mac.update(input);
    mac.verify_slice(&signature)
        .map_err(|_| "channel signature verification failed".into())
}

fn verify_signature(
    platform: Platform,
    secret: &str,
    headers: &HeaderMap,
    raw: &[u8],
    now: u64,
) -> Result<(), String> {
    if platform == Platform::Slack {
        let timestamp = header(headers, "x-slack-request-timestamp")?;
        let time = timestamp
            .parse::<u64>()
            .ok()
            .and_then(|n| n.checked_mul(1000))
            .ok_or("invalid Slack timestamp")?;
        if time.abs_diff(now) > FRESH_MS {
            return Err("Slack request timestamp is outside its five-minute window".into());
        }
        let mut signed = format!("v0:{timestamp}:").into_bytes();
        signed.extend_from_slice(raw);
        verify_hmac(
            secret.as_bytes(),
            header(headers, "x-slack-signature")?,
            "v0=",
            &signed,
        )
    } else if platform == Platform::Whatsapp {
        verify_hmac(
            secret.as_bytes(),
            header(headers, "x-hub-signature-256")?,
            "sha256=",
            raw,
        )
    } else {
        Err("polling channels do not accept HTTP message reports".into())
    }
}

fn slack_messages(profile: &Profile, value: &Value) -> Result<Vec<Incoming>, String> {
    let (team, app) = profile
        .account_id
        .split_once('/')
        .ok_or("invalid Slack account binding")?;
    if value["team_id"] != team || value["api_app_id"] != app {
        return Err("Slack workspace or App binding mismatch".into());
    }
    if value["type"] != "event_callback" {
        return Ok(Vec::new());
    }
    let event = &value["event"];
    if event["type"] != "message" || event["subtype"].is_string() || event["bot_id"].is_string() {
        return Ok(Vec::new());
    }
    let (text, truncated) = bounded_text(&event["text"])?;
    let ts = string(&event["ts"], "Slack timestamp")?;
    let seconds = ts
        .split_once('.')
        .map(|(s, _)| s)
        .unwrap_or(&ts)
        .parse::<u64>()
        .map_err(|_| "invalid Slack message timestamp")?;
    Ok(vec![Incoming {
        message_id: string(&value["event_id"], "Slack event ID")?,
        sender_id: string(&event["user"], "Slack sender")?,
        channel_id: string(&event["channel"], "Slack channel")?,
        thread_id: Some(string(
            event.get("thread_ts").unwrap_or(&event["ts"]),
            "Slack thread",
        )?),
        timestamp_ms: seconds.checked_mul(1000).ok_or("invalid Slack timestamp")?,
        text,
        truncated,
    }])
}

fn whatsapp_messages(profile: &Profile, value: &Value) -> Result<Vec<Incoming>, String> {
    if value["object"] != "whatsapp_business_account" {
        return Err("not a WhatsApp business webhook".into());
    }
    let mut messages = Vec::new();
    for entry in value["entry"]
        .as_array()
        .ok_or("invalid WhatsApp webhook entries")?
    {
        if entry["id"] != profile.account_id {
            return Err("WhatsApp business account mismatch".into());
        }
        for change in entry["changes"]
            .as_array()
            .ok_or("invalid WhatsApp changes")?
        {
            if change["field"] != "messages" {
                continue;
            }
            let value = &change["value"];
            if value["metadata"]["phone_number_id"] != profile.channel_id {
                return Err("WhatsApp sending number mismatch".into());
            }
            for message in value["messages"].as_array().into_iter().flatten() {
                if message["type"] != "text" {
                    continue;
                }
                let sender = if bsuid(&profile.sender_id) {
                    &message["from_user_id"]
                } else {
                    &message["from"]
                };
                let (text, truncated) = bounded_text(&message["text"]["body"])?;
                messages.push(Incoming {
                    message_id: string(&message["id"], "WhatsApp message ID")?,
                    sender_id: string(sender, "WhatsApp sender")?,
                    channel_id: profile.channel_id.clone(),
                    thread_id: None,
                    timestamp_ms: timestamp(&message["timestamp"])?,
                    text,
                    truncated,
                });
                if messages.len() > 100 {
                    return Err("WhatsApp webhook exceeds 100 text messages".into());
                }
            }
        }
    }
    Ok(messages)
}

fn reply_body(platform: Platform, incoming: &Incoming, text: &str) -> Value {
    match platform {
        Platform::Discord => json!({
            "content":text,"message_reference":{"message_id":incoming.message_id,"fail_if_not_exists":true},
            "allowed_mentions":{"parse":[],"replied_user":false},
        }),
        Platform::Telegram => {
            let mut body = json!({"chat_id":incoming.channel_id,"text":text,"reply_parameters":{"message_id":incoming.message_id.parse::<u64>().unwrap_or(0)}});
            if let Some(thread) = &incoming.thread_id {
                body["message_thread_id"] = json!(thread.parse::<u64>().unwrap_or(0));
            }
            body
        }
        Platform::Slack => {
            json!({"channel":incoming.channel_id,"thread_ts":incoming.thread_id,"text":text,"unfurl_links":false,"unfurl_media":false})
        }
        Platform::Whatsapp => {
            let mut body = json!({"messaging_product":"whatsapp","recipient_type":"individual","type":"text",
                "context":{"message_id":incoming.message_id},"text":{"body":text,"preview_url":false}});
            // BSUID is opaque. Its country prefix and all characters must survive unchanged.
            body[if bsuid(&incoming.sender_id) {
                "recipient"
            } else {
                "to"
            }] = json!(incoming.sender_id);
            body
        }
    }
}

pub(super) async fn send_reply(
    profile: &Profile,
    incoming: &Incoming,
    text: &str,
) -> Result<Value, String> {
    let body = reply_body(profile.platform, incoming, text);
    let result = match profile.platform {
        Platform::Discord => {
            discord(
                profile,
                Method::POST,
                &format!("/channels/{}/messages", incoming.channel_id),
                Some(body),
            )
            .await?
        }
        Platform::Telegram => telegram(profile, "sendMessage", body).await?,
        Platform::Slack => slack(profile, "chat.postMessage", body).await?,
        Platform::Whatsapp => {
            whatsapp(
                profile,
                Method::POST,
                &format!("{}/messages", profile.channel_id),
                Some(body),
            )
            .await?
        }
    };
    let id = match profile.platform {
        Platform::Discord => result["id"].clone(),
        Platform::Telegram => result["message_id"].clone(),
        Platform::Slack => result["ts"].clone(),
        Platform::Whatsapp => result["messages"][0]["id"].clone(),
    };
    if id.is_null() {
        return Err("channel provider returned no submission identifier".into());
    }
    Ok(json!({"message_id":id}))
}

async fn webhook(
    State(slots): State<Arc<tokio::sync::Semaphore>>,
    HttpPath((uid, platform)): HttpPath<(u32, String)>,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let Ok(_slot) = slots.try_acquire() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let result = async {
        let platform = Platform::parse(&platform)?;
        if platform.polling() || uid == 0 || headers.contains_key("content-encoding") {
            return Err("unsupported channel webhook".to_string());
        }
        let _lock = state_lock().lock().await;
        let mut profile = load(uid, platform)?
            .filter(|p| p.enabled)
            .ok_or("channel is not enabled")?;
        let secret_name = if platform == Platform::Slack {
            "slack_signing_secret"
        } else {
            "whatsapp_app_secret"
        };
        let secret = credential(&profile, &home(&profile)?, secret_name)?;
        let now = Utc::now().timestamp_millis() as u64;
        verify_signature(platform, &secret, &headers, &raw, now)?;
        let value: Value =
            serde_json::from_slice(&raw).map_err(|_| "invalid channel webhook JSON")?;
        if platform == Platform::Slack && value["type"] == "url_verification" {
            let challenge = string(&value["challenge"], "Slack challenge")?;
            return Ok(json!({"challenge":challenge}));
        }
        let messages = if platform == Platform::Slack {
            slack_messages(&profile, &value)?
        } else {
            whatsapp_messages(&profile, &value)?
        };
        for message in messages {
            admit(&mut profile, &message, now)?;
        }
        Ok::<_, String>(json!({"accepted":true}))
    };
    match tokio::time::timeout(Duration::from_secs(5), result).await {
        Ok(Ok(result)) => axum::Json(result).into_response(),
        Ok(Err(error)) if error.starts_with("channel rate limit:") => {
            StatusCode::TOO_MANY_REQUESTS.into_response()
        }
        Ok(Err(error)) if error.contains("indeterminate") || error.contains("journal") => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
        Ok(Err(_)) => StatusCode::FORBIDDEN.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn verify_whatsapp(
    HttpPath((uid, platform)): HttpPath<(u32, String)>,
    RawQuery(query): RawQuery,
) -> Response {
    let result = (|| -> Result<String, String> {
        if platform != "whatsapp" || uid == 0 {
            return Err("unsupported verification".into());
        }
        let profile = load(uid, Platform::Whatsapp)?
            .filter(|p| p.enabled)
            .ok_or("channel is not enabled")?;
        let mut params = HashMap::new();
        for (key, value) in
            reqwest::Url::parse(&format!("https://localhost/?{}", query.unwrap_or_default()))
                .map_err(|_| "invalid verification query")?
                .query_pairs()
        {
            if params.insert(key.to_string(), value.to_string()).is_some() {
                return Err("ambiguous verification query".into());
            }
        }
        if params.get("hub.mode").map(String::as_str) != Some("subscribe") {
            return Err("unsupported verification mode".into());
        }
        let token = credential(&profile, &home(&profile)?, "whatsapp_verify_token")?;
        let supplied = params
            .get("hub.verify_token")
            .ok_or("missing verification token")?;
        let mut mac =
            Hmac::<Sha256>::new_from_slice(token.as_bytes()).map_err(|_| "invalid verify token")?;
        mac.update(b"whatsapp-channel-verification");
        let mut other = Hmac::<Sha256>::new_from_slice(supplied.as_bytes())
            .map_err(|_| "invalid verify token")?;
        other.update(b"whatsapp-channel-verification");
        mac.verify_slice(&other.finalize().into_bytes())
            .map_err(|_| "verify token mismatch")?;
        let challenge = params
            .get("hub.challenge")
            .ok_or("missing verification challenge")?;
        if !decimal(challenge, false) {
            return Err("invalid verification challenge".into());
        }
        Ok(challenge.clone())
    })();
    match result {
        Ok(challenge) => challenge.into_response(),
        Err(_) => StatusCode::FORBIDDEN.into_response(),
    }
}

pub(super) fn spawn_webhooks() {
    tokio::spawn(async {
        // Public TLS termination belongs to the operator, not caller-selected App URLs.
        let listener = match tokio::net::TcpListener::bind(("127.0.0.1", 9743)).await {
            Ok(listener) => listener,
            Err(_) => {
                tracing::error!("managed channel webhook listener is unavailable");
                return;
            }
        };
        let router = Router::new()
            .route(
                "/channels/{uid}/{platform}",
                get(verify_whatsapp).post(webhook),
            )
            .layer(DefaultBodyLimit::max(MAX_HTTP_BYTES))
            .with_state(Arc::new(tokio::sync::Semaphore::new(8)));
        WEBHOOKS.store(true, Ordering::Relaxed);
        let _ = axum::serve(listener, router).await;
        WEBHOOKS.store(false, Ordering::Relaxed);
    });
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/channels/providers.rs"
    ));
}
