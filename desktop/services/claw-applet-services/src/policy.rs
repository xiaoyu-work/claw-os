// SPDX-License-Identifier: GPL-3.0-only

use serde::Deserialize;
use std::time::Duration;
use tokio::process::Command;

const POLICY_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureKind {
    Denied,
    Unavailable,
    Execution,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Failure {
    pub kind: FailureKind,
    pub message: String,
}

impl Failure {
    fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

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
    verb: Option<String>,
    scope: Option<DecisionScope>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    reason: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "lowercase",
    deny_unknown_fields
)]
enum DecisionScope {
    Name(String),
    Wild,
}

impl Scope<'_> {
    fn matches(self, actual: &DecisionScope) -> bool {
        match (self, actual) {
            (Self::Name(expected), DecisionScope::Name(actual)) => expected == actual,
            (Self::Wild, DecisionScope::Wild) => true,
            _ => false,
        }
    }
}

pub async fn require(verb: &str, scope: Scope<'_>) -> Result<(), String> {
    check(verb, scope).await.map_err(|error| error.message)
}

pub(crate) async fn check(verb: &str, scope: Scope<'_>) -> Result<(), Failure> {
    let mut command = Command::new(crate::command::cos_binary());
    command.args(["__policy", "check", verb]);
    scope.append_args(&mut command);
    let output = crate::command::output(&mut command, 16 * 1024, POLICY_TIMEOUT)
        .await
        .map_err(|error| {
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::TimedOut
            ) {
                Failure::new(
                    FailureKind::Unavailable,
                    format!("The ClawOS policy service is unavailable: {error}"),
                )
            } else {
                Failure::new(
                    FailureKind::Execution,
                    format!("Could not start the permission check for {verb}: {error}"),
                )
            }
        })?;

    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(Failure::new(
            FailureKind::Execution,
            if detail.is_empty() {
                format!("Permission check for {verb} exited with {}", output.status)
            } else {
                detail
            },
        ));
    }

    parse_decision(&output.stdout, verb, scope)
}

fn parse_decision(raw: &[u8], verb: &str, scope: Scope<'_>) -> Result<(), Failure> {
    let decision: Decision = serde_json::from_slice(raw).map_err(|error| {
        Failure::new(
            FailureKind::Execution,
            format!("Permission check for {verb} returned invalid data: {error}"),
        )
    })?;
    if decision.decision == "allow" {
        if decision.verb.as_deref() != Some(verb)
            || !decision
                .scope
                .as_ref()
                .is_some_and(|actual| scope.matches(actual))
        {
            return Err(Failure::new(
                FailureKind::Execution,
                "Permission response does not confirm the requested verb and scope.",
            ));
        }
        return Ok(());
    }
    if decision.decision != "deny" {
        return Err(Failure::new(
            FailureKind::Execution,
            "Permission response has an unknown decision.",
        ));
    }

    let detail = decision
        .summary
        .or_else(|| {
            decision.reason.map(|reason| match reason {
                serde_json::Value::String(message) => message,
                structured => structured.to_string(),
            })
        })
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| format!("Permission denied for {verb}."));
    Err(Failure::new(FailureKind::Denied, detail))
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/policy.rs"));
}
