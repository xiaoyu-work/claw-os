//! Request-wide accounting shared by context selection and the turn loop.

use crate::agent::llm::{metadata, Message, Tool};
use crate::config::AgentConfig;

use super::compressor::{estimate_text_tokens, estimate_tools_tokens, estimate_total_tokens};

pub const EXCERPT_MARKER: &str = "\n[... omitted; read the source for the full content]";

#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    #[error("invalid context budget: {0}")]
    InvalidBudget(String),
    #[error(
        "{section} needs approximately {required} tokens, exceeding the {limit}-token input budget"
    )]
    BudgetExceeded {
        section: &'static str,
        required: u32,
        limit: u32,
    },
    #[error("context source {origin}: {detail}")]
    Source { origin: String, detail: String },
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct ContextBudget {
    pub input_tokens: u32,
    pub memory_tokens: u32,
    pub model_window_tokens: Option<u32>,
    pub output_tokens: u32,
}

impl ContextBudget {
    pub fn from_config(config: &AgentConfig) -> Result<Self, ContextError> {
        if config.compress_target_tokens == 0 {
            return Err(ContextError::InvalidBudget(
                "agent.compress_target_tokens must be greater than zero".into(),
            ));
        }
        let model_window_tokens = std::iter::once(config.model.as_str())
            .chain(
                config
                    .provider_fallbacks
                    .iter()
                    .map(|entry| entry.model.as_str()),
            )
            .filter_map(|model| metadata::lookup(model).map(|metadata| metadata.context_window))
            .min();
        let input_tokens = match model_window_tokens {
            Some(window) => config
                .compress_target_tokens
                .min(window.saturating_sub(config.max_tokens.saturating_add(1024))),
            None => config.compress_target_tokens,
        };
        if input_tokens == 0 {
            return Err(ContextError::InvalidBudget(
                "the configured response and safety reserve exhaust the model window".into(),
            ));
        }
        Ok(Self {
            input_tokens,
            memory_tokens: (input_tokens / 8).min(2048),
            model_window_tokens,
            output_tokens: config.max_tokens,
        })
    }

    pub fn check(
        &self,
        section: &'static str,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<u32, ContextError> {
        let required = request_tokens(system, messages, tools);
        if required > self.input_tokens {
            return Err(ContextError::BudgetExceeded {
                section,
                required,
                limit: self.input_tokens,
            });
        }
        Ok(required)
    }
}

pub fn request_tokens(system: &str, messages: &[Message], tools: &[Tool]) -> u32 {
    estimate_total_tokens(Some(system), messages).saturating_add(estimate_tools_tokens(tools))
}

/// Return a UTF-8-safe excerpt whose marker is included in the budget.
pub fn excerpt(text: &str, tokens: u32) -> Option<String> {
    if estimate_text_tokens(text) <= tokens {
        return Some(text.to_string());
    }
    const MARKER: &str = EXCERPT_MARKER;
    if estimate_text_tokens(MARKER) >= tokens {
        return None;
    }
    let boundaries: Vec<usize> = text.char_indices().map(|(index, _)| index).collect();
    let mut low = 0;
    let mut high = boundaries.len();
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        let end = boundaries.get(middle).copied().unwrap_or(text.len());
        let candidate = format!("{}{MARKER}", &text[..end]);
        if estimate_text_tokens(&candidate) <= tokens {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    if low == 0 {
        return None;
    }
    let end = boundaries.get(low).copied().unwrap_or(text.len());
    Some(format!("{}{MARKER}", &text[..end]))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/context/budget.rs"
    ));
}
