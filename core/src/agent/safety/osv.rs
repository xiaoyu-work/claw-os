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
/// How long a successful osv.dev response is cached for. 24 h is the
/// industry-standard CVE feed refresh cadence — anything tighter
/// burns the public endpoint's rate limit without surfacing new
/// disclosures faster.
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Hard cap on the in-memory cache so a script that scans an enormous
/// monorepo (1000s of packages × many versions) doesn't grow the
/// process's RSS without bound. When full the oldest entry is
/// evicted on insert.
const CACHE_MAX_ENTRIES: usize = 1024;

type CacheKey = (String, String, String);

struct CacheEntry {
    inserted: std::time::Instant,
    vulns: Vec<OsvVulnerability>,
}

static OSV_CACHE: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<CacheKey, CacheEntry>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

fn cache_get(pkg: &Package) -> Option<Vec<OsvVulnerability>> {
    let key: CacheKey = (pkg.ecosystem.clone(), pkg.name.clone(), pkg.version.clone());
    let mut guard = OSV_CACHE.lock().ok()?;
    if let Some(entry) = guard.get(&key) {
        if entry.inserted.elapsed() < CACHE_TTL {
            return Some(entry.vulns.clone());
        }
        // Expired — drop it so a subsequent fetch repopulates.
        guard.remove(&key);
    }
    None
}

fn cache_put(pkg: &Package, vulns: &[OsvVulnerability]) {
    let key: CacheKey = (pkg.ecosystem.clone(), pkg.name.clone(), pkg.version.clone());
    let Ok(mut guard) = OSV_CACHE.lock() else {
        return;
    };
    if guard.len() >= CACHE_MAX_ENTRIES {
        // Evict the oldest entry. HashMap iteration is unordered so
        // we scan once — acceptable, this branch only fires after
        // 1024 distinct lookups have already happened in one process.
        if let Some(oldest_key) = guard
            .iter()
            .min_by_key(|(_, v)| v.inserted)
            .map(|(k, _)| k.clone())
        {
            guard.remove(&oldest_key);
        }
    }
    guard.insert(
        key,
        CacheEntry {
            inserted: std::time::Instant::now(),
            vulns: vulns.to_vec(),
        },
    );
}

/// Query osv.dev for vulnerabilities affecting one package version.
///
/// Returns `Ok(vec![])` if osv.dev returned no `vulns` for the package.
///
/// Results are cached in-process for [`CACHE_TTL`] (24 h). A repeated
/// scan of the same lockfile — or two unrelated lockfiles that share a
/// transitive dependency — only round-trips to osv.dev once. The cache
/// is bounded at [`CACHE_MAX_ENTRIES`] entries; oldest-first eviction
/// on overflow.
pub async fn query(pkg: &Package) -> Result<Vec<OsvVulnerability>, String> {
    if let Some(hit) = cache_get(pkg) {
        return Ok(hit);
    }
    let vulns = query_with_url(pkg, OSV_QUERY_URL, DEFAULT_TIMEOUT).await?;
    cache_put(pkg, &vulns);
    Ok(vulns)
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
    // rest looks like ` = "value" # optional comment`. The naive
    // approach of split_once('#') misclassifies values that legally
    // contain a `#` inside the quoted string (e.g. a URL fragment in
    // a registry-source key), so we hand-roll a tiny scanner that
    // tracks whether we are inside a double-quoted string and only
    // honours `#` as a comment marker when *outside* a string.
    let after_eq = rest.split_once('=')?.1.trim();

    let bytes = after_eq.as_bytes();
    // Find the position where a comment starts (if any). Walk the
    // bytes, flipping `in_string` on each unescaped `"`. The simple
    // backslash-escape handling matches TOML's basic-string rules
    // closely enough for the lockfile we actually parse — Cargo
    // writes `name = "foo"` / `version = "1.2.3"` with no embedded
    // quotes or escapes.
    let mut in_string = false;
    let mut escaped = false;
    let mut comment_at: Option<usize> = None;
    for (i, &b) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match b {
            b'\\' if in_string => escaped = true,
            b'"' => in_string = !in_string,
            b'#' if !in_string => {
                comment_at = Some(i);
                break;
            }
            _ => {}
        }
    }
    let no_comment = match comment_at {
        Some(i) => after_eq[..i].trim(),
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/safety/osv.rs"
    ));
}
