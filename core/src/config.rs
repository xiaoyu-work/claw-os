/// Configuration loading for Claw OS.
///
/// Reads `~/.config/cos/config.json` (or `COS_CONFIG_PATH` override)
/// and provides typed access to settings. Falls back to sensible
/// defaults if the config file is missing or malformed. The file is
/// per-user; `cos agent setup` (and the cosmic-settings agent page)
/// write to it under the running user's `$HOME`, so changes don't
/// need root.
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

static CONFIG: OnceLock<CosConfig> = OnceLock::new();

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
/// `~/.config/cos/config.json`. All fields have sensible defaults so the agent
/// can run without explicit configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Name of the LLM provider to use (must be registered, e.g.
    /// "anthropic", "openai", "ollama"). Default is empty string,
    /// meaning **not configured** — every AI call will fail with
    /// `LlmError::NotConfigured` until the operator runs
    /// `cos agent setup llm apply ...` (or the desktop initial-setup
    /// AI page). The "mock" provider is registered for tests but is
    /// never picked up automatically.
    #[serde(default = "default_agent_provider")]
    pub provider: String,

    /// Model identifier passed to the provider (e.g. "claude-sonnet-4.6",
    /// "gpt-4.1", "llama3.2:3b"). Empty when `provider` is empty.
    #[serde(default = "default_agent_model")]
    pub model: String,

    /// Ordered cross-provider fallback chain for the system agent. A fallback
    /// is attempted only for transport, upstream, authentication, quota, or
    /// rate-limit failures; caller/request errors never switch providers.
    #[serde(default)]
    pub provider_fallbacks: Vec<ProviderFallbackConfig>,

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

    /// Optional tool-name approval override. Capability risk remains the
    /// default policy: high/critical capability requests enter the durable
    /// approval queue automatically. When this list is non-empty, headless
    /// mode (no approver
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

    /// Whether to scan XDG directories for `claw.agent-api/v1` manifests
    /// and attach them alongside `mcp_servers` at startup. Defaults to
    /// `true` so an adapter package dropped under
    /// `/usr/share/claw/agent-api/` Just Works without an extra config
    /// edit. Set to `false` to lock the agent down to only the
    /// servers explicitly listed in `mcp_servers`.
    #[serde(default = "default_true")]
    pub agent_api_discovery_enabled: bool,

    /// Override the discovery search dirs. Empty / unset falls back
    /// to the standard XDG lookup (`$XDG_DATA_HOME/claw/agent-api/`
    /// + each `$XDG_DATA_DIRS/claw/agent-api/`). Used by tests and
    /// for in-repo development where adapters live next to the source.
    #[serde(default)]
    pub agent_api_paths: Vec<String>,


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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderFallbackConfig {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub api_key_credential: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api_key_credentials: Vec<String>,
    #[serde(default)]
    pub api_key_envs: Vec<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub extra_headers: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub request_timeout: Option<u64>,
    #[serde(default)]
    pub pool_strategy: Option<String>,
    #[serde(default)]
    pub pool_cooldown_secs: Option<u64>,
    #[serde(default)]
    pub aws_region: Option<String>,
    #[serde(default)]
    pub aws_access_key_credential: Option<String>,
    #[serde(default)]
    pub aws_access_key_env: Option<String>,
    #[serde(default)]
    pub aws_secret_key_credential: Option<String>,
    #[serde(default)]
    pub aws_secret_key_env: Option<String>,
    #[serde(default)]
    pub aws_session_token_credential: Option<String>,
    #[serde(default)]
    pub aws_session_token_env: Option<String>,
}

impl ProviderFallbackConfig {
    pub fn apply_to(&self, base: &AgentConfig) -> AgentConfig {
        let mut config = base.clone();
        config.provider = self.provider.clone();
        config.model = self.model.clone();
        config.provider_fallbacks.clear();
        config.api_key_credential = self.api_key_credential.clone();
        config.api_key_env = self.api_key_env.clone();
        config.api_key_credentials = self.api_key_credentials.clone();
        config.api_key_envs = self.api_key_envs.clone();
        config.base_url = self.base_url.clone();
        config.extra_headers = self.extra_headers.clone();
        if let Some(timeout) = self.request_timeout {
            config.request_timeout = timeout;
        }
        if let Some(strategy) = &self.pool_strategy {
            config.pool_strategy = strategy.clone();
        }
        if let Some(cooldown) = self.pool_cooldown_secs {
            config.pool_cooldown_secs = cooldown;
        }
        config.aws_region = self.aws_region.clone();
        config.aws_access_key_credential = self.aws_access_key_credential.clone();
        config.aws_access_key_env = self.aws_access_key_env.clone();
        config.aws_secret_key_credential = self.aws_secret_key_credential.clone();
        config.aws_secret_key_env = self.aws_secret_key_env.clone();
        config.aws_session_token_credential = self.aws_session_token_credential.clone();
        config.aws_session_token_env = self.aws_session_token_env.clone();
        config
    }
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
/// `provider="auto"` (the default) uses the bundled local Qwen3
/// embedding stack when the image ships both the model and the
/// `ort-genai` runtime. `provider="none"` disables embeddings
/// explicitly; `provider="agent-auto"` derives the embedder from the
/// main `[agent]` provider for users who want cloud embeddings to reuse
/// their chat credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedConfig {
    /// `"auto"` (default — bundled local Qwen3 when available) |
    /// `"agent-auto"` (derive from `[agent]` when possible) |
    /// `"none"` (explicit off) | `"openai"` | `"azure"` | `"ollama"` |
    /// `"qwen3-local"` | other self-hosted alias. When `"auto"` and
    /// the bundled local stack is absent, embeddings are disabled until
    /// the user runs `cos agent setup embed`.
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
    String::new()
}
fn default_agent_model() -> String {
    String::new()
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
            provider_fallbacks: Vec::new(),
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
            agent_api_discovery_enabled: true,
            agent_api_paths: Vec::new(),
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
///
/// Read or parse failures are surfaced via `tracing::error!` rather
/// than silently falling back to defaults. The previous silent-default
/// behaviour meant an operator who hand-edited
/// `~/.config/cos/config.json` and introduced a JSON syntax error
/// would get a fully default config (different shell, no AI provider,
/// network outbound enabled, etc.) with no indication anything was
/// wrong. The audit explicitly called out "we never silently keep
/// stale config on a parse failure" — when the on-disk file is
/// unreadable we still return defaults so cos can boot, but we log
/// the underlying error at ERROR severity so it shows up in logs and
/// observability dashboards.
fn load_from_disk() -> CosConfig {
    let path = std::env::var_os("COS_CONFIG_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(crate::paths::user_config_path);

    load_from_path(path.as_ref())
}

/// Load a `CosConfig` from a specific path on disk. Missing file
/// returns defaults silently (matches `load_from_disk` semantics).
/// Read/parse errors log at ERROR severity and fall back to defaults.
///
/// Public for clawd, which resolves the requesting peer's `$HOME` from
/// `SO_PEERCRED` and reads *their* `<home>/.config/cos/config.json`
/// rather than its own root-owned file.
pub fn load_from_path(path: &Path) -> CosConfig {
    if !path.is_file() {
        return CosConfig::default();
    }

    let data = match fs::read_to_string(path) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(
                "config: failed to read {}: {e} — falling back to defaults",
                path.display()
            );
            return CosConfig::default();
        }
    };

    match serde_json::from_str::<CosConfig>(&data) {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::error!(
                "config: failed to parse {} as JSON: {e} — falling back to defaults. \
                 Check the file with `jq . {}` and re-run cos.",
                path.display(),
                path.display()
            );
            CosConfig::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Per-task config override
//
// clawd is the system-level agent daemon. It runs as root with
// `HOME=/root` under systemd, so its own `config::get()` reads
// `/root/.config/cos/config.json` — empty by default. But the agent
// jobs it executes were submitted by *user* shells (uid != 0), whose
// per-user `~/.config/cos/config.json` carries the provider config the
// user actually set up via `cos agent setup llm`.
//
// To bridge the two we install a per-async-task override: clawd's
// worker resolves the job's owner home, loads that user's config, and
// wraps the entire job execution in `with_override(...)`. Every
// `config::get()` call from within that task — including ones deep
// inside the LLM gate, model task helpers, and tool implementations —
// transparently sees the user's config instead of clawd's.
//
// Lifetime: `get()` returns `&'static CosConfig` and ~50 call sites
// rely on that. To keep the signature we intern each distinct
// user-config payload (by content hash) into a leaked `Box`. After
// interning the same content twice returns the same pointer, so the
// leak is bounded by the number of *distinct* configs clawd sees over
// its lifetime — in practice a small constant per user.
// ---------------------------------------------------------------------------

tokio::task_local! {
    /// Optional override for `config::get()`. Set by
    /// `with_override(...)` for the duration of a single
    /// clawd-dispatched agent job; absent everywhere else.
    static CONFIG_OVERRIDE: &'static CosConfig;
}

static OVERRIDE_INTERN: OnceLock<Mutex<HashMap<u64, &'static CosConfig>>> = OnceLock::new();

fn intern_static(cfg: CosConfig) -> &'static CosConfig {
    let serialized = serde_json::to_string(&cfg).unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    serialized.hash(&mut hasher);
    let key = hasher.finish();

    let cache = OVERRIDE_INTERN.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(existing) = guard.get(&key) {
        return existing;
    }
    let leaked: &'static CosConfig = Box::leak(Box::new(cfg));
    guard.insert(key, leaked);
    leaked
}

/// Load `<home>/.config/cos/config.json` and intern it into a
/// `'static` slot suitable for `with_override`. The same on-disk file
/// content always returns the same pointer.
pub fn intern_for_home(home: &Path) -> &'static CosConfig {
    let path = crate::paths::user_config_path_for(home);
    intern_static(load_from_path(&path))
}

/// Re-read the standard user config (`~/.config/cos/config.json` or
/// `$COS_CONFIG_PATH`) from disk and intern it as a `'static` pointer
/// suitable for [`with_override`].
///
/// Long-running daemons like `cos agent serve` cache the process-wide
/// `CONFIG: OnceLock<CosConfig>` at startup and never observe later
/// writes — including writes the daemon itself makes via
/// `cos agent setup apply`. Wrap each request handler in
/// `with_override(intern_user_config(), ...)` so every `config::get()`
/// call in the handler sees the current on-disk state.
pub fn intern_user_config() -> &'static CosConfig {
    let path = std::env::var_os("COS_CONFIG_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(crate::paths::user_config_path);
    intern_static(load_from_path(&path))
}

/// Run `fut` with `cfg` installed as the per-task override visible to
/// every `config::get()` call inside it (and any task spawned via
/// `tokio::spawn` from within it, because `task_local` propagates).
/// Outside the scope `config::get()` returns the process-wide config
/// as before.
pub async fn with_override<Fut, R>(cfg: &'static CosConfig, fut: Fut) -> R
where
    Fut: Future<Output = R>,
{
    CONFIG_OVERRIDE.scope(cfg, fut).await
}

/// Test/inspection helper: snapshot of the currently active override
/// pointer (`None` outside any `with_override` scope).
#[cfg(test)]
fn current_override() -> Option<&'static CosConfig> {
    CONFIG_OVERRIDE.try_with(|c| *c).ok()
}

/// Get the global config. Inside a [`with_override`] scope this
/// returns the override; outside, it returns the process-wide config
/// loaded once from disk.
pub fn get() -> &'static CosConfig {
    if let Ok(cfg) = CONFIG_OVERRIDE.try_with(|c| *c) {
        return cfg;
    }
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/config.rs"
    ));
}
