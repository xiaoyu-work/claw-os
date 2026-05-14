// SPDX-License-Identifier: GPL-3.0-only
//
// Shared state, messages, view helpers and `cos agent setup` shell-out
// plumbing for the Agent settings page. Each modality (LLM / TTS / STT /
// image gen / embeddings) gets its own thin `Page` wrapper around `State`;
// all the actual logic lives here so the per-modality files stay tiny.
//
// The page never touches `/etc/cos/config.json` directly. Reads and writes
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
    pub busy: bool,
    pub last_apply: Option<Result<ApplyResult, String>>,
    pub last_test: Option<Result<TestResult, String>>,
    pub load_error: Option<String>,
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
            busy: false,
            last_apply: None,
            last_test: None,
            load_error: None,
        }
    }

    pub fn selected_provider(&self) -> Option<&ProviderEntry> {
        let providers = self.providers.as_ref()?;
        providers.providers.get(self.provider_idx?)
    }

    pub fn selected_model_name(&self) -> String {
        if let Some(provider) = self.selected_provider() {
            if !provider.models.is_empty() {
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
            }
            Message::ProviderSelected(idx) => {
                self.provider_idx = Some(idx);
                self.model_idx = None;
                self.custom_model.clear();
                self.last_test = None;
                self.extra_field_values.clear();
                if let Some(p) = self.selected_provider() {
                    self.key_mode = if p.needs_credential {
                        KeyMode::Stored
                    } else {
                        KeyMode::NotRequired
                    };
                    self.env_var_input = p.default_env.clone();
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
        let credential = if provider.needs_credential {
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
        .view::<P>(move |_, page, section| {
            let state = state_of(page);
            let view: Element<'_, Message> = actions_view(state);
            let wrap = wrap.clone();
            settings::section()
                .title(&section.title)
                .add(view.map(move |m| wrap(m)))
                .into()
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
        row = row.push(text::heading(&badge_label));
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

    let provider_labels: Vec<String> = providers
        .providers
        .iter()
        .map(|p| {
            if p.label.is_empty() || p.label == p.name {
                p.name.clone()
            } else {
                format!("{} — {}", p.name, p.label)
            }
        })
        .collect();

    let provider_dropdown = dropdown(
        &provider_labels,
        state.provider_idx,
        Message::ProviderSelected,
    );

    let provider_row = settings::item::builder(crate::fl!("agent-provider"))
        .flex_control(provider_dropdown.apply(Element::from));

    let mut col = column::with_capacity(8).spacing(12).push(provider_row);

    if let Some(provider) = state.selected_provider() {
        // Model picker. If we have a curated list, render a dropdown; the
        // text input is always available so users can override.
        if !provider.models.is_empty() {
            let model_names: Vec<String> =
                provider.models.iter().map(|m| m.name.clone()).collect();
            let model_dropdown =
                dropdown(&model_names, state.model_idx, Message::ModelSelected);
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
        let custom_value = if !state.custom_model.is_empty() {
            state.custom_model.as_str()
        } else if let Some(idx) = state.model_idx {
            provider
                .models
                .get(idx)
                .map(|m| m.name.as_str())
                .unwrap_or("")
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
        if !provider.needs_credential {
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
            let mode_labels: Vec<String> =
                KeyMode::PICKER.iter().map(|m| m.label()).collect();
            let mode_dropdown = dropdown(&mode_labels, mode_idx, |idx| {
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
            let mut item = settings::item::builder(label).flex_control(input);
            if !field.help.is_empty() {
                item = item.description(field.help.clone());
            }
            col = col.push(item);
        }
    }

    col.into()
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
