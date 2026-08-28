//! Model-facing progressive disclosure tool for Agent Skills.

use std::path::PathBuf;
use std::time::Instant;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value};

use super::{Tool, ToolResult};
use crate::agent::skills::disclosure;
use crate::agent::skills::loader::{self, LoadOptions, LoadResult, SkillOrigin};
use crate::agent::skills::provenance::{UsageRecord, UsageStore};

#[derive(Default)]
pub struct SkillDisclosure {
    roots: Option<(PathBuf, PathBuf)>,
    usage_path: Option<PathBuf>,
}

impl SkillDisclosure {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_paths(system_root: PathBuf, user_root: PathBuf, usage_path: PathBuf) -> Self {
        Self {
            roots: Some((system_root, user_root)),
            usage_path: Some(usage_path),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_roots(system_root: &std::path::Path, user_root: &std::path::Path) -> Self {
        Self::with_paths(
            system_root.to_path_buf(),
            user_root.to_path_buf(),
            user_root.join(".skills-usage.jsonl"),
        )
    }

    fn load_catalog(&self) -> LoadResult {
        let options = LoadOptions {
            include_body: false,
            ..LoadOptions::default()
        };
        match &self.roots {
            Some((system, user)) => loader::load_layered(system, user, &options),
            None => loader::load_catalog_default(),
        }
    }

    fn execute(&self, input: &Value) -> Result<DisclosureOutput, String> {
        let command = input
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing required field: command".to_string())?;
        let load = self.load_catalog();
        match command {
            "list" => {
                let offset = optional_usize(input, "offset", 0)?;
                let limit = optional_usize(input, "limit", 50)?;
                if limit == 0 || limit > disclosure::MAX_LIST_PAGE {
                    return Err(format!(
                        "limit must be between 1 and {}",
                        disclosure::MAX_LIST_PAGE
                    ));
                }
                let skills = disclosure::catalog_page(&load, offset, limit);
                let next_offset =
                    (offset + skills.len() < load.skills.len()).then_some(offset + skills.len());
                Ok(DisclosureOutput {
                    value: json!({
                        "disclosure_level": "metadata",
                        "total": load.skills.len(),
                        "offset": offset,
                        "limit": limit,
                        "next_offset": next_offset,
                        "skills": skills,
                        "disabled": load.disabled.len(),
                        "errors": load.errors.len(),
                    }),
                    untrusted_tag: Some(disclosure::SKILL_CATALOG_TAG),
                })
            }
            "read" => {
                let id = required_string(input, "id")?;
                let skill = load
                    .skills
                    .get(id)
                    .ok_or_else(|| format!("unknown or unavailable skill: {id}"))?;
                let hydrated = loader::hydrate(skill, &LoadOptions::default())?;
                Ok(DisclosureOutput {
                    value: disclosure::disclose_instructions(&hydrated)?,
                    untrusted_tag: (hydrated.origin != SkillOrigin::BuiltIn)
                        .then_some(disclosure::SKILL_CONTENT_TAG),
                })
            }
            "resource" => {
                let id = required_string(input, "id")?;
                let path = required_string(input, "path")?;
                let skill = load
                    .skills
                    .get(id)
                    .ok_or_else(|| format!("unknown or unavailable skill: {id}"))?;
                Ok(DisclosureOutput {
                    value: disclosure::disclose_resource(skill, path)?,
                    untrusted_tag: (skill.origin != SkillOrigin::BuiltIn)
                        .then_some(disclosure::SKILL_CONTENT_TAG),
                })
            }
            other => Err(format!(
                "unknown command `{other}`; expected list, read, or resource"
            )),
        }
    }
}

#[async_trait]
impl Tool for SkillDisclosure {
    fn name(&self) -> &str {
        "cos_skill"
    }

    fn description(&self) -> &str {
        "Progressively disclose Agent Skills: list metadata, read one matching SKILL.md, then read individual referenced resources only as needed."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["list", "read", "resource"],
                    "description": "Disclosure stage to perform."
                },
                "id": {
                    "type": "string",
                    "description": "Skill id from the metadata catalogue; required for read/resource."
                },
                "path": {
                    "type": "string",
                    "description": "Relative child-resource path returned by read; required for resource."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Zero-based metadata offset for list."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": disclosure::MAX_LIST_PAGE,
                    "description": "Metadata page size for list."
                }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let started = Instant::now();
        let command = input
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let skill_id = input.get("id").and_then(Value::as_str).map(str::to_string);
        let result = self.execute(&input);

        if matches!(command.as_str(), "read" | "resource") {
            if let Some(skill_id) = skill_id {
                let record = UsageRecord {
                    skill_id,
                    timestamp: Utc::now().to_rfc3339(),
                    success: result.is_ok(),
                    duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                    invoked_by: Some(format!("cos_skill:{command}")),
                    resource_path: (command == "resource")
                        .then(|| {
                            input
                                .get("path")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string()
                        })
                        .filter(|path| !path.is_empty()),
                };
                let usage_path = self
                    .usage_path
                    .clone()
                    .unwrap_or_else(crate::paths::agent_skills_usage_path);
                let store = UsageStore::new(usage_path);
                if let Err(error) = store.record(&record) {
                    tracing::warn!("cos_skill: failed to record usage: {error}");
                }
            }
        }

        match result {
            Ok(output) => {
                let serialized = serde_json::to_string_pretty(&output.value)
                    .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#));
                let content = output
                    .untrusted_tag
                    .map(|tag| crate::agent::safety::untrusted::wrap_untrusted(tag, &serialized))
                    .unwrap_or(serialized);
                ToolResult::ok(content)
            }
            Err(error) => ToolResult::err(error),
        }
    }

    fn parallel_safe(&self) -> bool {
        true
    }
}

struct DisclosureOutput {
    value: Value,
    untrusted_tag: Option<&'static str>,
}

fn required_string<'a>(input: &'a Value, field: &str) -> Result<&'a str, String> {
    input
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("missing required field: {field}"))
}

fn optional_usize(input: &Value, field: &str, default: usize) -> Result<usize, String> {
    match input.get(field) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| format!("{field} must be a non-negative integer")),
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/skills.rs"
    ));
}
