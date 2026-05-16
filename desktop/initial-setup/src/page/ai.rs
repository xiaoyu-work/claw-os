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
//     existing `cos agent setup llm apply` CLI through claw-os-sdk
//     exec.run. The bridge runs as the same unprivileged user as the
//     desktop session, so the config file lives under the running
//     user's `$HOME` and no privilege escalation is needed.
//
//   * The api-key is passed inline via `--api-key` rather than
//     `--api-key-stdin`. claw-os-sdk's exec verb doesn't carry stdin
//     to the spawned child today. The key is visible in `ps` for the
//     duration of the call — initial-setup runs once, on a freshly
//     installed system, so the leak window is acceptable; users who
//     need stronger handling can leave the field blank here and run
//     `sudo cos agent setup llm apply --api-key-stdin <key.txt` from
//     a terminal afterwards.

use cosmic::iced::{Alignment, Length};
use cosmic::{Element, Task, cosmic_theme, theme, widget};

use crate::{fl, page};

// User-facing provider options. Mirrors `core::agent::llm::registry::REGISTERED`
// minus `mock` (test-only) and `llama_local` (handled by `cos model load`,
// not configured here). Order is "common first" rather than alphabetical.
const PROVIDER_KEYS: &[&str] = &[
    "anthropic",
    "openai",
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

#[derive(Clone, Debug)]
pub enum Message {
    SelectProvider(usize),
    EditModel(String),
    EditApiKey(String),
    ToggleApiKeyVisibility,
    AppliedResult(ApplyOutcome),
}

#[derive(Clone, Debug)]
pub enum ApplyOutcome {
    Ok,
    /// Bridge denied or CLI exited non-zero. The string is a
    /// short, user-readable summary suitable for inline display.
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
    /// Last outcome of `apply_settings`, surfaced inline in the view.
    /// `None` until the user hits Finish.
    last_outcome: Option<ApplyOutcome>,
}

impl Default for Page {
    fn default() -> Self {
        Self {
            selected: None,
            model: String::new(),
            api_key: String::new(),
            api_key_hidden: true,
            last_outcome: None,
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
                if self.model.trim().is_empty() {
                    if let Some(m) = DEFAULT_MODELS.get(idx) {
                        self.model = (*m).to_string();
                    }
                }
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
            Message::AppliedResult(outcome) => {
                self.last_outcome = Some(outcome);
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

    /// Completed only when the user picked a provider and entered a
    /// non-empty model id. With both empty we let the user move on
    /// (page is optional) and the system stays "not configured" —
    /// which is the honest default for a fresh install.
    fn completed(&self) -> bool {
        self.selected
            .map(|i| i < PROVIDER_KEYS.len())
            .unwrap_or(false)
            && !self.model.trim().is_empty()
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

        let api_key_input = widget::settings::item::builder(fl!("ai-page", "api-key"))
            .description(fl!("ai-page", "api-key-description"))
            .control(widget::secure_input(
                "",
                &self.api_key,
                Some(Message::ToggleApiKeyVisibility.into()),
                self.api_key_hidden,
            ).on_input(|v| Message::EditApiKey(v).into()));

        let mut section = widget::settings::section()
            .add(provider_dropdown)
            .add(model_input)
            .add(api_key_input);

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
        let api_key = std::mem::take(&mut self.api_key);

        let fut = async move {
            let outcome = tokio::task::spawn_blocking(move || {
                apply_blocking(&provider, &model, &api_key)
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
/// `claw_os_sdk::exec::run` is itself blocking. Returns a verdict
/// suitable for the UI; tracing log lines mirror the structure used
/// by other pages (location.rs, a11y.rs).
fn apply_blocking(provider: &str, model: &str, api_key: &str) -> ApplyOutcome {
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
    if !api_key.is_empty() {
        argv.push("--api-key");
        argv.push(api_key);
    }
    match claw_os_sdk::exec::run(&argv, Some(30)) {
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
