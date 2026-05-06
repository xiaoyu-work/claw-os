/// Configuration loading for Claw OS.
///
/// Reads `/etc/cos/config.json` (or `COS_CONFIG_PATH` override) and
/// provides typed access to settings. Falls back to sensible defaults
/// if the config file is missing or malformed.
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

static CONFIG: OnceLock<CosConfig> = OnceLock::new();

const DEFAULT_CONFIG_PATH: &str = "/etc/cos/config.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CosConfig {
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default = "default_den")]
    pub den: String,
    #[serde(default)]
    pub exec: ExecConfig,
    #[serde(default)]
    pub net: NetConfig,
    #[serde(default)]
    pub web: WebConfig,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub embed: EmbedConfig,
    #[serde(default)]
    pub imagegen: ImageGenConfig,
    #[serde(default)]
    pub stt: SttConfig,
    #[serde(default)]
    pub tts: TtsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecConfig {
    #[serde(default = "default_exec_timeout")]
    pub timeout: u64,
    #[serde(default = "default_shell")]
    pub shell: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetConfig {
    #[serde(default = "default_net_timeout")]
    pub timeout: u64,
    #[serde(default = "default_true")]
    pub allow_outbound: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebConfig {
    /// Browser engine name (informational; cos-browser is currently the only
    /// supported engine).
    #[serde(default = "default_web_engine")]
    pub engine: String,
    /// Port on which `cos browser start` runs the CDP server.
    #[serde(default = "default_cdp_port")]
    pub cdp_port: u16,
    #[serde(default = "default_net_timeout")]
    pub timeout: u64,
    #[serde(default = "default_max_content_length")]
    pub max_content_length: usize,
}

/// Agent runtime configuration. Reads from `[agent]` block of
/// `/etc/cos/config.json`. All fields have sensible defaults so the agent
/// can run without explicit configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Name of the LLM provider to use (must be registered, e.g. "mock",
    /// "anthropic", "openai", "ollama"). Defaults to "mock" so the agent
    /// is functional out of the box for testing.
    #[serde(default = "default_agent_provider")]
    pub provider: String,

    /// Model identifier passed to the provider (e.g. "claude-sonnet-4.6",
    /// "gpt-4.1", "llama3.2:3b").
    #[serde(default = "default_agent_model")]
    pub model: String,

    /// Maximum number of agent turns (provider call → tool calls → ...) per
    /// `cos agent ask` invocation. Stops infinite tool-use loops.
    #[serde(default = "default_agent_max_turns")]
    pub max_turns: u32,

    /// Maximum tokens to request per provider call.
    #[serde(default = "default_agent_max_tokens")]
    pub max_tokens: u32,

    /// Sampling temperature (0.0–2.0). Provider-specific clamping applies.
    #[serde(default = "default_agent_temperature")]
    pub temperature: f32,

    /// Optional path to a Markdown file injected at the start of the system
    /// prompt. If unset, only the built-in scaffold prompt is used.
    #[serde(default)]
    pub system_prompt_path: Option<String>,

    /// Name of a credential stored via `cos credential store … --namespace
    /// agent` that holds the API key for this provider. Looked up at
    /// runtime — never read from the config file directly.
    /// Example: `"openai_api_key"` then store via:
    ///   `cos credential store openai_api_key sk-... --namespace agent`
    #[serde(default)]
    pub api_key_credential: Option<String>,

    /// Fallback environment variable name for the API key, used when the
    /// credential store has no entry. Example: `"OPENAI_API_KEY"`.
    #[serde(default)]
    pub api_key_env: Option<String>,

    /// Override the provider's default base URL. Lets one provider impl
    /// (`openai`) speak to OpenAI / xAI / DeepSeek / OpenRouter / Ollama /
    /// self-hosted vLLM/TGI/LMStudio just by changing this URL.
    /// Examples:
    ///   - OpenAI:     `https://api.openai.com/v1` (default)
    ///   - xAI:        `https://api.x.ai/v1`
    ///   - DeepSeek:   `https://api.deepseek.com/v1`
    ///   - OpenRouter: `https://openrouter.ai/api/v1`
    ///   - Ollama:     `http://localhost:11434/v1`
    #[serde(default)]
    pub base_url: Option<String>,

    /// Extra HTTP headers sent on every provider request. Some routers
    /// (e.g. OpenRouter) want `HTTP-Referer` / `X-Title`.
    #[serde(default)]
    pub extra_headers: std::collections::HashMap<String, String>,

    /// Per-request HTTP timeout in seconds. 0 = no timeout. Default 120s.
    #[serde(default = "default_agent_request_timeout")]
    pub request_timeout: u64,

    /// Enable provider-backed conversation compression. When the
    /// estimated total token count of the running conversation exceeds
    /// `compress_trigger_tokens`, the head of the message list is
    /// summarised by the same provider and replaced with a single
    /// `[CONTEXT SUMMARY]` user message; the tail is kept verbatim.
    /// Defaults to `false` so behaviour is unchanged for existing users
    /// — opt in once your sessions get long.
    #[serde(default)]
    pub compress_enabled: bool,

    /// Target total context budget in tokens. Used as the upper bound
    /// callers should size prompts to. Defaults to 80_000.
    #[serde(default = "default_compress_target")]
    pub compress_target_tokens: u32,

    /// Estimated-token threshold that triggers compression. Defaults to
    /// 60_000 (~75% of `compress_target_tokens`).
    #[serde(default = "default_compress_trigger")]
    pub compress_trigger_tokens: u32,

    /// Token budget reserved for the verbatim tail of recent messages.
    /// Defaults to 20_000.
    #[serde(default = "default_compress_keep_tail")]
    pub compress_keep_tail_tokens: u32,

    /// Maximum tokens for the synthesised summary itself. Defaults to
    /// 1024.
    #[serde(default = "default_compress_summary_max")]
    pub compress_summary_max_tokens: u32,
}

/// Embedding service configuration. Reads from `[embed]` block.
/// `provider="none"` (the default) means embedding is disabled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedConfig {
    /// `"none"` (disabled) | `"openai"` | `"ollama"` | self-hosted alias.
    #[serde(default = "default_embed_provider")]
    pub provider: String,

    /// Model identifier (e.g. `"text-embedding-3-small"`,
    /// `"nomic-embed-text"`, `"bge-small-en-v1.5"`).
    #[serde(default = "default_embed_model")]
    pub model: String,

    /// Credential store entry (namespace `agent`) holding the API key.
    #[serde(default)]
    pub api_key_credential: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Override the provider's default base URL. Lets the same provider
    /// impl talk to OpenAI / Azure OpenAI / Ollama / self-hosted vLLM.
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub extra_headers: std::collections::HashMap<String, String>,
    #[serde(default = "default_agent_request_timeout")]
    pub request_timeout: u64,
}

/// Image generation configuration. Reads from `[imagegen]` block.
/// `provider="none"` (the default) means image-gen is disabled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenConfig {
    /// `"none"` (disabled) | `"openai"` | self-hosted alias.
    #[serde(default = "default_imagegen_provider")]
    pub provider: String,

    /// Model identifier (e.g. `"gpt-image-2"`, `"dall-e-3"`).
    #[serde(default = "default_imagegen_model")]
    pub model: String,

    #[serde(default)]
    pub api_key_credential: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub extra_headers: std::collections::HashMap<String, String>,
    #[serde(default = "default_imagegen_timeout")]
    pub request_timeout: u64,

    /// Default image size, e.g. `"1024x1024"`. Provider-specific.
    #[serde(default)]
    pub default_size: Option<String>,
    /// Default quality, e.g. `"low"`, `"medium"`, `"high"`.
    #[serde(default)]
    pub default_quality: Option<String>,
    /// Default output format, e.g. `"png"`, `"jpeg"`, `"webp"`.
    #[serde(default = "default_imagegen_format")]
    pub default_format: String,
}

/// Speech-to-text config. Reads from `[stt]` block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttConfig {
    /// `"none"` (disabled) | `"openai"` | self-hosted alias.
    #[serde(default = "default_stt_provider")]
    pub provider: String,
    /// Model name (e.g. `"whisper-1"`, `"whisper-large-v3"`).
    #[serde(default = "default_stt_model")]
    pub model: String,
    #[serde(default)]
    pub api_key_credential: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub extra_headers: std::collections::HashMap<String, String>,
    #[serde(default = "default_agent_request_timeout")]
    pub request_timeout: u64,
    /// Default response shape, e.g. `"json"`, `"text"`, `"verbose_json"`.
    #[serde(default = "default_stt_response_format")]
    pub default_response_format: String,
}

/// Text-to-speech config. Reads from `[tts]` block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsConfig {
    /// `"none"` (disabled) | `"openai"` | self-hosted alias.
    #[serde(default = "default_tts_provider")]
    pub provider: String,
    /// Model name (e.g. `"tts-1"`, `"tts-1-hd"`, `"gpt-4o-mini-tts"`).
    #[serde(default = "default_tts_model")]
    pub model: String,
    #[serde(default)]
    pub api_key_credential: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub extra_headers: std::collections::HashMap<String, String>,
    #[serde(default = "default_agent_request_timeout")]
    pub request_timeout: u64,
    /// Default voice (alloy, echo, fable, onyx, nova, shimmer for OpenAI).
    #[serde(default = "default_tts_voice")]
    pub default_voice: String,
    /// Default output format (`mp3` | `opus` | `aac` | `flac` | `wav` | `pcm`).
    #[serde(default = "default_tts_format")]
    pub default_format: String,
}

fn default_version() -> String {
    env!("CARGO_PKG_VERSION").into()
}
fn default_den() -> String {
    "/den".into()
}
fn default_exec_timeout() -> u64 {
    300
}
fn default_shell() -> String {
    "/bin/bash".into()
}
fn default_net_timeout() -> u64 {
    30
}
fn default_true() -> bool {
    true
}
fn default_reader_url() -> String {
    // Retained for backward-compatible JSON parsing. The cos-browser engine
    // exposes CDP at a port (see default_cdp_port), not a URL — but old
    // config files may still set reader_url which we now ignore.
    "http://localhost:3000".into()
}
fn default_web_engine() -> String {
    "cos-browser".into()
}
fn default_cdp_port() -> u16 {
    9222
}
fn default_max_content_length() -> usize {
    50000
}
fn default_agent_provider() -> String {
    "mock".into()
}
fn default_agent_model() -> String {
    "mock-model".into()
}
fn default_agent_max_turns() -> u32 {
    10
}
fn default_agent_max_tokens() -> u32 {
    4096
}
fn default_agent_temperature() -> f32 {
    0.7
}
fn default_agent_request_timeout() -> u64 {
    120
}
fn default_compress_target() -> u32 {
    crate::agent::context::compressor::DEFAULT_TARGET_TOKENS
}
fn default_compress_trigger() -> u32 {
    crate::agent::context::compressor::DEFAULT_TRIGGER_TOKENS
}
fn default_compress_keep_tail() -> u32 {
    crate::agent::context::compressor::DEFAULT_KEEP_TAIL_TOKENS
}
fn default_compress_summary_max() -> u32 {
    crate::agent::context::compressor::DEFAULT_SUMMARY_MAX_TOKENS
}
fn default_embed_provider() -> String {
    "none".into()
}
fn default_embed_model() -> String {
    "text-embedding-3-small".into()
}
fn default_imagegen_provider() -> String {
    "none".into()
}
fn default_imagegen_model() -> String {
    "gpt-image-2".into()
}
fn default_imagegen_timeout() -> u64 {
    300
}
fn default_imagegen_format() -> String {
    "png".into()
}
fn default_stt_provider() -> String {
    "none".into()
}
fn default_stt_model() -> String {
    "whisper-1".into()
}
fn default_stt_response_format() -> String {
    "json".into()
}
fn default_tts_provider() -> String {
    "none".into()
}
fn default_tts_model() -> String {
    "tts-1".into()
}
fn default_tts_voice() -> String {
    "alloy".into()
}
fn default_tts_format() -> String {
    "mp3".into()
}

impl Default for ExecConfig {
    fn default() -> Self {
        Self {
            timeout: default_exec_timeout(),
            shell: default_shell(),
        }
    }
}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            timeout: default_net_timeout(),
            allow_outbound: true,
        }
    }
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            engine: default_web_engine(),
            cdp_port: default_cdp_port(),
            timeout: default_net_timeout(),
            max_content_length: default_max_content_length(),
        }
    }
}

impl Default for CosConfig {
    fn default() -> Self {
        Self {
            version: default_version(),
            den: default_den(),
            exec: ExecConfig::default(),
            net: NetConfig::default(),
            web: WebConfig::default(),
            agent: AgentConfig::default(),
            embed: EmbedConfig::default(),
            imagegen: ImageGenConfig::default(),
            stt: SttConfig::default(),
            tts: TtsConfig::default(),
        }
    }
}

impl Default for EmbedConfig {
    fn default() -> Self {
        Self {
            provider: default_embed_provider(),
            model: default_embed_model(),
            api_key_credential: None,
            api_key_env: None,
            base_url: None,
            extra_headers: std::collections::HashMap::new(),
            request_timeout: default_agent_request_timeout(),
        }
    }
}

impl Default for ImageGenConfig {
    fn default() -> Self {
        Self {
            provider: default_imagegen_provider(),
            model: default_imagegen_model(),
            api_key_credential: None,
            api_key_env: None,
            base_url: None,
            extra_headers: std::collections::HashMap::new(),
            request_timeout: default_imagegen_timeout(),
            default_size: None,
            default_quality: None,
            default_format: default_imagegen_format(),
        }
    }
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            provider: default_stt_provider(),
            model: default_stt_model(),
            api_key_credential: None,
            api_key_env: None,
            base_url: None,
            extra_headers: std::collections::HashMap::new(),
            request_timeout: default_agent_request_timeout(),
            default_response_format: default_stt_response_format(),
        }
    }
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            provider: default_tts_provider(),
            model: default_tts_model(),
            api_key_credential: None,
            api_key_env: None,
            base_url: None,
            extra_headers: std::collections::HashMap::new(),
            request_timeout: default_agent_request_timeout(),
            default_voice: default_tts_voice(),
            default_format: default_tts_format(),
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            provider: default_agent_provider(),
            model: default_agent_model(),
            max_turns: default_agent_max_turns(),
            max_tokens: default_agent_max_tokens(),
            temperature: default_agent_temperature(),
            system_prompt_path: None,
            api_key_credential: None,
            api_key_env: None,
            base_url: None,
            extra_headers: std::collections::HashMap::new(),
            request_timeout: default_agent_request_timeout(),
            compress_enabled: false,
            compress_target_tokens: default_compress_target(),
            compress_trigger_tokens: default_compress_trigger(),
            compress_keep_tail_tokens: default_compress_keep_tail(),
            compress_summary_max_tokens: default_compress_summary_max(),
        }
    }
}

/// Load config from disk, or return defaults if file is missing/invalid.
fn load_from_disk() -> CosConfig {
    let path = std::env::var("COS_CONFIG_PATH").unwrap_or_else(|_| DEFAULT_CONFIG_PATH.into());

    let path = Path::new(&path);
    if !path.is_file() {
        return CosConfig::default();
    }

    match fs::read_to_string(path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => CosConfig::default(),
    }
}

/// Get the global config (loaded once, cached).
pub fn get() -> &'static CosConfig {
    CONFIG.get_or_init(load_from_disk)
}

/// Return config values as environment variables for Python app subprocesses.
pub fn as_env_vars() -> Vec<(String, String)> {
    let cfg = get();
    vec![
        ("COS_EXEC_TIMEOUT".into(), cfg.exec.timeout.to_string()),
        ("COS_EXEC_SHELL".into(), cfg.exec.shell.clone()),
        ("COS_NET_TIMEOUT".into(), cfg.net.timeout.to_string()),
        (
            "COS_NET_ALLOW_OUTBOUND".into(),
            cfg.net.allow_outbound.to_string(),
        ),
        ("COS_WEB_ENGINE".into(), cfg.web.engine.clone()),
        ("COS_BROWSER_PORT".into(), cfg.web.cdp_port.to_string()),
        ("COS_WEB_TIMEOUT".into(), cfg.web.timeout.to_string()),
        (
            "COS_WEB_MAX_CONTENT_LENGTH".into(),
            cfg.web.max_content_length.to_string(),
        ),
        ("COS_DEN".into(), cfg.den.clone()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_sensible_values() {
        let cfg = CosConfig::default();
        assert_eq!(cfg.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(cfg.den, "/den");
        assert_eq!(cfg.exec.timeout, 300);
        assert_eq!(cfg.exec.shell, "/bin/bash");
        assert_eq!(cfg.net.timeout, 30);
        assert!(cfg.net.allow_outbound);
        assert_eq!(cfg.web.engine, "cos-browser");
        assert_eq!(cfg.web.cdp_port, 9222);
        assert_eq!(cfg.web.max_content_length, 50000);
    }

    #[test]
    fn parse_partial_config() {
        let json = r#"{"version": "1.0.0", "den": "/custom"}"#;
        let cfg: CosConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.version, "1.0.0");
        assert_eq!(cfg.den, "/custom");
        // Defaults for missing sections
        assert_eq!(cfg.exec.timeout, 300);
        assert_eq!(cfg.web.engine, "cos-browser");
        assert_eq!(cfg.web.cdp_port, 9222);
    }

    #[test]
    fn parse_full_config() {
        let json = r#"{
            "version": "0.1.0",
            "den": "/den",
            "exec": {"timeout": 600, "shell": "/bin/zsh"},
            "net": {"timeout": 10, "allow_outbound": false},
            "web": {"engine": "cos-browser", "cdp_port": 9333, "timeout": 60, "max_content_length": 100000}
        }"#;
        let cfg: CosConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.exec.timeout, 600);
        assert_eq!(cfg.exec.shell, "/bin/zsh");
        assert_eq!(cfg.net.timeout, 10);
        assert!(!cfg.net.allow_outbound);
        assert_eq!(cfg.web.cdp_port, 9333);
        assert_eq!(cfg.web.max_content_length, 100000);
    }

    #[test]
    fn as_env_vars_returns_all_keys() {
        let vars = as_env_vars();
        let keys: Vec<&str> = vars.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"COS_EXEC_TIMEOUT"));
        assert!(keys.contains(&"COS_NET_TIMEOUT"));
        assert!(keys.contains(&"COS_WEB_ENGINE"));
        assert!(keys.contains(&"COS_BROWSER_PORT"));
        assert!(keys.contains(&"COS_DEN"));
    }

    #[test]
    fn malformed_json_returns_defaults() {
        let json = "not valid json {{{";
        let cfg: CosConfig = serde_json::from_str(json).unwrap_or_default();
        assert_eq!(cfg.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(cfg.exec.timeout, 300);
    }
}
