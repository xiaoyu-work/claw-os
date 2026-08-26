//! Trusted interactive OAuth authorization initiated by the system Agent.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::super::{Tool, ToolResult};

pub struct CosOauthLoginTool;

impl CosOauthLoginTool {
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for CosOauthLoginTool {
    fn name(&self) -> &str {
        "cos_oauth_login"
    }

    fn description(&self) -> &str {
        "Start a trusted Google or Microsoft OAuth authorization from an \
         attended local system-Agent session. Use this when a bundled App \
         returns auth_required with a matching setup.agent_action. The system \
         browser and terminal handle user consent; access and refresh tokens \
         go directly to the default encrypted credential namespace and never \
         enter model-visible content. After authorized=true, retry the original \
         App operation once."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "enum": ["google", "microsoft"],
                    "description": "OAuth provider selected by the App's setup.agent_action."
                },
                "timeout_seconds": {
                    "type": "integer",
                    "minimum": 30,
                    "maximum": 900,
                    "default": 300,
                    "description": "Maximum time to wait for browser authorization."
                }
            },
            "required": ["provider"],
            "additionalProperties": false
        })
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let provider = match input.get("provider").and_then(Value::as_str) {
            Some(provider @ ("google" | "microsoft")) => provider.to_string(),
            Some(provider) => {
                return ToolResult::err(format!(
                    "unsupported OAuth provider: {provider}. supported: google, microsoft"
                ));
            }
            None => return ToolResult::err("missing 'provider' field"),
        };
        let timeout = match input.get("timeout_seconds") {
            Some(value) => match value.as_u64() {
                Some(seconds @ 30..=900) => Some(seconds),
                _ => return ToolResult::err("timeout_seconds must be between 30 and 900"),
            },
            None => None,
        };

        let mut args = vec![provider];
        if let Some(seconds) = timeout {
            args.extend(["--timeout".to_string(), seconds.to_string()]);
        }

        if crate::paths::is_routed_job()
            || crate::paths::current_owner_uid_override().is_some()
            || crate::paths::current_home_override().is_some()
        {
            return oauth_result(tokio::task::block_in_place(|| {
                crate::credential::run_agent_oauth_login(&args)
            }));
        }

        let result =
            tokio::task::spawn_blocking(move || crate::credential::run_agent_oauth_login(&args))
                .await;
        match result {
            Ok(result) => oauth_result(result),
            Err(error) => ToolResult::err(format!("OAuth login task failed: {error}")),
        }
    }

    fn parallel_safe(&self) -> bool {
        false
    }
}

fn oauth_result(result: Result<Value, String>) -> ToolResult {
    match result {
        Ok(value) => {
            ToolResult::ok(serde_json::to_string(&value).unwrap_or_else(|_| value.to_string()))
        }
        Err(message) => ToolResult::err(message),
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/cos_proxy/oauth_login.rs"
    ));
}
