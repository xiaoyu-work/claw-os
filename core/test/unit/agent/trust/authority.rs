use super::*;

#[test]
fn every_class_is_evidence_and_confers_no_authority() {
    for class in TrustClass::ALL {
        let label = class.evidence_label();
        assert_eq!(label, class.wire_tag());
        // The only thing a label can be turned into.
        assert_eq!(authority_of(class), NoAuthority(()));
    }
}

#[test]
fn even_system_policy_confers_no_authority() {
    let policy = LabeledSegment::of(SourceKind::SystemScaffold, "operator rules");
    assert_eq!(authority_of(&policy), NoAuthority(()));
    assert_eq!(authority_of(&TrustClass::SystemPolicy), NoAuthority(()));
}

#[test]
fn segment_evidence_label_names_source_and_class_only() {
    let segment = LabeledSegment::from_locator(SourceKind::McpToolResult, "github", "body");
    assert_eq!(
        segment.evidence_label(),
        "mcp_tool_result:github@untrusted-external"
    );
    assert!(!segment.evidence_label().contains("body"));
}

#[test]
fn no_authority_is_a_zero_sized_dead_end() {
    assert_eq!(std::mem::size_of::<NoAuthority>(), 0);
}

/// Requirement: a trust label must not be convertible into a
/// capability, a role, an approval or a policy decision. Rust cannot
/// assert the *absence* of an impl at runtime, so the crate source is
/// the assertion.
#[test]
fn trust_class_confers_no_authority_conversion_in_the_crate() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    walk(&src, &mut |path, text| {
        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.starts_with("//") || line.starts_with("///") || line.starts_with("*") {
                continue;
            }
            let converts_from_label = (line.contains("impl From<TrustClass>")
                || line.contains("impl TryFrom<TrustClass>")
                || line.contains("impl From<SourceKind>")
                || line.contains("impl TryFrom<SourceKind>")
                || line.contains("impl From<LabeledSegment>")
                || line.contains("impl From<&LabeledSegment>"))
                && !line.contains("for String");
            if converts_from_label {
                offenders.push(format!("{}:{}: {line}", path.display(), index + 1));
            }
        }
    });
    assert!(
        offenders.is_empty(),
        "trust labels must not convert into another type; found:\n{}",
        offenders.join("\n")
    );
}

/// The authority surfaces must not read a trust label at all.
#[test]
fn authority_modules_do_not_read_trust_labels() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let guarded = [
        root.join("caps"),
        root.join("policy.rs"),
        root.join("clawd"),
        root.join("approvals.rs"),
        root.join("agent").join("tools").join("guardrails.rs"),
    ];
    let mut offenders = Vec::new();
    for path in guarded.iter().filter(|path| path.exists()) {
        walk(path, &mut |file, text| {
            for (index, line) in text.lines().enumerate() {
                if line.contains("TrustClass") || line.contains("LabeledSegment") {
                    offenders.push(format!("{}:{}: {}", file.display(), index + 1, line.trim()));
                }
            }
        });
    }
    assert!(
        offenders.is_empty(),
        "capability/policy/approval code must not consume trust labels; found:\n{}",
        offenders.join("\n")
    );
}

fn walk(path: &std::path::Path, visit: &mut impl FnMut(&std::path::Path, &str)) {
    if path.is_file() {
        if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            if let Ok(text) = std::fs::read_to_string(path) {
                visit(path, &text);
            }
        }
        return;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        walk(&entry.path(), visit);
    }
}
