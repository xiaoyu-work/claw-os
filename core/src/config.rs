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
    #[serde(default = "default_home")]
    pub home: String,
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
    /// Defaults to `true` — long-running sessions on a system-level
    /// agent OS are the norm, not the exception, and a runaway context
    /// is the difference between "agent that quietly keeps working"
    /// and "agent that hits the provider's context limit and dies
    /// mid-task". Set to `false` only if you have a reason to want the
    /// raw transcript on every turn (e.g. eval / regression harnesses).
    #[serde(default = "default_compress_enabled")]
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

    /// Whether to scrub `<think>…</think>` / `<thinking>…</thinking>` /
    /// `<reasoning>…</reasoning>` blocks from assistant history before
    /// each turn. Reasoning models (DeepSeek R1, Qwen QwQ, llama.cpp
    /// finetunes) emit these as ephemeral chain-of-thought; resending
    /// them on every subsequent turn wastes tokens with no benefit.
    /// Defaults to `true` because the operation is a pure regex pass
    /// and the false-positive rate is essentially zero — only blocks
    /// inside the recognised tag pairs are removed.
    #[serde(default = "default_think_scrub_enabled")]
    pub think_scrub_enabled: bool,

    /// Redact secrets (API keys, tokens, AWS keys, GitHub PATs, etc.)
    /// from messages BEFORE they are persisted to the SQLite-FTS5
    /// memory database. Default `true`. Rationale: memory is searchable
    /// forever; once a secret lands in `memory.db` it stays in the
    /// FTS5 index and is fair game for future `cos_recall` calls. The
    /// regex pass is the same one used by `safety::redact::Redactor`,
    /// targeted at high-value low-FP patterns (`sk-…`, `ghp_…`,
    /// `AKIA…`, JWT-shaped tokens, PEM keys, `Bearer …`, URL creds).
    /// Set to `false` only if you keep memory.db on encrypted media
    /// and want full-fidelity recall.
    #[serde(default = "default_redact_memory_enabled")]
    pub redact_memory_enabled: bool,

    /// Tool guardrails: optional allow-list. When `Some`, only the named
    /// tools are surfaced to the model and accepted by the dispatcher;
    /// every other registered tool is denied. When `None` (default)
    /// every registered tool is permitted unless `tool_deny` blocks it.
    /// Allow-list semantics: empty `Some(vec![])` denies everything.
    #[serde(default)]
    pub tool_allow: Option<Vec<String>>,

    /// Tool guardrails: explicit deny-list. Always wins over `tool_allow`.
    /// Useful for shipping the same agent loop in different security
    /// contexts (e.g. prevent prompt-injection from invoking
    /// `cos_sandbox` exec by adding it here).
    #[serde(default)]
    pub tool_deny: Vec<String>,

    /// Approval gate: tools that require explicit approval before each
    /// invocation. Empty by default (gate short-circuits to Approved
    /// for everything). When non-empty, headless mode (no approver
    /// configured) emits a synthetic `tool_result` with
    /// `is_error: true` and the deferral prompt — the agent sees it
    /// and can ask the user. Names matched literally against
    /// `ToolCall.name`.
    #[serde(default)]
    pub dangerous_tools: Vec<String>,

    /// Approval gate: tools that always pass approval without prompting,
    /// even if listed in `dangerous_tools`. Useful for explicit
    /// per-context overrides (e.g. allow `cos_proc kill` in an
    /// orchestrator context but require approval everywhere else).
    #[serde(default)]
    pub auto_approve_tools: Vec<String>,

    /// Approval gate: tools that are always blocked. Takes precedence
    /// over `auto_approve_tools` and `dangerous_tools`. The dispatcher
    /// emits a synthetic `tool_result` with `is_error: true`.
    #[serde(default)]
    pub auto_deny_tools: Vec<String>,

    /// Auxiliary LLM provider — name registered in
    /// `agent::llm::registry`. When set, lightweight subtasks
    /// (title generation, classification, query rewriting) are routed
    /// here instead of the primary `provider`. `None` (default)
    /// disables the auxiliary path; callers fall back to the primary.
    /// Honours the same credential resolution as the primary
    /// (`api_key_credential` / `api_key_env`) by default — overrides
    /// can be added later if a separate API key is needed.
    #[serde(default)]
    pub auxiliary_provider: Option<String>,

    /// Auxiliary model id. Required when `auxiliary_provider` is set
    /// (build returns an error otherwise). Typically a smaller / cheaper
    /// SKU than the primary model.
    #[serde(default)]
    pub auxiliary_model: Option<String>,

    /// Monthly token-unit budget for the kernel-resident **system
    /// agent** — i.e. the user's authorised agent reachable via
    /// `cos agent ask`, `cos agent chat`, the cos-agent-bridge HTTP
    /// service, and friends. The system agent is not an installed
    /// app but it still flows through the same gate as real apps;
    /// usage rolls up under the pseudo-app id `system.agent` and is
    /// visible alongside other apps in `cos agent budget show`. Set to
    /// `0` to disable the unit cap entirely. Default: 10_000_000.
    #[serde(default = "default_system_budget_units")]
    pub system_budget_monthly_units: u64,

    /// Hard cap on `max_tokens` for auxiliary calls. Defaults to 1024
    /// — these subtasks are *meant* to be short. Capping at construction
    /// time prevents an accidental flagship-sized request from sneaking
    /// through.
    #[serde(default = "default_auxiliary_max_tokens")]
    pub auxiliary_max_tokens: u32,

    /// Optional sampling temperature for auxiliary calls. `None`
    /// (default) lets the auxiliary provider use its own default.
    #[serde(default)]
    pub auxiliary_temperature: Option<f32>,

    /// Enable transparent retry-with-exponential-backoff around every
    /// `Provider::chat` call. Defaults to `false` — existing
    /// behaviour is "fail fast on transient errors". When true,
    /// `RetryPolicy::standard()` is used (modulo `retry_max_attempts`).
    /// Honours server-supplied `Retry-After` from
    /// [`crate::agent::llm::LlmError::RateLimited`].
    #[serde(default)]
    pub retry_enabled: bool,

    /// Max attempts (inclusive of the first try) when `retry_enabled`.
    /// Defaults to 3. A value of 1 disables retries even when
    /// `retry_enabled` is true (semantically equivalent to off).
    #[serde(default = "default_retry_max_attempts")]
    pub retry_max_attempts: u32,

    /// Multi-key credential pool — credential-store entry names. When
    /// either this or `api_key_envs` is non-empty, the provider builds
    /// a key-rotation pool from the resolved entries (see
    /// [`crate::agent::llm::credential_pool`]) and supersedes the
    /// single-key fields (`api_key_credential` / `api_key_env`). Order
    /// is preserved — sticky strategy will start from index 0. Empty
    /// or unresolved entries are silently dropped at construction.
    #[serde(default)]
    pub api_key_credentials: Vec<String>,

    /// Multi-key credential pool — environment variable names. See
    /// `api_key_credentials`.
    #[serde(default)]
    pub api_key_envs: Vec<String>,

    /// Pool selection strategy. One of `"sticky"` (default — stay on
    /// one key until it fails), `"round-robin"`, or `"least-errors"`.
    /// Ignored when no pool is configured.
    #[serde(default = "default_pool_strategy")]
    pub pool_strategy: String,

    /// Pool cooldown applied to a key after a CooldownWorthy failure
    /// (auth/quota), in seconds. `0` disables cooldowns (failures are
    /// still counted toward LeastErrors picking). Defaults to 60.
    #[serde(default = "default_pool_cooldown_secs")]
    pub pool_cooldown_secs: u64,

    /// External Model Context Protocol (MCP) servers the agent should
    /// attach to at startup. Each entry spawns a child process,
    /// performs the MCP handshake, lists its tools, and registers
    /// every advertised tool under the prefix `mcp_<name>_<remote>`.
    /// Failures are best-effort: a misconfigured MCP server is
    /// logged and skipped, never fatal to the agent loop. See
    /// [`McpServerConfig`] for the per-server fields.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,

    // -- AWS Bedrock credentials -----------------------------------------
    //
    // These fields are only consumed by the `bedrock` provider. They
    // exist on `AgentConfig` (rather than a nested `bedrock = {…}`
    // block) for two reasons:
    //
    //   1. Other providers ignore them — adding new optional fields to
    //      AgentConfig is a no-op for openai/anthropic/gemini/llama_local.
    //   2. Lookup precedence is uniform across providers. Every other
    //      provider already follows the `*_credential` (cos credential
    //      store) → `*_env` (env var fallback) ladder, and we want
    //      Bedrock to feel the same.
    //
    // All seven are optional; sensible defaults apply at lookup time
    // (`AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` /
    // `AWS_SESSION_TOKEN` env vars and `us-east-1` region).
    /// AWS region for Bedrock. Defaults to `us-east-1` when unset.
    /// Bedrock supports a fixed set of regions; consult AWS docs for
    /// model-region availability (e.g. Claude Opus 4 is in `us-west-2`
    /// and `us-east-1` only as of this writing).
    #[serde(default)]
    pub aws_region: Option<String>,

    /// Name of a credential stored via `cos credential store …
    /// --namespace agent` that holds the AWS access key ID. Looked up
    /// at runtime — never read from the config file directly.
    #[serde(default)]
    pub aws_access_key_credential: Option<String>,

    /// Fallback environment variable for the AWS access key ID.
    /// Defaults to `AWS_ACCESS_KEY_ID` (the standard AWS SDK env name)
    /// when unset.
    #[serde(default)]
    pub aws_access_key_env: Option<String>,

    /// Name of a credential stored via `cos credential` that holds the
    /// AWS secret access key.
    #[serde(default)]
    pub aws_secret_key_credential: Option<String>,

    /// Fallback environment variable for the AWS secret access key.
    /// Defaults to `AWS_SECRET_ACCESS_KEY` when unset.
    #[serde(default)]
    pub aws_secret_key_env: Option<String>,

    /// Name of a credential stored via `cos credential` that holds an
    /// AWS session token (for temporary STS / IAM-role / SSO creds).
    /// Optional — only required when using temporary credentials.
    #[serde(default)]
    pub aws_session_token_credential: Option<String>,

    /// Fallback environment variable for the AWS session token.
    /// Defaults to `AWS_SESSION_TOKEN` when unset.
    #[serde(default)]
    pub aws_session_token_env: Option<String>,
}

/// One external MCP server attached to the agent. Read from the
/// `[[agent.mcp_servers]]` table in `config.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Stable, snake_case identifier. Becomes the prefix in registered
    /// tool names: `mcp_<name>_<remote_tool>`. Must be unique across
    /// all entries in this list (the registry will overwrite earlier
    /// duplicates silently otherwise).
    pub name: String,

    /// Executable to spawn. Resolved against `PATH`.
    pub command: String,

    /// Positional / flag args passed verbatim to the child.
    #[serde(default)]
    pub args: Vec<String>,

    /// Extra environment variables for the child. Inherited variables
    /// pass through unchanged unless overridden here.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,

    /// Working directory for the child. `None` inherits the parent's
    /// `cwd` (typical case for stateless servers).
    #[serde(default)]
    pub cwd: Option<String>,

    /// Per-RPC timeout (initialize, tools/list, tools/call) in seconds.
    /// `0` disables the timeout entirely. Defaults to 30 — short
    /// enough that a hung server doesn't block the agent for long,
    /// long enough that legitimately slow tools (e.g. large database
    /// queries) finish.
    #[serde(default = "default_mcp_timeout_secs")]
    pub timeout_secs: u64,

    /// Disable this entry without removing it from config. Defaults
    /// to `true` so adding an entry attaches it.
    #[serde(default = "default_mcp_enabled")]
    pub enabled: bool,
}

/// Embedding service configuration. Reads from `[embed]` block.
/// `provider="auto"` (the default) derives the embedder from the
/// main `[agent]` provider when it speaks an OpenAI-compatible
/// `/embeddings` shape (openai / azure / xai / deepseek / openrouter
/// / ollama). `provider="none"` disables embeddings explicitly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedConfig {
    /// `"auto"` (default — derive from `[agent]` when possible) |
    /// `"none"` (explicit off) | `"openai"` | `"azure"` | `"ollama"` |
    /// `"qwen3-local"` | other self-hosted alias. When `"auto"` and
    /// the main agent provider isn't OpenAI-shape (e.g. `mock`,
    /// `anthropic`, `gemini`, `bedrock`), the embedder is silently
    /// disabled with a `debug!` log line.
    #[serde(default = "default_embed_provider")]
    pub provider: String,

    /// Model identifier (e.g. `"text-embedding-3-small"`,
    /// `"text-embedding-3-large"`, `"nomic-embed-text"`,
    /// `"bge-small-en-v1.5"`). Empty falls back to
    /// `crate::model::tasks::embed::MODEL_NAME`.
    ///
    /// **Switching models invalidates every row in `semantic.db`** —
    /// vector spaces are not interchangeable. `SemanticStore` enforces
    /// this with a stickiness check that returns `ModelMismatch` on the
    /// first row from a new model. To migrate, run
    /// `cos agent semantic clear-all --yes` and re-index.
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

    /// Local model directory (used when `provider = "qwen3-local"`).
    /// Points at an Olive-exported onnxruntime-genai bundle (with
    /// `genai_config.json` + `model.onnx` + `model.onnx.data` + the
    /// tokenizer files). When unset, the embedder falls back to the
    /// canonical registry slot
    /// `<models_dir>/qwen3-embedding-0.6b/v1/`.
    #[serde(default)]
    pub model_dir: Option<String>,
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
fn default_home() -> String {
    // Linux-native: resolve $HOME at runtime so each user gets their own
    // workspace. Falls back to /root when HOME is unset (e.g., minimal
    // Docker images where the entrypoint hasn't sourced any profile yet).
    std::env::var("HOME").unwrap_or_else(|_| "/root".into())
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
fn default_compress_enabled() -> bool {
    true
}
fn default_think_scrub_enabled() -> bool {
    true
}
fn default_redact_memory_enabled() -> bool {
    true
}
fn default_auxiliary_max_tokens() -> u32 {
    1024
}
fn default_system_budget_units() -> u64 {
    10_000_000
}
fn default_retry_max_attempts() -> u32 {
    3
}
fn default_pool_strategy() -> String {
    "sticky".into()
}
fn default_pool_cooldown_secs() -> u64 {
    60
}
fn default_mcp_timeout_secs() -> u64 {
    30
}
fn default_mcp_enabled() -> bool {
    true
}
fn default_embed_provider() -> String {
    "auto".into()
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
            home: default_home(),
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
            model_dir: None,
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
            compress_enabled: default_compress_enabled(),
            compress_target_tokens: default_compress_target(),
            compress_trigger_tokens: default_compress_trigger(),
            compress_keep_tail_tokens: default_compress_keep_tail(),
            compress_summary_max_tokens: default_compress_summary_max(),
            think_scrub_enabled: default_think_scrub_enabled(),
            redact_memory_enabled: default_redact_memory_enabled(),
            tool_allow: None,
            tool_deny: Vec::new(),
            dangerous_tools: Vec::new(),
            auto_approve_tools: Vec::new(),
            auto_deny_tools: Vec::new(),
            system_budget_monthly_units: default_system_budget_units(),
            auxiliary_provider: None,
            auxiliary_model: None,
            auxiliary_max_tokens: default_auxiliary_max_tokens(),
            auxiliary_temperature: None,
            retry_enabled: false,
            retry_max_attempts: default_retry_max_attempts(),
            api_key_credentials: Vec::new(),
            api_key_envs: Vec::new(),
            pool_strategy: default_pool_strategy(),
            pool_cooldown_secs: default_pool_cooldown_secs(),
            mcp_servers: Vec::new(),
            aws_region: None,
            aws_access_key_credential: None,
            aws_access_key_env: None,
            aws_secret_key_credential: None,
            aws_secret_key_env: None,
            aws_session_token_credential: None,
            aws_session_token_env: None,
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
        ("COS_HOME".into(), cfg.home.clone()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_sensible_values() {
        let cfg = CosConfig::default();
        assert_eq!(cfg.version, env!("CARGO_PKG_VERSION"));
        // home defaults to $HOME at runtime, falling back to /root when unset.
        let expected_home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
        assert_eq!(cfg.home, expected_home);
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
        let json = r#"{"version": "1.0.0", "home": "/custom"}"#;
        let cfg: CosConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.version, "1.0.0");
        assert_eq!(cfg.home, "/custom");
        // Defaults for missing sections
        assert_eq!(cfg.exec.timeout, 300);
        assert_eq!(cfg.web.engine, "cos-browser");
        assert_eq!(cfg.web.cdp_port, 9222);
    }

    #[test]
    fn parse_full_config() {
        let json = r#"{
            "version": "0.1.0",
            "home": "/home/cos",
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
        assert!(keys.contains(&"COS_HOME"));
    }

    #[test]
    fn malformed_json_returns_defaults() {
        let json = "not valid json {{{";
        let cfg: CosConfig = serde_json::from_str(json).unwrap_or_default();
        assert_eq!(cfg.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(cfg.exec.timeout, 300);
    }
}
