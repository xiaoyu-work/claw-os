//! GitHub Copilot OAuth device flow and live model discovery for agent setup.

use serde_json::{json, Value};
use std::io::Write;
use std::time::{Duration, Instant};

use crate::agent::llm;

use super::{read_line, store_credential};

/// Authentication kind exposed to UIs so they render the device-flow branch.
pub(super) fn auth_kind_for(provider: &str) -> Option<&'static str> {
    match provider {
        "copilot" => Some("oauth_device"),
        _ => None,
    }
}

/// Default picker model while the live catalogue is unavailable.
pub(super) fn default_model_name(provider: &str) -> Option<String> {
    match provider {
        "copilot" => Some("gpt-4o".into()),
        _ => None,
    }
}

/// Credential name the OAuth-device path stores the long-lived GitHub
/// token under in the `agent` namespace. Centralised so the parent apply
/// path and the OAuth writer agree on a single string.
pub(super) const COPILOT_GITHUB_TOKEN_CREDENTIAL: &str = "copilot_github_token";
const MIN_OAUTH_POLL_SECS: u64 = 5;

pub(super) struct OAuthTerminalLogin {
    pub(super) credential_name: String,
    pub(super) models: Vec<String>,
}

pub(super) fn oauth_device_terminal_login(
    provider: &str,
    e: &mut impl Write,
) -> Result<OAuthTerminalLogin, String> {
    ensure_oauth_provider(provider)?;
    match provider {
        "copilot" => copilot_terminal_login(e),
        other => Err(format!(
            "oauth terminal login: unsupported provider `{other}`"
        )),
    }
}

fn copilot_terminal_login(e: &mut impl Write) -> Result<OAuthTerminalLogin, String> {
    let _ = writeln!(e);
    let _ = writeln!(e, "GitHub Copilot sign-in");
    let _ = writeln!(
        e,
        "This terminal flow works in WSL, Docker, SSH, and other headless Linux environments."
    );

    if let Some(github_token) =
        crate::credential::try_load(COPILOT_GITHUB_TOKEN_CREDENTIAL, "agent")
            .map_err(|err| format!("read credential `{COPILOT_GITHUB_TOKEN_CREDENTIAL}`: {err}"))?
            .filter(|token| !token.trim().is_empty())
    {
        match fetch_copilot_model_names(&github_token) {
            Ok(models) => {
                let _ = writeln!(
                    e,
                    "Existing GitHub sign-in found in `{}`; reusing it.",
                    COPILOT_GITHUB_TOKEN_CREDENTIAL
                );
                return Ok(OAuthTerminalLogin {
                    credential_name: COPILOT_GITHUB_TOKEN_CREDENTIAL.to_string(),
                    models,
                });
            }
            Err(err) => {
                let _ = writeln!(
                    e,
                    "Stored GitHub sign-in could not be used ({err}); starting a new device login."
                );
            }
        }
    }

    let dc = block_on(llm::providers::copilot_auth::start_device_flow())?
        .map_err(|err| format!("oauth-start: {err}"))?;
    let mut interval = dc.interval.max(MIN_OAUTH_POLL_SECS);
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(dc.expires_in))
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(dc.expires_in.min(900)));

    let _ = writeln!(e);
    let _ = writeln!(e, "Open this URL in any browser:");
    let _ = writeln!(e, "  {}", dc.verification_uri);
    let _ = writeln!(e, "Enter this code:");
    let _ = writeln!(e, "  {}", dc.user_code);
    let _ = writeln!(e);
    let _ = writeln!(
        e,
        "Waiting for GitHub approval (expires in {}s)...",
        dc.expires_in
    );
    let _ = e.flush();

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(
                "GitHub device code expired before approval; rerun setup to try again".into(),
            );
        }
        std::thread::sleep(Duration::from_secs(interval).min(remaining));

        let outcome = block_on(llm::providers::copilot_auth::poll_device_flow(
            &dc.device_code,
        ))?
        .map_err(|err| format!("oauth-poll: {err}"))?;
        use llm::providers::copilot_auth::PollOutcome;
        match outcome {
            PollOutcome::Pending => {
                let _ = write!(e, ".");
                let _ = e.flush();
            }
            PollOutcome::SlowDown { interval: next } => {
                interval = next.max(MIN_OAUTH_POLL_SECS);
                let _ = writeln!(e);
                let _ = writeln!(
                    e,
                    "GitHub asked us to slow down; polling every {interval}s."
                );
                let _ = e.flush();
            }
            PollOutcome::Expired => {
                let _ = writeln!(e);
                return Err(
                    "GitHub device code expired before approval; rerun setup to try again".into(),
                );
            }
            PollOutcome::Denied => {
                let _ = writeln!(e);
                return Err("GitHub Copilot sign-in was denied".into());
            }
            PollOutcome::Authorized { github_token, .. } => {
                let _ = writeln!(e);
                store_credential(COPILOT_GITHUB_TOKEN_CREDENTIAL, &github_token).map_err(
                    |err| {
                        format!(
                        "oauth-poll: stored token rejected by credential store: {err}\n\
                         hint: rerun as a user with write access to the agent credential namespace."
                    )
                    },
                )?;
                let _ = writeln!(
                    e,
                    "✓ GitHub sign-in complete; credential stored as `{}`",
                    COPILOT_GITHUB_TOKEN_CREDENTIAL
                );
                let models = match fetch_copilot_model_names(&github_token) {
                    Ok(models) => models,
                    Err(err) => {
                        let _ =
                            writeln!(e, "⚠  signed in, but Copilot model discovery failed: {err}");
                        Vec::new()
                    }
                };
                return Ok(OAuthTerminalLogin {
                    credential_name: COPILOT_GITHUB_TOKEN_CREDENTIAL.to_string(),
                    models,
                });
            }
        }
    }
}

fn fetch_copilot_model_names(github_token: &str) -> Result<Vec<String>, String> {
    Ok(model_names_from_values(block_on(fetch_copilot_models(
        github_token,
    ))??))
}

pub(super) fn model_names_from_values(values: Vec<Value>) -> Vec<String> {
    values
        .into_iter()
        .filter_map(|v| {
            v.get("name")
                .and_then(|name| name.as_str())
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(ToOwned::to_owned)
        })
        .collect()
}

pub(super) fn pick_oauth_model(
    provider: &str,
    models: &[String],
    e: &mut impl Write,
) -> Result<String, String> {
    let preferred_default = default_model_name(provider);
    let default = if models.is_empty() {
        preferred_default.unwrap_or_default()
    } else {
        preferred_default
            .filter(|candidate| models.iter().any(|m| m == candidate))
            .or_else(|| models.first().cloned())
            .unwrap_or_default()
    };

    if models.is_empty() {
        let _ = writeln!(
            e,
            "No live model list was returned for `{provider}` — enter a Copilot model identifier."
        );
    } else {
        let _ = writeln!(e, "Available models for `{provider}`:");
        for (i, model) in models.iter().enumerate() {
            let _ = writeln!(e, "  {:>2}. {}", i + 1, model);
        }
    }

    if models.is_empty() {
        if default.is_empty() {
            let _ = write!(e, "Model name: ");
        } else {
            let _ = write!(e, "Model name [{default}]: ");
        }
    } else if default.is_empty() {
        let _ = write!(
            e,
            "Pick a number (1-{}) or type a model name: ",
            models.len()
        );
    } else {
        let _ = write!(
            e,
            "Pick a number (1-{}) or type a model name [{}]: ",
            models.len(),
            default
        );
    }
    let _ = e.flush();
    let raw = read_line()?.trim().to_string();
    if raw.is_empty() {
        if default.is_empty() {
            return Err("model name cannot be empty".into());
        }
        return Ok(default);
    }

    if !models.is_empty() {
        if let Ok(idx) = raw.parse::<usize>() {
            if idx < 1 || idx > models.len() {
                return Err(format!("out of range: {idx} (expected 1-{})", models.len()));
            }
            return Ok(models[idx - 1].clone());
        }
        if !models.iter().any(|m| m == &raw) {
            let _ = writeln!(e);
            let _ = writeln!(e, "⚠  `{raw}` was not returned by Copilot model discovery.");
            let _ = write!(e, "Use it anyway? (y/N): ");
            let _ = e.flush();
            let yn = read_line()?.trim().to_ascii_lowercase();
            if !matches!(yn.as_str(), "y" | "yes") {
                return Err("aborted: unknown Copilot model name".into());
            }
        }
    }
    Ok(raw)
}

pub(super) fn require_provider(provider: Option<&str>, sub: &str) -> Result<String, String> {
    match provider {
        Some(p) if !p.trim().is_empty() => Ok(p.trim().to_string()),
        _ => Err(format!(
            "`{sub}` requires --provider <name>. Only `copilot` is supported today."
        )),
    }
}

fn ensure_oauth_provider(provider: &str) -> Result<(), String> {
    match auth_kind_for(provider) {
        Some(_) => Ok(()),
        None => Err(format!(
            "provider `{provider}` does not use OAuth device flow. \
             Use `apply --api-key{{,-stdin,-env}}` instead."
        )),
    }
}

fn block_on<F, T>(fut: F) -> Result<T, String>
where
    F: std::future::Future<Output = T>,
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    Ok(rt.block_on(fut))
}

/// `cos agent setup oauth-start --provider copilot` → emits the
/// device-authorization codes the UI shows the user.
pub(super) fn oauth_start_cmd(provider: Option<&str>) -> Result<Value, String> {
    let provider = require_provider(provider, "oauth-start")?;
    ensure_oauth_provider(&provider)?;
    // Only Copilot for now — keep the dispatch explicit so adding a
    // second OAuth provider is a visible diff.
    if provider != "copilot" {
        return Err(format!("oauth-start: unsupported provider `{provider}`"));
    }
    let dc = block_on(llm::providers::copilot_auth::start_device_flow())?
        .map_err(|e| format!("oauth-start: {e}"))?;
    Ok(json!({
        "provider": provider,
        "device_code": dc.device_code,
        "user_code": dc.user_code,
        "verification_uri": dc.verification_uri,
        "expires_in": dc.expires_in,
        "interval": dc.interval,
    }))
}

/// `cos agent setup oauth-poll --provider copilot --device-code <code>`
/// → single poll. The UI loops on its own schedule and stops when this
/// command emits `status: "ok"` (token stored) or a terminal failure.
pub(super) fn oauth_poll_cmd(
    provider: Option<&str>,
    device_code: Option<&str>,
) -> Result<Value, String> {
    let provider = require_provider(provider, "oauth-poll")?;
    ensure_oauth_provider(&provider)?;
    if provider != "copilot" {
        return Err(format!("oauth-poll: unsupported provider `{provider}`"));
    }
    let code = device_code
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "oauth-poll requires --device-code <code>".to_string())?;
    let outcome = block_on(llm::providers::copilot_auth::poll_device_flow(code))?
        .map_err(|e| format!("oauth-poll: {e}"))?;
    use llm::providers::copilot_auth::PollOutcome;
    match outcome {
        PollOutcome::Pending => Ok(json!({"status": "pending"})),
        PollOutcome::SlowDown { interval } => {
            Ok(json!({"status": "slow_down", "interval": interval}))
        }
        PollOutcome::Expired => Ok(json!({"status": "expired"})),
        PollOutcome::Denied => Ok(json!({"status": "denied"})),
        PollOutcome::Authorized { github_token, .. } => {
            // Persist the long-lived GitHub token so subsequent `apply`
            // + chat traffic can exchange it for short-lived Copilot
            // tokens on demand. We store under a fixed credential name
            // so a re-sign-in cleanly overwrites the prior token.
            store_credential(COPILOT_GITHUB_TOKEN_CREDENTIAL, &github_token).map_err(|e| {
                format!(
                    "oauth-poll: stored token rejected by credential store: {e}\n\
                     hint: rerun as a user with write access to the agent credential namespace."
                )
            })?;
            Ok(json!({
                "status": "ok",
                "provider": provider,
                "credential": COPILOT_GITHUB_TOKEN_CREDENTIAL,
            }))
        }
    }
}

/// `cos agent setup models --provider copilot` → fetch the live model
/// catalogue from Copilot's `/models` endpoint using the stored token.
/// Returns selectable chat models with their negotiated wire protocol.
/// Embedding, internal, disabled, and unsupported-endpoint entries are
/// excluded before UIs render the dropdown.
pub(super) fn models_cmd(provider: Option<&str>) -> Result<Value, String> {
    let provider = require_provider(provider, "models")?;
    if provider != "copilot" {
        return Err(
            "models: live discovery is only supported for `copilot` today; \
             other providers expose their model lists via `--providers`"
                .to_string(),
        );
    }
    let github_token = crate::credential::try_load(COPILOT_GITHUB_TOKEN_CREDENTIAL, "agent")
        .map_err(|e| format!("read credential `{COPILOT_GITHUB_TOKEN_CREDENTIAL}`: {e}"))?
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            "GitHub Copilot is not signed in. Run `oauth-start` + `oauth-poll` first \
             (or use the desktop AI settings page)."
                .to_string()
        })?;

    let models = block_on(fetch_copilot_models(&github_token))??;
    Ok(json!({
        "provider": provider,
        "models": models,
    }))
}

async fn fetch_copilot_models(github_token: &str) -> Result<Vec<Value>, String> {
    let copilot = llm::providers::copilot_auth::ensure_copilot_token(github_token)
        .await
        .map_err(|e| format!("copilot auth: {e}"))?;
    let models = llm::providers::copilot_auth::ensure_copilot_models(&copilot)
        .await
        .map_err(|e| format!("Copilot /models: {e}"))?;

    let mut out: Vec<Value> = models
        .iter()
        .filter(|model| model.is_selectable_chat_model())
        .filter_map(|model| {
            let wire_api = model.wire_api()?;
            Some(json!({
                "name": model.id,
                "wire_api": wire_api.config_name(),
            }))
        })
        .collect();
    out.sort_by(|a, b| {
        a.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .cmp(b.get("name").and_then(|v| v.as_str()).unwrap_or(""))
    });
    Ok(out)
}
