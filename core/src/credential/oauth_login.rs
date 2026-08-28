//! Interactive OAuth login for installed applications.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Stdio;
use std::time::{Duration, Instant};

use base64::engine::{general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{json, Value};

use crate::caps::Verb;

const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_SCOPES: &str = "openid email https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/gmail.send https://www.googleapis.com/auth/calendar.events";
const GOOGLE_REQUIRED_SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/gmail.readonly",
    "https://www.googleapis.com/auth/gmail.send",
    "https://www.googleapis.com/auth/calendar.events",
];
const MICROSOFT_SCOPES: &str =
    "offline_access openid email User.Read Mail.Read Mail.Send Calendars.ReadWrite";
const DEFAULT_TIMEOUT_SECS: u64 = 300;
const MAX_CALLBACK_BYTES: usize = 16 * 1024;
const APP_ACCESS_TOKEN_TIER: u8 = 2;
const REFRESH_TOKEN_TIER: u8 = 0;

pub(super) fn cmd_oauth_login(
    store: &dyn super::CredentialStore,
    args: &[String],
) -> Result<Value, String> {
    let direct_cli = crate::proc::current_session_info_for_caps()
        .is_some_and(|session| is_direct_oauth_login_session(&session));
    if !direct_cli {
        return Err(
            "interactive OAuth login must be run directly in the user's terminal".to_string(),
        );
    }
    run_oauth_login(store, args)
}

pub(super) fn cmd_agent_oauth_login(
    store: &dyn super::CredentialStore,
    args: &[String],
) -> Result<Value, String> {
    let attended_agent = crate::proc::current_session_info_for_caps()
        .is_some_and(|session| is_attended_agent_oauth_session(&session));
    if !attended_agent {
        return Err(
            "Agent-initiated OAuth requires an attended local `cos agent ask`, \
             `cos agent live`, or `cos agent chat` session"
                .to_string(),
        );
    }
    run_oauth_login(store, args)
}

fn run_oauth_login(store: &dyn super::CredentialStore, args: &[String]) -> Result<Value, String> {
    let (namespace, provider, no_open, timeout_secs) = parse_args(args)?;
    match provider.as_str() {
        "google" => google_login(store, &namespace, no_open, timeout_secs),
        "microsoft" => microsoft_login(store, &namespace, no_open, timeout_secs),
        _ => Err(format!(
            "unsupported OAuth login provider: {provider}. supported: google, microsoft"
        )),
    }
}

pub(super) fn is_same_pid_admin_cli_session(session: &crate::proc::SessionInfo) -> bool {
    session.pid == std::process::id()
        && session.app_id.is_none()
        && session.role.as_deref() == Some(crate::caps::Role::Admin.name())
}

fn is_direct_oauth_login_session(session: &crate::proc::SessionInfo) -> bool {
    is_same_pid_admin_cli_session(session)
        && session
            .command
            .windows(2)
            .any(|args| args == ["credential", "oauth-login"])
}

fn is_attended_agent_oauth_session(session: &crate::proc::SessionInfo) -> bool {
    is_same_pid_admin_cli_session(session)
        && session
            .command
            .windows(2)
            .any(|args| args[0] == "agent" && matches!(args[1].as_str(), "ask" | "live" | "chat"))
}

fn parse_args(args: &[String]) -> Result<(String, String, bool, u64), String> {
    let mut namespace = "default".to_string();
    let mut provider = None;
    let mut no_open = false;
    let mut timeout_secs = DEFAULT_TIMEOUT_SECS;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--namespace" if i + 1 < args.len() => {
                namespace = args[i + 1].clone();
                i += 2;
            }
            "--no-open" => {
                no_open = true;
                i += 1;
            }
            "--timeout" if i + 1 < args.len() => {
                timeout_secs = args[i + 1]
                    .parse::<u64>()
                    .map_err(|_| "--timeout must be an integer number of seconds".to_string())?;
                if !(30..=900).contains(&timeout_secs) {
                    return Err("--timeout must be between 30 and 900 seconds".to_string());
                }
                i += 2;
            }
            value if value.starts_with("--") => {
                return Err(format!("unknown oauth-login option: {value}"));
            }
            value if provider.is_none() => {
                provider = Some(value.to_string());
                i += 1;
            }
            value => {
                return Err(format!("unexpected oauth-login argument: {value}"));
            }
        }
    }
    super::validate_credential_component("namespace", &namespace)?;
    let provider = provider.ok_or(
        "usage: cos credential oauth-login <google|microsoft> [--namespace NS] [--no-open] [--timeout SECS]",
    )?;
    Ok((namespace, provider, no_open, timeout_secs))
}

fn google_login(
    store: &dyn super::CredentialStore,
    namespace: &str,
    no_open: bool,
    timeout_secs: u64,
) -> Result<Value, String> {
    let (client_id, client_secret) = google_client_config(store, namespace)?;
    preflight_token_storage(namespace, &["GOOGLE_ACCESS_TOKEN", "GOOGLE_REFRESH_TOKEN"])?;

    let listener =
        TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("bind OAuth callback: {e}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("configure OAuth callback: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("inspect OAuth callback: {e}"))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}/oauth/callback");
    let (verifier, challenge) = pkce_pair()?;
    let state = random_token(32)?;
    let authorization_url =
        google_authorization_url(&client_id, &redirect_uri, &challenge, &state, GOOGLE_SCOPES);

    let browser_opened = !no_open && open_browser(&authorization_url);
    eprintln!("Open this URL to authorize Google:");
    eprintln!("{authorization_url}");
    eprintln!("Waiting for the browser callback on 127.0.0.1:{port} ...");

    let code = wait_for_callback(&listener, &state, Duration::from_secs(timeout_secs))?;
    let mut body = format!(
        "client_id={}&code={}&code_verifier={}&redirect_uri={}&grant_type=authorization_code",
        super::urlencoded(&client_id),
        super::urlencoded(&code),
        super::urlencoded(&verifier),
        super::urlencoded(&redirect_uri),
    );
    body.push_str("&client_secret=");
    body.push_str(&super::urlencoded(&client_secret));
    let raw = super::http_post(GOOGLE_TOKEN_URL, &body, "application/x-www-form-urlencoded")?;
    let token: Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse Google token response: {e}"))?;
    if let Some(error) = token.get("error").and_then(Value::as_str) {
        let description = token
            .get("error_description")
            .and_then(Value::as_str)
            .unwrap_or(error);
        return Err(format!("Google OAuth failed: {description}"));
    }
    let access_token = token
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| "Google token response did not contain access_token".to_string())?;
    let refresh_token = token.get("refresh_token").and_then(Value::as_str);
    let expires_in = token
        .get("expires_in")
        .and_then(Value::as_u64)
        .unwrap_or(3600);
    let granted_scopes = google_granted_scopes(&token)?;
    super::require_secret(
        Verb::SECRET_WRITE,
        super::credential_scope(namespace, "GOOGLE_ACCESS_TOKEN")?,
    )?;
    store_token(
        store,
        namespace,
        "GOOGLE_ACCESS_TOKEN",
        access_token,
        APP_ACCESS_TOKEN_TIER,
        Some(expires_in),
        Some(format!(
            "cos credential oauth-refresh google --namespace {namespace}"
        )),
    )?;

    if let Some(refresh_token) = refresh_token {
        super::require_secret(
            Verb::SECRET_WRITE,
            super::credential_scope(namespace, "GOOGLE_REFRESH_TOKEN")?,
        )?;
        store_token(
            store,
            namespace,
            "GOOGLE_REFRESH_TOKEN",
            refresh_token,
            REFRESH_TOKEN_TIER,
            None,
            None,
        )?;
    }

    Ok(json!({
        "provider": "google",
        "authorized": true,
        "namespace": namespace,
        "expires_in": expires_in,
        "refresh_token_stored": refresh_token.is_some(),
        "browser_opened": browser_opened,
        "redirect_uri": redirect_uri,
        "scopes": granted_scopes,
    }))
}

fn google_granted_scopes(token: &Value) -> Result<Vec<String>, String> {
    let granted = token
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or(GOOGLE_SCOPES)
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let missing = GOOGLE_REQUIRED_SCOPES
        .iter()
        .filter(|required| !granted.iter().any(|scope| scope == **required))
        .copied()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(format!(
            "Google authorization did not grant required scopes: {}",
            missing.join(", ")
        ));
    }
    Ok(granted)
}

fn microsoft_login(
    store: &dyn super::CredentialStore,
    namespace: &str,
    no_open: bool,
    timeout_secs: u64,
) -> Result<Value, String> {
    let (client_id, tenant_id) = microsoft_client_config(store, namespace)?;
    super::validate_credential_component("Microsoft tenant", &tenant_id)?;
    preflight_token_storage(
        namespace,
        &["MICROSOFT_ACCESS_TOKEN", "MICROSOFT_REFRESH_TOKEN"],
    )?;
    let device_url =
        format!("https://login.microsoftonline.com/{tenant_id}/oauth2/v2.0/devicecode");
    let token_url = format!("https://login.microsoftonline.com/{tenant_id}/oauth2/v2.0/token");
    let raw = super::http_post(
        &device_url,
        &format!(
            "client_id={}&scope={}",
            super::urlencoded(&client_id),
            super::urlencoded(MICROSOFT_SCOPES),
        ),
        "application/x-www-form-urlencoded",
    )?;
    let device: Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse Microsoft device response: {e}"))?;
    if let Some(error) = device.get("error").and_then(Value::as_str) {
        let description = device
            .get("error_description")
            .and_then(Value::as_str)
            .unwrap_or(error);
        return Err(format!("Microsoft OAuth failed: {description}"));
    }
    let device_code = device
        .get("device_code")
        .and_then(Value::as_str)
        .ok_or_else(|| "Microsoft device response did not contain device_code".to_string())?;
    let user_code = device
        .get("user_code")
        .and_then(Value::as_str)
        .ok_or_else(|| "Microsoft device response did not contain user_code".to_string())?;
    let verification_uri = device
        .get("verification_uri")
        .and_then(Value::as_str)
        .ok_or_else(|| "Microsoft device response did not contain verification_uri".to_string())?;
    let browser_url = device
        .get("verification_uri_complete")
        .and_then(Value::as_str)
        .unwrap_or(verification_uri);
    let expires_in = device
        .get("expires_in")
        .and_then(Value::as_u64)
        .unwrap_or(timeout_secs);
    let mut interval = device.get("interval").and_then(Value::as_u64).unwrap_or(5);
    let deadline = Instant::now() + Duration::from_secs(timeout_secs.min(expires_in));
    let browser_opened = !no_open && open_browser(browser_url);

    eprintln!("Open this URL to authorize Microsoft:");
    eprintln!("{verification_uri}");
    eprintln!("Enter code: {user_code}");

    let token = loop {
        if Instant::now() >= deadline {
            return Err("Microsoft OAuth login timed out".to_string());
        }
        std::thread::sleep(Duration::from_secs(interval));
        let raw = super::http_post(
            &token_url,
            &format!(
                "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&client_id={}&device_code={}",
                super::urlencoded(&client_id),
                super::urlencoded(device_code),
            ),
            "application/x-www-form-urlencoded",
        )?;
        let token: Value = serde_json::from_str(&raw)
            .map_err(|e| format!("parse Microsoft token response: {e}"))?;
        match token.get("error").and_then(Value::as_str) {
            None => break token,
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                interval = (interval + 5).min(30);
                continue;
            }
            Some("authorization_declined") | Some("access_denied") => {
                return Err("Microsoft authorization was denied".to_string());
            }
            Some("expired_token") | Some("bad_verification_code") => {
                return Err("Microsoft authorization code expired".to_string());
            }
            Some(error) => {
                let description = token
                    .get("error_description")
                    .and_then(Value::as_str)
                    .unwrap_or(error);
                return Err(format!("Microsoft OAuth failed: {description}"));
            }
        }
    };

    let access_token = token
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| "Microsoft token response did not contain access_token".to_string())?;
    let refresh_token = token
        .get("refresh_token")
        .and_then(Value::as_str)
        .ok_or_else(|| "Microsoft token response did not contain refresh_token".to_string())?;
    let access_expires = token
        .get("expires_in")
        .and_then(Value::as_u64)
        .unwrap_or(3600);
    for name in ["MICROSOFT_ACCESS_TOKEN", "MICROSOFT_REFRESH_TOKEN"] {
        super::require_secret(
            Verb::SECRET_WRITE,
            super::credential_scope(namespace, name)?,
        )?;
    }
    store_token(
        store,
        namespace,
        "MICROSOFT_REFRESH_TOKEN",
        refresh_token,
        REFRESH_TOKEN_TIER,
        None,
        None,
    )?;
    store_token(
        store,
        namespace,
        "MICROSOFT_ACCESS_TOKEN",
        access_token,
        APP_ACCESS_TOKEN_TIER,
        Some(access_expires),
        Some(format!(
            "cos credential oauth-refresh microsoft --namespace {namespace}"
        )),
    )?;

    Ok(json!({
        "provider": "microsoft",
        "authorized": true,
        "namespace": namespace,
        "expires_in": access_expires,
        "refresh_token_stored": true,
        "browser_opened": browser_opened,
        "verification_uri": verification_uri,
        "scopes": MICROSOFT_SCOPES.split_whitespace().collect::<Vec<_>>(),
    }))
}

fn preflight_token_storage(namespace: &str, names: &[&str]) -> Result<(), String> {
    for name in names {
        super::require_secret(
            Verb::SECRET_WRITE,
            super::credential_scope(namespace, name)?,
        )?;
    }
    Ok(())
}

fn store_token(
    store: &dyn super::CredentialStore,
    namespace: &str,
    name: &str,
    value: &str,
    min_tier: u8,
    ttl: Option<u64>,
    refresh_cmd: Option<String>,
) -> Result<(), String> {
    let id = super::CredentialId::parse(namespace, name)?;
    store.store(super::StoreRequest {
        id: &id,
        value,
        min_tier,
        ttl,
        refresh_cmd,
    })?;
    Ok(())
}

pub(super) fn google_client_config(
    store: &dyn super::CredentialStore,
    namespace: &str,
) -> Result<(String, String), String> {
    let client_id = client_setting(
        store,
        "COS_GOOGLE_OAUTH_CLIENT_ID",
        "GOOGLE_CLIENT_ID",
        namespace,
    )?
    .ok_or_else(|| {
        format!(
            "Google OAuth client is not configured. Store GOOGLE_CLIENT_ID in \
             credential namespace `{namespace}` or set COS_GOOGLE_OAUTH_CLIENT_ID, \
             then retry Google authorization. Configure OAuth client values through \
             trusted system settings, not model chat."
        )
    })?;
    let client_secret = client_setting(
        store,
        "COS_GOOGLE_OAUTH_CLIENT_SECRET",
        "GOOGLE_CLIENT_SECRET",
        namespace,
    )?
    .ok_or_else(|| {
        format!(
            "Google OAuth client secret is not configured. Store \
             GOOGLE_CLIENT_SECRET in credential namespace `{namespace}` or set \
             COS_GOOGLE_OAUTH_CLIENT_SECRET, then retry Google authorization. \
             Configure OAuth client values through trusted system settings, not \
             model chat."
        )
    })?;
    Ok((client_id, client_secret))
}

pub(super) fn google_client_config_for_daemon(
    store: &dyn super::CredentialStore,
    namespace: &str,
) -> Result<(String, String), String> {
    let client_id = daemon_client_setting(
        store,
        "COS_GOOGLE_OAUTH_CLIENT_ID",
        "GOOGLE_CLIENT_ID",
        namespace,
    )?
    .ok_or_else(|| "Google OAuth client id is not configured".to_string())?;
    let client_secret = daemon_client_setting(
        store,
        "COS_GOOGLE_OAUTH_CLIENT_SECRET",
        "GOOGLE_CLIENT_SECRET",
        namespace,
    )?
    .ok_or_else(|| "Google OAuth client secret is not configured".to_string())?;
    Ok((client_id, client_secret))
}

pub(super) fn microsoft_client_config(
    store: &dyn super::CredentialStore,
    namespace: &str,
) -> Result<(String, String), String> {
    let client_id = client_setting(
        store,
        "COS_MICROSOFT_OAUTH_CLIENT_ID",
        "MICROSOFT_CLIENT_ID",
        namespace,
    )?
    .ok_or_else(|| {
        format!(
            "Microsoft OAuth client is not configured. Store MICROSOFT_CLIENT_ID \
             in credential namespace `{namespace}` or set \
             COS_MICROSOFT_OAUTH_CLIENT_ID, then retry Microsoft authorization. \
             Configure OAuth client values through trusted system settings, not \
             model chat."
        )
    })?;
    let tenant_id = client_setting(
        store,
        "COS_MICROSOFT_OAUTH_TENANT_ID",
        "MICROSOFT_TENANT_ID",
        namespace,
    )?
    .unwrap_or_else(|| "common".to_string());
    Ok((client_id, tenant_id))
}

pub(super) fn microsoft_client_config_for_daemon(
    store: &dyn super::CredentialStore,
    namespace: &str,
) -> Result<(String, String), String> {
    let client_id = daemon_client_setting(
        store,
        "COS_MICROSOFT_OAUTH_CLIENT_ID",
        "MICROSOFT_CLIENT_ID",
        namespace,
    )?
    .ok_or_else(|| "Microsoft OAuth client id is not configured".to_string())?;
    let tenant_id = daemon_client_setting(
        store,
        "COS_MICROSOFT_OAUTH_TENANT_ID",
        "MICROSOFT_TENANT_ID",
        namespace,
    )?
    .unwrap_or_else(|| "common".to_string());
    Ok((client_id, tenant_id))
}

fn client_setting(
    store: &dyn super::CredentialStore,
    env_name: &str,
    credential_name: &str,
    namespace: &str,
) -> Result<Option<String>, String> {
    if let Ok(value) = std::env::var(env_name) {
        let value = value.trim();
        if !value.is_empty() {
            return Ok(Some(value.to_string()));
        }
    }
    let id = super::CredentialId::parse(namespace, credential_name)?;
    if !store.contains(&id)? {
        return Ok(None);
    }
    store.load(&id, true).map(Some)
}

fn daemon_client_setting(
    store: &dyn super::CredentialStore,
    env_name: &str,
    credential_name: &str,
    namespace: &str,
) -> Result<Option<String>, String> {
    if let Ok(value) = std::env::var(env_name) {
        let value = value.trim();
        if !value.is_empty() {
            return Ok(Some(value.to_string()));
        }
    }
    let id = super::CredentialId::parse(namespace, credential_name)?;
    if !store.contains(&id)? {
        return Ok(None);
    }
    store.load(&id, false).map(Some)
}

fn pkce_pair() -> Result<(String, String), String> {
    let verifier = random_token(64)?;
    let digest = super::sha256::hash(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(digest);
    Ok((verifier, challenge))
}

fn random_token(bytes: usize) -> Result<String, String> {
    let mut raw = vec![0u8; bytes];
    super::os_random_bytes(&mut raw).map_err(|e| format!("generate OAuth random value: {e}"))?;
    Ok(URL_SAFE_NO_PAD.encode(raw))
}

fn google_authorization_url(
    client_id: &str,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
    scopes: &str,
) -> String {
    format!(
        "{GOOGLE_AUTH_URL}?client_id={}&redirect_uri={}&response_type=code&scope={}&code_challenge={}&code_challenge_method=S256&access_type=offline&prompt=consent&include_granted_scopes=true&state={}",
        super::urlencoded(client_id),
        super::urlencoded(redirect_uri),
        super::urlencoded(scopes),
        super::urlencoded(challenge),
        super::urlencoded(state),
    )
}

fn open_browser(url: &str) -> bool {
    for (program, args) in [
        ("xdg-open", vec![url]),
        ("gio", vec!["open", url]),
        ("wslview", vec![url]),
    ] {
        let result = std::process::Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if result.is_ok_and(|status| status.success()) {
            return true;
        }
    }
    false
}

fn wait_for_callback(
    listener: &TcpListener,
    expected_state: &str,
    timeout: Duration,
) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let result = read_callback(&mut stream, expected_state);
                let success = result.is_ok();
                let _ = write_callback_response(&mut stream, success);
                return result;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err("Google OAuth login timed out".to_string());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(format!("accept OAuth callback: {error}")),
        }
    }
}

fn read_callback(stream: &mut TcpStream, expected_state: &str) -> Result<String, String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("configure OAuth callback read: {e}"))?;
    let mut request = Vec::new();
    let mut chunk = [0u8; 1024];
    while request.len() < MAX_CALLBACK_BYTES {
        let read = stream
            .read(&mut chunk)
            .map_err(|e| format!("read OAuth callback: {e}"))?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    if request.len() >= MAX_CALLBACK_BYTES {
        return Err("OAuth callback exceeded size limit".to_string());
    }
    let request = std::str::from_utf8(&request).map_err(|_| "OAuth callback was not UTF-8")?;
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| "OAuth callback request was malformed".to_string())?;
    parse_callback_target(target, expected_state)
}

fn parse_callback_target(target: &str, expected_state: &str) -> Result<String, String> {
    if !target.starts_with("/oauth/callback?") {
        return Err("OAuth callback path did not match".to_string());
    }
    let query = target
        .split_once('?')
        .map(|(_, query)| query)
        .ok_or_else(|| "OAuth callback did not contain query parameters".to_string())?;
    let values = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (key, percent_decode(value)))
        .collect::<std::collections::BTreeMap<_, _>>();
    if let Some(error) = values.get("error") {
        return Err(format!("Google authorization was denied: {error}"));
    }
    if values.get("state").map(String::as_str) != Some(expected_state) {
        return Err("OAuth callback state did not match".to_string());
    }
    values
        .get("code")
        .filter(|code| !code.is_empty())
        .cloned()
        .ok_or_else(|| "OAuth callback did not contain an authorization code".to_string())
}

fn percent_decode(value: &str) -> String {
    let value = value.replace('+', " ");
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                decoded.push((high << 4) | low);
                i += 3;
                continue;
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn write_callback_response(stream: &mut TcpStream, success: bool) -> std::io::Result<()> {
    let message = if success {
        "Google authorization completed. You can close this tab."
    } else {
        "Google authorization failed. Return to the terminal for details."
    };
    let body =
        format!("<!doctype html><meta charset=\"utf-8\"><title>Claw OS</title><p>{message}</p>");
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/credential/oauth_login.rs"
    ));
}
