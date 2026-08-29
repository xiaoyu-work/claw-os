// Model-input builder coverage.
//
// Two properties, both enforced by inventorying the crate source
// because Rust cannot assert the *absence* of a construction at
// runtime:
//
// 1. Every place that builds a provider `Message` or a `system` string
//    is either a labelled path (it goes through `PromptProjection` /
//    `LabeledSegment`) or an explicitly reviewed exception.
// 2. Nothing in the capability, policy, approval or guardrail surfaces
//    reads a trust label or free model text.

use super::*;

/// Files allowed to construct a provider message directly.
///
/// Each entry is reviewed: it either builds a request that carries no
/// untrusted content at all (a probe, a scripted mock), or it is the
/// labelled builder itself.
const LABELLED_OR_REVIEWED: &[&str] = &[
    // The labelled builders themselves.
    "agent/trust/projection.rs",
    "agent/trust/segment.rs",
    // Runtime request assembly: goes through PromptProjection.
    "agent/runtime/loop_.rs",
    "agent/runtime/turn.rs",
    // Model-authored summaries; labelled ModelGenerated with lineage.
    "agent/context/compressor.rs",
    // Fixed-text probes and auxiliary calls that embed no ingested
    // content in the system channel.
    "agent/provider_commands.rs",
    "agent/conversation_commands.rs",
    "agent/curator_author.rs",
    "agent/llm/auxiliary.rs",
    "agent/media/vision/analyze.rs",
    // The App AI gate builds its own bounded request from an App
    // prompt; the gate, not the trust module, owns that contract.
    "ai/gate.rs",
    // Type definitions and provider adapters project what they are
    // given; they never originate content.
    "agent/llm/types.rs",
    "agent/llm/accumulate.rs",
];

fn rust_sources(root: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn relative(path: &std::path::Path) -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    path.strip_prefix(&root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn is_code(line: &str) -> bool {
    let line = line.trim_start();
    !(line.starts_with("//") || line.starts_with("*") || line.starts_with("#["))
}

/// Requirement: a new call site that pushes a raw string into a
/// provider message has to be reviewed and added here, which is the
/// moment to ask whether it should be labelled instead.
#[test]
fn every_provider_message_construction_is_labelled_or_reviewed() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&root, &mut files);

    let mut unreviewed = Vec::new();
    for file in &files {
        let name = relative(file);
        if LABELLED_OR_REVIEWED.iter().any(|ok| name.ends_with(ok)) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        // A module that declares its own `ChatRequest` (the web API
        // body, the App AI gate request) is a different type entirely.
        let declares_own_request = text.contains("pub struct ChatRequest");
        for (index, line) in text.lines().enumerate() {
            if !is_code(line) {
                continue;
            }
            let builds_request = if declares_own_request {
                line.contains("llm::ChatRequest {")
            } else {
                // A qualified path other than `llm::` names another
                // crate's request type.
                line.contains("llm::ChatRequest {")
                    || (line.contains("ChatRequest {") && !line.contains("::ChatRequest {"))
            };
            let constructs = line.contains("Message::user_text(")
                || line.contains("Message::system_text(")
                || line.contains("Message::assistant_text(")
                || line.contains("llm::Message {")
                || builds_request;
            if constructs {
                unreviewed.push(format!("{name}:{}: {}", index + 1, line.trim()));
            }
        }
    }
    assert!(
        unreviewed.is_empty(),
        "unreviewed model-input construction; label it through \
         PromptProjection/LabeledSegment or add the file to \
         LABELLED_OR_REVIEWED after review:\n{}",
        unreviewed.join("\n")
    );
}

/// The system channel may only ever be handed policy text.
///
/// Any assignment of `system:` from something other than the
/// projection, a fixed literal, or `None` is a review point.
#[test]
fn no_module_builds_system_content_from_an_ingested_string() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&root, &mut files);

    let ingestion_markers = [
        "wrap_labeled",
        "wrap_untrusted",
        "tool_result",
        "render_call_result",
        "descriptor.description",
        "page_text",
        "transcript",
    ];
    let mut offenders = Vec::new();
    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        let name = relative(file);
        for (index, line) in text.lines().enumerate() {
            if !is_code(line) || !line.contains("system:") {
                continue;
            }
            if ingestion_markers.iter().any(|marker| line.contains(marker)) {
                offenders.push(format!("{name}:{}: {}", index + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "ingested content assigned to the system channel:\n{}",
        offenders.join("\n")
    );
}

/// Every registry source must be reachable from a labelled builder.
#[test]
fn every_source_kind_has_a_reachable_builder() {
    for kind in SourceKind::ALL {
        let segment = LabeledSegment::of(*kind, "payload");
        assert_eq!(segment.class(), kind.class());
        // A projection routes it without the caller choosing a channel.
        let mut projection = PromptProjection::new();
        projection.push(segment);
        assert!(
            projection.channels_are_separated(),
            "{kind} broke channel separation"
        );
    }
}

/// An unknown tag never resolves to a trusted source, so a stored row
/// naming a source this binary does not know stays refused.
#[test]
fn unknown_sources_default_to_legacy_unknown() {
    for tag in ["", "not_a_source", "system_scaffold_v2", "SYSTEM_SCAFFOLD"] {
        let kind = SourceKind::from_tag(tag);
        assert_eq!(kind, SourceKind::Unknown, "tag {tag:?}");
        assert_eq!(kind.class(), TrustClass::LegacyUnknown);
        assert!(!kind.class().is_policy());
    }
}
