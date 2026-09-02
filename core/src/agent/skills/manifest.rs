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
    #[error("manifest input is {len} bytes; exceeds parser cap {cap} bytes")]
    TooLarge { len: usize, cap: usize },
    #[error(
        "a `signature:` frontmatter block is no longer an authentication \
         mechanism; sign the package with `cos provenance sign --kind skill`"
    )]
    LegacySignatureBlock,
}

/// Hard cap on raw manifest input size — protects the parser from
/// pathological inputs that could exhaust memory or CPU while
/// scanning for delimiters. `loader::LoadOptions::max_manifest_bytes`
/// is the per-call source-of-truth cap; this is a parser-internal
/// fallback that kicks in when the parser is reached via a path
/// that didn't pre-check (e.g. unit tests, downstream callers).
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

/// Parse a SKILL.md document. Returns the typed manifest plus the
/// markdown body that followed the frontmatter.
pub fn parse(input: &str) -> Result<SkillDocument, ManifestError> {
    if input.len() > MAX_MANIFEST_BYTES {
        return Err(ManifestError::TooLarge {
            len: input.len(),
            cap: MAX_MANIFEST_BYTES,
        });
    }
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

    // A `signature:` frontmatter block used to carry a manifest-only
    // ed25519 signature. That scheme covered the manifest fields but
    // neither the skill body, its scripts nor its resources, so it is
    // gone: authentication is the `claw.provenance/v1` package
    // envelope, which binds the whole file tree. Refusing the old key
    // is a loud migration rather than a silently ignored control.
    if raw.contains_key("signature") {
        return Err(ManifestError::LegacySignatureBlock);
    }

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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/skills/manifest.rs"
    ));
}
