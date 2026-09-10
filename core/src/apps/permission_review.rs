//! App permission disclosure, not permission grants or publisher trust.
//! Consumers must authenticate the manifest before presenting this review.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::caps::manifest::{
    AiPolicy, Arg, Desktop, Manifest, McpAccess, McpLifecycle, Need, NeedCondition, ScopeBinding,
    ScopeTransform,
};
use crate::caps::{catalog, Risk, Verb};
use crate::i18n::LocalizedText;

#[derive(Clone, Debug, Serialize)]
pub struct PermissionUse {
    pub entry: String,
    pub purpose: LocalizedText,
    pub arguments: Vec<Arg>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RequestedPermission {
    pub verb: Verb,
    pub label: String,
    pub description: String,
    pub risk: Risk,
    pub scope: ScopeBinding,
    pub when: Option<NeedCondition>,
    pub uses: Vec<PermissionUse>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ServiceDisclosure {
    pub lifecycle: McpLifecycle,
    pub access: McpAccess,
}

#[derive(Clone, Debug, Serialize)]
pub struct PermissionReview {
    pub schema_version: u32,
    pub app_id: String,
    pub app_version: String,
    pub name: LocalizedText,
    pub permissions: Vec<RequestedPermission>,
    pub ai_policy: Option<AiPolicy>,
    pub service: Option<ServiceDisclosure>,
    pub desktop: Option<Desktop>,
    pub contract_digest: String,
    pub permissions_granted: bool,
}

impl PermissionReview {
    pub fn from_manifest(manifest: &Manifest) -> Result<Self, String> {
        let mut requested = BTreeMap::new();
        for (name, operation) in &manifest.operations {
            collect(
                &mut requested,
                &format!("operation:{name}"),
                &operation.args,
                &operation.needs,
            )?;
        }
        if let Some(service) = &manifest.mcp {
            for tool in &service.tools {
                collect(
                    &mut requested,
                    &format!("mcp:{}", tool.name),
                    &tool.args,
                    &tool.needs,
                )?;
            }
        }
        let mut review = Self {
            schema_version: 1,
            app_id: manifest.id.clone(),
            app_version: manifest.version.clone(),
            name: manifest.name.clone(),
            permissions: requested.into_values().collect(),
            ai_policy: manifest.ai.clone(),
            service: manifest.mcp.as_ref().map(|service| ServiceDisclosure {
                lifecycle: service.lifecycle,
                access: service.access.clone(),
            }),
            desktop: manifest.desktop.clone(),
            contract_digest: String::new(),
            permissions_granted: false,
        };
        let contract = serde_json::to_vec(&review.contract())
            .map_err(|error| format!("serialize App permission contract: {error}"))?;
        review.contract_digest = format!("sha256:{:x}", Sha256::digest(contract));
        Ok(review)
    }

    fn contract(&self) -> Value {
        let permissions: Vec<Value> = self
            .permissions
            .iter()
            .map(|permission| {
                let uses: Vec<Value> = permission
                    .uses
                    .iter()
                    .map(|usage| {
                        let arguments: Vec<Value> = usage
                            .arguments
                            .iter()
                            .map(|argument| {
                                json!({
                                    "name": argument.name,
                                    "kind": argument.kind,
                                    "binding": argument.effective_binding(),
                                    "required": argument.required,
                                    "required_when": argument.required_when,
                                    "repeatable": argument.repeatable,
                                    "choices": argument.choices,
                                    "default": argument.default,
                                })
                            })
                            .collect();
                        json!({ "entry": usage.entry, "arguments": arguments })
                    })
                    .collect();
                json!({
                    "verb": permission.verb,
                    "scope": permission.scope,
                    "when": permission.when,
                    "uses": uses,
                })
            })
            .collect();
        json!({
            "schema_version": self.schema_version,
            "app_id": self.app_id,
            "permissions": permissions,
            "ai_policy": self.ai_policy,
            "service": self.service,
            "desktop": self.desktop,
        })
    }

    pub fn format_for_review(&self) -> Result<String, String> {
        let quote = |value: &str| {
            serde_json::to_string(value)
                .map_err(|error| format!("format App permission text: {error}"))
        };
        let mut text = format!(
            "\nApp permission requests: {} ({}) version {}\n",
            quote(self.name.current())?,
            quote(&self.app_id)?,
            quote(&self.app_version)?,
        );
        text.push_str("Installing this App does not grant these permissions.\n");
        if self.permissions.is_empty() {
            text.push_str("No capability requests are declared by its operations or MCP tools.\n");
        }
        for permission in &self.permissions {
            text.push_str(&format!(
                "\n- [{}] {} ({})\n  {}\n  Scope: {}\n",
                permission.risk.as_str(),
                permission.label,
                permission.verb,
                permission.description,
                scope_description(&permission.scope)?,
            ));
            if let Some(condition) = &permission.when {
                text.push_str(&format!(
                    "  Conditional request: {}\n",
                    serde_json::to_string(condition)
                        .map_err(|error| format!("format permission condition: {error}"))?,
                ));
            }
            for usage in &permission.uses {
                text.push_str(&format!(
                    "  {} - App's reason: {}\n",
                    quote(&usage.entry)?,
                    quote(usage.purpose.current())?,
                ));
            }
        }
        if let Some(service) = &self.service {
            text.push_str(&format!(
                "\nService lifecycle: {}\nExternal Agent access requested: {}\n",
                serde_json::to_string(&service.lifecycle)
                    .map_err(|error| format!("format App lifecycle: {error}"))?,
                service.access.external_agents,
            ));
        }
        if let Some(desktop) = &self.desktop {
            text.push_str(&format!(
                "\nDesktop entry requested. File types: {}\n",
                serde_json::to_string(&desktop.mime_types)
                    .map_err(|error| format!("format App file types: {error}"))?,
            ));
        }
        if let Some(policy) = &self.ai_policy {
            text.push_str(&format!(
                "\nAI policy (requires separate consent):\n{}\n",
                serde_json::to_string_pretty(policy)
                    .map_err(|error| format!("format App AI policy: {error}"))?,
            ));
        }
        text.push_str(
            "\nArgument-derived resources are selected and authorized when used.\n\
             Conditional requests are listed even when that feature is not currently in use.\n",
        );
        Ok(text)
    }
}

fn collect(
    requested: &mut BTreeMap<String, RequestedPermission>,
    entry: &str,
    arguments: &[Arg],
    needs: &[Need],
) -> Result<(), String> {
    for need in needs {
        let key = serde_json::to_string(&(need.verb, &need.scope, &need.when))
            .map_err(|error| format!("serialize App permission request: {error}"))?;
        let metadata = catalog::lookup(need.verb)
            .ok_or_else(|| format!("capability {} has no review metadata", need.verb))?;
        let permission = requested.entry(key).or_insert_with(|| RequestedPermission {
            verb: need.verb,
            label: metadata.label.current().to_string(),
            description: metadata.blurb.current().to_string(),
            risk: metadata.risk,
            scope: need.scope.clone(),
            when: need.when.clone(),
            uses: Vec::new(),
        });
        permission.uses.push(PermissionUse {
            entry: entry.to_string(),
            purpose: need.why.clone(),
            arguments: arguments.to_vec(),
        });
        permission
            .uses
            .sort_by(|left, right| left.entry.cmp(&right.entry));
    }
    Ok(())
}

fn scope_description(scope: &ScopeBinding) -> Result<String, String> {
    let quote = |value: &str| {
        serde_json::to_string(value).map_err(|error| format!("format permission scope: {error}"))
    };
    Ok(match scope {
        ScopeBinding::Fixed { scope } => quote(&scope.to_string())?,
        ScopeBinding::FromArg { arg, transform } => {
            let selection = match transform {
                ScopeTransform::Identity => "value",
                ScopeTransform::Parent => "parent directory",
                ScopeTransform::UrlHost => "URL host and port",
            };
            format!(
                "{selection} of argument {}, selected when used",
                quote(arg)?
            )
        }
        ScopeBinding::FromArgMap { arg, values } => format!(
            "argument {} selects from {}",
            quote(arg)?,
            serde_json::to_string(values)
                .map_err(|error| format!("format permission scope choices: {error}"))?,
        ),
        ScopeBinding::FromArgOrWild { arg, wild_when } => format!(
            "argument {}, with wildcard selection for {}",
            quote(arg)?,
            quote(wild_when)?,
        ),
        ScopeBinding::Wild => {
            "the caller's granted scopes; this is not a grant of unrestricted access".to_string()
        }
    })
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/apps/permission_review.rs"
    ));
}
