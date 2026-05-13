//! Dependency-vulnerability lookup against the public [osv.dev] API.
//!
//! Two-stage design:
//!
//! 1. **Pure parsers** that pull `(ecosystem, name, version)` triples out
//!    of common lockfile formats (Cargo.lock, package-lock.json,
//!    requirements.txt, pip's `==` pin syntax, go.sum). No I/O, no
//!    network — fully unit-testable.
//! 2. **`query` / `query_batch`** that POST to
//!    `https://api.osv.dev/v1/query` (or `v1/querybatch`) and parse the
//!    JSON response into `OsvVulnerability` records. Network-bound,
//!    exercised by the CLI but skipped in cfg(test).
//!
//! The CLI wraps this as `cos agent osv parse <file>` (offline) and
//! `cos agent osv check <file>` (offline parse + online query).
//!
//! [osv.dev]: https://osv.dev/docs/

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

/// One package coordinate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub ecosystem: String,
    pub name: String,
    pub version: String,
}

impl Package {
    pub fn new(
        ecosystem: impl Into<String>,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self {
            ecosystem: ecosystem.into(),
            name: name.into(),
            version: version.into(),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "ecosystem": self.ecosystem,
            "name":      self.name,
            "version":   self.version,
        })
    }
}

/// One OSV vulnerability record (subset of the schema we surface to
/// callers — the full schema is huge).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct OsvVulnerability {
    pub id: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub modified: Option<String>,
    #[serde(default)]
    pub published: Option<String>,
    #[serde(default)]
    pub references: Vec<OsvReference>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct OsvReference {
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub url: String,
}

impl OsvVulnerability {
    pub fn to_json(&self) -> Value {
        json!({
            "id":        self.id,
            "summary":   self.summary,
            "aliases":   self.aliases,
            "modified":  self.modified,
            "published": self.published,
            "references": self.references.iter().map(|r| json!({
                "type": r.kind,
                "url":  r.url,
            })).collect::<Vec<_>>(),
        })
    }
}

const OSV_QUERY_URL: &str = "https://api.osv.dev/v1/query";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Query osv.dev for vulnerabilities affecting one package version.
///
/// Returns `Ok(vec![])` if osv.dev returned no `vulns` for the package.
pub async fn query(pkg: &Package) -> Result<Vec<OsvVulnerability>, String> {
    query_with_url(pkg, OSV_QUERY_URL, DEFAULT_TIMEOUT).await
}

/// Same as [`query`], but with explicit URL and timeout (for tests
/// against a mock server).
pub async fn query_with_url(
    pkg: &Package,
    url: &str,
    timeout: Duration,
) -> Result<Vec<OsvVulnerability>, String> {
    let body = json!({
        "version": pkg.version,
        "package": {
            "name":      pkg.name,
            "ecosystem": pkg.ecosystem,
        }
    });
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(format!("cos-agent/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("osv: build client: {e}"))?;
    let resp = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("osv: POST {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "osv: {} returned HTTP {}",
            url,
            resp.status().as_u16()
        ));
    }
    let value: Value = resp
        .json()
        .await
        .map_err(|e| format!("osv: response not JSON: {e}"))?;
    parse_query_response(&value)
}

/// Parse an osv.dev `/v1/query` response payload into a vulnerability
/// list. Returns `Ok(vec![])` if `vulns` is missing or empty (a
/// "no known vulnerabilities" reply from osv.dev).
pub fn parse_query_response(payload: &Value) -> Result<Vec<OsvVulnerability>, String> {
    let arr = match payload.get("vulns") {
        Some(Value::Array(arr)) => arr,
        Some(Value::Null) | None => return Ok(vec![]),
        Some(other) => {
            return Err(format!(
                "osv: 'vulns' must be array, got {}",
                discriminant_str(other)
            ));
        }
    };
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let parsed: OsvVulnerability =
            serde_json::from_value(v.clone()).map_err(|e| format!("osv: vuln payload: {e}"))?;
        out.push(parsed);
    }
    Ok(out)
}

fn discriminant_str(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// ---------------------------------------------------------------------
// Lockfile parsers (pure)
// ---------------------------------------------------------------------

/// Parse a lockfile by filename. Recognised:
///
/// - `Cargo.lock` → `crates.io` ecosystem.
/// - `package-lock.json` → `npm` ecosystem.
/// - `requirements.txt` (pip pinned) → `PyPI` ecosystem.
/// - `go.sum` → `Go` ecosystem.
///
/// Unknown filenames return `Err`.
pub fn parse_lockfile(path: &Path, body: &str) -> Result<Vec<Package>, String> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("osv: invalid lockfile path: {}", path.display()))?
        .to_ascii_lowercase();
    match name.as_str() {
        "cargo.lock" => parse_cargo_lock(body),
        "package-lock.json" => parse_package_lock_json(body),
        "requirements.txt" => Ok(parse_requirements_txt(body)),
        "go.sum" => Ok(parse_go_sum(body)),
        other => Err(format!("osv: unknown lockfile: {other}")),
    }
}

/// Cargo.lock — TOML `[[package]]` blocks with `name = "..."` and
/// `version = "..."`. We use a tiny line-based scanner instead of
/// pulling in a `toml` crate dep.
pub fn parse_cargo_lock(body: &str) -> Result<Vec<Package>, String> {
    let mut out = Vec::new();
    let mut in_pkg = false;
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;

    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed == "[[package]]" {
            // Flush previous block if it was complete.
            if let (Some(n), Some(v)) = (name.take(), version.take()) {
                out.push(Package::new("crates.io", n, v));
            }
            in_pkg = true;
            name = None;
            version = None;
            continue;
        }
        if trimmed.starts_with('[') {
            // Different table header → flush.
            if let (Some(n), Some(v)) = (name.take(), version.take()) {
                out.push(Package::new("crates.io", n, v));
            }
            in_pkg = false;
            continue;
        }
        if !in_pkg {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("name") {
            if let Some(val) = parse_toml_string_value(rest) {
                name = Some(val);
            }
        } else if let Some(rest) = trimmed.strip_prefix("version") {
            if let Some(val) = parse_toml_string_value(rest) {
                version = Some(val);
            }
        }
    }
    if let (Some(n), Some(v)) = (name, version) {
        out.push(Package::new("crates.io", n, v));
    }
    Ok(out)
}

fn parse_toml_string_value(rest: &str) -> Option<String> {
    // rest = " = \"value\"" possibly with trailing comment
    let after_eq = rest.split_once('=')?.1.trim();
    // Strip a possible comment.
    let no_comment = match after_eq.split_once('#') {
        Some((before, _)) => before.trim(),
        None => after_eq,
    };
    let unquoted = no_comment.strip_prefix('"')?.strip_suffix('"')?;
    Some(unquoted.to_string())
}

/// package-lock.json — supports both lockfileVersion 1 (`dependencies`)
/// and 2/3 (`packages` keyed by `node_modules/<name>`).
pub fn parse_package_lock_json(body: &str) -> Result<Vec<Package>, String> {
    let v: Value =
        serde_json::from_str(body).map_err(|e| format!("osv: package-lock json: {e}"))?;
    let mut out = Vec::new();
    if let Some(packages) = v.get("packages").and_then(|p| p.as_object()) {
        for (key, val) in packages {
            // Skip the root entry (empty key).
            if key.is_empty() {
                continue;
            }
            let name = match key.rsplit_once("node_modules/") {
                Some((_, n)) => n.to_string(),
                None => continue,
            };
            if name.is_empty() {
                continue;
            }
            if let Some(version) = val.get("version").and_then(|v| v.as_str()) {
                out.push(Package::new("npm", name, version));
            }
        }
    }
    if out.is_empty() {
        if let Some(deps) = v.get("dependencies").and_then(|d| d.as_object()) {
            collect_npm_deps_v1(deps, &mut out);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.version.cmp(&b.version)));
    out.dedup_by(|a, b| a.name == b.name && a.version == b.version);
    Ok(out)
}

fn collect_npm_deps_v1(deps: &serde_json::Map<String, Value>, out: &mut Vec<Package>) {
    for (name, val) in deps {
        if let Some(version) = val.get("version").and_then(|v| v.as_str()) {
            out.push(Package::new("npm", name.clone(), version.to_string()));
        }
        if let Some(nested) = val.get("dependencies").and_then(|d| d.as_object()) {
            collect_npm_deps_v1(nested, out);
        }
    }
}

/// requirements.txt — only honour exact `name==version` pins (which is
/// what OSV needs anyway). Loose specifiers (`>=`, `~=`, `<`, etc.)
/// are ignored.
pub fn parse_requirements_txt(body: &str) -> Vec<Package> {
    let mut out = Vec::new();
    for raw in body.lines() {
        let line = match raw.split_once('#') {
            Some((before, _)) => before.trim(),
            None => raw.trim(),
        };
        if line.is_empty() || line.starts_with('-') {
            continue;
        }
        // We want strict `name==version`. Reject markers (`;`) suffixes.
        let core = match line.split_once(';') {
            Some((before, _)) => before.trim(),
            None => line,
        };
        let Some((name, version)) = core.split_once("==") else {
            continue;
        };
        // Strip extras: `name[extra]==1.2.3`.
        let name = name.split_once('[').map(|(n, _)| n).unwrap_or(name).trim();
        let version = version.trim();
        if name.is_empty() || version.is_empty() {
            continue;
        }
        // Reject obviously-non-version tokens.
        if version.contains([' ', '\t']) {
            continue;
        }
        out.push(Package::new("PyPI", name, version));
    }
    out
}

/// go.sum — lines look like `<module> <version>[/go.mod] <hash>`.
/// Take the first two fields, strip `/go.mod` if present, dedupe.
pub fn parse_go_sum(body: &str) -> Vec<Package> {
    let mut seen: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();
    for raw in body.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(name) = parts.next() else { continue };
        let Some(mut version) = parts.next() else {
            continue;
        };
        if let Some(stripped) = version.strip_suffix("/go.mod") {
            version = stripped;
        }
        if name.is_empty() || version.is_empty() {
            continue;
        }
        seen.insert((name.to_string(), version.to_string()));
    }
    seen.into_iter()
        .map(|(n, v)| Package::new("Go", n, v))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Cargo.lock ----

    #[test]
    fn cargo_lock_parses_two_packages() {
        let lock = r#"
# This file is automatically @generated by Cargo.
version = 4

[[package]]
name = "serde"
version = "1.0.219"

[[package]]
name = "tokio"
version = "1.43.0"
dependencies = ["bytes"]
"#;
        let pkgs = parse_cargo_lock(lock).expect("ok");
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[0], Package::new("crates.io", "serde", "1.0.219"));
        assert_eq!(pkgs[1], Package::new("crates.io", "tokio", "1.43.0"));
    }

    #[test]
    fn cargo_lock_ignores_metadata_block() {
        let lock = r#"[[package]]
name = "foo"
version = "0.1.0"

[metadata]
foo = "bar"
"#;
        let pkgs = parse_cargo_lock(lock).expect("ok");
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "foo");
    }

    #[test]
    fn cargo_lock_handles_trailing_comment_in_value() {
        let lock = r#"[[package]]
name = "foo" # comment
version = "0.1.0" # other
"#;
        let pkgs = parse_cargo_lock(lock).expect("ok");
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "foo");
        assert_eq!(pkgs[0].version, "0.1.0");
    }

    #[test]
    fn cargo_lock_empty_returns_empty() {
        let pkgs = parse_cargo_lock("").expect("ok");
        assert!(pkgs.is_empty());
    }

    // ---- package-lock.json ----

    #[test]
    fn package_lock_v3_packages_format() {
        let body = r#"{
  "name": "myapp",
  "lockfileVersion": 3,
  "packages": {
    "": { "name": "myapp", "version": "1.0.0" },
    "node_modules/lodash": { "version": "4.17.21" },
    "node_modules/react": { "version": "18.2.0" }
  }
}"#;
        let pkgs = parse_package_lock_json(body).expect("ok");
        assert_eq!(pkgs.len(), 2);
        assert!(pkgs
            .iter()
            .any(|p| p.name == "lodash" && p.version == "4.17.21"));
        assert!(pkgs
            .iter()
            .any(|p| p.name == "react" && p.version == "18.2.0"));
        for p in &pkgs {
            assert_eq!(p.ecosystem, "npm");
        }
    }

    #[test]
    fn package_lock_v1_dependencies_format() {
        let body = r#"{
  "name": "old",
  "lockfileVersion": 1,
  "dependencies": {
    "lodash": { "version": "4.17.21" },
    "axios":  { "version": "1.7.2", "dependencies": {
      "follow-redirects": { "version": "1.15.6" }
    }}
  }
}"#;
        let pkgs = parse_package_lock_json(body).expect("ok");
        assert!(pkgs.iter().any(|p| p.name == "lodash"));
        assert!(pkgs.iter().any(|p| p.name == "axios"));
        assert!(pkgs.iter().any(|p| p.name == "follow-redirects"));
    }

    #[test]
    fn package_lock_dedupes_same_name_version() {
        let body = r#"{
  "lockfileVersion": 3,
  "packages": {
    "node_modules/lodash": { "version": "4.17.21" },
    "node_modules/foo/node_modules/lodash": { "version": "4.17.21" }
  }
}"#;
        let pkgs = parse_package_lock_json(body).expect("ok");
        assert_eq!(pkgs.iter().filter(|p| p.name == "lodash").count(), 1);
    }

    #[test]
    fn package_lock_invalid_json_errors() {
        let err = parse_package_lock_json("not json").unwrap_err();
        assert!(err.contains("package-lock"));
    }

    // ---- requirements.txt ----

    #[test]
    fn requirements_parses_pin_lines() {
        let body = "django==4.2.0\nrequests==2.32.0\n";
        let pkgs = parse_requirements_txt(body);
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[0], Package::new("PyPI", "django", "4.2.0"));
    }

    #[test]
    fn requirements_skips_loose_specifiers_and_comments() {
        let body = "# header\nflask>=2.0\nclick~=8.0\nrequests==2.32.0\n-r dev.txt\n";
        let pkgs = parse_requirements_txt(body);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "requests");
    }

    #[test]
    fn requirements_strips_extras_and_markers() {
        let body = "uvicorn[standard]==0.30.0 ; python_version >= '3.10'\n";
        let pkgs = parse_requirements_txt(body);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "uvicorn");
        assert_eq!(pkgs[0].version, "0.30.0");
    }

    #[test]
    fn requirements_ignores_inline_comments() {
        let body = "django==4.2.0  # latest LTS\n";
        let pkgs = parse_requirements_txt(body);
        assert_eq!(pkgs.len(), 1);
    }

    // ---- go.sum ----

    #[test]
    fn go_sum_parses_module_lines() {
        let body = "github.com/stretchr/testify v1.10.0 h1:Xv5erBjTwe/5IxqUQTdXv5kgmIvbHo3QQyRwhJsOfJA=\n\
                    github.com/stretchr/testify v1.10.0/go.mod h1:r2ic/lqez/lEtzL7wO/rwa5dbSLXVDPFyf8C91i36aY=\n\
                    rsc.io/quote v1.5.2 h1:wWYsXxXc8DrGdAxwM6IhGXowD9o0pNdLg7njVcRQc/8=\n";
        let pkgs = parse_go_sum(body);
        assert_eq!(pkgs.len(), 2);
        let mut names: Vec<&str> = pkgs.iter().map(|p| p.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["github.com/stretchr/testify", "rsc.io/quote"]);
        for p in &pkgs {
            assert_eq!(p.ecosystem, "Go");
            assert!(!p.version.contains("/go.mod"));
        }
    }

    #[test]
    fn go_sum_skips_blank_lines() {
        let body = "\n\nrsc.io/quote v1.5.2 h1:abc\n\n";
        let pkgs = parse_go_sum(body);
        assert_eq!(pkgs.len(), 1);
    }

    // ---- parse_lockfile dispatch ----

    #[test]
    fn parse_lockfile_routes_by_filename() {
        let p = parse_lockfile(
            Path::new("Cargo.lock"),
            "[[package]]\nname = \"foo\"\nversion = \"0.1.0\"\n",
        )
        .expect("ok");
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].ecosystem, "crates.io");
    }

    #[test]
    fn parse_lockfile_case_insensitive_filename() {
        let p = parse_lockfile(
            Path::new("CARGO.LOCK"),
            "[[package]]\nname = \"foo\"\nversion = \"0.1.0\"\n",
        )
        .expect("ok");
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn parse_lockfile_unknown_filename_errs() {
        let err = parse_lockfile(Path::new("Pipfile.lock"), "{}").unwrap_err();
        assert!(err.contains("unknown lockfile"));
    }

    // ---- parse_query_response ----

    #[test]
    fn parse_response_empty_returns_empty_vec() {
        let v: Value = serde_json::from_str("{}").unwrap();
        assert!(parse_query_response(&v).unwrap().is_empty());
    }

    #[test]
    fn parse_response_null_vulns_returns_empty_vec() {
        let v: Value = serde_json::from_str(r#"{"vulns": null}"#).unwrap();
        assert!(parse_query_response(&v).unwrap().is_empty());
    }

    #[test]
    fn parse_response_extracts_id_and_summary() {
        let body = r#"{
  "vulns": [
    {
      "id": "GHSA-xxxx-yyyy-zzzz",
      "summary": "Cross-site scripting in foo",
      "aliases": ["CVE-2024-12345"],
      "modified": "2024-05-01T00:00:00Z",
      "published": "2024-04-15T00:00:00Z",
      "references": [
        { "type": "ADVISORY", "url": "https://example.com/a" }
      ]
    }
  ]
}"#;
        let v: Value = serde_json::from_str(body).unwrap();
        let vulns = parse_query_response(&v).expect("ok");
        assert_eq!(vulns.len(), 1);
        let vuln = &vulns[0];
        assert_eq!(vuln.id, "GHSA-xxxx-yyyy-zzzz");
        assert_eq!(vuln.summary.as_deref(), Some("Cross-site scripting in foo"));
        assert_eq!(vuln.aliases, vec!["CVE-2024-12345"]);
        assert_eq!(vuln.references.len(), 1);
        assert_eq!(vuln.references[0].kind, "ADVISORY");
    }

    #[test]
    fn parse_response_rejects_non_array_vulns() {
        let v: Value = serde_json::from_str(r#"{"vulns": "oops"}"#).unwrap();
        let err = parse_query_response(&v).unwrap_err();
        assert!(err.contains("array"));
    }

    // ---- Package json round-trip ----

    #[test]
    fn package_to_json_round_trips() {
        let p = Package::new("npm", "lodash", "4.17.21");
        let v = p.to_json();
        assert_eq!(v.get("ecosystem").and_then(|s| s.as_str()), Some("npm"));
        assert_eq!(v.get("name").and_then(|s| s.as_str()), Some("lodash"));
        assert_eq!(v.get("version").and_then(|s| s.as_str()), Some("4.17.21"));
    }
}
