// SPDX-License-Identifier: GPL-3.0-only
//
// Initial-setup AI page — picks the system-wide LLM provider/model
// (and optionally an API key) so the OS owner doesn't have to drop to
// a terminal post-install to run `cos agent setup llm apply`.
//
// Pre-1.0 design notes:
//
//   * The page is **optional + skippable**. Fresh installs ship with
//     `AgentConfig::provider = ""` (not configured), and that's a
//     perfectly fine state to leave the system in — every AI call
//     just fails fast with `LlmError::NotConfigured`. The user can
//     return here via `cos agent setup llm apply ...` at any time.
//
//   * Applying writes to `~/.config/cos/config.json` by invoking the
//     existing `cos agent setup llm apply` CLI through cos-runtime
//     exec.run. The bridge runs as the same unprivileged user as the
//     desktop session, so the config file lives under the running
//     user's `$HOME` and no privilege escalation is needed.
//
//   * The api-key is passed inline via `--api-key` rather than
//     `--api-key-stdin`. cos-runtime's exec verb doesn't carry stdin
//     to the spawned child today. The key is visible in `ps` for the
//     duration of the call — initial-setup runs once, on a freshly
//     installed system, so the leak window is acceptable; users who
//     need stronger handling can leave the field blank here and run
//     `sudo cos agent setup llm apply --api-key-stdin <key.txt` from
//     a terminal afterwards.
//
//   * Per-provider extras (Azure endpoint + API version, today) are
//     mirrored from `core::agent::setup::extra_fields_for` so the
//     wizard surfaces the same fields cosmic-settings does. Hard-coded
//     rather than fetched via `--providers` because the wizard runs
//     before `cos` is necessarily on PATH and we want zero spawning
//     until the user clicks Finish.

use std::collections::HashMap;

use cosmic::iced::{Alignment, Length};
use cosmic::{Element, Task, cosmic_theme, theme, widget};

use crate::{fl, page};

// User-facing provider options. Mirrors `core::agent::llm::registry::REGISTERED`
// minus `mock` (test-only) and `llama_local` (handled by `cos model load`,
// not configured here). Order is "common first" rather than alphabetical.
//
// "copilot" is the only provider using the device-flow OAuth path instead
// of an API key — see [`is_oauth_provider`] and the `OauthState` panel in
// `view`.
const PROVIDER_KEYS: &[&str] = &[
    "anthropic",
    "openai",
    "copilot",
    "gemini",
    "openrouter",
    "ollama",
    "xai",
    "deepseek",
    "azure",
    "bedrock",
];

const PROVIDER_LABELS: &[&str] = &[
    "Anthropic Claude",
    "OpenAI",
    "GitHub Copilot",
    "Google Gemini",
    "OpenRouter",
    "Ollama (local)",
    "xAI",
    "DeepSeek",
    "Azure OpenAI",
    "AWS Bedrock",
];

const DEFAULT_MODELS: &[&str] = &[
    // anthropic
    "claude-sonnet-4-5",
    // openai
    "gpt-4o-mini",
    // copilot — pin to the same default the web UI lands on after sign-in
    "claude-sonnet-4.6",
    // gemini
    "gemini-2.5-flash",
    // openrouter
    "openrouter/auto",
    // ollama
    "llama3.2:3b",
    // xai
    "grok-2-latest",
    // deepseek
    "deepseek-chat",
    // azure
    "",
    // bedrock
    "anthropic.claude-3-5-sonnet-20241022-v2:0",
];

/// Providers whose `auth_kind == "oauth_device"`. The wizard renders a
/// dedicated sign-in panel for these instead of an API-key field.
fn is_oauth_provider(provider: &str) -> bool {
    matches!(provider, "copilot")
}

/// One per-provider input rendered below the standard model + API key
/// rows. `cli_flag` is the long flag understood by `cos agent setup
/// llm apply` (e.g. `--base-url`, `--api-version`). `key` mirrors the
/// `extra_fields[].key` value emitted by `cos agent setup --providers`
/// for cross-checking against the kernel.
struct ExtraFieldSpec {
    key: &'static str,
    cli_flag: &'static str,
    label_attr: &'static str,
    required: bool,
}

/// Provider-keyed extra-field tables. Must stay in sync with
/// `core::agent::setup::extra_fields_for`. Today only Azure declares
/// extras; everything else falls back to the empty slice.
const AZURE_EXTRAS: &[ExtraFieldSpec] = &[
    ExtraFieldSpec {
        key: "base_url",
        cli_flag: "--base-url",
        label_attr: "azure-endpoint",
        required: true,
    },
    ExtraFieldSpec {
        key: "api_version",
        cli_flag: "--api-version",
        label_attr: "azure-api-version",
        required: false,
    },
];

fn extras_for(provider: &str) -> &'static [ExtraFieldSpec] {
    match provider {
        "azure" => AZURE_EXTRAS,
        _ => &[],
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    SelectProvider(usize),
    EditModel(String),
    EditApiKey(String),
    ToggleApiKeyVisibility,
    EditExtra(&'static str, String),
    AppliedResult(ApplyOutcome),
    /// User clicked "Sign in with GitHub" on the OAuth panel.
    StartOauth,
    /// `cos agent setup llm oauth-start --provider copilot` returned
    /// the device-authorization codes. The wizard displays the
    /// `user_code` and starts the polling task.
    OauthStarted {
        user_code: String,
        verification_uri: String,
        device_code: String,
        interval: u64,
    },
    /// Polling finished — either authorized or terminal error.
    OauthCompleted(Result<(), String>),
}

#[derive(Clone, Debug)]
pub enum ApplyOutcome {
    Ok,
    /// Bridge denied or CLI exited non-zero. The string is a
    /// short, user-readable summary suitable for inline display.
    Failed(String),
}

/// OAuth flow state for providers like GitHub Copilot. Replaces the
/// API-key field in the wizard view. Owned by the AI page; reset to
/// `Idle` whenever the user picks a different provider so a stale
/// "Authorized" state can't leak into an unrelated apply.
#[derive(Clone, Debug, Default)]
pub enum OauthState {
    #[default]
    Idle,
    /// `oauth-start` subprocess in flight.
    Starting,
    /// `oauth-start` returned. The polling task is alive in the
    /// background and the UI is showing `user_code` for the user to
    /// type into the verification URL.
    Polling {
        user_code: String,
        verification_uri: String,
    },
    /// `oauth-poll` returned `status=ok` — the GitHub token has been
    /// persisted in the credential store and `apply` may proceed.
    Authorized,
    /// `oauth-start` or the poll loop terminated with a non-recoverable
    /// error. The user can retry by clicking the Sign in button again.
    Failed(String),
}

impl From<Message> for super::Message {
    fn from(message: Message) -> Self {
        super::Message::Ai(message)
    }
}

impl From<Message> for crate::Message {
    fn from(message: Message) -> Self {
        crate::Message::PageMessage(message.into())
    }
}

pub struct Page {
    selected: Option<usize>,
    model: String,
    api_key: String,
    api_key_hidden: bool,
    /// Per-provider extra field values, keyed by `ExtraFieldSpec.key`
    /// (e.g. `base_url`, `api_version`). Preserved across provider
    /// changes so a user that toggles azure → openai → azure doesn't
    /// lose their typed endpoint.
    extras: HashMap<&'static str, String>,
    /// Last outcome of `apply_settings`, surfaced inline in the view.
    /// `None` until the user hits Finish.
    last_outcome: Option<ApplyOutcome>,
    /// Device-flow state for OAuth providers (Copilot today). Reset
    /// to `Idle` whenever the user picks a non-OAuth provider so the
    /// stale "Authorized" badge can't leak.
    oauth: OauthState,
}

impl Default for Page {
    fn default() -> Self {
        Self {
            selected: None,
            model: String::new(),
            api_key: String::new(),
            api_key_hidden: true,
            extras: HashMap::new(),
            last_outcome: None,
            oauth: OauthState::Idle,
        }
    }
}

impl Page {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, message: Message) -> Task<page::Message> {
        match message {
            Message::SelectProvider(idx) => {
                self.selected = Some(idx);
                // Pre-fill a sensible default model for the picked
                // provider, but only if the user hasn't already typed
                // a custom value — never clobber explicit input.
                if self.model.trim().is_empty()
                    && let Some(m) = DEFAULT_MODELS.get(idx)
                {
                    self.model = (*m).to_string();
                }
                // Switching provider resets any in-flight OAuth state
                // so a previous Copilot sign-in can't masquerade as
                // approval for a different provider.
                self.oauth = OauthState::Idle;
            }
            Message::EditModel(value) => {
                self.model = value;
            }
            Message::EditApiKey(value) => {
                self.api_key = value;
            }
            Message::ToggleApiKeyVisibility => {
                self.api_key_hidden = !self.api_key_hidden;
            }
            Message::EditExtra(key, value) => {
                if value.is_empty() {
                    self.extras.remove(key);
                } else {
                    self.extras.insert(key, value);
                }
            }
            Message::AppliedResult(outcome) => {
                self.last_outcome = Some(outcome);
            }
            Message::StartOauth => {
                let provider = self
                    .selected
                    .and_then(|i| PROVIDER_KEYS.get(i))
                    .copied()
                    .unwrap_or_default()
                    .to_string();
                if !is_oauth_provider(&provider) {
                    return Task::none();
                }
                self.oauth = OauthState::Starting;
                let fut = async move {
                    let res = tokio::task::spawn_blocking(move || {
                        oauth_start_blocking(&provider)
                    })
                    .await
                    .unwrap_or_else(|e| Err(format!("internal join error: {e}")));
                    match res {
                        Ok(s) => page::Message::Ai(Message::OauthStarted {
                            user_code: s.user_code,
                            verification_uri: s.verification_uri,
                            device_code: s.device_code,
                            interval: s.interval,
                        }),
                        Err(e) => page::Message::Ai(Message::OauthCompleted(Err(e))),
                    }
                };
                return cosmic::task::future(fut);
            }
            Message::OauthStarted {
                user_code,
                verification_uri,
                device_code,
                interval,
            } => {
                self.oauth = OauthState::Polling {
                    user_code,
                    verification_uri,
                };
                let provider = self
                    .selected
                    .and_then(|i| PROVIDER_KEYS.get(i))
                    .copied()
                    .unwrap_or("copilot")
                    .to_string();
                let fut = async move {
                    let res = tokio::task::spawn_blocking(move || {
                        oauth_poll_blocking(&provider, &device_code, interval)
                    })
                    .await
                    .unwrap_or_else(|e| Err(format!("internal join error: {e}")));
                    page::Message::Ai(Message::OauthCompleted(res))
                };
                return cosmic::task::future(fut);
            }
            Message::OauthCompleted(Ok(())) => {
                self.oauth = OauthState::Authorized;
            }
            Message::OauthCompleted(Err(reason)) => {
                self.oauth = OauthState::Failed(reason);
            }
        }
        Task::none()
    }
}

impl page::Page for Page {
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn title(&self) -> String {
        fl!("ai-page")
    }

    fn skippable(&self) -> bool {
        true
    }

    fn optional(&self) -> bool {
        true
    }

    fn width(&self) -> f32 {
        800.0
    }

    /// Completed only when the user picked a provider, entered a
    /// non-empty model id, and either (a) for OAuth providers, the
    /// device-flow finished with `Authorized`, or (b) for non-OAuth
    /// providers, filled every `required` extra field declared by the
    /// picked provider (today: Azure endpoint).
    fn completed(&self) -> bool {
        let Some(idx) = self.selected else {
            return false;
        };
        let Some(provider) = PROVIDER_KEYS.get(idx) else {
            return false;
        };
        if self.model.trim().is_empty() {
            return false;
        }
        if is_oauth_provider(provider) {
            return matches!(self.oauth, OauthState::Authorized);
        }
        for field in extras_for(provider) {
            if field.required {
                match self.extras.get(field.key) {
                    Some(v) if !v.trim().is_empty() => {}
                    _ => return false,
                }
            }
        }
        true
    }

    fn view(&self) -> Element<'_, page::Message> {
        let cosmic_theme::Spacing { space_s, space_m, .. } = theme::spacing();

        let description = widget::text::body(fl!("ai-page", "description"))
            .align_x(Alignment::Center)
            .width(Length::Fill);

        let provider_dropdown = widget::settings::item::builder(fl!("ai-page", "provider"))
            .description(fl!("ai-page", "provider-description"))
            .control(widget::dropdown(
                PROVIDER_LABELS,
                self.selected,
                |idx| Message::SelectProvider(idx).into(),
            ));

        let model_input = widget::settings::item::builder(fl!("ai-page", "model"))
            .description(fl!("ai-page", "model-description"))
            .control(
                widget::text_input("", &self.model)
                    .on_input(|v| Message::EditModel(v).into()),
            );

        // Whether the currently-selected provider uses OAuth. Replaces
        // the API key + extras rows with a sign-in panel when true.
        let oauth_provider = self
            .selected
            .and_then(|i| PROVIDER_KEYS.get(i))
            .map(|p| is_oauth_provider(p))
            .unwrap_or(false);

        let mut section = widget::settings::section()
            .add(provider_dropdown)
            .add(model_input);

        if oauth_provider {
            section = section.add(oauth_panel(&self.oauth));
        } else {
            let api_key_input = widget::settings::item::builder(fl!("ai-page", "api-key"))
                .description(fl!("ai-page", "api-key-description"))
                .control(widget::secure_input(
                    "",
                    &self.api_key,
                    Some(Message::ToggleApiKeyVisibility.into()),
                    self.api_key_hidden,
                ).on_input(|v| Message::EditApiKey(v).into()));
            section = section.add(api_key_input);

            if let Some(idx) = self.selected
                && let Some(provider) = PROVIDER_KEYS.get(idx)
            {
                for field in extras_for(provider) {
                    let value = self.extras.get(field.key).cloned().unwrap_or_default();
                    let key = field.key;
                    let (label, description) = match field.label_attr {
                        "azure-endpoint" => (
                            fl!("ai-page", "azure-endpoint"),
                            fl!("ai-page", "azure-endpoint-description"),
                        ),
                        "azure-api-version" => (
                            fl!("ai-page", "azure-api-version"),
                            fl!("ai-page", "azure-api-version-description"),
                        ),
                        other => (other.to_string(), String::new()),
                    };
                    let item = widget::settings::item::builder(label)
                        .description(description)
                        .control(
                            widget::text_input("", value)
                                .on_input(move |v| Message::EditExtra(key, v).into()),
                        );
                    section = section.add(item);
                }
            }
        }

        if let Some(outcome) = &self.last_outcome {
            let line = match outcome {
                ApplyOutcome::Ok => widget::text::body(fl!("ai-page", "apply-ok")),
                ApplyOutcome::Failed(reason) => widget::text::body(format!(
                    "{}: {}",
                    fl!("ai-page", "apply-failed"),
                    reason
                )),
            };
            section = section.add(widget::settings::item_row(vec![line.into()]));
        }

        widget::column::with_children(vec![
            description.into(),
            widget::space::vertical().height(space_s).into(),
            section.into(),
        ])
        .align_x(Alignment::Center)
        .spacing(space_m)
        .into()
    }

    /// Persist the picked provider/model/api-key by invoking
    /// `cos agent setup llm apply` through claw-os-sdk. Runs on
    /// `Finish` (see main.rs Message::Finish), same as every other
    /// page's `apply_settings`. Failure is logged AND surfaced into
    /// the page state so the next render shows a redacted reason.
    ///
    /// For OAuth providers (Copilot today) the api-key argument is
    /// always blank: the GitHub token lives in clawd's credential
    /// store, persisted during `oauth-poll`. We still pass `--model`
    /// so the apply verifies against the actual deployment id.
    fn apply_settings(&mut self) -> Task<page::Message> {
        // Honour `completed()` — skipped pages have nothing to write.
        let Some(idx) = self.selected else {
            return Task::none();
        };
        let provider = match PROVIDER_KEYS.get(idx) {
            Some(p) => (*p).to_string(),
            None => return Task::none(),
        };
        let model = self.model.trim().to_string();
        if model.is_empty() {
            return Task::none();
        }
        // Move the credential out of the page state so we don't leave
        // a copy hanging around in the UI struct after apply.
        let api_key = if is_oauth_provider(&provider) {
            // Clear any stale value but don't forward it — Copilot's
            // credential lives in the kernel credential store, not in
            // the wizard.
            self.api_key.clear();
            String::new()
        } else {
            std::mem::take(&mut self.api_key)
        };

        // Snapshot only the extras the picked provider declares —
        // ignore any leftover values from a previously-selected
        // provider so we never pass `--base-url` to e.g. openai.
        let extras: Vec<(&'static str, String)> = extras_for(&provider)
            .iter()
            .filter_map(|f| {
                self.extras.get(f.key).map(|v| v.trim().to_string()).and_then(|v| {
                    if v.is_empty() { None } else { Some((f.cli_flag, v)) }
                })
            })
            .collect();

        let fut = async move {
            let outcome = tokio::task::spawn_blocking(move || {
                apply_blocking(&provider, &model, &api_key, &extras)
            })
            .await
            .unwrap_or_else(|join_err| {
                ApplyOutcome::Failed(format!("internal join error: {join_err}"))
            });
            page::Message::Ai(Message::AppliedResult(outcome))
        };
        cosmic::task::future(fut)
    }
}

/// Synchronous worker — runs on a blocking pool because
/// `cos_runtime::exec::run` is itself blocking. Returns a verdict
/// suitable for the UI; tracing log lines mirror the structure used
/// by other pages (location.rs, a11y.rs).
fn apply_blocking(
    provider: &str,
    model: &str,
    api_key: &str,
    extras: &[(&'static str, String)],
) -> ApplyOutcome {
    let mut argv: Vec<&str> = vec![
        "cos",
        "agent",
        "setup",
        "llm",
        "apply",
        "--provider",
        provider,
        "--model",
        model,
    ];
    for (flag, value) in extras {
        argv.push(flag);
        argv.push(value);
    }
    if !api_key.is_empty() {
        argv.push("--api-key");
        argv.push(api_key);
    }
    match cos_runtime::exec::run(&argv, Some(30)) {
        Ok(r) if r.exit_code == 0 => {
            tracing::info!(
                provider,
                model,
                "cos agent setup llm apply succeeded via claw-os-sdk"
            );
            ApplyOutcome::Ok
        }
        Ok(r) => {
            // Redact: stderr can contain hints, but never the api-key
            // (the CLI never echoes it). Truncate to keep the UI honest.
            let mut summary = r.stderr.trim().to_string();
            if summary.is_empty() {
                summary = format!("cos exited with status {}", r.exit_code);
            }
            if summary.len() > 240 {
                summary.truncate(240);
                summary.push('…');
            }
            tracing::warn!(
                provider,
                model,
                exit_code = r.exit_code,
                stderr = %r.stderr,
                "cos agent setup llm apply failed (non-zero exit)"
            );
            ApplyOutcome::Failed(summary)
        }
        Err(why) => {
            if why.is_denied() {
                tracing::warn!(?why, "exec.run cos agent setup llm apply denied by claw-os-sdk");
            } else {
                tracing::error!(?why, "exec.run cos agent setup llm apply failed");
            }
            ApplyOutcome::Failed(format!("{why}"))
        }
    }
}

// ----------------------------------------------------------------------
// OAuth (device-flow) — Copilot only at the moment.
//
// The CLI surface we drive:
//
//   cos agent setup llm oauth-start --provider copilot
//     → prints `{user_code, verification_uri, device_code, interval, …}`
//
//   cos agent setup llm oauth-poll --provider copilot --device-code <c>
//     → prints `{status: "pending" | "slow_down" | "expired" |
//                          "denied"   | "ok"}`
//
// We poll until `ok` (success), `expired`/`denied` (terminal), or the
// hard deadline below trips. `slow_down` bumps the interval by 5s as
// recommended by RFC 8628 §3.5.
// ----------------------------------------------------------------------

/// Total wall-clock window for the device-flow poll loop. Mirrors the
/// 10-minute timeout the web UI uses (`core/src/agent/web/ui/src/pages/
/// settings.tsx`). GitHub's device-flow codes also expire around this
/// window so anything longer would just hit `expired`.
const OAUTH_POLL_DEADLINE_SECS: u64 = 600;

/// Lower bound on the poll interval. GitHub recommends >=5s.
const OAUTH_MIN_INTERVAL_SECS: u64 = 5;

/// Subset of `oauth-start` JSON we surface in the UI.
struct OauthStart {
    user_code: String,
    verification_uri: String,
    device_code: String,
    interval: u64,
}

/// Run `cos agent setup llm oauth-start --provider X` and parse the
/// emitted JSON. The CLI prints the whole envelope to stdout; we ignore
/// extras (`expires_in`) the UI doesn't render.
fn oauth_start_blocking(provider: &str) -> Result<OauthStart, String> {
    let argv: Vec<&str> = vec![
        "cos",
        "agent",
        "setup",
        "llm",
        "oauth-start",
        "--provider",
        provider,
    ];
    let result = match cos_runtime::exec::run(&argv, Some(30)) {
        Ok(r) => r,
        Err(why) => {
            if why.is_denied() {
                tracing::warn!(?why, "exec.run cos agent setup llm oauth-start denied");
            } else {
                tracing::error!(?why, "exec.run cos agent setup llm oauth-start failed");
            }
            return Err(format!("{why}"));
        }
    };
    if result.exit_code != 0 {
        let mut summary = result.stderr.trim().to_string();
        if summary.is_empty() {
            summary = format!("cos exited with status {}", result.exit_code);
        }
        return Err(summary);
    }
    let body: serde_json::Value = serde_json::from_str(&result.stdout)
        .map_err(|e| format!("oauth-start: unexpected output: {e}"))?;
    let user_code = body
        .get("user_code")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "oauth-start: missing user_code".to_string())?
        .to_string();
    let verification_uri = body
        .get("verification_uri")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "oauth-start: missing verification_uri".to_string())?
        .to_string();
    let device_code = body
        .get("device_code")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "oauth-start: missing device_code".to_string())?
        .to_string();
    let interval = body
        .get("interval")
        .and_then(|v| v.as_u64())
        .unwrap_or(OAUTH_MIN_INTERVAL_SECS)
        .max(OAUTH_MIN_INTERVAL_SECS);
    Ok(OauthStart {
        user_code,
        verification_uri,
        device_code,
        interval,
    })
}

/// Block-poll `cos agent setup llm oauth-poll` until the device-flow
/// returns a terminal outcome or the overall deadline expires.
fn oauth_poll_blocking(
    provider: &str,
    device_code: &str,
    mut interval: u64,
) -> Result<(), String> {
    interval = interval.max(OAUTH_MIN_INTERVAL_SECS);
    let started = std::time::Instant::now();
    loop {
        if started.elapsed().as_secs() >= OAUTH_POLL_DEADLINE_SECS {
            return Err("Sign-in timed out".into());
        }
        std::thread::sleep(std::time::Duration::from_secs(interval));

        let argv: Vec<&str> = vec![
            "cos",
            "agent",
            "setup",
            "llm",
            "oauth-poll",
            "--provider",
            provider,
            "--device-code",
            device_code,
        ];
        let result = match cos_runtime::exec::run(&argv, Some(30)) {
            Ok(r) => r,
            Err(why) => {
                // Transient bridge errors get retried — the deadline
                // above caps total wait. Permanent errors surface as
                // non-zero exit_code below.
                tracing::debug!(?why, "exec.run cos agent setup llm oauth-poll errored, retrying");
                continue;
            }
        };
        if result.exit_code != 0 {
            let mut summary = result.stderr.trim().to_string();
            if summary.is_empty() {
                summary = format!("cos exited with status {}", result.exit_code);
            }
            return Err(summary);
        }
        let body: serde_json::Value = match serde_json::from_str(&result.stdout) {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(stdout = %result.stdout, "oauth-poll: unparseable stdout: {e}");
                continue;
            }
        };
        match body.get("status").and_then(|v| v.as_str()) {
            Some("ok") => return Ok(()),
            Some("pending") => continue,
            Some("slow_down") => {
                interval = body
                    .get("interval")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(interval + 5);
                continue;
            }
            Some("expired") => return Err("Code expired".into()),
            Some("denied") => return Err("Sign-in denied".into()),
            other => {
                return Err(format!(
                    "oauth-poll: unexpected status {}",
                    other.unwrap_or("(missing)")
                ));
            }
        }
    }
}

/// Render the device-flow panel that replaces the API-key field for
/// OAuth providers. The button is always present (so users can retry
/// after `Failed`); the user-code block only appears while polling.
fn oauth_panel<'a>(state: &OauthState) -> Element<'a, page::Message> {
    let cosmic_theme::Spacing { space_xs, space_s, .. } = theme::spacing();

    let mut children: Vec<Element<'a, page::Message>> = Vec::new();
    children.push(widget::text::body(fl!("ai-page", "oauth-description")).into());

    match state {
        OauthState::Polling {
            user_code,
            verification_uri,
        } => {
            children.push(widget::text::body(fl!("ai-page", "oauth-instructions")).into());
            // Verification URL — we don't make it clickable from the
            // wizard (no browser context on a fresh install for some
            // headless flavors); the user copies it onto another
            // device, which is exactly the device-flow's intent.
            children.push(widget::text::monotext(verification_uri.clone()).into());
            children.push(widget::text::title4(user_code.clone()).into());
            children.push(widget::text::caption(fl!("ai-page", "oauth-waiting")).into());
        }
        OauthState::Authorized => {
            children.push(widget::text::body(fl!("ai-page", "oauth-authorized")).into());
        }
        OauthState::Failed(reason) => {
            children.push(
                widget::text::body(format!("{}: {}", fl!("ai-page", "oauth-failed"), reason))
                    .into(),
            );
        }
        OauthState::Idle | OauthState::Starting => {}
    }

    let button_label = if matches!(state, OauthState::Authorized) {
        fl!("ai-page", "oauth-signin-again")
    } else {
        fl!("ai-page", "oauth-signin")
    };
    let mut btn = widget::button::standard(button_label);
    // Disable the button while we're already mid-flight so the user
    // can't queue duplicate `oauth-start` invocations.
    if !matches!(state, OauthState::Starting | OauthState::Polling { .. }) {
        btn = btn.on_press(page::Message::Ai(Message::StartOauth));
    }
    children.push(btn.into());

    let col = widget::column::with_children(children).spacing(space_xs);
    widget::settings::item_row(vec![
        widget::container(col).padding(space_s).into(),
    ])
    .into()
}
