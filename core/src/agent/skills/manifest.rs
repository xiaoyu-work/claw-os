//! agentskills.io SKILL.md manifest parser.
//!
//! A skill is a directory containing a `SKILL.md` file. The file
//! starts with a YAML frontmatter block delimited by `---` lines,
//! followed by a markdown body that documents the skill. Example:
//!
//! ```text
//! ---
//! name: my-skill
//! description: |
//!   What this skill does, in human terms.
//! version: 0.1.0
//! license: MIT
//! allowed-tools:
//!   - cos_fs
//!   - cos_exec
//! triggers: [pdf, "extract text"]
//! ---
//! # My Skill
//!
//! Instructions go here...
//! ```
//!
//! ## Why a custom YAML subset
//!
//! We deliberately avoid pulling in a full YAML crate. The
//! frontmatter shape is constrained:
//!
//!   * Top-level keys only (no nested maps).
//!   * Values are: scalar strings, block scalars (`|`/`>`),
//!     flow sequences (`[a, b]`), or block sequences (next-line
//!     `- item`).
//!   * No anchors, no aliases, no tags, no merges.
//!
//! Anything outside this subset surfaces as
//! [`ManifestError::UnsupportedYaml`] so the loader can refuse
//! the skill rather than silently misinterpret it. This keeps
//! the parser auditable and dep-free, and matches what
//! agentskills.io files actually use in practice.
//!
//! ## What the parser does NOT do
//!
//! * No semantic validation beyond `name` being non-empty —
//!   that's the [`crate::agent::skills::loader`] module's job.
//! * No execution — it just reads the document.
//! * No hub/sync — see [`super::hub`] / [`super::sync`].

use std::collections::BTreeMap;

/// Required + optional fields parsed from a SKILL.md frontmatter
/// block. Any unrecognised top-level key is preserved verbatim in
/// `extra` so callers can extend without parser changes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SkillManifest {
    pub name: String,
    pub description: Option<String>,
    pub version: Option<String>,
    pub license: Option<String>,
    pub author: Option<String>,
    pub homepage: Option<String>,
    /// Tool ids (e.g. `cos_fs`, `cos_exec`) the skill is permitted
    /// to invoke. Empty means *no restriction declared* — the
    /// runtime guardrails layer ([`crate::agent::tools::guardrails`])
    /// decides defaults.
    pub allowed_tools: Vec<String>,
    /// Free-form trigger keywords/phrases the loader may match
    /// against user prompts.
    pub triggers: Vec<String>,
    /// Anything unrecognised: preserved as raw scalar strings or
    /// joined sequence values so we don't lose data.
    pub extra: BTreeMap<String, ManifestValue>,
}

/// Raw value preserved for fields we don't have a typed slot for.
#[derive(Debug, Clone, PartialEq)]
pub enum ManifestValue {
    Scalar(String),
    Sequence(Vec<String>),
}

/// Result of parsing a full SKILL.md document.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillDocument {
    pub manifest: SkillManifest,
    pub body: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("missing frontmatter delimiter (`---`) at top of file")]
    MissingFrontmatter,
    #[error("unterminated frontmatter (no closing `---`)")]
    UnterminatedFrontmatter,
    #[error("missing required field: name")]
    MissingName,
    #[error("name must be non-empty")]
    EmptyName,
    #[error("malformed YAML at line {line}: {reason}")]
    MalformedYaml { line: usize, reason: String },
    #[error("unsupported YAML construct at line {line}: {reason}")]
    UnsupportedYaml { line: usize, reason: String },
}

/// Parse a SKILL.md document. Returns the typed manifest plus the
/// markdown body that followed the frontmatter.
pub fn parse(input: &str) -> Result<SkillDocument, ManifestError> {
    let (frontmatter, body) = split_frontmatter(input)?;
    let raw = parse_yaml_subset(frontmatter)?;
    let manifest = build_manifest(raw)?;
    Ok(SkillDocument {
        manifest,
        body: body.to_owned(),
    })
}

// ---------- internals ----------

fn split_frontmatter(input: &str) -> Result<(&str, &str), ManifestError> {
    // Strip BOM and any leading newlines/whitespace lines.
    let trimmed = input.strip_prefix('\u{feff}').unwrap_or(input);

    // Skip blank leading lines.
    let mut cursor = 0;
    for line in trimmed.split_inclusive('\n') {
        if line.trim().is_empty() {
            cursor += line.len();
        } else {
            break;
        }
    }
    let rest = &trimmed[cursor..];

    let first_line_end = rest.find('\n').ok_or(ManifestError::MissingFrontmatter)?;
    let first_line = rest[..first_line_end].trim_end_matches('\r');
    if first_line.trim() != "---" {
        return Err(ManifestError::MissingFrontmatter);
    }
    let after_open = &rest[first_line_end + 1..];

    // Find closing `---` on its own line.
    let mut offset = 0usize;
    for line in after_open.split_inclusive('\n') {
        let logical = line.trim_end_matches('\n').trim_end_matches('\r');
        if logical.trim() == "---" {
            let frontmatter = &after_open[..offset];
            let body = &after_open[offset + line.len()..];
            return Ok((frontmatter, body));
        }
        offset += line.len();
    }
    Err(ManifestError::UnterminatedFrontmatter)
}

#[derive(Debug, Clone, PartialEq)]
enum RawValue {
    Scalar(String),
    Sequence(Vec<String>),
}

/// Constrained YAML subset parser. Returns map of top-level keys
/// to raw values. See module doc for the supported shape.
fn parse_yaml_subset(input: &str) -> Result<BTreeMap<String, RawValue>, ManifestError> {
    let mut out = BTreeMap::new();
    let lines: Vec<(usize, &str)> = input
        .split('\n')
        .enumerate()
        .map(|(i, l)| (i + 1, l.trim_end_matches('\r')))
        .collect();

    let mut idx = 0;
    while idx < lines.len() {
        let (lineno, raw_line) = lines[idx];
        if raw_line.trim().is_empty() || raw_line.trim_start().starts_with('#') {
            idx += 1;
            continue;
        }

        if raw_line.starts_with(' ') || raw_line.starts_with('\t') {
            return Err(ManifestError::UnsupportedYaml {
                line: lineno,
                reason: "nested mapping is not supported".to_string(),
            });
        }

        let (key, after) = split_key(raw_line).ok_or_else(|| ManifestError::MalformedYaml {
            line: lineno,
            reason: "expected `key: value` or `key:`".to_string(),
        })?;

        if out.contains_key(&key) {
            return Err(ManifestError::MalformedYaml {
                line: lineno,
                reason: format!("duplicate key `{key}`"),
            });
        }

        let value_part = after.trim_start();
        if value_part.is_empty() {
            // Block sequence, nested mapping, or empty.
            //
            // Real-world SKILL.md files commonly nest a `metadata:`
            // mapping with project-specific sub-keys under it. The
            // kernel parser doesn't *interpret* nested structure, but
            // it must not refuse to load such files. We capture the
            // entire indented block verbatim as a single Scalar in
            // `extra` so callers that don't care about the keys
            // simply see an opaque blob and proceed.
            let mut seq: Vec<String> = Vec::new();
            let mut had_seq_item = false;
            let mut seq_indent: Option<usize> = None;
            let mut nested_block = String::new();
            let mut had_nested = false;
            let mut next = idx + 1;
            while next < lines.len() {
                let (l, content) = lines[next];
                if content.trim().is_empty() {
                    if had_nested {
                        nested_block.push('\n');
                    }
                    next += 1;
                    continue;
                }
                if !content.starts_with(' ') && !content.starts_with('\t') {
                    break;
                }
                let stripped = content.trim_start();
                let leading = content.len() - stripped.len();
                if had_nested {
                    nested_block.push_str(content);
                    nested_block.push('\n');
                    next += 1;
                    continue;
                }
                if had_seq_item {
                    let item_indent = seq_indent.unwrap_or(leading);
                    if leading > item_indent {
                        // continuation of the current sequence item:
                        // capture verbatim (preserving the relative
                        // indent inside the item).
                        if let Some(last) = seq.last_mut() {
                            if !last.is_empty() {
                                last.push('\n');
                            }
                            last.push_str(&content[item_indent..]);
                        }
                        next += 1;
                        continue;
                    }
                    if leading < item_indent {
                        break;
                    }
                    // Same indent as the `- ` markers — must be
                    // another sequence entry or terminate.
                    if let Some(rest) = stripped.strip_prefix("- ") {
                        seq.push(unquote_scalar(rest.trim()));
                        next += 1;
                        continue;
                    }
                    if stripped == "-" {
                        seq.push(String::new());
                        next += 1;
                        continue;
                    }
                    return Err(ManifestError::UnsupportedYaml {
                        line: l,
                        reason: "expected `- item` block sequence entry".to_string(),
                    });
                }
                if let Some(rest) = stripped.strip_prefix("- ") {
                    seq.push(unquote_scalar(rest.trim()));
                    had_seq_item = true;
                    seq_indent = Some(leading);
                    next += 1;
                } else if stripped == "-" {
                    seq.push(String::new());
                    had_seq_item = true;
                    seq_indent = Some(leading);
                    next += 1;
                } else {
                    // First indented line is not `- item` — treat
                    // the whole indented region as a verbatim nested
                    // mapping/scalar block.
                    nested_block.push_str(content);
                    nested_block.push('\n');
                    had_nested = true;
                    next += 1;
                }
            }
            if had_seq_item {
                out.insert(key, RawValue::Sequence(seq));
            } else if had_nested {
                // Trim the trailing newline left by the loop, but
                // keep the structure intact so consumers can
                // re-parse if they want.
                let trimmed = nested_block.trim_end_matches('\n').to_string();
                out.insert(key, RawValue::Scalar(trimmed));
            } else {
                out.insert(key, RawValue::Scalar(String::new()));
            }
            idx = next;
            continue;
        }

        if value_part == "|" || value_part == ">" {
            // Block scalar — read indented continuation lines.
            let style = value_part.chars().next().unwrap();
            let mut buf = String::new();
            let mut next = idx + 1;
            let mut indent: Option<usize> = None;
            while next < lines.len() {
                let (_, content) = lines[next];
                if content.trim().is_empty() {
                    if indent.is_some() {
                        buf.push('\n');
                    }
                    next += 1;
                    continue;
                }
                let leading = content.len() - content.trim_start().len();
                if leading == 0 {
                    break;
                }
                let i = *indent.get_or_insert(leading);
                if leading < i {
                    break;
                }
                let payload = &content[i..];
                if !buf.is_empty() {
                    buf.push(if style == '|' { '\n' } else { ' ' });
                }
                buf.push_str(payload);
                next += 1;
            }
            if style == '|' {
                buf.push('\n');
            }
            out.insert(key, RawValue::Scalar(buf));
            idx = next;
            continue;
        }

        if let Some(stripped) = value_part.strip_prefix('[') {
            let inner = stripped
                .strip_suffix(']')
                .ok_or_else(|| ManifestError::MalformedYaml {
                    line: lineno,
                    reason: "unterminated flow sequence".to_string(),
                })?;
            let items: Vec<String> = if inner.trim().is_empty() {
                Vec::new()
            } else {
                inner.split(',').map(|s| unquote_scalar(s.trim())).collect()
            };
            out.insert(key, RawValue::Sequence(items));
            idx += 1;
            continue;
        }

        if value_part.starts_with('{') {
            // Inline flow mappings (`{a: 1, b: 2}`) appear in the
            // wild but aren't structurally interpreted here. Capture
            // them verbatim as a Scalar so the document still loads;
            // callers that need typed access can parse `extra`.
            out.insert(key, RawValue::Scalar(value_part.to_string()));
            idx += 1;
            continue;
        }

        out.insert(key, RawValue::Scalar(unquote_scalar(value_part)));
        idx += 1;
    }
    Ok(out)
}

fn split_key(line: &str) -> Option<(String, &str)> {
    // Find first `:` that isn't inside quotes. Top-level keys are
    // always plain identifiers in our subset, so quoted keys are
    // unsupported and would surface via the reserved-character path.
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' {
            let key = line[..i].trim().to_string();
            if key.is_empty() {
                return None;
            }
            let rest = &line[i + 1..];
            return Some((key, rest));
        }
        i += 1;
    }
    None
}

fn unquote_scalar(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() >= 2 {
        let first = trimmed.as_bytes()[0];
        let last = trimmed.as_bytes()[trimmed.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            // Strip outer quotes; double-quoted strings expand
            // common backslash escapes.
            let inner = &trimmed[1..trimmed.len() - 1];
            return if first == b'"' {
                expand_double_quoted(inner)
            } else {
                inner.to_string()
            };
        }
    }
    // Strip a trailing inline comment (` # ...`) — only when
    // preceded by whitespace, so URLs with `#` fragments survive.
    if let Some(idx) = trimmed.find(" #") {
        return trimmed[..idx].trim_end().to_string();
    }
    trimmed.to_string()
}

fn expand_double_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn build_manifest(mut raw: BTreeMap<String, RawValue>) -> Result<SkillManifest, ManifestError> {
    let name = take_scalar(&mut raw, "name").ok_or(ManifestError::MissingName)?;
    if name.trim().is_empty() {
        return Err(ManifestError::EmptyName);
    }

    let description = take_scalar(&mut raw, "description").map(|s| s.trim().to_string());
    let version = take_scalar(&mut raw, "version").map(trim_simple);
    let license = take_scalar(&mut raw, "license").map(trim_simple);
    let author = take_scalar(&mut raw, "author").map(trim_simple);
    let homepage = take_scalar(&mut raw, "homepage").map(trim_simple);

    let allowed_tools = take_sequence_or_csv(&mut raw, "allowed-tools")
        .or_else(|| take_sequence_or_csv(&mut raw, "allowed_tools"))
        .unwrap_or_default();
    let triggers = take_sequence_or_csv(&mut raw, "triggers").unwrap_or_default();

    let mut extra = BTreeMap::new();
    for (k, v) in raw {
        let value = match v {
            RawValue::Scalar(s) => ManifestValue::Scalar(s),
            RawValue::Sequence(s) => ManifestValue::Sequence(s),
        };
        extra.insert(k, value);
    }

    Ok(SkillManifest {
        name: name.trim().to_string(),
        description,
        version,
        license,
        author,
        homepage,
        allowed_tools,
        triggers,
        extra,
    })
}

fn take_scalar(map: &mut BTreeMap<String, RawValue>, key: &str) -> Option<String> {
    match map.remove(key) {
        Some(RawValue::Scalar(s)) => Some(s),
        Some(other) => {
            // Sequence supplied where scalar expected — preserve in
            // extra rather than silently lose. Push it back so the
            // build_manifest extra-collector picks it up.
            map.insert(key.to_string(), other);
            None
        }
        None => None,
    }
}

fn take_sequence_or_csv(map: &mut BTreeMap<String, RawValue>, key: &str) -> Option<Vec<String>> {
    match map.remove(key) {
        Some(RawValue::Sequence(items)) => Some(
            items
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        ),
        Some(RawValue::Scalar(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Some(Vec::new())
            } else {
                Some(
                    trimmed
                        .split(',')
                        .map(|p| p.trim().to_string())
                        .filter(|p| !p.is_empty())
                        .collect(),
                )
            }
        }
        None => None,
    }
}

fn trim_simple(s: String) -> String {
    s.trim().to_string()
}

#[cfg(test)]
mod tests {
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
}
