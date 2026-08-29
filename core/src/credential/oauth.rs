use super::*;

// ===========================================================================
// OAuth refresh
// ===========================================================================

/// Refresh an OAuth token by exchanging a refresh token for a new access token.
///
/// Usage: cos credential oauth-refresh <provider> [--namespace NS]
///
/// Supported providers: google, microsoft
///
/// Reads <PROVIDER>_REFRESH_TOKEN and <PROVIDER>_CLIENT_ID, <PROVIDER>_CLIENT_SECRET
/// from the credential store, exchanges for a new access token, and stores it.
pub(super) fn cmd_oauth_refresh(
    store: &dyn CredentialStore,
    args: &[String],
) -> Result<Value, String> {
    let (ns_opt, rest) = parse_namespace_flag(args);
    let namespace = ns_opt.unwrap_or_else(|| "default".into());
    validate_credential_component("namespace", &namespace).map_err(|error| error.to_string())?;

    let provider = rest
        .first()
        .ok_or("usage: cos credential oauth-refresh <google|microsoft> [--namespace NS]")?;

    match provider.as_str() {
        "google" => oauth_refresh_google(store, &namespace),
        "microsoft" => oauth_refresh_microsoft(store, &namespace),
        _ => Err(format!(
            "unsupported OAuth provider: {provider}. supported: google, microsoft"
        )),
    }
}

fn oauth_refresh_google(store: &dyn CredentialStore, namespace: &str) -> Result<Value, String> {
    let refresh_token = load_authorized_oauth_credential(store, "GOOGLE_REFRESH_TOKEN", namespace)?;
    let (client_id, client_secret) = oauth_login::google_client_config(store, namespace)?;
    let refresh_tier = minimum_tier(store, "GOOGLE_REFRESH_TOKEN", namespace)?;
    let output_tier =
        optional_minimum_tier(store, "GOOGLE_ACCESS_TOKEN", namespace)?.unwrap_or(refresh_tier);
    require_secret(
        Verb::SECRET_WRITE,
        credential_scope(namespace, "GOOGLE_ACCESS_TOKEN").map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    refresh_google_tokens(
        store,
        namespace,
        &refresh_token,
        &client_id,
        &client_secret,
        output_tier,
    )
}

fn refresh_google_tokens(
    store: &dyn CredentialStore,
    namespace: &str,
    refresh_token: &str,
    client_id: &str,
    client_secret: &str,
    output_tier: u8,
) -> Result<Value, String> {
    let body = format!(
        "grant_type=refresh_token&refresh_token={}&client_id={}&client_secret={}",
        urlencoded(refresh_token),
        urlencoded(client_id),
        urlencoded(client_secret),
    );

    let result = http_post(
        "https://oauth2.googleapis.com/token",
        &body,
        "application/x-www-form-urlencoded",
    )?;

    let token_data: serde_json::Value = serde_json::from_str(&result)
        .map_err(|e| format!("failed to parse token response: {e}"))?;

    let access_token = token_data["access_token"]
        .as_str()
        .ok_or("no access_token in response")?;

    let expires_in = token_data["expires_in"].as_u64().unwrap_or(3600);
    store_token(
        store,
        namespace,
        "GOOGLE_ACCESS_TOKEN",
        access_token,
        output_tier,
        Some(expires_in),
        Some(format!(
            "cos credential oauth-refresh google --namespace {namespace}"
        )),
    )?;

    Ok(json!({
        "provider": "google",
        "refreshed": true,
        "expires_in": expires_in,
        "namespace": namespace,
    }))
}

fn oauth_refresh_microsoft(store: &dyn CredentialStore, namespace: &str) -> Result<Value, String> {
    let refresh_token =
        load_authorized_oauth_credential(store, "MICROSOFT_REFRESH_TOKEN", namespace)?;
    let (client_id, tenant_id) = oauth_login::microsoft_client_config(store, namespace)?;
    let refresh_tier = minimum_tier(store, "MICROSOFT_REFRESH_TOKEN", namespace)?;
    let access_tier =
        optional_minimum_tier(store, "MICROSOFT_ACCESS_TOKEN", namespace)?.unwrap_or(refresh_tier);
    require_secret(
        Verb::SECRET_WRITE,
        credential_scope(namespace, "MICROSOFT_ACCESS_TOKEN").map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    require_secret(
        Verb::SECRET_WRITE,
        credential_scope(namespace, "MICROSOFT_REFRESH_TOKEN")
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    refresh_microsoft_tokens(
        store,
        namespace,
        &refresh_token,
        &client_id,
        &tenant_id,
        refresh_tier,
        access_tier,
    )
}

fn refresh_microsoft_tokens(
    store: &dyn CredentialStore,
    namespace: &str,
    refresh_token: &str,
    client_id: &str,
    tenant_id: &str,
    refresh_tier: u8,
    access_tier: u8,
) -> Result<Value, String> {
    let body = format!(
        "grant_type=refresh_token&refresh_token={}&client_id={}&scope={}",
        urlencoded(refresh_token),
        urlencoded(client_id),
        urlencoded("offline_access openid email User.Read Mail.Read Mail.Send Calendars.ReadWrite"),
    );

    let url = format!("https://login.microsoftonline.com/{tenant_id}/oauth2/v2.0/token");

    let result = http_post(&url, &body, "application/x-www-form-urlencoded")?;

    let token_data: serde_json::Value = serde_json::from_str(&result)
        .map_err(|e| format!("failed to parse token response: {e}"))?;

    let access_token = token_data["access_token"]
        .as_str()
        .ok_or("no access_token in response")?;

    let expires_in = token_data["expires_in"].as_u64().unwrap_or(3600);
    // Also store new refresh token if returned (Microsoft rotates them)
    if let Some(new_refresh) = token_data["refresh_token"].as_str() {
        store_token(
            store,
            namespace,
            "MICROSOFT_REFRESH_TOKEN",
            new_refresh,
            refresh_tier,
            None,
            None,
        )?;
    }

    store_token(
        store,
        namespace,
        "MICROSOFT_ACCESS_TOKEN",
        access_token,
        access_tier,
        Some(expires_in),
        Some(format!(
            "cos credential oauth-refresh microsoft --namespace {namespace}"
        )),
    )?;

    Ok(json!({
        "provider": "microsoft",
        "refreshed": true,
        "expires_in": expires_in,
        "namespace": namespace,
    }))
}

pub(super) fn direct_oauth_refresh(
    store: &dyn CredentialStore,
    provider: &str,
    namespace: &str,
) -> Result<Value, String> {
    match provider {
        "google" => oauth_refresh_google(store, namespace),
        "microsoft" => oauth_refresh_microsoft(store, namespace),
        _ => Err(format!("unsupported OAuth provider: {provider}")),
    }
}

pub(super) fn request_brokered_oauth_refresh(name: &str, namespace: &str) -> Result<Value, String> {
    let session = crate::proc::current_session_id()
        .ok_or_else(|| "OAuth refresh broker requires an active session".to_string())?;
    let response = crate::clawd::client::request_blocking(
        crate::paths::clawd_socket_path(),
        crate::clawd::protocol::Request::build(
            crate::clawd::routes::Command::CredentialOauthRefresh,
            json!({
                "session": session,
                "namespace": namespace,
                "credential": name,
            }),
        ),
    )?;
    if response.ok {
        Ok(response.result.unwrap_or(Value::Null))
    } else {
        Err(response
            .error
            .map(|error| error.message)
            .unwrap_or_else(|| "clawd OAuth refresh failed".to_string()))
    }
}

pub(crate) fn broker_refresh_access_token(
    store: &dyn CredentialStore,
    name: &str,
    namespace: &str,
) -> Result<Value, String> {
    match name {
        "GOOGLE_ACCESS_TOKEN" => {
            let refresh_token =
                load_broker_oauth_credential(store, "GOOGLE_REFRESH_TOKEN", namespace)?;
            let (client_id, client_secret) =
                oauth_login::google_client_config_for_daemon(store, namespace)?;
            let access_tier = minimum_tier(store, "GOOGLE_ACCESS_TOKEN", namespace)?;
            refresh_google_tokens(
                store,
                namespace,
                &refresh_token,
                &client_id,
                &client_secret,
                access_tier,
            )
        }
        "MICROSOFT_ACCESS_TOKEN" => {
            let refresh_token =
                load_broker_oauth_credential(store, "MICROSOFT_REFRESH_TOKEN", namespace)?;
            let (client_id, tenant_id) =
                oauth_login::microsoft_client_config_for_daemon(store, namespace)?;
            let refresh_tier = minimum_tier(store, "MICROSOFT_REFRESH_TOKEN", namespace)?;
            let access_tier = minimum_tier(store, "MICROSOFT_ACCESS_TOKEN", namespace)?;
            refresh_microsoft_tokens(
                store,
                namespace,
                &refresh_token,
                &client_id,
                &tenant_id,
                refresh_tier,
                access_tier,
            )
        }
        _ => Err(format!(
            "credential `{name}` is not eligible for brokered OAuth refresh"
        )),
    }
}

fn load_authorized_oauth_credential(
    store: &dyn CredentialStore,
    name: &str,
    namespace: &str,
) -> Result<String, String> {
    let id = CredentialId::parse(namespace, name).map_err(|error| error.to_string())?;
    require_secret(
        Verb::SECRET_READ,
        credential_scope(namespace, name).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    store.load(&id, true).map_err(|error| error.to_string())
}

/// Load refresh material inside clawd after the credential OAuth route has
/// authorized the peer and assumed its filesystem identity.
fn load_broker_oauth_credential(
    store: &dyn CredentialStore,
    name: &str,
    namespace: &str,
) -> Result<String, String> {
    let id = CredentialId::parse(namespace, name).map_err(|error| error.to_string())?;
    store.load(&id, false).map_err(|error| error.to_string())
}

fn minimum_tier(store: &dyn CredentialStore, name: &str, namespace: &str) -> Result<u8, String> {
    optional_minimum_tier(store, name, namespace)?
        .ok_or_else(|| format!("credential not found: {name} (namespace: {namespace})"))
}

fn optional_minimum_tier(
    store: &dyn CredentialStore,
    name: &str,
    namespace: &str,
) -> Result<Option<u8>, String> {
    let id = CredentialId::parse(namespace, name).map_err(|error| error.to_string())?;
    store.minimum_tier(&id).map_err(|error| error.to_string())
}

fn store_token(
    store: &dyn CredentialStore,
    namespace: &str,
    name: &str,
    value: &str,
    min_tier: u8,
    ttl: Option<u64>,
    refresh_cmd: Option<String>,
) -> Result<(), String> {
    let id = CredentialId::parse(namespace, name).map_err(|error| error.to_string())?;
    store
        .store(StoreRequest {
            id: &id,
            value,
            min_tier,
            ttl,
            refresh_cmd,
        })
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub(super) fn broker_oauth_provider<'a>(refresh_cmd: &'a str, namespace: &str) -> Option<&'a str> {
    let parts = refresh_cmd.split_whitespace().collect::<Vec<_>>();
    let provider = match parts.as_slice() {
        ["cos", "credential", "oauth-refresh", provider] => *provider,
        ["cos", "credential", "oauth-refresh", provider, "--namespace", requested]
            if *requested == namespace =>
        {
            *provider
        }
        _ => return None,
    };
    Some(provider)
}

// ===========================================================================
// HTTP and encoding helpers
// ===========================================================================

/// Build the `curl` `Command` for an OAuth token POST.
///
/// Notably this builder does **not** accept the request body and does not put
/// any secret into argv. The body is supplied later via stdin
/// (`--data-binary @-`) so that `client_secret`, `refresh_token`, etc. cannot
/// be read by any same-uid process via `/proc/<pid>/cmdline` (the HIGH
/// "OAuth client_secret / refresh_token leak via argv" audit finding).
pub(super) fn build_curl_post(url: &str, content_type: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new("/usr/bin/curl");
    cmd.env_clear();
    cmd.args([
        "-s",
        "-S",
        "-X",
        "POST",
        "-H",
        &format!("Content-Type: {content_type}"),
        "--data-binary",
        "@-", // read body from stdin
        "--connect-timeout",
        "10",
        "--max-time",
        "30",
        url,
    ]);
    cmd
}

/// Simple URL-encoded POST. Body is piped to `curl` on stdin so the secret
/// never appears in argv.
pub(super) fn http_post(url: &str, body: &str, content_type: &str) -> Result<String, String> {
    use std::io::Write;
    use std::process::Stdio;

    let mut cmd = build_curl_post(url, content_type);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to execute curl: {e}"))?;

    // Write body to stdin, then close it so curl knows the body is complete.
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "failed to open curl stdin".to_string())?;
        stdin
            .write_all(body.as_bytes())
            .map_err(|e| format!("failed to write request body to curl stdin: {e}"))?;
    }
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .map_err(|e| format!("failed to wait for curl: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("HTTP POST failed: {}", stderr.trim()));
    }

    String::from_utf8(output.stdout).map_err(|e| format!("response not valid UTF-8: {e}"))
}

/// Simple percent-encoding for URL form data.
pub(super) fn urlencoded(s: &str) -> String {
    let mut result = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(b as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", b));
            }
        }
    }
    result
}
