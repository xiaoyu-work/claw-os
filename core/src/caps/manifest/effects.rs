//! Expected effects declared by an App, never proof of execution or authority.

use serde::{Deserialize, Serialize};

use super::{ArgKind, Manifest, ManifestError};
use crate::i18n::LocalizedText;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    Read,
    Create,
    Update,
    Delete,
    External,
    Execute,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectRecovery {
    NotApplicable,
    Reversible,
    Compensatable,
    Irreversible,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationEffect {
    pub kind: EffectKind,
    pub label: LocalizedText,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_arg: Option<String>,
    #[serde(default)]
    pub recovery: EffectRecovery,
}

pub(super) fn validate(manifest: &Manifest) -> Result<(), ManifestError> {
    for (name, operation) in &manifest.operations {
        let invalid = |detail: &str| ManifestError::EffectInvalid {
            operation: name.clone(),
            detail: detail.to_string(),
        };
        if operation.effects.len() > 16 {
            return Err(invalid("at most 16 effects may be declared"));
        }
        for effect in &operation.effects {
            effect.label.validate().map_err(|error| invalid(&error))?;
            if effect
                .label
                .iter()
                .any(|(_, value)| value.len() > 512 || value.chars().any(char::is_control))
            {
                return Err(invalid(
                    "effect labels must fit 512 bytes and contain no controls",
                ));
            }
            if let Some(target) = &effect.target_arg {
                let arg = operation
                    .args
                    .iter()
                    .find(|arg| arg.name == *target)
                    .ok_or_else(|| invalid("target_arg must name an operation argument"))?;
                if !matches!(arg.kind, ArgKind::Path | ArgKind::Host | ArgKind::Name) {
                    return Err(invalid(
                        "effect targets must be path, host, or name arguments",
                    ));
                }
            }
            if effect.kind == EffectKind::Read
                && !matches!(
                    effect.recovery,
                    EffectRecovery::NotApplicable | EffectRecovery::Unknown
                )
            {
                return Err(invalid(
                    "read effects cannot claim a mutation recovery mode",
                ));
            }
            if effect.kind != EffectKind::Read && effect.recovery == EffectRecovery::NotApplicable {
                return Err(invalid(
                    "non-read effects must state a recovery mode or unknown",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/caps/manifest/effects.rs"
    ));
}
