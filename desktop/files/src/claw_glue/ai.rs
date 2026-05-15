//! Files ↔ AI bridge.
//!
//! Every AI-touching operation in the Files app routes through the
//! kernel-level apps (`apps/doc`, `apps/docs`, …) over the
//! `cos app <id> <op>` boundary so capability gating, audit logging
//! and budget accounting all happen exactly once, in the place the
//! kernel expects them. The desktop process never speaks to a model
//! directly.
//!
//! All entry points are `async`. They are designed to be driven from
//! `cosmic::Task::future(…)` so the UI thread stays responsive while
//! the model is thinking. Errors are flattened into a single
//! human-presentable `String` because that is what the existing
//! `AiSummaryStatus::Error` shape already carries; structured details
//! are logged but not surfaced to the user.
//!
//! The bridge has no opinion about UI: it returns plain `Result`s and
//! lets the caller decide which dialog page, sidebar card or toast it
//! lands in.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

/// One hit returned by `docs.search` (Recoll). Matches the JSON
/// shape emitted by `apps/docs/main.py`. Fields kept optional because
/// Recoll's output is best-effort — corrupt PDFs, deleted files since
/// last index, and oddball MIME types all show up in the wild.
#[derive(Clone, Debug, Deserialize)]
pub struct SearchHit {
    #[serde(default)]
    pub path: PathBuf,
    #[serde(default)]
    pub mtype: String,
    #[serde(default)]
    pub mtime: Option<i64>,
    #[serde(default, rename = "abstract")]
    pub abstract_text: String,
}

/// Look up the `cos` binary. `CLAW_COS_BIN` lets the test harness
/// inject a stub; otherwise we trust `PATH`.
fn cos_bin() -> String {
    std::env::var("CLAW_COS_BIN").unwrap_or_else(|_| "cos".into())
}

/// Run `cos app <app_id> <op>` with the given trailing arguments and
/// parse stdout as JSON. Returns the decoded value, or a
/// human-readable error string covering process-launch failures,
/// non-zero exits with empty stdout, garbled JSON, and explicit
/// `{"error": "..."}` envelopes from the app.
async fn invoke_app(app_id: &str, op: &str, extra: &[&str]) -> Result<Value, String> {
    let bin = cos_bin();
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.args(["app", app_id, op]);
    cmd.args(extra);

    let output = cmd
        .output()
        .await
        .map_err(|e| format!("failed to invoke {bin}: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "cos produced no output ({})\n{}",
            output.status, stderr
        ));
    }
    let value: Value = serde_json::from_str(trimmed)
        .map_err(|e| format!("bad JSON from cos: {e}\n---\n{trimmed}"))?;
    if let Some(err) = value.get("error").and_then(|v| v.as_str()) {
        return Err(err.to_string());
    }
    Ok(value)
}

/// Pull a single string field out of a JSON response. Used by the
/// `doc.*` ops which all return one-shot text outputs keyed by op
/// name (`summary`, `explanation`, `rewritten`, …).
fn extract_string_field(value: &Value, field: &str) -> Result<String, String> {
    value
        .get(field)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("AI response missing '{field}' field"))
}

fn path_arg(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("non-UTF-8 path cannot cross the bridge: {path:?}"))
}

/// Summarise a single file. Invokes `cos app doc summarize --file
/// <path>` and pulls the `summary` field out of the response.
///
/// This is the function the right-click "AI summary" menu item has
/// been calling since the AI was first wired into Files.
pub async fn summarize(path: PathBuf) -> Result<String, String> {
    let p = path_arg(&path)?;
    let value = invoke_app("doc", "summarize", &["--file", p]).await?;
    extract_string_field(&value, "summary")
}

/// Explain the contents of a file in plain language. Wraps
/// `cos app doc explain --file <path>`. The kernel's `apps/doc`
/// returns `{"explanation": "..."}`.
#[allow(dead_code)]
pub async fn explain(path: PathBuf) -> Result<String, String> {
    let p = path_arg(&path)?;
    let value = invoke_app("doc", "explain", &["--file", p]).await?;
    extract_string_field(&value, "explanation")
}

/// Rewrite a document according to a natural-language instruction.
/// Wraps `cos app doc rewrite --file <path> --instruction <text>`.
/// Returns the rewritten body as a single string; callers are
/// responsible for showing it for review and applying the change
/// (Files never auto-writes back).
#[allow(dead_code)]
pub async fn rewrite(path: PathBuf, instruction: String) -> Result<String, String> {
    let p = path_arg(&path)?;
    let value = invoke_app(
        "doc",
        "rewrite",
        &["--file", p, "--instruction", &instruction],
    )
    .await?;
    extract_string_field(&value, "rewritten")
}

/// Indexed full-text search across the user's documents. Routes
/// through `cos app docs search --query <q> --max-results <n>`
/// (Recoll, see `apps/docs`). `max_results` is clamped by the app
/// itself to a sane upper bound, so we pass it through unchanged.
#[allow(dead_code)]
pub async fn search(query: String, max_results: usize) -> Result<Vec<SearchHit>, String> {
    let max = max_results.to_string();
    let value = invoke_app(
        "docs",
        "search",
        &["--query", &query, "--max-results", &max],
    )
    .await?;
    parse_search_hits(&value)
}

/// "More like this" against the local document index. Currently a
/// thin convenience over [`search`] using the target file's basename
/// (sans extension) as the query — Recoll already weights matching
/// terms, so this is enough to surface the same-topic neighbours.
/// When Recoll's true `qopts.minrelevance` API is wired through
/// `apps/docs`, this implementation can switch over without changing
/// the call sites.
#[allow(dead_code)]
pub async fn find_similar(path: PathBuf, max_results: usize) -> Result<Vec<SearchHit>, String> {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .ok_or_else(|| format!("cannot derive a search query from path: {path:?}"))?;
    let query = stem.trim().to_string();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    search(query, max_results).await
}

fn parse_search_hits(value: &Value) -> Result<Vec<SearchHit>, String> {
    let arr = value
        .get("hits")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "AI response missing 'hits' array".to_string())?;
    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        let hit: SearchHit = serde_json::from_value(entry.clone())
            .map_err(|e| format!("bad search hit shape: {e}"))?;
        out.push(hit);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_search_hits_handles_well_formed_payload() {
        let payload = json!({
            "hits": [
                {
                    "path": "/home/u/Documents/q3.pdf",
                    "mtype": "application/pdf",
                    "mtime": 1_700_000_000_i64,
                    "abstract": "Quarterly revenue summary…"
                },
                {
                    "path": "/home/u/Documents/q3-notes.md",
                    "mtype": "text/markdown",
                    "mtime": 1_700_100_000_i64,
                    "abstract": ""
                }
            ]
        });
        let hits = parse_search_hits(&payload).expect("parse");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].path, PathBuf::from("/home/u/Documents/q3.pdf"));
        assert_eq!(hits[0].mtype, "application/pdf");
        assert_eq!(hits[0].mtime, Some(1_700_000_000));
        assert!(hits[0].abstract_text.starts_with("Quarterly"));
        assert_eq!(hits[1].abstract_text, "");
    }

    #[test]
    fn parse_search_hits_tolerates_partial_fields() {
        let payload = json!({ "hits": [ { "path": "/x" } ] });
        let hits = parse_search_hits(&payload).expect("parse");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].mtype, "");
        assert_eq!(hits[0].mtime, None);
    }

    #[test]
    fn parse_search_hits_rejects_missing_hits_key() {
        let payload = json!({ "results": [] });
        let err = parse_search_hits(&payload).expect_err("missing key must error");
        assert!(err.contains("'hits'"));
    }

    #[test]
    fn extract_string_field_returns_explicit_field_error() {
        let payload = json!({ "explanation": "ok" });
        let got = extract_string_field(&payload, "summary").expect_err("missing field");
        assert!(got.contains("'summary'"));
    }
}
