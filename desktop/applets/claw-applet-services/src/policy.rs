// SPDX-License-Identifier: GPL-3.0-only

use serde::Deserialize;
use std::time::Duration;
use tokio::{process::Command, time::timeout};

const POLICY_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy)]
pub enum Scope<'a> {
    Name(&'a str),
    Wild,
}

impl Scope<'_> {
    fn append_args(self, command: &mut Command) {
        match self {
            Self::Name(name) => {
                command.args(["--name", name]);
            }
            Self::Wild => {
                command.arg("--wild");
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct Decision {
    decision: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

pub async fn require(verb: &str, scope: Scope<'_>) -> Result<(), String> {
    let mut command = Command::new(cos_binary());
    command.args(["__policy", "check", verb]);
    scope.append_args(&mut command);
    command.kill_on_drop(true);

    let output = timeout(POLICY_TIMEOUT, command.output())
        .await
        .map_err(|_| format!("Permission check for {verb} timed out."))?
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "The ClawOS policy service is not installed.".to_string()
            } else {
                format!("Could not start the permission check for {verb}: {error}")
            }
        })?;

    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("Permission check for {verb} exited with {}", output.status)
        } else {
            detail
        });
    }

    parse_decision(&output.stdout, verb)
}

fn parse_decision(raw: &[u8], verb: &str) -> Result<(), String> {
    let decision: Decision = serde_json::from_slice(raw)
        .map_err(|error| format!("Permission check for {verb} returned invalid data: {error}"))?;
    if decision.decision == "allow" {
        return Ok(());
    }

    let detail = decision
        .summary
        .or(decision.reason)
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| format!("Permission denied for {verb}."));
    Err(detail)
}

fn cos_binary() -> String {
    std::env::var("COS_BIN").unwrap_or_else(|_| "cos".to_string())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/policy.rs"
    ));
}
