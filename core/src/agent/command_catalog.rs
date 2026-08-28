use serde_json::{json, Value};

/// Describe the internal Agent command namespace for CLI discovery.
pub(crate) fn dev_help() -> Value {
    json!({
        "namespace": "cos agent dev",
        "summary": "Internal building blocks and power-user diagnostics. Not part of the stable user-facing surface.",
        "subcommands": [
            "insights", "usage", "audit", "replay", "run-log",
            "providers", "provider-doctor", "llm",
            "prompt", "tools", "guardrails", "approval",
            "redact", "think-scrub", "tokens", "title", "summarise", "classify",
            "display", "binary-ext", "file-safety", "context",
            "compress", "aux", "retry", "vision", "osv",
            "curator", "nudge", "shell-hooks", "media",
            "semantic", "interrupt", "learn", "hooks",
        ],
    })
}
