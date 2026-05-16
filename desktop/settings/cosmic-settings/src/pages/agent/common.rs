// SPDX-License-Identifier: GPL-3.0-only
//
// Shared state, messages, view helpers and `cos agent setup` shell-out
// plumbing for the Agent settings page. Each modality (LLM / TTS / STT /
// image gen / embeddings) gets its own thin `Page` wrapper around `State`;
// all the actual logic lives here so the per-modality files stay tiny.
//
// The page never touches `~/.config/cos/config.json` directly. Reads and writes
// are funnelled through the kernel's non-interactive subcommands:
//
//   * `cos agent setup --providers <modality>` -> provider/model catalogue
//   * `cos agent setup <modality> --status`    -> currently-configured row
//   * `cos agent setup <modality> apply ...`   -> persist a new selection
//   * `cos agent setup <modality> test`        -> upstream / resolvability probe
//   * `cos agent setup <modality> --reset`     -> clear the row
//
// This keeps the atomic-write + credential-store logic in one place
// (`core/src/agent/setup.rs`) and the UI as a thin client. The kernel
// emits structured JSON on both happy and error paths so we can render
// the same error envelope ("error" / "details" / "fix") the wizard prints.

use std::borrow::Cow;
use std::process::Stdio;

use cosmic::iced::{Alignment, Length};
use cosmic::widget::{self, button, column, container, dropdown, row, settings, text};
use cosmic::{Apply, Element, Task};
use cosmic_settings_page::Section;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::pages;

// ---------------------------------------------------------------------------
// Modality
// ---------------------------------------------------------------------------

/// Mirrors `core::agent::setup::Modality`. Kept independent so the UI doesn't
/// have to pull in the kernel crate (which is `target = x86_64-unknown-linux-musl`
/// and not in cosmic-settings' graph).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, PartialOrd, Ord)]
pub enum Modality {
    Llm,
    Tts,
    Stt,
    Imagegen,
    Embed,
}

impl Modality {
    pub const ALL: [Modality; 5] = [
        Modality::Llm,
        Modality::Tts,
        Modality::Stt,
        Modality::Imagegen,
        Modality::Embed,
    ];

    pub const fn as_arg(self) -> &'static str {
        match self {
            Modality::Llm => "llm",
            Modality::Tts => "tts",
            Modality::Stt => "stt",
            Modality::Imagegen => "imagegen",
            Modality::Embed => "embed",
        }
    }

    pub const fn page_id(self) -> &'static str {
        match self {
            Modality::Llm => "agent-llm",
            Modality::Tts => "agent-tts",
            Modality::Stt => "agent-stt",
            Modality::Imagegen => "agent-imagegen",
            Modality::Embed => "agent-embed",
        }
    }

    pub const fn icon_name(self) -> &'static str {
        match self {
            Modality::Llm => "user-available-symbolic",
            Modality::Tts => "audio-speakers-symbolic",
            Modality::Stt => "audio-input-microphone-symbolic",
            Modality::Imagegen => "image-x-generic-symbolic",
            Modality::Embed => "view-list-symbolic",
        }
    }

    pub fn title(self) -> String {
        match self {
            Modality::Llm => crate::fl!("agent-llm"),
            Modality::Tts => crate::fl!("agent-tts"),
            Modality::Stt => crate::fl!("agent-stt"),
            Modality::Imagegen => crate::fl!("agent-imagegen"),
            Modality::Embed => crate::fl!("agent-embed"),
        }
    }

    pub fn description(self) -> String {
        match self {
            Modality::Llm => crate::fl!("agent-llm", "desc"),
            Modality::Tts => crate::fl!("agent-tts", "desc"),
            Modality::Stt => crate::fl!("agent-stt", "desc"),
            Modality::Imagegen => crate::fl!("agent-imagegen", "desc"),
            Modality::Embed => crate::fl!("agent-embed", "desc"),
        }
    }
}

// ---------------------------------------------------------------------------
// JSON envelopes shared with `core/src/agent/setup.rs`
// ---------------------------------------------------------------------------

/// `--providers <modality>` payload.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ProvidersDoc {
    #[serde(default)]
    pub modality: String,
    #[serde(default)]
    pub default_provider: Option<String>,
    #[serde(default)]
    pub providers: Vec<ProviderEntry>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProviderEntry {
    pub name: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub needs_credential: bool,
    #[serde(default)]
    pub default_env: String,
    #[serde(default)]
    pub default_model: String,
    #[serde(default)]
    pub models: Vec<ModelEntry>,
    /// Declarative list of additional inputs the UI should render for
    /// this provider (e.g. Azure endpoint URL + API version). Empty for
    /// providers that only need model + credential.
    #[serde(default)]
    pub extra_fields: Vec<ExtraField>,
    /// Non-default authentication kinds. Today only `Some("oauth_device")`
    /// (GitHub Copilot) is recognised — the credential pane renders a
    /// sign-in flow instead of an API-key textbox. Absent for providers
    /// that use a plain API key, which keeps the existing UI untouched.
    #[serde(default)]
    pub auth_kind: Option<String>,
}

impl ProviderEntry {
    /// Cheap convenience — keeps view-time match arms readable.
    pub fn is_oauth_device(&self) -> bool {
        self.auth_kind.as_deref() == Some("oauth_device")
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExtraField {
    pub key: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub placeholder: String,
    #[serde(default)]
    pub help: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub secret: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ModelEntry {
    pub name: String,
    // The LLM catalogue ships richer per-model metadata; we ignore the rest
    // here and only render the name in the dropdown.
}

/// `<modality> --status` payload.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Status {
    #[serde(default)]
    pub modality: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub api_key_credential: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub config_path: Option<String>,
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub reason: Option<ErrorEnvelope>,
    /// Raw `base_url` value as it lives on disk (may include
    /// `?api-version=…` for Azure).
    #[serde(default)]
    pub base_url: Option<String>,
    /// `base_url` minus its query string, for pre-populating the
    /// "endpoint" input separately from the "API version" input.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// `api-version` value parsed out of `base_url`'s query string.
    #[serde(default)]
    pub api_version: Option<String>,
}

/// `<modality> apply ...` payload.
#[derive(Clone, Debug, Deserialize)]
pub struct ApplyResult {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub key_source: String,
    #[serde(default)]
    pub config_path: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
}

/// `<modality> test` payload.
#[derive(Clone, Debug, Deserialize)]
pub struct TestResult {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub attempted: bool,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub reason: Option<serde_json::Value>,
    #[serde(default)]
    pub hint: Option<String>,
}

/// Common error envelope nested under `reason` on most failures.
#[derive(Clone, Debug, Deserialize)]
pub struct ErrorEnvelope {
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub details: String,
    #[serde(default)]
    pub fix: Option<String>,
}

// ---------------------------------------------------------------------------
// Page state
// ---------------------------------------------------------------------------

/// How the user wants the API key to be stored.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyMode {
    /// Save the typed secret into the agent credential store.
    Stored,
    /// Don't store anything; have the kernel read from an env var at runtime.
    EnvVar,
    /// Provider doesn't need a credential at all (e.g. edge TTS, local
    /// embeddings). Form is read-only.
    NotRequired,
}

impl KeyMode {
    pub const PICKER: [KeyMode; 2] = [KeyMode::Stored, KeyMode::EnvVar];

    pub fn label(self) -> String {
        match self {
            KeyMode::Stored => crate::fl!("agent-key-mode-stored"),
            KeyMode::EnvVar => crate::fl!("agent-key-mode-env"),
            KeyMode::NotRequired => crate::fl!("agent-key-mode-none"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct State {
    pub modality: Modality,
    pub status: Option<Status>,
    pub providers: Option<ProvidersDoc>,
    /// Index into `providers.providers`. `None` until loaded.
    pub provider_idx: Option<usize>,
    /// Index into the selected provider's `models` list. `None` if the user
    /// hasn't picked one yet or the catalogue is empty (free-form model).
    pub model_idx: Option<usize>,
    pub custom_model: String,
    pub api_key_input: String,
    pub api_key_hidden: bool,
    pub env_var_input: String,
    pub key_mode: KeyMode,
    /// Live values for the selected provider's `extra_fields`, keyed by
    /// `ExtraField.key` (e.g. "base_url" → Azure endpoint URL).
    /// Cleared when the user switches provider.
    pub extra_field_values: std::collections::HashMap<String, String>,
    /// Outstanding device-authorization flow (only set while the user is
    /// in the middle of signing in with a `oauth_device` provider).
    /// Cleared after success / failure / cancel.
    pub oauth_device: Option<DeviceCodeView>,
    /// True between `oauth-start` returning and the final terminal poll.
    /// Drives the spinner + disables the sign-in button.
    pub oauth_polling: bool,
    /// Surfaces oauth-specific errors (denied, expired, network) so they
    /// don't get conflated with the global `last_apply` error.
    pub oauth_error: Option<String>,
    /// Live-fetched Copilot model names (populated after sign-in via the
    /// `models` subcommand). Takes precedence over the static
    /// `provider.models` list when non-empty. Cleared on provider
    /// switch and on sign-out.
    pub live_models: Vec<String>,
    pub busy: bool,
    pub last_apply: Option<Result<ApplyResult, String>>,
    pub last_test: Option<Result<TestResult, String>>,
    pub load_error: Option<String>,
    /// Cached dropdown labels — owned here so the iced widgets can borrow
    /// them with a lifetime tied to `State`, not to a local in `view()`.
    /// Recomputed on provider/model changes via `refresh_labels`.
    provider_labels: Vec<String>,
    model_labels: Vec<String>,
    mode_labels: Vec<String>,
}

/// Subset of the kernel's `oauth-start` payload the UI cares about.
/// Stored in `State.oauth_device` for the lifetime of one sign-in flow.
#[derive(Clone, Debug)]
pub struct DeviceCodeView {
    pub user_code: String,
    pub verification_uri: String,
    pub device_code: String,
    /// Seconds between polls. Bumped by the kernel when GitHub returns
    /// `slow_down`. Clamped to a sane floor below.
    pub interval: u64,
}

/// Terminal state of a single `oauth-poll` invocation.
#[derive(Clone, Debug)]
pub enum PollStatus {
    Pending,
    SlowDown { interval: u64 },
    Authorized { credential: String },
    Expired,
    Denied,
}

impl State {
    pub fn new(modality: Modality) -> Self {
        Self {
            modality,
            status: None,
            providers: None,
            provider_idx: None,
            model_idx: None,
            custom_model: String::new(),
            api_key_input: String::new(),
            api_key_hidden: true,
            env_var_input: String::new(),
            key_mode: KeyMode::Stored,
            extra_field_values: std::collections::HashMap::new(),
            oauth_device: None,
            oauth_polling: false,
            oauth_error: None,
            live_models: Vec::new(),
            busy: false,
            last_apply: None,
            last_test: None,
            load_error: None,
            provider_labels: Vec::new(),
            model_labels: Vec::new(),
            mode_labels: KeyMode::PICKER.iter().map(|m| m.label()).collect(),
        }
    }

    pub fn selected_provider(&self) -> Option<&ProviderEntry> {
        let providers = self.providers.as_ref()?;
        providers.providers.get(self.provider_idx?)
    }

    /// Rebuild the cached dropdown labels from the current providers /
    /// selected provider. Cheap (small allocs) and called after every state
    /// mutation that affects what the dropdowns show.
    fn refresh_labels(&mut self) {
        self.provider_labels = self
            .providers
            .as_ref()
            .map(|p| {
                p.providers
                    .iter()
                    .map(|entry| {
                        if entry.label.is_empty() || entry.label == entry.name {
                            entry.name.clone()
                        } else {
                            format!("{} — {}", entry.name, entry.label)
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        // For OAuth providers the static `models` list is intentionally
        // empty (live-fetched after sign-in). We populate the dropdown
        // labels from `live_models` so the user picks against the same
        // catalogue Copilot would route to. Static-catalogue providers
        // keep their existing behaviour.
        self.model_labels = if let Some(p) = self.selected_provider() {
            if p.is_oauth_device() && !self.live_models.is_empty() {
                self.live_models.clone()
            } else {
                p.models.iter().map(|m| m.name.clone()).collect()
            }
        } else {
            Vec::new()
        };
    }

    pub fn selected_model_name(&self) -> String {
        if let Some(provider) = self.selected_provider() {
            if provider.is_oauth_device() && !self.live_models.is_empty() {
                if let Some(idx) = self.model_idx {
                    if let Some(name) = self.live_models.get(idx) {
                        return name.clone();
                    }
                }
                if !self.custom_model.is_empty() {
                    return self.custom_model.clone();
                }
            } else if !provider.models.is_empty() {
                if let Some(idx) = self.model_idx {
                    if let Some(model) = provider.models.get(idx) {
                        return model.name.clone();
                    }
                }
                if !self.custom_model.is_empty() {
                    return self.custom_model.clone();
                }
                return provider.default_model.clone();
            }
            if !self.custom_model.is_empty() {
                return self.custom_model.clone();
            }
            return provider.default_model.clone();
        }
        self.custom_model.clone()
    }

    /// Re-seed local form state when fresh `status` + `providers` data arrives.
    fn seed_from_loaded(&mut self) {
        let Some(providers) = &self.providers else {
            return;
        };
        let status_provider = self
            .status
            .as_ref()
            .map(|s| s.provider.as_str())
            .unwrap_or("");

        let provider_idx = providers
            .providers
            .iter()
            .position(|p| p.name == status_provider)
            .or_else(|| {
                providers.default_provider.as_ref().and_then(|name| {
                    providers.providers.iter().position(|p| &p.name == name)
                })
            })
            .or(if providers.providers.is_empty() {
                None
            } else {
                Some(0)
            });

        self.provider_idx = provider_idx;

        let status_model = self
            .status
            .as_ref()
            .map(|s| s.model.as_str())
            .unwrap_or("");

        if let Some(provider) = provider_idx.and_then(|i| providers.providers.get(i)) {
            self.model_idx = provider.models.iter().position(|m| m.name == status_model);
            if self.model_idx.is_none() && !status_model.is_empty() {
                self.custom_model = status_model.to_string();
            } else {
                self.custom_model.clear();
            }

            if !provider.needs_credential {
                self.key_mode = KeyMode::NotRequired;
            } else if let Some(status) = &self.status {
                if status.api_key_env.is_some() {
                    self.key_mode = KeyMode::EnvVar;
                    if let Some(env) = &status.api_key_env {
                        self.env_var_input = env.clone();
                    }
                } else {
                    self.key_mode = KeyMode::Stored;
                }
                if self.env_var_input.is_empty() {
                    self.env_var_input = provider.default_env.clone();
                }
            }

            // Seed any provider-declared extra inputs from the matching
            // status fields. Currently the only two are `base_url`
            // (Azure endpoint, sans api-version) and `api_version`,
            // both produced by the kernel's status command.
            self.extra_field_values.clear();
            if let Some(status) = &self.status {
                for field in &provider.extra_fields {
                    let value = match field.key.as_str() {
                        "base_url" => status
                            .endpoint
                            .clone()
                            .or_else(|| status.base_url.clone()),
                        "api_version" => status.api_version.clone(),
                        _ => None,
                    };
                    if let Some(v) = value {
                        if !v.is_empty() {
                            self.extra_field_values.insert(field.key.clone(), v);
                        }
                    }
                }
            }
        }

        self.refresh_labels();
    }

    // -----------------------------------------------------------------------
    // Update
    // -----------------------------------------------------------------------

    pub fn update<F>(&mut self, msg: Message, wrap: F) -> Task<pages::Message>
    where
        F: Fn(Message) -> pages::Message + Send + Sync + Clone + 'static,
    {
        match msg {
            Message::LoadDone(loaded) => {
                self.load_error = None;
                match loaded.providers {
                    Ok(p) => self.providers = Some(p),
                    Err(e) => self.load_error = Some(e),
                }
                match loaded.status {
                    Ok(s) => self.status = Some(s),
                    Err(e) => {
                        if self.load_error.is_none() {
                            self.load_error = Some(e);
                        }
                    }
                }
                self.seed_from_loaded();
                self.busy = false;
                // If the user is already signed in to an oauth_device
                // provider and we have no live models yet, kick off the
                // discovery request — the dropdown should be populated
                // the first time the user lands on this page.
                if let Some(provider) = self.selected_provider() {
                    if provider.is_oauth_device()
                        && self.is_signed_in_to_selected_provider()
                        && self.live_models.is_empty()
                    {
                        let provider_name = provider.name.clone();
                        let wrap2 = wrap.clone();
                        return Task::future(async move {
                            wrap2(Message::OauthModelsFetched(
                                fetch_oauth_models(&provider_name).await,
                            ))
                        });
                    }
                }
            }
            Message::ProviderSelected(idx) => {
                self.provider_idx = Some(idx);
                self.model_idx = None;
                self.custom_model.clear();
                self.last_test = None;
                self.extra_field_values.clear();
                self.oauth_device = None;
                self.oauth_polling = false;
                self.oauth_error = None;
                self.live_models.clear();
                if let Some((needs_credential, default_env)) =
                    self.selected_provider().map(|p| (p.needs_credential, p.default_env.clone()))
                {
                    self.key_mode = if needs_credential {
                        KeyMode::Stored
                    } else {
                        KeyMode::NotRequired
                    };
                    self.env_var_input = default_env;
                }
                self.refresh_labels();
                // Pull the live model list if we just selected an
                // already-signed-in OAuth provider.
                if let Some(provider) = self.selected_provider() {
                    if provider.is_oauth_device()
                        && self.is_signed_in_to_selected_provider()
                    {
                        let provider_name = provider.name.clone();
                        let wrap2 = wrap.clone();
                        return Task::future(async move {
                            wrap2(Message::OauthModelsFetched(
                                fetch_oauth_models(&provider_name).await,
                            ))
                        });
                    }
                }
            }
            Message::ModelSelected(idx) => {
                self.model_idx = Some(idx);
                self.custom_model.clear();
            }
            Message::CustomModelInput(s) => {
                self.custom_model = s;
                self.model_idx = None;
            }
            Message::ApiKeyInput(s) => {
                self.api_key_input = s;
            }
            Message::TogglePasswordVisibility => {
                self.api_key_hidden = !self.api_key_hidden;
            }
            Message::EnvVarInput(s) => {
                self.env_var_input = s;
            }
            Message::KeyModeSelected(mode) => {
                if self
                    .selected_provider()
                    .is_some_and(|p| p.needs_credential)
                {
                    self.key_mode = mode;
                }
            }
            Message::ExtraFieldInput(key, value) => {
                if value.is_empty() {
                    self.extra_field_values.remove(&key);
                } else {
                    self.extra_field_values.insert(key, value);
                }
            }
            Message::Save => {
                if self.busy {
                    return Task::none();
                }
                if let Some(args) = self.build_apply_args() {
                    self.busy = true;
                    let modality = self.modality;
                    let wrap2 = wrap.clone();
                    return Task::future(async move {
                        wrap2(Message::Saved(apply(modality, args).await))
                    });
                }
            }
            Message::Saved(result) => {
                self.busy = false;
                if let Ok(applied) = &result {
                    // Successful apply: clear sensitive state and refresh
                    // status from the kernel.
                    self.api_key_input.clear();
                    let _ = applied;
                    let modality = self.modality;
                    self.last_apply = Some(result);
                    let wrap2 = wrap.clone();
                    return Task::future(async move {
                        wrap2(Message::LoadDone(load(modality).await))
                    });
                }
                self.last_apply = Some(result);
            }
            Message::Test => {
                if self.busy {
                    return Task::none();
                }
                self.busy = true;
                let modality = self.modality;
                let wrap2 = wrap.clone();
                return Task::future(async move {
                    wrap2(Message::Tested(test(modality).await))
                });
            }
            Message::Tested(result) => {
                self.busy = false;
                self.last_test = Some(result);
            }
            Message::Reset => {
                if self.busy {
                    return Task::none();
                }
                self.busy = true;
                let modality = self.modality;
                let wrap2 = wrap.clone();
                return Task::future(async move {
                    wrap2(Message::ResetDone(reset(modality).await))
                });
            }
            Message::ResetDone(result) => {
                self.busy = false;
                match result {
                    Ok(()) => {
                        self.api_key_input.clear();
                        self.env_var_input.clear();
                        self.custom_model.clear();
                        self.last_test = None;
                        self.last_apply = None;
                        let modality = self.modality;
                        let wrap2 = wrap.clone();
                        return Task::future(async move {
                            wrap2(Message::LoadDone(load(modality).await))
                        });
                    }
                    Err(e) => {
                        self.last_apply = Some(Err(e));
                    }
                }
            }
            Message::Refresh => {
                if self.busy {
                    return Task::none();
                }
                self.busy = true;
                let modality = self.modality;
                let wrap2 = wrap.clone();
                return Task::future(async move {
                    wrap2(Message::LoadDone(load(modality).await))
                });
            }
            Message::OauthSignIn => {
                // Don't start a second flow while one is still in flight.
                if self.oauth_polling || self.oauth_device.is_some() {
                    return Task::none();
                }
                let provider_name = match self.selected_provider() {
                    Some(p) if p.is_oauth_device() => p.name.clone(),
                    _ => return Task::none(),
                };
                self.oauth_polling = true;
                self.oauth_error = None;
                let wrap2 = wrap.clone();
                return Task::future(async move {
                    wrap2(Message::OauthStartDone(oauth_start(&provider_name).await))
                });
            }
            Message::OauthStartDone(result) => {
                match result {
                    Ok(device) => {
                        let interval = device.interval.max(MIN_OAUTH_POLL_SECS);
                        let provider_name = self
                            .selected_provider()
                            .map(|p| p.name.clone())
                            .unwrap_or_default();
                        let code = device.device_code.clone();
                        self.oauth_device = Some(device);
                        // First poll fires after `interval` — gives the
                        // user time to open the URL and type the code.
                        let wrap2 = wrap.clone();
                        return Task::future(async move {
                            sleep_secs(interval).await;
                            wrap2(Message::OauthPollTick(provider_name, code))
                        });
                    }
                    Err(e) => {
                        self.oauth_polling = false;
                        self.oauth_device = None;
                        self.oauth_error = Some(e);
                    }
                }
            }
            Message::OauthPollTick(provider_name, device_code) => {
                // Guard against stale ticks after a sign-out / cancel.
                let still_relevant = self.oauth_polling
                    && self
                        .oauth_device
                        .as_ref()
                        .map(|d| d.device_code == device_code)
                        .unwrap_or(false);
                if !still_relevant {
                    return Task::none();
                }
                let wrap2 = wrap.clone();
                let provider = provider_name.clone();
                let code = device_code.clone();
                return Task::future(async move {
                    wrap2(Message::OauthPollDone(
                        provider,
                        code,
                        oauth_poll(&provider_name, &device_code).await,
                    ))
                });
            }
            Message::OauthPollDone(provider_name, device_code, result) => {
                let still_relevant = self
                    .oauth_device
                    .as_ref()
                    .map(|d| d.device_code == device_code)
                    .unwrap_or(false);
                if !still_relevant {
                    return Task::none();
                }
                match result {
                    Ok(PollStatus::Pending) => {
                        let interval = self
                            .oauth_device
                            .as_ref()
                            .map(|d| d.interval)
                            .unwrap_or(MIN_OAUTH_POLL_SECS)
                            .max(MIN_OAUTH_POLL_SECS);
                        let wrap2 = wrap.clone();
                        return Task::future(async move {
                            sleep_secs(interval).await;
                            wrap2(Message::OauthPollTick(provider_name, device_code))
                        });
                    }
                    Ok(PollStatus::SlowDown { interval }) => {
                        let interval = interval.max(MIN_OAUTH_POLL_SECS);
                        if let Some(device) = self.oauth_device.as_mut() {
                            device.interval = interval;
                        }
                        let wrap2 = wrap.clone();
                        return Task::future(async move {
                            sleep_secs(interval).await;
                            wrap2(Message::OauthPollTick(provider_name, device_code))
                        });
                    }
                    Ok(PollStatus::Authorized { credential: _ }) => {
                        self.oauth_polling = false;
                        self.oauth_device = None;
                        self.oauth_error = None;
                        // The kernel stored the long-lived token under
                        // the credential name baked into auth_kind=
                        // oauth_device flows; we now refresh status
                        // (so the UI re-renders the "signed in" branch)
                        // and kick off the live model fetch.
                        let modality = self.modality;
                        let wrap_for_load = wrap.clone();
                        let wrap_for_models = wrap.clone();
                        let provider_for_models = provider_name.clone();
                        return Task::batch(vec![
                            Task::future(async move {
                                wrap_for_load(Message::LoadDone(load(modality).await))
                            }),
                            Task::future(async move {
                                wrap_for_models(Message::OauthModelsFetched(
                                    fetch_oauth_models(&provider_for_models).await,
                                ))
                            }),
                        ]);
                    }
                    Ok(PollStatus::Expired) => {
                        self.oauth_polling = false;
                        self.oauth_device = None;
                        self.oauth_error = Some(crate::fl!("agent-oauth-expired"));
                    }
                    Ok(PollStatus::Denied) => {
                        self.oauth_polling = false;
                        self.oauth_device = None;
                        self.oauth_error = Some(crate::fl!("agent-oauth-denied"));
                    }
                    Err(e) => {
                        self.oauth_polling = false;
                        self.oauth_device = None;
                        self.oauth_error = Some(e);
                    }
                }
            }
            Message::OauthCancel => {
                // The kernel has no "abort device flow" endpoint —
                // expired codes are GC'd server-side. Locally we just
                // drop the in-flight state so polling stops.
                self.oauth_polling = false;
                self.oauth_device = None;
                self.oauth_error = None;
            }
            Message::OauthSignOut => {
                // Clearing the modality on the kernel side is the
                // safest revoke: the next request will see no
                // credential and error out cleanly. The token blob
                // itself is left in the credential store so the user
                // can sign back in without re-doing the device flow
                // unless they explicitly revoke from github.com.
                if self.busy {
                    return Task::none();
                }
                self.busy = true;
                self.oauth_device = None;
                self.oauth_polling = false;
                self.oauth_error = None;
                self.live_models.clear();
                let modality = self.modality;
                let wrap2 = wrap.clone();
                return Task::future(async move {
                    wrap2(Message::ResetDone(reset(modality).await))
                });
            }
            Message::OauthModelsFetched(result) => {
                match result {
                    Ok(models) => {
                        self.live_models = models;
                        self.refresh_labels();
                    }
                    Err(e) => {
                        // Don't blow away `oauth_error` if it was set
                        // by something more important. Models fetch is
                        // best-effort: the user can still type a name
                        // in the custom-model textbox.
                        if self.oauth_error.is_none() {
                            self.oauth_error = Some(format!(
                                "{}: {e}",
                                crate::fl!("agent-oauth-models-failed")
                            ));
                        }
                    }
                }
            }
        }
        Task::none()
    }

    /// Initial load triggered from `Page::on_enter`.
    pub fn on_enter<F>(&mut self, wrap: F) -> Task<pages::Message>
    where
        F: Fn(Message) -> pages::Message + Send + Sync + 'static,
    {
        self.busy = true;
        let modality = self.modality;
        Task::future(async move { wrap(Message::LoadDone(load(modality).await)) })
    }

    /// Build the `apply` argv from the current form state. Returns `None` if
    /// there is nothing to write (e.g. no provider selected).
    fn build_apply_args(&self) -> Option<ApplyArgs> {
        let provider = self.selected_provider()?;
        let model = self.selected_model_name();
        let credential = if provider.is_oauth_device() {
            // OAuth providers establish their credential out-of-band via
            // `oauth-poll`. The kernel re-reads it on `apply` so the
            // form has nothing left to do here.
            CredentialArg::None
        } else if provider.needs_credential {
            match self.key_mode {
                KeyMode::Stored => {
                    if self.api_key_input.is_empty() {
                        CredentialArg::KeepExisting
                    } else {
                        CredentialArg::Stdin(self.api_key_input.clone())
                    }
                }
                KeyMode::EnvVar => {
                    if self.env_var_input.is_empty() {
                        return None;
                    }
                    CredentialArg::Env(self.env_var_input.clone())
                }
                KeyMode::NotRequired => CredentialArg::None,
            }
        } else {
            CredentialArg::None
        };

        let mut extras = Vec::with_capacity(provider.extra_fields.len());
        for field in &provider.extra_fields {
            let raw = self
                .extra_field_values
                .get(&field.key)
                .map(String::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            extras.push((field.key.clone(), raw));
        }

        Some(ApplyArgs {
            provider: provider.name.clone(),
            model,
            credential,
            extras,
        })
    }

    /// Whether the currently selected provider is `auth_kind=oauth_device`
    /// and the kernel reports a stored credential under that provider's
    /// expected name. Drives the sign-in / signed-in branch in the view.
    pub fn is_signed_in_to_selected_provider(&self) -> bool {
        let Some(provider) = self.selected_provider() else {
            return false;
        };
        if !provider.is_oauth_device() {
            return false;
        }
        // We rely on the kernel's `--status` reporting the provider's
        // currently-configured credential name. Any non-empty value
        // counts as "signed in" — we don't hard-code the name on the
        // UI side so a future second OAuth provider doesn't need a
        // matching client-side edit.
        self.status
            .as_ref()
            .and_then(|s| s.api_key_credential.as_deref())
            .map(|n| !n.is_empty())
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Message {
    LoadDone(Loaded),
    ProviderSelected(usize),
    ModelSelected(usize),
    CustomModelInput(String),
    ApiKeyInput(String),
    TogglePasswordVisibility,
    EnvVarInput(String),
    KeyModeSelected(KeyMode),
    /// User edited one of the provider-declared `extra_fields` inputs
    /// (Azure endpoint URL, API version, …). First field is the
    /// `ExtraField.key` it came from.
    ExtraFieldInput(String, String),
    Save,
    Saved(Result<ApplyResult, String>),
    Test,
    Tested(Result<TestResult, String>),
    Reset,
    ResetDone(Result<(), String>),
    Refresh,
    /// User clicked the "Sign in with GitHub" button on an
    /// `auth_kind=oauth_device` provider.
    OauthSignIn,
    /// Result of the initial `oauth-start` shell-out.
    OauthStartDone(Result<DeviceCodeView, String>),
    /// Timer-driven poll. Carries the provider + device_code that the
    /// tick belongs to so stale ticks (from a cancelled flow) can be
    /// detected and dropped.
    OauthPollTick(String, String),
    /// Result of a single `oauth-poll` invocation. Carries provider +
    /// device_code for the same staleness check, plus the parsed
    /// status enum.
    OauthPollDone(String, String, Result<PollStatus, String>),
    /// User clicked the "Cancel" button while the device flow was
    /// still in flight. Stops polling; no kernel call.
    OauthCancel,
    /// User clicked "Sign out" on an already-signed-in OAuth
    /// provider. Triggers a kernel `--reset` for this modality.
    OauthSignOut,
    /// Result of the `models --provider <name>` shell-out fired after
    /// a successful sign-in (or on page entry if already signed in).
    OauthModelsFetched(Result<Vec<String>, String>),
}

/// Combined `--providers` + `--status` payload for a single load round-trip.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub providers: Result<ProvidersDoc, String>,
    pub status: Result<Status, String>,
}

// ---------------------------------------------------------------------------
// Shell-out helpers (cos agent setup ...)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct ApplyArgs {
    provider: String,
    model: String,
    credential: CredentialArg,
    /// Provider-declared extra inputs (key → value), straight from
    /// `extra_fields`. Empty values are passed through so the kernel
    /// can clear stored extras.
    extras: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
enum CredentialArg {
    /// Provider doesn't need (or shouldn't keep) a credential.
    None,
    /// User typed a new secret; pipe it via stdin so it never hits argv.
    Stdin(String),
    /// User picked the env-var passthrough mode.
    Env(String),
    /// User left the secret field empty -> reuse whatever is already stored.
    KeepExisting,
}

pub async fn load(modality: Modality) -> Loaded {
    let providers_fut = providers(modality);
    let status_fut = status(modality);
    let (providers, status) = tokio::join!(providers_fut, status_fut);
    Loaded { providers, status }
}

async fn providers(modality: Modality) -> Result<ProvidersDoc, String> {
    let stdout = cos_setup(&["--providers", modality.as_arg()], None).await?;
    serde_json::from_str(&stdout).map_err(|e| format!("invalid providers JSON: {e}"))
}

async fn status(modality: Modality) -> Result<Status, String> {
    let stdout = cos_setup(&[modality.as_arg(), "--status"], None).await?;
    serde_json::from_str(&stdout).map_err(|e| format!("invalid status JSON: {e}"))
}

async fn apply(modality: Modality, args: ApplyArgs) -> Result<ApplyResult, String> {
    let provider = args.provider;
    let model = args.model;
    let modality_arg = modality.as_arg();
    let mut argv: Vec<String> = vec![
        modality_arg.into(),
        "apply".into(),
        "--provider".into(),
        provider,
    ];
    if !model.is_empty() {
        argv.push("--model".into());
        argv.push(model);
    }
    for (key, value) in &args.extras {
        // Skip empties: today the kernel has no "clear extras" flag,
        // and an empty `--base-url` would fail Azure validation.
        if value.is_empty() {
            continue;
        }
        argv.push(format!("--{}", key.replace('_', "-")));
        argv.push(value.clone());
    }
    let stdin = match args.credential {
        CredentialArg::None | CredentialArg::KeepExisting => None,
        CredentialArg::Stdin(secret) => {
            argv.push("--api-key-stdin".into());
            Some(secret)
        }
        CredentialArg::Env(env) => {
            argv.push("--api-key-env".into());
            argv.push(env);
            None
        }
    };
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let stdout = cos_setup(&refs, stdin).await?;
    serde_json::from_str(&stdout).map_err(|e| format!("invalid apply JSON: {e}"))
}

async fn test(modality: Modality) -> Result<TestResult, String> {
    let stdout = cos_setup(&[modality.as_arg(), "test"], None).await?;
    serde_json::from_str(&stdout).map_err(|e| format!("invalid test JSON: {e}"))
}

async fn reset(modality: Modality) -> Result<(), String> {
    cos_setup(&[modality.as_arg(), "--reset"], None)
        .await
        .map(|_| ())
}

// ---------------------------------------------------------------------------
// OAuth-device shell-outs (currently only Copilot)
// ---------------------------------------------------------------------------

/// Floor on the polling interval honoured by the UI. Protects us from
/// the kernel (or a misbehaving fixture) returning `interval=0`, which
/// would spin the executor.
const MIN_OAUTH_POLL_SECS: u64 = 5;

async fn oauth_start(provider: &str) -> Result<DeviceCodeView, String> {
    let stdout = cos_setup(&["llm", "oauth-start", "--provider", provider], None).await?;
    let v: serde_json::Value =
        serde_json::from_str(&stdout).map_err(|e| format!("invalid oauth-start JSON: {e}"))?;
    let user_code = v
        .get("user_code")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "oauth-start: missing user_code".to_string())?
        .to_string();
    let verification_uri = v
        .get("verification_uri")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "oauth-start: missing verification_uri".to_string())?
        .to_string();
    let device_code = v
        .get("device_code")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "oauth-start: missing device_code".to_string())?
        .to_string();
    let interval = v
        .get("interval")
        .and_then(|x| x.as_u64())
        .unwrap_or(MIN_OAUTH_POLL_SECS);
    Ok(DeviceCodeView {
        user_code,
        verification_uri,
        device_code,
        interval,
    })
}

async fn oauth_poll(provider: &str, device_code: &str) -> Result<PollStatus, String> {
    let stdout = cos_setup(
        &[
            "llm",
            "oauth-poll",
            "--provider",
            provider,
            "--device-code",
            device_code,
        ],
        None,
    )
    .await?;
    let v: serde_json::Value =
        serde_json::from_str(&stdout).map_err(|e| format!("invalid oauth-poll JSON: {e}"))?;
    let status = v
        .get("status")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "oauth-poll: missing status".to_string())?;
    match status {
        "pending" => Ok(PollStatus::Pending),
        "slow_down" => Ok(PollStatus::SlowDown {
            interval: v
                .get("interval")
                .and_then(|x| x.as_u64())
                .unwrap_or(MIN_OAUTH_POLL_SECS),
        }),
        "ok" => Ok(PollStatus::Authorized {
            credential: v
                .get("credential")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string(),
        }),
        "expired" => Ok(PollStatus::Expired),
        "denied" => Ok(PollStatus::Denied),
        other => Err(format!("oauth-poll: unexpected status `{other}`")),
    }
}

async fn fetch_oauth_models(provider: &str) -> Result<Vec<String>, String> {
    let stdout = cos_setup(&["llm", "models", "--provider", provider], None).await?;
    let v: serde_json::Value =
        serde_json::from_str(&stdout).map_err(|e| format!("invalid models JSON: {e}"))?;
    let arr = v
        .get("models")
        .and_then(|x| x.as_array())
        .ok_or_else(|| "models: missing `models` array".to_string())?;
    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        if let Some(name) = entry.get("name").and_then(|x| x.as_str()) {
            if !name.is_empty() {
                out.push(name.to_string());
            }
        }
    }
    Ok(out)
}

async fn sleep_secs(secs: u64) {
    tokio::time::sleep(std::time::Duration::from_secs(secs.max(MIN_OAUTH_POLL_SECS))).await;
}

/// Invoke `cos agent setup <argv...>`, optionally piping a secret via stdin.
/// Returns stdout on success. On failure returns stderr (or stdout if stderr
/// is empty — `cos` writes JSON error envelopes there too).
async fn cos_setup(extra_argv: &[&str], stdin_input: Option<String>) -> Result<String, String> {
    let mut cmd = Command::new("cos");
    cmd.arg("agent")
        .arg("setup")
        .args(extra_argv)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if stdin_input.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn `cos`: {e}"))?;

    if let Some(input) = stdin_input {
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(input.as_bytes())
                .await
                .map_err(|e| format!("failed to write stdin: {e}"))?;
            drop(stdin);
        }
    }

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| format!("failed to wait on `cos`: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    if output.status.success() {
        return Ok(stdout);
    }

    // Non-zero exit: prefer stderr (where JSON error envelopes go), fall back
    // to stdout, and finally a generic message keyed off the exit status.
    let mut message = if !stderr.trim().is_empty() {
        stderr
    } else if !stdout.trim().is_empty() {
        stdout
    } else {
        format!("`cos agent setup` exited with {}", output.status)
    };
    // If stderr is itself a JSON error envelope, pull the human-readable
    // pieces out so we surface "error: details" instead of raw braces.
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&message) {
        let err = value.get("error").and_then(|v| v.as_str()).unwrap_or("");
        let details = value
            .get("details")
            .and_then(|v| v.as_str())
            .or_else(|| value.get("reason").and_then(|v| v.as_str()))
            .unwrap_or("");
        let fix = value.get("fix").and_then(|v| v.as_str()).unwrap_or("");
        let mut combined = String::new();
        if !err.is_empty() {
            combined.push_str(err);
        }
        if !details.is_empty() {
            if !combined.is_empty() {
                combined.push_str(": ");
            }
            combined.push_str(details);
        }
        if !fix.is_empty() {
            if !combined.is_empty() {
                combined.push_str("\nFix: ");
            }
            combined.push_str(fix);
        }
        if !combined.is_empty() {
            message = combined;
        }
    }
    Err(message)
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

pub fn status_section<P, F>(
    state_of: fn(&P) -> &State,
    wrap: F,
) -> Section<pages::Message>
where
    P: cosmic_settings_page::Page<pages::Message> + 'static,
    F: Fn(Message) -> pages::Message + Send + Sync + Clone + 'static,
{
    Section::default()
        .title(crate::fl!("agent-status"))
        .view::<P>(move |_, page, section| {
            let state = state_of(page);
            let view: Element<'_, Message> = status_view(state);
            let wrap = wrap.clone();
            settings::section()
                .title(&section.title)
                .add(view.map(move |m| wrap(m)))
                .into()
        })
}

pub fn provider_section<P, F>(
    state_of: fn(&P) -> &State,
    wrap: F,
) -> Section<pages::Message>
where
    P: cosmic_settings_page::Page<pages::Message> + 'static,
    F: Fn(Message) -> pages::Message + Send + Sync + Clone + 'static,
{
    Section::default()
        .title(crate::fl!("agent-configuration"))
        .view::<P>(move |_, page, section| {
            let state = state_of(page);
            let view: Element<'_, Message> = configuration_view(state);
            let wrap = wrap.clone();
            settings::section()
                .title(&section.title)
                .add(view.map(move |m| wrap(m)))
                .into()
        })
}

pub fn actions_section<P, F>(
    state_of: fn(&P) -> &State,
    wrap: F,
) -> Section<pages::Message>
where
    P: cosmic_settings_page::Page<pages::Message> + 'static,
    F: Fn(Message) -> pages::Message + Send + Sync + Clone + 'static,
{
    Section::default()
        .title(crate::fl!("agent-actions"))
        .view::<P>(move |_, page, _section| {
            // Actions render without the `settings::section()` card
            // background — three buttons + a status caption don't need
            // the visual weight of a framed container, and the page
            // already renders the "Actions" heading above this view.
            let state = state_of(page);
            let view: Element<'_, Message> = actions_view(state);
            let wrap = wrap.clone();
            view.map(move |m| wrap(m))
        })
}

fn status_view(state: &State) -> Element<'_, Message> {
    let mut col = column::with_capacity(4).spacing(8);

    if let Some(err) = &state.load_error {
        col = col.push(text::body(format!(
            "{}: {err}",
            crate::fl!("agent-status-load-error")
        )));
    }

    if let Some(status) = &state.status {
        let badge_label = if status.ready {
            crate::fl!("agent-status-ready")
        } else {
            crate::fl!("agent-status-not-ready")
        };
        let mut row = row::with_capacity(3).spacing(12).align_y(Alignment::Center);
        row = row.push(text::heading(badge_label));
        if !status.provider.is_empty() {
            row = row.push(text::body(format!(
                "{} / {}",
                status.provider,
                if status.model.is_empty() {
                    Cow::Borrowed("—")
                } else {
                    Cow::Borrowed(status.model.as_str())
                }
            )));
        }
        col = col.push(row);

        if let Some(reason) = &status.reason {
            let mut why = String::new();
            if !reason.error.is_empty() {
                why.push_str(&reason.error);
            }
            if !reason.details.is_empty() {
                if !why.is_empty() {
                    why.push_str(": ");
                }
                why.push_str(&reason.details);
            }
            if !why.is_empty() {
                col = col.push(text::caption(why));
            }
        }
        if let Some(path) = &status.config_path {
            col = col.push(text::caption(format!(
                "{}: {path}",
                crate::fl!("agent-config-path")
            )));
        }
    } else if state.load_error.is_none() {
        col = col.push(text::body(crate::fl!("agent-status-loading")));
    }

    col.into()
}

fn configuration_view(state: &State) -> Element<'_, Message> {
    let Some(providers) = &state.providers else {
        return text::body(crate::fl!("agent-providers-loading")).into();
    };
    if providers.providers.is_empty() {
        return text::body(crate::fl!("agent-providers-empty")).into();
    }

    let provider_dropdown = dropdown(
        &state.provider_labels,
        state.provider_idx,
        Message::ProviderSelected,
    );

    let provider_row = settings::item::builder(crate::fl!("agent-provider"))
        .flex_control(provider_dropdown.apply(Element::from));

    let mut col = column::with_capacity(8).spacing(12).push(provider_row);

    if let Some(provider) = state.selected_provider() {
        // Model picker. If we have a curated list, render a dropdown; the
        // text input is always available so users can override. For
        // OAuth providers the dropdown is populated from `live_models`
        // (fetched post-sign-in); when empty it's hidden, leaving only
        // the free-form textbox until the user signs in.
        let dropdown_visible = if provider.is_oauth_device() {
            !state.live_models.is_empty()
        } else {
            !provider.models.is_empty()
        };
        if dropdown_visible {
            let model_dropdown = dropdown(
                &state.model_labels,
                state.model_idx,
                Message::ModelSelected,
            );
            col = col.push(
                settings::item::builder(crate::fl!("agent-model"))
                    .flex_control(model_dropdown.apply(Element::from)),
            );
        }

        let placeholder = if provider.default_model.is_empty() {
            String::new()
        } else {
            provider.default_model.clone()
        };
        let custom_value: &str = if !state.custom_model.is_empty() {
            state.custom_model.as_str()
        } else if let Some(idx) = state.model_idx {
            if provider.is_oauth_device() {
                state
                    .live_models
                    .get(idx)
                    .map(String::as_str)
                    .unwrap_or("")
            } else {
                provider
                    .models
                    .get(idx)
                    .map(|m| m.name.as_str())
                    .unwrap_or("")
            }
        } else {
            ""
        };
        let custom_input = widget::text_input(placeholder, custom_value)
            .on_input(Message::CustomModelInput);
        col = col.push(
            settings::item::builder(crate::fl!("agent-model-custom"))
                .description(crate::fl!("agent-model-custom", "desc"))
                .flex_control(custom_input.apply(Element::from)),
        );

        // Credential section.
        if provider.is_oauth_device() {
            col = col.push(oauth_credential_view(state));
        } else if !provider.needs_credential {
            col = col.push(
                container(text::body(crate::fl!("agent-key-not-required")))
                    .padding(8)
                    .apply(Element::from),
            );
        } else {
            let mode_idx = match state.key_mode {
                KeyMode::Stored => Some(0usize),
                KeyMode::EnvVar => Some(1usize),
                KeyMode::NotRequired => None,
            };
            let mode_dropdown = dropdown(&state.mode_labels, mode_idx, |idx| {
                Message::KeyModeSelected(KeyMode::PICKER[idx])
            });
            col = col.push(
                settings::item::builder(crate::fl!("agent-key-mode"))
                    .flex_control(mode_dropdown.apply(Element::from)),
            );

            match state.key_mode {
                KeyMode::Stored => {
                    let placeholder = crate::fl!("agent-key-placeholder");
                    let secure = widget::text_input::secure_input(
                        placeholder,
                        &state.api_key_input,
                        Some(Message::TogglePasswordVisibility),
                        state.api_key_hidden,
                    )
                    .on_input(Message::ApiKeyInput);
                    col = col.push(
                        settings::item::builder(crate::fl!("agent-key-input"))
                            .description(crate::fl!("agent-key-input", "desc"))
                            .flex_control(secure.apply(Element::from)),
                    );
                    if let Some(status) = &state.status {
                        if let Some(name) = &status.api_key_credential {
                            col = col.push(text::caption(format!(
                                "{}: {name}",
                                crate::fl!("agent-key-stored-as")
                            )));
                        }
                    }
                }
                KeyMode::EnvVar => {
                    let env_input = widget::text_input(
                        provider.default_env.as_str(),
                        &state.env_var_input,
                    )
                    .on_input(Message::EnvVarInput);
                    col = col.push(
                        settings::item::builder(crate::fl!("agent-key-env"))
                            .description(crate::fl!("agent-key-env", "desc"))
                            .flex_control(env_input.apply(Element::from)),
                    );
                }
                KeyMode::NotRequired => {}
            }
        }

        // Provider-declared extra inputs (e.g. Azure endpoint URL +
        // API version). Rendered as a plain text input for non-secret
        // fields and a masked input for secrets. Labels come straight
        // from the kernel's `--providers` JSON so localizing them is
        // the kernel's responsibility, not ours.
        for field in &provider.extra_fields {
            let value = state
                .extra_field_values
                .get(&field.key)
                .map(String::as_str)
                .unwrap_or("");
            let label = if field.label.is_empty() {
                field.key.clone()
            } else if field.required {
                format!("{} *", field.label)
            } else {
                field.label.clone()
            };
            let key_for_msg = field.key.clone();
            let on_input = move |v: String| {
                Message::ExtraFieldInput(key_for_msg.clone(), v)
            };
            let input: Element<'_, Message> = if field.secret {
                widget::text_input::secure_input(
                    field.placeholder.as_str(),
                    value,
                    Some(Message::TogglePasswordVisibility),
                    state.api_key_hidden,
                )
                .on_input(on_input)
                .apply(Element::from)
            } else {
                widget::text_input(field.placeholder.as_str(), value)
                    .on_input(on_input)
                    .apply(Element::from)
            };
            let mut builder = settings::item::builder(label);
            if !field.help.is_empty() {
                builder = builder.description(field.help.clone());
            }
            col = col.push(builder.flex_control(input));
        }
    }

    col.into()
}

/// Render the credential pane for providers whose `auth_kind` is
/// `oauth_device`. Three states: (1) signed in — show the stored
/// credential name + Sign out button; (2) device-flow in progress —
/// show user code, verification URL, and a Cancel button; (3) idle —
/// show a Sign in button. Errors from previous attempts (denied /
/// expired / network) are surfaced inline so the user knows why
/// nothing happened.
fn oauth_credential_view(state: &State) -> Element<'_, Message> {
    let mut inner = column::with_capacity(4).spacing(12);

    if state.is_signed_in_to_selected_provider() {
        let credential = state
            .status
            .as_ref()
            .and_then(|s| s.api_key_credential.as_deref())
            .unwrap_or("");
        let signed_in_label = if credential.is_empty() {
            crate::fl!("agent-oauth-signed-in")
        } else {
            format!("{} ({credential})", crate::fl!("agent-oauth-signed-in"))
        };
        let sign_out_btn = if state.busy {
            button::standard(crate::fl!("agent-oauth-sign-out"))
        } else {
            button::standard(crate::fl!("agent-oauth-sign-out"))
                .on_press(Message::OauthSignOut)
        };
        let pane = row::with_capacity(2)
            .spacing(12)
            .align_y(Alignment::Center)
            .push(text::body(signed_in_label))
            .push(sign_out_btn);
        inner = inner.push(
            settings::item::builder(crate::fl!("agent-key-mode"))
                .flex_control(Element::from(pane)),
        );
    } else if let Some(device) = &state.oauth_device {
        let cancel_btn = button::destructive(crate::fl!("agent-oauth-cancel"))
            .on_press(Message::OauthCancel);
        let instructions = crate::fl!(
            "agent-oauth-instructions",
            url = device.verification_uri.as_str()
        );
        let body = column::with_capacity(4)
            .spacing(8)
            .push(text::body(instructions))
            .push(text::heading(device.user_code.clone()))
            .push(text::caption(crate::fl!("agent-oauth-waiting")))
            .push(cancel_btn);
        inner = inner.push(
            settings::item::builder(crate::fl!("agent-oauth-user-code"))
                .flex_control(Element::from(body)),
        );
    } else {
        let sign_in_btn = if state.busy || state.oauth_polling {
            button::suggested(crate::fl!("agent-oauth-sign-in"))
        } else {
            button::suggested(crate::fl!("agent-oauth-sign-in"))
                .on_press(Message::OauthSignIn)
        };
        inner = inner.push(
            settings::item::builder(crate::fl!("agent-key-mode"))
                .flex_control(Element::from(sign_in_btn)),
        );
    }

    if let Some(err) = &state.oauth_error {
        inner = inner.push(text::caption(format!(
            "{}: {err}",
            crate::fl!("agent-oauth-failed")
        )));
    }

    inner.into()
}

fn actions_view(state: &State) -> Element<'_, Message> {
    let save_btn = if state.busy {
        button::suggested(crate::fl!("agent-save"))
    } else {
        button::suggested(crate::fl!("agent-save")).on_press(Message::Save)
    };
    let test_btn = if state.busy {
        button::standard(crate::fl!("agent-test"))
    } else {
        button::standard(crate::fl!("agent-test")).on_press(Message::Test)
    };
    let reset_btn = if state.busy {
        button::destructive(crate::fl!("agent-reset"))
    } else {
        button::destructive(crate::fl!("agent-reset")).on_press(Message::Reset)
    };

    let buttons = row::with_capacity(3)
        .spacing(12)
        .align_y(Alignment::Center)
        .push(save_btn)
        .push(test_btn)
        .push(reset_btn);

    let mut col = column::with_capacity(4).spacing(12).push(buttons);

    if let Some(result) = &state.last_apply {
        let line = match result {
            Ok(r) => format!(
                "{} {} / {} ({}: {})",
                crate::fl!("agent-apply-ok"),
                r.provider,
                if r.model.is_empty() { "—" } else { &r.model },
                crate::fl!("agent-key-source"),
                r.key_source
            ),
            Err(e) => format!("{}: {e}", crate::fl!("agent-apply-failed")),
        };
        col = col.push(text::body(line));
    }

    if let Some(result) = &state.last_test {
        let line = match result {
            Ok(r) if r.ok => {
                format!(
                    "✓ {} ({})",
                    crate::fl!("agent-test-ok"),
                    r.kind
                )
            }
            Ok(r) => {
                let hint = r.hint.clone().unwrap_or_default();
                let why = r
                    .reason
                    .as_ref()
                    .map(|v| describe_reason(v))
                    .unwrap_or_default();
                let mut s = format!("✗ {}", crate::fl!("agent-test-failed"));
                if !why.is_empty() {
                    s.push_str(": ");
                    s.push_str(&why);
                }
                if !hint.is_empty() {
                    s.push_str("\n");
                    s.push_str(&hint);
                }
                s
            }
            Err(e) => format!("{}: {e}", crate::fl!("agent-test-failed")),
        };
        col = col.push(text::body(line));
    }

    col.width(Length::Fill).into()
}

fn describe_reason(value: &serde_json::Value) -> String {
    if let Some(s) = value.as_str() {
        return s.to_string();
    }
    if let Some(obj) = value.as_object() {
        let error = obj.get("error").and_then(|v| v.as_str()).unwrap_or("");
        let details = obj.get("details").and_then(|v| v.as_str()).unwrap_or("");
        match (error.is_empty(), details.is_empty()) {
            (true, true) => value.to_string(),
            (false, true) => error.to_string(),
            (true, false) => details.to_string(),
            (false, false) => format!("{error}: {details}"),
        }
    } else {
        value.to_string()
    }
}
