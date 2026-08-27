//! Canonical names and lifetime policy for curated memory facts.
//!
//! The curator treats model output as untrusted suggestions. This module
//! normalizes those suggestions into a small, deterministic ontology before
//! dedupe, secret filtering, persistence, or prompt projection.

/// Default lifetime for discoverable environment observations.
pub const DEFAULT_OBSERVED_TTL_DAYS: u32 = 30;
/// No model-provided observation may remain current for longer than this.
pub const MAX_OBSERVED_TTL_DAYS: u32 = 90;

const SHORT_OBSERVED_TTL_DAYS: u32 = 1;
const MEDIUM_OBSERVED_TTL_DAYS: u32 = 7;
const MAX_COMPONENT_CHARS: usize = 64;
const MAX_VALUE_CHARS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactLifetime {
    Durable,
    Observed,
    Session,
    Procedure,
}

impl FactLifetime {
    pub fn parse(value: &str) -> Option<Self> {
        match normalize_identifier(value)?.as_str() {
            "durable" | "permanent" | "long_term" => Some(Self::Durable),
            "observed" | "observation" | "volatile" | "environment_state" => Some(Self::Observed),
            "session" | "task" | "transient" | "ephemeral" => Some(Self::Session),
            "procedure" | "workflow" | "runbook" | "instructions" => Some(Self::Procedure),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Durable => "durable",
            Self::Observed => "observed",
            Self::Session => "session",
            Self::Procedure => "procedure",
        }
    }

    pub fn is_memory_eligible(self) -> bool {
        matches!(self, Self::Durable | Self::Observed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalSlot {
    pub entity: String,
    pub attribute: String,
    pub value: String,
}

impl CanonicalSlot {
    pub fn key(&self) -> String {
        format!("{}.{}", self.entity, self.attribute)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernedFact {
    pub slot: CanonicalSlot,
    pub lifetime: FactLifetime,
    pub ttl_days: Option<u32>,
}

/// Normalize one structured fact and apply the repository's lifetime policy.
///
/// Model declarations cannot turn discoverable live state into unbounded
/// durable memory, or turn preferences and reusable resolutions into
/// short-lived inventory.
pub fn normalize_fact(
    category: &str,
    entity: &str,
    attribute: &str,
    value: &str,
    declared_lifetime: Option<FactLifetime>,
    requested_ttl_days: Option<u32>,
) -> Option<GovernedFact> {
    let slot = normalize_slot(entity, attribute, value)?;
    let lifetime = classify_lifetime(category, &slot, declared_lifetime);
    let ttl_days = (lifetime == FactLifetime::Observed).then(|| {
        requested_ttl_days
            .unwrap_or_else(|| default_ttl_days(&slot))
            .clamp(1, MAX_OBSERVED_TTL_DAYS)
    });
    Some(GovernedFact {
        slot,
        lifetime,
        ttl_days,
    })
}

/// Canonicalize the logical slot without assigning migration-time lifetime.
///
/// Prompt projection uses this for both new governed lines and legacy lines.
/// Legacy lines intentionally retain their old no-expiry behavior.
pub fn normalize_slot(entity: &str, attribute: &str, value: &str) -> Option<CanonicalSlot> {
    let entity = canonical_entity(&normalize_identifier(entity)?).to_string();
    let mut attribute = normalize_identifier(attribute)?;
    let mut value = collapse_whitespace(value);
    if value.is_empty() || value.chars().count() > MAX_VALUE_CHARS {
        return None;
    }

    attribute = canonical_attribute(&entity, &attribute).to_string();

    let installation = canonical_installation_value(&value);
    if attribute == "version" {
        if let Some(state) = installation {
            attribute = "installation".to_string();
            value = state.to_string();
        }
    } else if attribute == "installation" {
        if let Some(state) = installation {
            value = state.to_string();
        } else if looks_like_version(&value) {
            attribute = "version".to_string();
        }
    } else if matches!(
        attribute.as_str(),
        "status" | "state" | "availability" | "presence"
    ) && !is_runtime_entity(&entity)
    {
        if let Some(state) = installation {
            attribute = "installation".to_string();
            value = state.to_string();
        }
    }

    if entity.chars().count() > MAX_COMPONENT_CHARS
        || attribute.chars().count() > MAX_COMPONENT_CHARS
    {
        return None;
    }

    Some(CanonicalSlot {
        entity,
        attribute,
        value,
    })
}

/// Parse a rendered `entity.attribute = value` body without applying aliases.
pub fn split_structured_body(body: &str) -> Option<(String, String)> {
    let (lhs, rhs) = body.split_once(" = ")?;
    let key = lhs.trim();
    if key.is_empty() || key.contains(char::is_whitespace) {
        return None;
    }
    let (entity, attribute) = key.split_once('.')?;
    if entity.is_empty() || attribute.is_empty() || attribute.contains('.') {
        return None;
    }
    let value = rhs.trim();
    if value.is_empty() {
        return None;
    }
    Some((key.to_ascii_lowercase(), value.to_string()))
}

pub fn installation_is_absent(value: &str) -> bool {
    canonical_installation_value(value) == Some("not_found")
}

pub fn date_from_unix_s(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}")
}

pub fn date_from_unix_ms(ts_ms: i64) -> Option<String> {
    (ts_ms >= 0).then(|| date_from_unix_s((ts_ms as u64) / 1_000))
}

pub fn date_to_epoch_days(value: &str) -> Option<u64> {
    let mut parts = value.trim().split('-');
    let year = parts.next()?.parse::<i64>().ok()?;
    let month = parts.next()?.parse::<u32>().ok()?;
    let day = parts.next()?.parse::<u32>().ok()?;
    if parts.next().is_some() || !(1970..=9999).contains(&year) {
        return None;
    }
    let max_day = days_in_month(year, month)?;
    if day == 0 || day > max_day {
        return None;
    }

    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = month as i64 + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    (days >= 0).then_some(days as u64)
}

fn normalize_identifier(value: &str) -> Option<String> {
    let mut out = String::new();
    let mut pending_separator = false;
    for c in value.trim().chars() {
        if c.is_alphanumeric() {
            if pending_separator && !out.is_empty() {
                out.push('_');
            }
            for lower in c.to_lowercase() {
                out.push(lower);
            }
            pending_separator = false;
        } else {
            pending_separator = true;
        }
    }
    (!out.is_empty()).then_some(out)
}

fn canonical_entity(entity: &str) -> &str {
    match entity {
        "operating_system"
        | "operating_system_info"
        | "system_os"
        | "linux_distribution"
        | "linux_distro" => "os",
        "claw_os_agent"
        | "claw_agent"
        | "claw_daemon"
        | "clawd"
        | "cos_agent"
        | "cos_system_agent"
        | "system_agent"
        | "system_agent_deployment"
        | "agent_runtime" => "claw_os",
        "system_memory" | "physical_memory" | "ram" => "memory",
        "nodejs" | "node_js" => "node",
        "python3" => "python",
        other => other,
    }
}

fn canonical_attribute<'a>(entity: &str, attribute: &'a str) -> &'a str {
    if entity == "os" {
        return match attribute {
            "name" | "distribution" | "distro" | "base_distribution" | "platform" => "distribution",
            "release" | "release_version" | "version_number" | "os_version" => "version",
            other => other,
        };
    }
    if entity == "claw_os" {
        return match attribute {
            "deployment" | "deployment_state" | "installed" | "install_status"
            | "installation_state" | "presence" => "installation",
            "installed_version" | "release_version" | "version_number" => "version",
            other => other,
        };
    }
    if entity == "memory" {
        return match attribute {
            "size" | "memory_size" | "ram_size" | "total" | "total_memory" => "capacity",
            other => other,
        };
    }
    match attribute {
        "installed"
        | "is_installed"
        | "install_status"
        | "installation_status"
        | "installation_state" => "installation",
        "desired_version" | "version_preference" => "preferred_version",
        "installed_version" | "package_version" | "release_version" | "version_number" => "version",
        other => other,
    }
}

fn canonical_installation_value(value: &str) -> Option<&'static str> {
    match normalize_identifier(value)?.as_str() {
        "not_found" | "not_installed" | "missing" | "absent" | "unavailable" | "false" | "no" => {
            Some("not_found")
        }
        "installed" | "present" | "found" | "available" | "true" | "yes" => Some("installed"),
        _ => None,
    }
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn looks_like_version(value: &str) -> bool {
    let trimmed = value.trim().trim_start_matches(['v', 'V']);
    !trimmed.is_empty()
        && trimmed.chars().any(|c| c.is_ascii_digit())
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'))
}

fn classify_lifetime(
    category: &str,
    slot: &CanonicalSlot,
    declared: Option<FactLifetime>,
) -> FactLifetime {
    let category = normalize_identifier(category).unwrap_or_else(|| "unknown".to_string());

    if let Some(lifetime @ (FactLifetime::Session | FactLifetime::Procedure)) = declared {
        return lifetime;
    }
    if matches!(
        category.as_str(),
        "procedure" | "workflow" | "runbook" | "instruction" | "instructions"
    ) {
        return FactLifetime::Procedure;
    }
    if matches!(
        category.as_str(),
        "session" | "task" | "transient" | "ephemeral"
    ) || is_session_slot(slot)
    {
        return FactLifetime::Session;
    }
    if is_procedure_slot(slot) {
        return FactLifetime::Procedure;
    }
    if is_live_state(slot) && !is_explicit_preference_slot(slot) {
        return FactLifetime::Observed;
    }
    if matches!(
        category.as_str(),
        "preference" | "pref" | "identity" | "id" | "skill" | "skills" | "resolution" | "fix"
    ) {
        return FactLifetime::Durable;
    }
    if category == "environment" || category == "env" {
        return if is_durable_convention(slot) {
            FactLifetime::Durable
        } else {
            FactLifetime::Observed
        };
    }
    declared.unwrap_or(FactLifetime::Durable)
}

fn is_session_slot(slot: &CanonicalSlot) -> bool {
    matches!(slot.entity.as_str(), "session" | "task" | "current_task")
        || matches!(
            slot.attribute.as_str(),
            "active_issue" | "active_task" | "current_goal" | "current_task" | "task_status"
        )
}

fn is_procedure_slot(slot: &CanonicalSlot) -> bool {
    matches!(
        slot.attribute.as_str(),
        "commands" | "instructions" | "procedure" | "runbook" | "steps" | "workflow"
    )
}

fn is_explicit_preference_slot(slot: &CanonicalSlot) -> bool {
    slot.attribute == "preference" || slot.attribute.starts_with("preferred_")
}

fn is_live_state(slot: &CanonicalSlot) -> bool {
    matches!(
        slot.attribute.as_str(),
        "availability"
            | "capacity"
            | "distribution"
            | "enabled"
            | "installation"
            | "memory_size"
            | "package_count"
            | "pid"
            | "running"
            | "state"
            | "status"
            | "uptime"
            | "version"
    ) || slot.attribute.starts_with("current_")
}

fn is_runtime_entity(entity: &str) -> bool {
    entity == "process"
        || entity == "sensor"
        || entity == "service"
        || entity.ends_with("_process")
        || entity.ends_with("_sensor")
        || entity.ends_with("_service")
}

fn is_durable_convention(slot: &CanonicalSlot) -> bool {
    matches!(
        slot.attribute.as_str(),
        "branch_naming"
            | "build_command"
            | "convention"
            | "default"
            | "format_command"
            | "layout"
            | "lint_command"
            | "naming"
            | "package_manager"
            | "path_convention"
            | "style"
            | "test_command"
            | "toolchain"
    ) || matches!(slot.entity.as_str(), "codebase" | "project" | "repository")
}

fn default_ttl_days(slot: &CanonicalSlot) -> u32 {
    if slot.entity == "process"
        || slot.entity.ends_with("_process")
        || matches!(slot.attribute.as_str(), "running" | "uptime")
    {
        SHORT_OBSERVED_TTL_DAYS
    } else if slot.entity == "service"
        || slot.entity == "sensor"
        || slot.entity.ends_with("_service")
        || slot.entity.ends_with("_sensor")
        || matches!(slot.attribute.as_str(), "availability" | "status")
    {
        MEDIUM_OBSERVED_TTL_DAYS
    } else {
        DEFAULT_OBSERVED_TTL_DAYS
    }
}

fn days_in_month(year: i64, month: u32) -> Option<u32> {
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 => Some(if leap { 29 } else { 28 }),
        _ => None,
    }
}

fn days_to_ymd(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let day_of_era = (z - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = (year_of_era as i64 + era * 400) as i32;
    let day_of_year =
        (day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100)) as u32;
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/memory/ontology.rs"
    ));
}
