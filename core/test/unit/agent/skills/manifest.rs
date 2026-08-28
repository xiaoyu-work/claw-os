use super::*;

fn doc(input: &str) -> SkillDocument {
    parse(input).expect("parses")
}

#[test]
fn parses_minimal_manifest() {
    let s = doc("---\nname: foo\n---\nbody\n");
    assert_eq!(s.manifest.name, "foo");
    assert_eq!(s.body, "body\n");
    assert!(s.manifest.description.is_none());
}

#[test]
fn parses_full_manifest() {
    let input = "---\nname: my-skill\ndescription: |\n  Multi-line\n  description here\nversion: 1.2.3\nlicense: MIT\nauthor: Xiaoyu Zhang\nhomepage: https://example.com/path#frag\nallowed-tools:\n  - cos_fs\n  - cos_exec\ntriggers: [pdf, \"extract text\"]\n---\n# Body\n";
    let s = doc(input);
    assert_eq!(s.manifest.name, "my-skill");
    assert_eq!(
        s.manifest.description.as_deref(),
        Some("Multi-line\ndescription here")
    );
    assert_eq!(s.manifest.version.as_deref(), Some("1.2.3"));
    assert_eq!(s.manifest.license.as_deref(), Some("MIT"));
    assert_eq!(s.manifest.author.as_deref(), Some("Xiaoyu Zhang"));
    assert_eq!(
        s.manifest.homepage.as_deref(),
        Some("https://example.com/path#frag")
    );
    assert_eq!(s.manifest.allowed_tools, vec!["cos_fs", "cos_exec"]);
    assert_eq!(s.manifest.triggers, vec!["pdf", "extract text"]);
    assert_eq!(s.body, "# Body\n");
}

#[test]
fn missing_frontmatter_errors() {
    let err = parse("# just a markdown\n").unwrap_err();
    assert!(matches!(err, ManifestError::MissingFrontmatter));
}

#[test]
fn unterminated_frontmatter_errors() {
    let err = parse("---\nname: foo\nno closing\n").unwrap_err();
    assert!(matches!(err, ManifestError::UnterminatedFrontmatter));
}

#[test]
fn missing_name_errors() {
    let err = parse("---\ndescription: x\n---\nbody\n").unwrap_err();
    assert!(matches!(err, ManifestError::MissingName));
}

#[test]
fn empty_name_errors() {
    let err = parse("---\nname: \"\"\n---\n").unwrap_err();
    assert!(matches!(err, ManifestError::EmptyName));
}

#[test]
fn duplicate_keys_error() {
    let err = parse("---\nname: a\nname: b\n---\n").unwrap_err();
    assert!(matches!(err, ManifestError::MalformedYaml { .. }));
}

#[test]
fn orphan_indentation_after_scalar_errors() {
    // When a key already has a scalar value, a following indented
    // line has no parent — that's still malformed YAML.
    let err = parse("---\nname: foo\n  inner: x\n---\n").unwrap_err();
    assert!(matches!(err, ManifestError::UnsupportedYaml { .. }));
}

#[test]
fn nested_mapping_under_empty_key_is_tolerated() {
    // Some vendored skill manifests use nested metadata blocks: the
    // parent key has an empty value followed by an indented
    // sub-mapping. Captured verbatim into `extra`.
    let s =
        doc("---\nname: foo\nmetadata:\n  vendor:\n    tags: [a, b]\n    related: [c]\n---\n");
    let m = s.manifest;
    assert_eq!(m.name, "foo");
    let stored = m.extra.get("metadata").expect("metadata preserved");
    match stored {
        ManifestValue::Scalar(v) => {
            assert!(v.contains("vendor:"));
            assert!(v.contains("tags: [a, b]"));
        }
        other => panic!("expected scalar, got {other:?}"),
    }
}

#[test]
fn flow_mapping_is_tolerated_as_scalar() {
    // `meta: {a: 1, b: 2}` round-trips into extras as an
    // opaque scalar — caller can re-parse if it cares.
    let s = doc("---\nname: foo\nmeta: {a: 1, b: 2}\n---\n");
    let stored = s.manifest.extra.get("meta").expect("meta preserved");
    match stored {
        ManifestValue::Scalar(v) => assert!(v.starts_with('{') && v.ends_with('}')),
        other => panic!("expected scalar, got {other:?}"),
    }
}

#[test]
fn block_scalar_pipe_preserves_newlines() {
    let s = doc("---\nname: x\ndescription: |\n  line1\n  line2\n---\n");
    assert_eq!(s.manifest.description.as_deref(), Some("line1\nline2"));
}

#[test]
fn block_scalar_folded_joins_lines() {
    let s = doc("---\nname: x\ndescription: >\n  line1\n  line2\n---\n");
    assert_eq!(s.manifest.description.as_deref(), Some("line1 line2"));
}

#[test]
fn flow_sequence_empty() {
    let s = doc("---\nname: x\nallowed-tools: []\n---\n");
    assert!(s.manifest.allowed_tools.is_empty());
}

#[test]
fn flow_sequence_unterminated_errors() {
    let err = parse("---\nname: x\nallowed-tools: [a, b\n---\n").unwrap_err();
    assert!(matches!(err, ManifestError::MalformedYaml { .. }));
}

#[test]
fn block_sequence_with_quoted_items() {
    let s = doc("---\nname: x\ntriggers:\n  - 'one'\n  - \"two\"\n  - three\n---\n");
    assert_eq!(s.manifest.triggers, vec!["one", "two", "three"]);
}

#[test]
fn comments_are_ignored() {
    let s = doc("---\n# header comment\nname: x  # inline\nversion: 1.0  # ver\n---\n");
    assert_eq!(s.manifest.name, "x");
    assert_eq!(s.manifest.version.as_deref(), Some("1.0"));
}

#[test]
fn url_hash_fragment_not_treated_as_comment() {
    // URLs starting with # immediately after value should keep
    // the fragment. Only ` #` (space + hash) is a comment.
    let s = doc("---\nname: x\nhomepage: https://example.com/#anchor\n---\n");
    assert_eq!(
        s.manifest.homepage.as_deref(),
        Some("https://example.com/#anchor")
    );
}

#[test]
fn double_quoted_escapes_expand() {
    let s = doc("---\nname: x\ndescription: \"line1\\nline2\\ttabbed\"\n---\n");
    assert_eq!(
        s.manifest.description.as_deref(),
        Some("line1\nline2\ttabbed")
    );
}

#[test]
fn single_quoted_escapes_are_literal() {
    let s = doc("---\nname: x\ndescription: 'no \\n escape'\n---\n");
    assert_eq!(s.manifest.description.as_deref(), Some("no \\n escape"));
}

#[test]
fn allowed_tools_underscore_alias() {
    let s = doc("---\nname: x\nallowed_tools: [a, b]\n---\n");
    assert_eq!(s.manifest.allowed_tools, vec!["a", "b"]);
}

#[test]
fn allowed_tools_csv_scalar_form() {
    let s = doc("---\nname: x\nallowed-tools: a, b, c\n---\n");
    assert_eq!(s.manifest.allowed_tools, vec!["a", "b", "c"]);
}

#[test]
fn extra_fields_preserved_as_raw() {
    let s = doc("---\nname: x\ncustom-key: hello\nfeatures:\n  - foo\n  - bar\n---\n");
    match s.manifest.extra.get("custom-key") {
        Some(ManifestValue::Scalar(v)) => assert_eq!(v, "hello"),
        other => panic!("unexpected: {other:?}"),
    }
    match s.manifest.extra.get("features") {
        Some(ManifestValue::Sequence(v)) => assert_eq!(v, &vec!["foo", "bar"]),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn body_starts_after_closing_delimiter() {
    let s = doc("---\nname: x\n---\n# Heading\n\nParagraph.\n");
    assert_eq!(s.body, "# Heading\n\nParagraph.\n");
}

#[test]
fn body_can_be_empty() {
    let s = doc("---\nname: x\n---\n");
    assert!(s.body.is_empty());
}

#[test]
fn handles_crlf_line_endings() {
    let s = doc("---\r\nname: x\r\nversion: 1.0\r\n---\r\nbody\r\n");
    assert_eq!(s.manifest.name, "x");
    assert_eq!(s.manifest.version.as_deref(), Some("1.0"));
}

#[test]
fn handles_bom_prefix() {
    let s = doc("\u{feff}---\nname: x\n---\n");
    assert_eq!(s.manifest.name, "x");
}

#[test]
fn handles_leading_blank_lines() {
    let s = doc("\n\n---\nname: x\n---\n");
    assert_eq!(s.manifest.name, "x");
}

#[test]
fn missing_opening_delimiter_after_blanks_errors() {
    let err = parse("\n\nname: x\n").unwrap_err();
    assert!(matches!(err, ManifestError::MissingFrontmatter));
}

#[test]
fn legacy_signature_frontmatter_is_refused_not_ignored() {
    // A manifest-only signature covered the frontmatter but neither the
    // instruction body nor the skill's scripts. Silently ignoring the
    // key would leave authors believing they had signed something, so
    // the parser refuses and points at the package envelope.
    let pubkey = "1".repeat(64);
    let sig_val = "2".repeat(128);
    let raw = format!(
        "---\nname: signed\nsignature:\n  algorithm: ed25519\n  public_key: {pubkey}\n  value: {sig_val}\n---\n"
    );
    let err = parse(&raw).unwrap_err();
    assert!(matches!(err, ManifestError::LegacySignatureBlock), "{err}");
    assert!(format!("{err}").contains("cos provenance sign"));
}

#[test]
fn manifest_without_a_signature_block_still_parses() {
    let s = doc("---\nname: plain\nversion: 1.0\n---\nbody\n");
    assert_eq!(s.manifest.name, "plain");
    assert!(!s.manifest.extra.contains_key("signature"));
}
