//! Structural evidence binding for final agent answers.
//!
//! The model cites runtime tool calls with:
//! `[evidence:<tool_call_id> confidence=<0.00-1.00>]`.
//! This verifier binds each citation to an actual tool result from the
//! in-memory trajectory and records a SHA-256 digest of that exact result.
//! It deliberately does not claim semantic entailment; it proves provenance.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

use crate::agent::llm::{ContentBlock, Message};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceStatus {
    NotRequired,
    Verified,
    Partial,
    Missing,
    Invalid,
}

impl EvidenceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotRequired => "not-required",
            Self::Verified => "verified",
            Self::Partial => "partial",
            Self::Missing => "missing",
            Self::Invalid => "invalid",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceSource {
    pub tool_call_id: String,
    pub tool_name: String,
    pub is_error: bool,
    pub result_sha256: String,
    pub result_bytes: usize,
    pub sequence: usize,
    pub binding_relevant: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceClaim {
    pub claim: String,
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_confidence: Option<f64>,
    pub verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceReport {
    pub status: EvidenceStatus,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding_confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_confidence: Option<f64>,
    pub verified_claims: usize,
    pub total_claims: usize,
    pub sources: Vec<EvidenceSource>,
    pub claims: Vec<EvidenceClaim>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub interpretation: String,
}

impl Default for EvidenceReport {
    fn default() -> Self {
        Self {
            status: EvidenceStatus::NotRequired,
            required: false,
            binding_confidence: None,
            claim_confidence: None,
            verified_claims: 0,
            total_claims: 0,
            sources: Vec::new(),
            claims: Vec::new(),
            warnings: Vec::new(),
            interpretation: "No live-system evidence was required for this answer.".to_string(),
        }
    }
}

#[derive(Default)]
pub struct EvidenceLedger {
    messages: Vec<Message>,
}

impl EvidenceLedger {
    pub fn observe(&mut self, messages: &[Message]) {
        for message in messages {
            let blocks = message
                .content
                .iter()
                .filter(|block| {
                    matches!(
                        block,
                        ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. }
                    )
                })
                .cloned()
                .collect::<Vec<_>>();
            if !blocks.is_empty() {
                self.messages.push(Message {
                    role: message.role,
                    content: blocks,
                });
            }
        }
    }

    pub fn verify(&self, user_prompt: &str, answer: &str) -> EvidenceReport {
        verify_answer(user_prompt, answer, &self.messages)
    }
}

pub fn verify_answer(user_prompt: &str, answer: &str, messages: &[Message]) -> EvidenceReport {
    let (sources, mut warnings) = collect_sources(messages);
    let source_map = sources
        .iter()
        .map(|source| (source.tool_call_id.as_str(), source))
        .collect::<BTreeMap<_, _>>();
    let mut source_aliases = BTreeMap::<&str, Option<&EvidenceSource>>::new();
    for source in &sources {
        let Some((_, suffix)) = source.tool_call_id.split_once("::") else {
            continue;
        };
        if suffix.is_empty() {
            continue;
        }
        match source_aliases.entry(suffix) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Some(source));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                entry.insert(None);
            }
        }
    }
    let mut claims = parse_claims(answer);
    let required = sources.iter().any(|source| source.binding_relevant)
        || looks_like_live_system_request(user_prompt);

    for claim in &mut claims {
        let source = source_map
            .get(claim.tool_call_id.as_str())
            .copied()
            .or_else(|| {
                source_aliases
                    .get(claim.tool_call_id.as_str())
                    .copied()
                    .flatten()
            });
        let Some(source) = source else {
            claim.issue =
                Some("tool call id was not present in this runtime trajectory".to_string());
            continue;
        };
        let Some(confidence) = claim.declared_confidence else {
            claim.issue = Some("citation omitted a numeric confidence".to_string());
            continue;
        };
        if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
            claim.issue = Some("confidence must be between 0.0 and 1.0".to_string());
            continue;
        }
        let effective = if source.is_error {
            confidence.min(0.4)
        } else {
            confidence
        };
        claim.verified = true;
        claim.effective_confidence = Some(round2(effective));
        if source.is_error && confidence > 0.4 {
            claim.issue = Some(
                "tool result was an error; effective confidence was capped at 0.4".to_string(),
            );
        }
    }

    let verified_claims = claims.iter().filter(|claim| claim.verified).count();
    let total_claims = claims.len();
    let (status, binding_confidence, claim_confidence, interpretation) = if total_claims == 0 {
        if required {
            warnings.push(
                "answer has no valid `[evidence:<tool_call_id> confidence=<0..1>]` citation"
                    .to_string(),
            );
            (
                EvidenceStatus::Missing,
                Some(0.0),
                None,
                "Live-system claims are unverified because the answer did not cite runtime evidence."
                    .to_string(),
            )
        } else {
            (
                EvidenceStatus::NotRequired,
                None,
                None,
                "No live-system evidence was required for this answer.".to_string(),
            )
        }
    } else if verified_claims == total_claims {
        let average = claims
            .iter()
            .filter_map(|claim| claim.effective_confidence)
            .sum::<f64>()
            / verified_claims as f64;
        (
            EvidenceStatus::Verified,
            Some(1.0),
            Some(round2(average)),
            "Every evidence citation is bound to an exact runtime tool result. Binding proves provenance, not semantic entailment."
                .to_string(),
        )
    } else if verified_claims > 0 {
        let average = claims
            .iter()
            .filter_map(|claim| claim.effective_confidence)
            .sum::<f64>()
            / verified_claims as f64;
        let coverage = verified_claims as f64 / total_claims as f64;
        warnings.push("one or more evidence citations could not be verified".to_string());
        (
            EvidenceStatus::Partial,
            Some(round2(coverage)),
            Some(round2(average)),
            "Only part of the answer's evidence metadata is bound to runtime tool results."
                .to_string(),
        )
    } else {
        warnings.push("none of the answer's evidence citations could be verified".to_string());
        (
            EvidenceStatus::Invalid,
            Some(0.0),
            None,
            "The answer supplied evidence metadata, but none referenced a valid runtime tool result."
                .to_string(),
        )
    };

    EvidenceReport {
        status,
        required,
        binding_confidence,
        claim_confidence,
        verified_claims,
        total_claims,
        sources,
        claims,
        warnings,
        interpretation,
    }
}

fn collect_sources(messages: &[Message]) -> (Vec<EvidenceSource>, Vec<String>) {
    let mut tools = BTreeMap::<String, (String, serde_json::Value)>::new();
    let mut ambiguous = BTreeSet::<String>::new();
    for message in messages {
        for block in &message.content {
            if let ContentBlock::ToolUse { id, name, input } = block {
                let (name, input) =
                    crate::agent::tools::progressive::resolve_visible_identity(name, input)
                        .unwrap_or_else(|| (name.clone(), input.clone()));
                match tools.get(id) {
                    Some((existing, _)) if existing != &name => {
                        ambiguous.insert(id.clone());
                    }
                    Some(_) => {
                        ambiguous.insert(id.clone());
                    }
                    None => {
                        tools.insert(id.clone(), (name, input));
                    }
                }
            }
        }
    }

    let mut results = BTreeMap::<String, EvidenceSource>::new();
    let mut sequence = 0;
    for message in messages {
        for block in &message.content {
            let ContentBlock::ToolResult {
                tool_use_id,
                is_error,
                content,
            } = block
            else {
                continue;
            };
            sequence += 1;
            if results.contains_key(tool_use_id) {
                ambiguous.insert(tool_use_id.clone());
                continue;
            }
            let Some((tool_name, tool_input)) = tools.get(tool_use_id) else {
                ambiguous.insert(tool_use_id.clone());
                continue;
            };
            results.insert(
                tool_use_id.clone(),
                EvidenceSource {
                    tool_call_id: tool_use_id.clone(),
                    tool_name: tool_name.clone(),
                    is_error: *is_error,
                    result_sha256: hex::encode(Sha256::digest(content.as_bytes())),
                    result_bytes: content.len(),
                    sequence,
                    binding_relevant: tool_requires_binding(tool_name, tool_input),
                },
            );
        }
    }
    for id in &ambiguous {
        results.remove(id);
    }
    let warnings = ambiguous
        .into_iter()
        .map(|id| format!("ambiguous or unpaired tool call id was excluded: {id}"))
        .collect();
    let mut sources = results.into_values().collect::<Vec<_>>();
    sources.sort_by_key(|source| source.sequence);
    (sources, warnings)
}

fn parse_claims(answer: &str) -> Vec<EvidenceClaim> {
    const PREFIX: &str = "[evidence:";
    let mut claims = Vec::new();
    let mut cursor = 0;
    while let Some(relative_start) = answer[cursor..].find(PREFIX) {
        let start = cursor + relative_start;
        let Some(relative_end) = answer[start..].find(']') else {
            break;
        };
        let end = start + relative_end;
        let body = &answer[start + PREFIX.len()..end];
        let mut parts = body.split_whitespace();
        let call_id = parts
            .next()
            .unwrap_or_default()
            .trim_end_matches([',', ';'])
            .to_string();
        let declared_confidence = parts.find_map(parse_confidence);
        let claim = claim_before_marker(answer, start);
        claims.push(EvidenceClaim {
            claim,
            tool_call_id: call_id,
            declared_confidence,
            verified: false,
            effective_confidence: None,
            issue: None,
        });
        cursor = end + 1;
    }
    claims
}

fn parse_confidence(value: &str) -> Option<f64> {
    value
        .strip_prefix("confidence=")
        .or_else(|| value.strip_prefix("confidence:"))
        .and_then(|raw| raw.trim_end_matches([',', ';']).parse::<f64>().ok())
}

fn claim_before_marker(answer: &str, marker_start: usize) -> String {
    let before = &answer[..marker_start];
    let current_line = before.rsplit('\n').next().unwrap_or_default().trim();
    let candidate = if current_line.is_empty() {
        before
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .trim()
    } else {
        current_line
    };
    strip_markers(candidate)
        .trim_start_matches(['-', '*', ' ', '\t'])
        .chars()
        .take(1024)
        .collect()
}

pub(crate) fn strip_markers(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(relative_start) = value[cursor..].find("[evidence:") {
        let start = cursor + relative_start;
        output.push_str(&value[cursor..start]);
        while output.ends_with([' ', '\t']) {
            output.pop();
        }
        let Some(relative_end) = value[start..].find(']') else {
            cursor = value.len();
            break;
        };
        cursor = start + relative_end + 1;
    }
    if cursor < value.len() {
        output.push_str(&value[cursor..]);
    }
    output.trim().to_string()
}

fn looks_like_live_system_request(prompt: &str) -> bool {
    let text = prompt.to_lowercase();
    [
        "my computer",
        "my system",
        "this computer",
        "this system",
        "current system",
        "current cpu",
        "current memory",
        "current disk",
        "current network",
        "current service",
        "why is my",
        "本机",
        "我的电脑",
        "当前系统",
        "当前 cpu",
        "当前内存",
        "当前磁盘",
        "当前网络",
        "现在我的",
        "为什么我的",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn tool_requires_binding(name: &str, input: &serde_json::Value) -> bool {
    if matches!(
        name,
        "cos_diagnose" | "cos_sysinfo" | "cos_doctor" | "cos_usage"
    ) {
        return true;
    }
    if name.starts_with("app_")
        || (name == "cos_tool_call"
            && input
                .get("name")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|target| target.starts_with("app_")))
    {
        return true;
    }
    let command = input
        .get("command")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let read_commands: &[&str] = match name {
        "cos_proc" => &["status", "output", "list", "wait", "result", "stats"],
        "cos_service" => &["status", "health", "list", "logs"],
        "cos_browser" => &["status", "health"],
        "cos_netfilter" => &["list", "check", "export", "rate-limits"],
        "cos_cron" => &["list", "status", "logs"],
        "cos_checkpoint" => &["diff", "list", "status", "quota-status", "namespaces"],
        "cos_credential" => &["list"],
        "cos_trace" => &["show", "list"],
        "cos_watch" => &["history"],
        "cos_ipc" => &["list", "locks"],
        "cos_model" => &["list", "status", "info"],
        _ => &[],
    };
    if read_commands.contains(&command) {
        return true;
    }
    false
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/runtime/evidence.rs"
    ));
}
