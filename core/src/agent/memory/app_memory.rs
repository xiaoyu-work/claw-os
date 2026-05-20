//! App-owned slice of the agent's memory.
//!
//! Apps (system-bundled or user-installed) can voluntarily push
//! searchable summaries of their own activity into the agent's memory
//! so that the agent can later answer "what happened?" questions
//! across apps without re-reading every per-app data store.
//!
//! This is a thin facade on top of the existing
//! [`crate::agent::memory::sqlite_fts::MemoryDb`] (FTS5) and
//! [`crate::agent::memory::semantic::SemanticStore`] (vector) stores;
//! it does not introduce a new physical storage. Every app-emitted
//! row is namespaced so the user can later inspect or forget it
//! per-app from `cos agent memory`.
//!
//! ## Namespacing
//!
//! - FTS5 row: `session_id = "app:<source>"`, `role = "app"`.
//! - Semantic row: `namespace = "app/<source>"`, `key = "<row_id>"`.
//!
//! `source` is the app id (`expense-tracker`, `calendar`, …). The
//! bridge that exposes `memory.write` to apps enforces that an app
//! can only write rows for its own `source` via the `MEMORY_WRITE`
//! capability whose default scope is `self:<source>`.
//!
//! ## Content format
//!
//! The natural, human-readable summary is stored as the first line(s)
//! of the FTS5 content column. Optional structured fields
//! (`kind`, `entity_id`, `tags`, `link`) are encoded as suffix lines
//! so the FTS index picks them up too (a user searching for an entity
//! id finds the row). [`AppMemoryRow::from_content`] recovers the
//! structured fields when listing.
//!
//! Semantic indexing only embeds the natural summary — not the
//! suffix — so vector recall is not polluted by tag/id soup.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::semantic::{SemanticError, SemanticStore, SemanticStoreExt};
use super::sqlite_fts::{MemoryDb, MemoryError, MessageRow, SearchHit};

/// Maximum number of bytes accepted in a single `remember` call. Rows
/// stored beyond this point are truncated in-place by the FTS5 layer,
/// which would make the resulting row useless for recall and waste an
/// embed call. Rejecting up-front keeps the failure mode visible to
/// the caller.
pub const MAX_REMEMBER_BYTES: usize = 32 * 1024;

/// Maximum number of tags per row. Anything beyond this is dropped at
/// `remember` time with a warning in the response (not an error) so
/// noisy apps degrade gracefully.
pub const MAX_TAGS: usize = 8;

/// Maximum characters per tag.
pub const MAX_TAG_CHARS: usize = 48;

/// Validation regex for a `source` value: kebab/snake/lowercase with
/// digits, 1–64 chars, must start with a letter. Matches the existing
/// app id rules ([`crate::caps::manifest::Manifest`]).
fn validate_source(source: &str) -> Result<(), String> {
    if source.is_empty() || source.len() > 64 {
        return Err(format!(
            "memory: source must be 1..=64 characters (got {})",
            source.len()
        ));
    }
    let mut chars = source.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() {
        return Err("memory: source must start with a-z".into());
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
            return Err(format!(
                "memory: source must match [a-z][a-z0-9_-]*, found `{c}`"
            ));
        }
    }
    Ok(())
}

/// One structured memory entry pushed by an app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppMemoryEntry {
    /// App id (kebab-case). Constrained by the `memory.write`
    /// capability scope to the app's own id.
    pub source: String,
    /// Natural, FTS-indexable summary. Required and non-empty.
    pub text: String,
    /// Optional category tag: `event`, `fact`, `preference`, `note`,
    /// … Free-form, lowercased.
    pub kind: Option<String>,
    /// Optional stable id the app uses to refer to the underlying
    /// record (e.g. `expense-42`). Lets the agent dedupe and link
    /// back.
    pub entity_id: Option<String>,
    /// Free-form lowercase tags. Truncated to [`MAX_TAGS`].
    pub tags: Vec<String>,
    /// Optional "open this in the source app" handle. Stored as a
    /// shell command line the agent (or the user) can run to inspect
    /// the underlying record (e.g.
    /// `cos app expense-tracker show 42`).
    pub link: Option<String>,
}

impl AppMemoryEntry {
    fn validate(&self) -> Result<(), String> {
        validate_source(&self.source)?;
        if self.text.trim().is_empty() {
            return Err("memory: text must not be empty".into());
        }
        if self.text.len() > MAX_REMEMBER_BYTES {
            return Err(format!(
                "memory: text exceeds {MAX_REMEMBER_BYTES} bytes ({} bytes given)",
                self.text.len()
            ));
        }
        if let Some(k) = &self.kind {
            if k.len() > 32 {
                return Err("memory: kind must be <= 32 chars".into());
            }
        }
        if let Some(e) = &self.entity_id {
            if e.len() > 128 {
                return Err("memory: entity_id must be <= 128 chars".into());
            }
        }
        Ok(())
    }

    /// Render the row into the content string we store in
    /// [`MemoryDb`]. The natural text comes first so FTS5 snippets
    /// surface it; structured fields follow as labelled suffix lines
    /// so FTS5 still indexes them (a user looking up an entity id
    /// finds the row).
    fn to_content(&self) -> String {
        let mut out = self.text.trim().to_string();
        let mut tail: Vec<String> = Vec::new();
        // Always tag the source so it's visible in raw recall hits.
        tail.push(format!("Source: {}", self.source));
        if let Some(k) = &self.kind {
            if !k.trim().is_empty() {
                tail.push(format!("Kind: {}", k.trim()));
            }
        }
        if let Some(e) = &self.entity_id {
            if !e.trim().is_empty() {
                tail.push(format!("Entity: {}", e.trim()));
            }
        }
        if !self.tags.is_empty() {
            tail.push(format!("Tags: {}", self.tags.join(", ")));
        }
        if let Some(l) = &self.link {
            if !l.trim().is_empty() {
                tail.push(format!("Link: {}", l.trim()));
            }
        }
        if !tail.is_empty() {
            out.push_str("\n\n");
            out.push_str(&tail.join("\n"));
        }
        out
    }

    fn sanitize(mut self) -> Self {
        // Lowercase + clip tags, drop empties, dedup, cap count.
        let mut seen: std::collections::BTreeSet<String> = Default::default();
        let mut cleaned: Vec<String> = Vec::new();
        for t in self.tags.drain(..) {
            let t = t.trim().to_ascii_lowercase();
            if t.is_empty() || t.len() > MAX_TAG_CHARS {
                continue;
            }
            if seen.insert(t.clone()) {
                cleaned.push(t);
                if cleaned.len() >= MAX_TAGS {
                    break;
                }
            }
        }
        self.tags = cleaned;
        if let Some(k) = self.kind.as_mut() {
            *k = k.trim().to_ascii_lowercase();
        }
        self.source = self.source.trim().to_string();
        self
    }
}

/// Result of a successful [`remember`]. The agent's "what changed?"
/// dashboard can stream these to the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RememberOutcome {
    pub row_id: i64,
    pub session_id: String,
    /// Number of bytes the content stored after rendering. Useful for
    /// quota / budget reporting later.
    pub stored_bytes: usize,
    /// `true` if a semantic embedding was also computed.
    pub indexed_semantic: bool,
    /// Truncated text actually stored (echo of input minus trim).
    pub text: String,
}

/// The session-id form used in [`MemoryDb`] for app-owned rows.
pub fn session_id_for(source: &str) -> String {
    format!("app:{source}")
}

/// The semantic-store namespace form for app-owned rows.
pub fn semantic_namespace_for(source: &str) -> String {
    format!("app/{source}")
}

/// Persist one app-emitted memory entry. Writes FTS5 first
/// (durable), then opportunistically embeds for semantic search.
/// Returns an outcome describing what was stored.
///
/// Semantic indexing is awaited (we don't `spawn_index` here)
/// because:
/// 1. The bridge runs as a one-shot `cos __memory remember`
///    subprocess and would otherwise exit before the background
///    embed finishes.
/// 2. Apps already block on `policy.require` / `snapshot.write` so
///    one more synchronous call is acceptable.
///
/// Apps that want to avoid the embed latency set `indexable=false`,
/// in which case only FTS5 is touched.
pub async fn remember(
    db: &MemoryDb,
    store: Option<&Arc<SemanticStore>>,
    entry: AppMemoryEntry,
    indexable: bool,
) -> Result<RememberOutcome, RememberError> {
    let entry = entry.sanitize();
    entry.validate().map_err(RememberError::Invalid)?;

    let session_id = session_id_for(&entry.source);
    let content = entry.to_content();
    let stored_bytes = content.len();

    let row_id = db
        .record_message(&session_id, "app", &content)
        .map_err(RememberError::Db)?;

    let mut indexed_semantic = false;
    if indexable {
        if let Some(store) = store {
            let namespace = semantic_namespace_for(&entry.source);
            let key = row_id.to_string();
            match store.index(&namespace, &key, &entry.text).await {
                Ok(_) => indexed_semantic = true,
                Err(SemanticError::Disabled) => {
                    // No embedder configured — silently skip. The FTS
                    // row is still there.
                }
                Err(e) => return Err(RememberError::Semantic(e)),
            }
        }
    }

    Ok(RememberOutcome {
        row_id,
        session_id,
        stored_bytes,
        indexed_semantic,
        text: entry.text,
    })
}

/// Open the default-path semantic store with the configured embedder.
/// Returns `None` if embedding is disabled — the FTS5 path still works.
pub fn open_default_store() -> Option<Arc<SemanticStore>> {
    match SemanticStore::open_default() {
        Ok(Some(s)) => Some(Arc::new(s)),
        Ok(None) => None,
        Err(e) => {
            tracing::warn!("app_memory: semantic store unavailable ({e})");
            None
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RememberError {
    #[error("invalid app memory entry: {0}")]
    Invalid(String),
    #[error("memory db: {0}")]
    Db(#[from] MemoryError),
    #[error("semantic store: {0}")]
    Semantic(#[from] SemanticError),
}

// ---------------------------------------------------------------------------
// Read side: list / show / search / forget.
//
// Used by the user-facing `cos agent memory` CLI to inspect or
// redact what apps have pushed.
// ---------------------------------------------------------------------------

/// One memory row plus the structured fields parsed back out of its
/// content suffix.
#[derive(Debug, Clone, Serialize)]
pub struct AppMemoryRow {
    pub id: i64,
    pub source: String,
    pub ts_ms: i64,
    pub text: String,
    pub kind: Option<String>,
    pub entity_id: Option<String>,
    pub tags: Vec<String>,
    pub link: Option<String>,
    /// FTS5 bm25 rank when this row was produced by a search; `None`
    /// otherwise (list / show).
    pub rank: Option<f64>,
}

impl AppMemoryRow {
    fn from_row(row: MessageRow, rank: Option<f64>) -> Self {
        let parsed = parse_content(&row.content);
        let source = row
            .session_id
            .strip_prefix("app:")
            .unwrap_or(&row.session_id)
            .to_string();
        Self {
            id: row.id,
            source: parsed.source.unwrap_or(source),
            ts_ms: row.ts_ms,
            text: parsed.text,
            kind: parsed.kind,
            entity_id: parsed.entity_id,
            tags: parsed.tags,
            link: parsed.link,
            rank,
        }
    }
}

struct ParsedContent {
    text: String,
    source: Option<String>,
    kind: Option<String>,
    entity_id: Option<String>,
    tags: Vec<String>,
    link: Option<String>,
}

/// Parse the structured suffix produced by
/// [`AppMemoryEntry::to_content`] back out of a stored content
/// string. Unknown / missing suffix lines leave the corresponding
/// field as `None` / empty.
fn parse_content(content: &str) -> ParsedContent {
    // The suffix is the trailing block separated from the natural
    // text by a blank line. Walk lines from the end as long as they
    // match `Label: value`.
    let lines: Vec<&str> = content.lines().collect();
    let mut suffix_start = lines.len();
    for (i, line) in lines.iter().enumerate().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            // Blank line — possible separator.
            if suffix_start < lines.len() {
                suffix_start = i + 1;
                break;
            } else {
                continue;
            }
        }
        if !is_suffix_line(trimmed) {
            // Stop walking once we hit a non-suffix line.
            break;
        }
        suffix_start = i;
    }
    let mut parsed = ParsedContent {
        text: String::new(),
        source: None,
        kind: None,
        entity_id: None,
        tags: Vec::new(),
        link: None,
    };
    let suffix = &lines[suffix_start..];
    for line in suffix {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("Source: ") {
            parsed.source = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("Kind: ") {
            parsed.kind = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("Entity: ") {
            parsed.entity_id = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("Tags: ") {
            parsed.tags = rest
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
        } else if let Some(rest) = line.strip_prefix("Link: ") {
            parsed.link = Some(rest.trim().to_string());
        }
    }
    // Whatever was before the suffix block is the natural text.
    let text_end = suffix_start.saturating_sub(0);
    let body_lines = &lines[..text_end];
    // Trim trailing blank lines from body.
    let mut end = body_lines.len();
    while end > 0 && body_lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    parsed.text = body_lines[..end].join("\n").trim().to_string();
    if parsed.text.is_empty() {
        // No suffix detected — full content IS the text.
        parsed.text = content.trim().to_string();
    }
    parsed
}

fn is_suffix_line(line: &str) -> bool {
    matches!(
        line.split_once(':'),
        Some(("Source" | "Kind" | "Entity" | "Tags" | "Link", _))
    )
}

/// List recent app-owned memory rows.
///
/// - `source = Some("expense-tracker")`: only that app.
/// - `source = None`: every app (rows where session_id starts with
///   `app:`).
///
/// Rows come back newest-first.
pub fn list(
    db: &MemoryDb,
    source: Option<&str>,
    limit: usize,
) -> Result<Vec<AppMemoryRow>, MemoryError> {
    let conn = db.lock_conn()?;
    let (sql, sid) = match source {
        Some(s) => (
            "SELECT id, session_id, role, content, ts_ms FROM messages
             WHERE session_id = ? AND role = 'app'
             ORDER BY ts_ms DESC, id DESC LIMIT ?",
            Some(session_id_for(s)),
        ),
        None => (
            "SELECT id, session_id, role, content, ts_ms FROM messages
             WHERE session_id LIKE 'app:%' AND role = 'app'
             ORDER BY ts_ms DESC, id DESC LIMIT ?",
            None,
        ),
    };
    let mut stmt = conn.prepare(sql)?;
    let mut out = Vec::new();
    let rows_iter: Box<dyn Iterator<Item = Result<MessageRow, rusqlite::Error>>> = if let Some(s) =
        sid.as_deref()
    {
        let rows = stmt
            .query_map(rusqlite::params![s, limit as i64], super::sqlite_fts::row_to_message)?
            .collect::<Vec<_>>();
        Box::new(rows.into_iter())
    } else {
        let rows = stmt
            .query_map(rusqlite::params![limit as i64], super::sqlite_fts::row_to_message)?
            .collect::<Vec<_>>();
        Box::new(rows.into_iter())
    };
    for r in rows_iter {
        let row = r?;
        out.push(AppMemoryRow::from_row(row, None));
    }
    Ok(out)
}

/// Look up a single row by id. Returns `None` if it doesn't exist or
/// isn't an app-owned row.
pub fn show(db: &MemoryDb, id: i64) -> Result<Option<AppMemoryRow>, MemoryError> {
    let conn = db.lock_conn()?;
    let mut stmt = conn.prepare(
        "SELECT id, session_id, role, content, ts_ms FROM messages
         WHERE id = ? AND role = 'app' AND session_id LIKE 'app:%'",
    )?;
    let mut rows = stmt.query_map(rusqlite::params![id], super::sqlite_fts::row_to_message)?;
    match rows.next() {
        Some(Ok(row)) => Ok(Some(AppMemoryRow::from_row(row, None))),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// FTS5 search over app-owned rows. Optionally constrained to one
/// source.
pub fn search(
    db: &MemoryDb,
    query: &str,
    source: Option<&str>,
    limit: usize,
) -> Result<Vec<AppMemoryRow>, MemoryError> {
    let hits: Vec<SearchHit> = match source {
        Some(s) => db.search_session(&session_id_for(s), query, limit)?,
        None => {
            // Search across everything, then filter to app-owned rows.
            // Pull extra hits to compensate for the filter.
            let raw = db.search(query, limit.saturating_mul(2).max(limit))?;
            raw.into_iter()
                .filter(|h| h.row.session_id.starts_with("app:") && h.row.role == "app")
                .take(limit)
                .collect()
        }
    };
    Ok(hits
        .into_iter()
        .map(|h| AppMemoryRow::from_row(h.row, Some(h.rank)))
        .collect())
}

/// Delete every app-owned row, or every row for a single source.
/// Returns the count deleted.
///
/// Also clears the corresponding semantic-store namespace(s) when
/// `store` is provided so the agent's vector recall stays in sync
/// with the FTS5 view.
pub fn forget_source(
    db: &MemoryDb,
    store: Option<&Arc<SemanticStore>>,
    source: &str,
) -> Result<usize, MemoryError> {
    validate_source(source).map_err(|m| MemoryError::Poisoned(m))?;
    let n = db.clear_session(&session_id_for(source))?;
    if let Some(s) = store {
        if let Err(e) = s.clear_namespace(&semantic_namespace_for(source)) {
            tracing::warn!("app_memory: semantic clear_namespace failed: {e}");
        }
    }
    Ok(n)
}

/// Delete one row by id. Returns `true` if a row was removed.
pub fn forget_row(
    db: &MemoryDb,
    store: Option<&Arc<SemanticStore>>,
    id: i64,
) -> Result<bool, MemoryError> {
    // First fetch the row so we know which semantic namespace/key to
    // also clean up.
    let row = show(db, id)?;
    let conn = db.lock_conn()?;
    let n = conn.execute(
        "DELETE FROM messages WHERE id = ? AND role = 'app' AND session_id LIKE 'app:%'",
        rusqlite::params![id],
    )?;
    drop(conn);
    if n > 0 {
        if let (Some(s), Some(row)) = (store, row) {
            let _ = s.remove(&semantic_namespace_for(&row.source), &id.to_string());
        }
    }
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::memory::semantic::SemanticStore;

    fn open_db() -> MemoryDb {
        MemoryDb::open_in_memory().unwrap()
    }

    fn open_store() -> Arc<SemanticStore> {
        Arc::new(SemanticStore::open_in_memory(None).unwrap())
    }

    fn entry(source: &str, text: &str) -> AppMemoryEntry {
        AppMemoryEntry {
            source: source.into(),
            text: text.into(),
            kind: None,
            entity_id: None,
            tags: Vec::new(),
            link: None,
        }
    }

    #[tokio::test]
    async fn remember_writes_fts_row_and_returns_outcome() {
        let db = open_db();
        let out = remember(&db, None, entry("expense-tracker", "Lunch at Eatsa"), false)
            .await
            .unwrap();
        assert!(out.row_id > 0);
        assert_eq!(out.session_id, "app:expense-tracker");
        assert!(out.stored_bytes > 0);
        assert!(!out.indexed_semantic, "no store given");
    }

    #[tokio::test]
    async fn remember_rejects_empty_text() {
        let db = open_db();
        let err = remember(&db, None, entry("a", "   \n  "), false)
            .await
            .unwrap_err();
        assert!(matches!(err, RememberError::Invalid(_)));
    }

    #[tokio::test]
    async fn remember_rejects_invalid_source() {
        let db = open_db();
        let err = remember(&db, None, entry("BadCaps", "hi"), false)
            .await
            .unwrap_err();
        assert!(matches!(err, RememberError::Invalid(_)));
    }

    #[tokio::test]
    async fn list_returns_only_app_rows_newest_first() {
        let db = open_db();
        remember(&db, None, entry("a", "first"), false).await.unwrap();
        remember(&db, None, entry("a", "second"), false).await.unwrap();
        remember(&db, None, entry("b", "third"), false).await.unwrap();
        // Also stuff a regular session message — must not appear.
        db.record_message("ses_xx", "user", "private prompt").unwrap();
        let rows = list(&db, None, 10).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].text, "third");
        assert_eq!(rows[1].text, "second");
        assert_eq!(rows[2].text, "first");
    }

    #[tokio::test]
    async fn list_filtered_by_source() {
        let db = open_db();
        remember(&db, None, entry("a", "alpha"), false).await.unwrap();
        remember(&db, None, entry("b", "beta"), false).await.unwrap();
        let rows = list(&db, Some("a"), 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "alpha");
        assert_eq!(rows[0].source, "a");
    }

    #[tokio::test]
    async fn show_returns_structured_fields() {
        let db = open_db();
        let mut e = entry("expense-tracker", "Marriott NYC $487.50");
        e.kind = Some("event".into());
        e.entity_id = Some("expense-42".into());
        e.tags = vec!["expense".into(), "hotel".into(), "Hotel".into()]; // dedup case-insensitively
        e.link = Some("cos app expense-tracker show 42".into());
        let out = remember(&db, None, e, false).await.unwrap();
        let row = show(&db, out.row_id).unwrap().expect("row exists");
        assert_eq!(row.text, "Marriott NYC $487.50");
        assert_eq!(row.kind.as_deref(), Some("event"));
        assert_eq!(row.entity_id.as_deref(), Some("expense-42"));
        assert_eq!(row.tags, vec!["expense", "hotel"]);
        assert_eq!(row.link.as_deref(), Some("cos app expense-tracker show 42"));
        assert_eq!(row.source, "expense-tracker");
    }

    #[tokio::test]
    async fn search_finds_by_natural_text_and_by_tag() {
        let db = open_db();
        let mut e = entry("expense-tracker", "Lunch at Eatsa with the team");
        e.tags = vec!["lunch".into(), "team".into()];
        remember(&db, None, e, false).await.unwrap();
        remember(&db, None, entry("calendar", "Dinner at Eatsa solo"), false)
            .await
            .unwrap();

        let hits = search(&db, "Eatsa", None, 10).unwrap();
        assert_eq!(hits.len(), 2);

        let hits = search(&db, "team", None, 10).unwrap();
        assert_eq!(hits.len(), 1, "tags are FTS-indexed");
        assert_eq!(hits[0].source, "expense-tracker");
    }

    #[tokio::test]
    async fn search_scoped_by_source() {
        let db = open_db();
        remember(&db, None, entry("a", "shared keyword"), false)
            .await
            .unwrap();
        remember(&db, None, entry("b", "shared keyword"), false)
            .await
            .unwrap();
        let hits = search(&db, "shared", Some("a"), 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source, "a");
    }

    #[tokio::test]
    async fn forget_source_removes_only_that_source() {
        let db = open_db();
        remember(&db, None, entry("a", "alpha"), false).await.unwrap();
        remember(&db, None, entry("b", "beta"), false).await.unwrap();
        let n = forget_source(&db, None, "a").unwrap();
        assert_eq!(n, 1);
        let rows = list(&db, None, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source, "b");
    }

    #[tokio::test]
    async fn forget_row_removes_only_that_row() {
        let db = open_db();
        let out1 = remember(&db, None, entry("a", "first"), false).await.unwrap();
        let _out2 = remember(&db, None, entry("a", "second"), false).await.unwrap();
        let ok = forget_row(&db, None, out1.row_id).unwrap();
        assert!(ok);
        let rows = list(&db, None, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "second");
    }

    #[tokio::test]
    async fn forget_row_returns_false_for_unknown_id() {
        let db = open_db();
        let ok = forget_row(&db, None, 9_999_999).unwrap();
        assert!(!ok);
    }

    #[tokio::test]
    async fn remember_with_disabled_embedder_does_not_error() {
        let db = open_db();
        let store = open_store(); // no embedder
        let out = remember(
            &db,
            Some(&store),
            entry("expense-tracker", "indexable text"),
            true,
        )
        .await
        .unwrap();
        // Should fall back gracefully when the store is configured
        // without an embedder.
        assert!(!out.indexed_semantic);
    }

    #[test]
    fn parse_content_recovers_full_metadata() {
        let content = "Hello world\n\nSource: my-app\nKind: event\nEntity: x-1\nTags: a, b\nLink: cos app my-app show x-1";
        let p = parse_content(content);
        assert_eq!(p.text, "Hello world");
        assert_eq!(p.source.as_deref(), Some("my-app"));
        assert_eq!(p.kind.as_deref(), Some("event"));
        assert_eq!(p.entity_id.as_deref(), Some("x-1"));
        assert_eq!(p.tags, vec!["a", "b"]);
        assert_eq!(
            p.link.as_deref(),
            Some("cos app my-app show x-1")
        );
    }

    #[test]
    fn parse_content_with_no_suffix() {
        let content = "Just text, no metadata";
        let p = parse_content(content);
        assert_eq!(p.text, "Just text, no metadata");
        assert!(p.source.is_none());
        assert!(p.kind.is_none());
        assert!(p.tags.is_empty());
    }

    #[test]
    fn parse_content_with_colons_in_body() {
        // A body that contains "X: y" lines should NOT be misparsed as
        // metadata because the suffix block must be separated by a
        // blank line.
        let content = "Note: I had a thought\nIt was about cats\n\nSource: my-app";
        let p = parse_content(content);
        assert_eq!(p.text, "Note: I had a thought\nIt was about cats");
        assert_eq!(p.source.as_deref(), Some("my-app"));
    }

    #[test]
    fn validate_source_rejects_garbage() {
        assert!(validate_source("").is_err());
        assert!(validate_source("Bad").is_err());
        assert!(validate_source("9start").is_err());
        assert!(validate_source("with space").is_err());
        assert!(validate_source("a-good_one1").is_ok());
    }

    #[test]
    fn entry_sanitize_dedups_and_lowercases_tags() {
        let e = AppMemoryEntry {
            source: "expense-tracker".into(),
            text: "x".into(),
            kind: Some("  Event  ".into()),
            entity_id: None,
            tags: vec![
                "Hotel".into(),
                "hotel".into(),
                "".into(),
                "Hotel".into(),
                "TRAVEL".into(),
            ],
            link: None,
        }
        .sanitize();
        assert_eq!(e.source, "expense-tracker");
        assert_eq!(e.kind.as_deref(), Some("event"));
        assert_eq!(e.tags, vec!["hotel", "travel"]);
    }
}
