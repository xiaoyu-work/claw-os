use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::clawd::protocol::{Request, RequestId};
use crate::clawd::routes::Command;
use crate::clawd::wire::requests as body;

pub const MAX_CONTROL_BYTES: usize = 1024 * 1024;
pub const MAX_CONTROL_CALLS: u32 = 4096;
pub const MAX_ACTIVE_CALLS: usize = 8;
pub const MAX_ACTIVE_SESSIONS: usize = 16;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    pub request: body::AppSessionRegister,
    pub invocation_id: crate::clawd::wire::bounded::Token,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationEnd {
    pub invocation_id: crate::clawd::wire::bounded::Token,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCallEnd {
    pub request: body::AppSessionSetTransient,
    pub call_id: crate::clawd::wire::bounded::Token,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppCheck {
    pub session_id: crate::clawd::wire::bounded::Token,
    #[serde(deserialize_with = "package_digest")]
    pub package_digest: String,
}

fn package_digest<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let digest = String::deserialize(deserializer)?;
    if !crate::provenance::envelope::is_sha256_ref(&digest) {
        return Err(serde::de::Error::custom("invalid App package digest"));
    }
    Ok(digest)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "method",
    content = "params",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum AppHostCall {
    Begin(crate::operations::invocation::AppInvocation),
    BeginSession(crate::operations::invocation::AppSessionInvocation),
    End(InvocationEnd),
    Register(Registration),
    Bind(body::AppSessionBind),
    Relay(body::AppSessionRelay),
    Deregister(body::AppSessionDeregister),
    ApprovalStatus(body::PermissionStatus),
    Check(AppCheck),
    StartCall(body::AppSessionSetTransient),
    EndCall(SessionCallEnd),
}

impl AppHostCall {
    pub fn from_request(request: &Request, invocation: Option<String>) -> Result<Self, String> {
        let params = (request.command.route().decode)(request.params.clone())
            .map_err(|_| "invalid App-host control parameters".to_string())?;
        let call = match request.command {
            Command::AppSessionRegister => {
                let registration: body::AppSessionRegister =
                    serde_json::from_value(params).map_err(|error| error.to_string())?;
                if !matches!(
                    registration.kind.as_ref().map(|kind| kind.as_str()),
                    Some("operation" | "mcp")
                ) {
                    return Err(
                        "this App host accepts only declared operations or App sessions"
                            .to_string(),
                    );
                }
                let invocation = invocation.ok_or_else(|| {
                    "App-host registration requires a prepared original invocation".to_string()
                })?;
                Self::Register(Registration {
                    request: registration,
                    invocation_id: crate::clawd::wire::bounded::Token::parse(&invocation)
                        .map_err(str::to_string)?,
                })
            }
            command => {
                if invocation.is_some() {
                    return Err("package context is valid only for App registration".to_string());
                }
                match command {
                    Command::AppSessionBind => Self::Bind(
                        serde_json::from_value(params).map_err(|error| error.to_string())?,
                    ),
                    Command::AppSessionRelay => Self::Relay(
                        serde_json::from_value(params).map_err(|error| error.to_string())?,
                    ),
                    Command::AppSessionDeregister => Self::Deregister(
                        serde_json::from_value(params).map_err(|error| error.to_string())?,
                    ),
                    Command::PermissionStatus => Self::ApprovalStatus(
                        serde_json::from_value(params).map_err(|error| error.to_string())?,
                    ),
                    _ => {
                        return Err(
                            "broker command is not on the controlled App-host surface".to_string()
                        )
                    }
                }
            }
        };
        call.validate()?;
        Ok(call)
    }

    pub fn command(&self) -> Option<Command> {
        Some(match self {
            Self::Register(_) => Command::AppSessionRegister,
            Self::Bind(_) => Command::AppSessionBind,
            Self::Relay(_) => Command::AppSessionRelay,
            Self::Deregister(_) => Command::AppSessionDeregister,
            Self::ApprovalStatus(_) => Command::PermissionStatus,
            Self::StartCall(_) | Self::EndCall(_) => Command::AppSessionSetTransient,
            Self::Check(_) | Self::Begin(_) | Self::BeginSession(_) | Self::End(_) => return None,
        })
    }

    pub fn params(&self) -> Result<Value, String> {
        let value = match self {
            Self::Begin(value) => serde_json::to_value(value),
            Self::BeginSession(value) => serde_json::to_value(value),
            Self::End(value) => serde_json::to_value(value),
            Self::Register(value) => serde_json::to_value(&value.request),
            Self::Bind(value) => serde_json::to_value(value),
            Self::Relay(value) => serde_json::to_value(value),
            Self::Deregister(value) => serde_json::to_value(value),
            Self::ApprovalStatus(value) => serde_json::to_value(value),
            Self::Check(value) => serde_json::to_value(value),
            Self::StartCall(value) => serde_json::to_value(value),
            Self::EndCall(value) => serde_json::to_value(&value.request),
        };
        value.map_err(|error| error.to_string())
    }

    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Bind(value) => Some(value.session_id.as_str()),
            Self::Relay(value) => Some(value.session_id.as_str()),
            Self::Deregister(value) => Some(value.session_id.as_str()),
            Self::Check(value) => Some(value.session_id.as_str()),
            Self::StartCall(value) => Some(value.session_id.as_str()),
            Self::EndCall(value) => Some(value.request.session_id.as_str()),
            _ => None,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if serde_json::to_vec(self)
            .map_err(|error| error.to_string())?
            .len()
            > MAX_CONTROL_BYTES
        {
            return Err("App-host control exceeds 1 MiB".to_string());
        }
        if let Self::Register(registration) = self {
            match registration.request.kind.as_ref().map(|kind| kind.as_str()) {
                Some("operation") if registration.request.operation.is_some() => {}
                Some("mcp")
                    if registration.request.operation.is_none()
                        && registration.request.args.is_none() => {}
                _ => {
                    return Err("App host requires a declared operation or App session".to_string())
                }
            }
        }
        if let Self::Begin(invocation) = self {
            if !crate::provenance::envelope::is_sha256_ref(&invocation.package_digest) {
                return Err("invalid App-host package digest".to_string());
            }
            serde_json::from_value::<body::AppSessionRegister>(serde_json::json!({
                "app_id": invocation.app_id,
                "kind": "operation",
                "operation": invocation.operation,
                "args": invocation.args,
            }))
            .map_err(|error| format!("invalid original App invocation: {error}"))?;
        }
        if let Self::Check(check) = self {
            if !crate::provenance::envelope::is_sha256_ref(&check.package_digest) {
                return Err("invalid App-host package check".to_string());
            }
        }
        if let Self::BeginSession(invocation) = self {
            if !crate::provenance::envelope::is_sha256_ref(&invocation.package_digest) {
                return Err("invalid App session package digest".to_string());
            }
            serde_json::from_value::<body::AppSessionRegister>(serde_json::json!({
                "app_id": invocation.app_id, "kind":"mcp",
            }))
            .map_err(|error| format!("invalid App session invocation: {error}"))?;
        }
        if let Self::StartCall(call) = self {
            if call.call.is_none() {
                return Err("session call start requires declared tool arguments".to_string());
            }
        }
        if let Self::EndCall(call) = self {
            if call.request.call.is_some() {
                return Err("session call end must not install capabilities".to_string());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppHostRequest {
    pub task_id: String,
    pub correlation_id: u64,
    pub request_id: RequestId,
    pub call: AppHostCall,
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agentd/app_host/protocol.rs"
    ));
}
