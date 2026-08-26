//! Prompt-cache breakpoint markers (Anthropic-compatible).
//!
//! Anthropic's prompt caching lets the API server cache up to 4
//! "breakpoints" per request and reuse the prefix on subsequent calls
//! at ~10× cheaper rate. Cache hits also reduce TTFT by skipping
//! re-tokenisation of the prefix.
//!
//! Our [`ChatRequest`] is provider-neutral, so the breakpoint can't
//! be a first-class field — we'd need provider-specific shape.
//! Instead breakpoints are stored as opaque markers in
//! `request.extra` under reserved keys:
//!
//!   * `__cache_breakpoints` — `[u32]` indices into `messages`.
//!     A breakpoint at index `i` means "cache everything up to and
//!     including message `i`". The breakpoint is realised by
//!     attaching `cache_control: {"type":"ephemeral"}` to the LAST
//!     content block of message `i` when the request is wire-encoded.
//!
//!   * `__cache_system` — `bool`. When true, attach `cache_control`
//!     to the system prompt block. (System is a string in our
//!     [`ChatRequest`] and a content-block-array on the wire — the
//!     anthropic provider promotes it to a single text block when
//!     this marker is set.)
//!
//!   * `__cache_tools` — `bool`. When true, attach `cache_control`
//!     to the LAST tool definition. Caches static tool schemas.
//!
//! Providers that don't support prompt caching (OpenAI compat,
//! Gemini, llama.cpp local) ignore these keys when merging extras.
//! The Anthropic provider strips them before forwarding so they
//! never leak into the wire request.
//!
//! ## Limits and constraints
//!
//!   * Anthropic enforces a hard cap of **4 breakpoints per request**.
//!     [`mark_breakpoint`] returns `Err` if you'd exceed it.
//!     [`set_breakpoints`] truncates with a warning span.
//!   * Breakpoint indices must be valid for the message vec at the
//!     point the request is sent. We don't validate at marker time
//!     (messages may grow between marking and sending) — the
//!     anthropic provider drops out-of-range markers silently.
//!   * Cached prefixes must be at least N tokens (Anthropic: 1024
//!     for sonnet/opus). We don't enforce this — the API will reject
//!     too-small caches and the response will succeed without a
//!     cache hit. Caller is responsible for choosing meaningful
//!     breakpoints.
//!
//! ## Recommended caching strategy
//!
//! 1. Mark `system` cached on every request — system prompts are
//!    ~stable across a session.
//! 2. Mark `tools` cached on every request — tool definitions are
//!    fully stable.
//! 3. Optionally mark a breakpoint after each successful tool turn
//!    — preserves the round-trip's reasoning.
//! 4. Never mark the LAST message (no benefit on first request, and
//!    invalidates the cache on the next).
//!
//! Library-only this commit; the runtime doesn't yet auto-mark
//! breakpoints.

use crate::agent::llm::ChatRequest;

/// Hard maximum imposed by the Anthropic API.
pub const MAX_CACHE_BREAKPOINTS: usize = 4;

/// Reserved key in `ChatRequest::extra` for the breakpoint indices
/// array. Public so providers can read/strip it.
pub const KEY_BREAKPOINTS: &str = "__cache_breakpoints";

/// Reserved key in `ChatRequest::extra` for the system-prompt cache
/// flag.
pub const KEY_SYSTEM: &str = "__cache_system";

/// Reserved key in `ChatRequest::extra` for the tools-array cache
/// flag.
pub const KEY_TOOLS: &str = "__cache_tools";

/// Error type for caching operations.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CacheMarkError {
    #[error(
        "cannot add breakpoint: at limit of {MAX_CACHE_BREAKPOINTS} (Anthropic API max). \
         Drop an existing breakpoint first."
    )]
    AtLimit,
    #[error("breakpoint at index {0} already marked (no-op duplicate)")]
    Duplicate(u32),
}

/// Ensure `extra` is an object — replaces null/non-object values
/// with an empty object so we can insert keys into it. Returns the
/// mutable map.
fn extra_as_object(req: &mut ChatRequest) -> &mut serde_json::Map<String, serde_json::Value> {
    if !req.extra.is_object() {
        req.extra = serde_json::Value::Object(serde_json::Map::new());
    }
    req.extra.as_object_mut().expect("just ensured object")
}

/// Mark message at `msg_index` as a cache breakpoint. Returns
/// [`CacheMarkError::AtLimit`] if 4 breakpoints already marked,
/// or [`CacheMarkError::Duplicate`] if `msg_index` is already a
/// breakpoint (call is a no-op in that case but reported for
/// visibility).
pub fn mark_breakpoint(req: &mut ChatRequest, msg_index: u32) -> Result<(), CacheMarkError> {
    let mut current = get_breakpoints(req);
    if current.contains(&msg_index) {
        return Err(CacheMarkError::Duplicate(msg_index));
    }
    if current.len() >= MAX_CACHE_BREAKPOINTS {
        return Err(CacheMarkError::AtLimit);
    }
    current.push(msg_index);
    current.sort_unstable();
    set_breakpoints_unchecked(req, current);
    Ok(())
}

/// Read the currently-marked breakpoint indices, sorted ascending.
/// Returns empty if none / extra is not an object.
pub fn get_breakpoints(req: &ChatRequest) -> Vec<u32> {
    req.extra
        .as_object()
        .and_then(|o| o.get(KEY_BREAKPOINTS))
        .and_then(|v| v.as_array())
        .map(|arr| {
            let mut out: Vec<u32> = arr
                .iter()
                .filter_map(|v| v.as_u64())
                .filter_map(|n| u32::try_from(n).ok())
                .collect();
            out.sort_unstable();
            out.dedup();
            out
        })
        .unwrap_or_default()
}

/// Replace the breakpoint set wholesale. Truncates to
/// [`MAX_CACHE_BREAKPOINTS`] (keeps the lowest-indexed entries —
/// earlier prefixes are typically more valuable to cache).
pub fn set_breakpoints(req: &mut ChatRequest, mut indices: Vec<u32>) {
    indices.sort_unstable();
    indices.dedup();
    if indices.len() > MAX_CACHE_BREAKPOINTS {
        tracing::warn!(
            requested = indices.len(),
            cap = MAX_CACHE_BREAKPOINTS,
            "set_breakpoints truncated to API limit"
        );
        indices.truncate(MAX_CACHE_BREAKPOINTS);
    }
    set_breakpoints_unchecked(req, indices);
}

fn set_breakpoints_unchecked(req: &mut ChatRequest, indices: Vec<u32>) {
    let map = extra_as_object(req);
    if indices.is_empty() {
        map.remove(KEY_BREAKPOINTS);
    } else {
        map.insert(KEY_BREAKPOINTS.into(), serde_json::json!(indices));
    }
}

/// Mark the system prompt as cached. No-op if no system prompt set.
pub fn mark_system_cached(req: &mut ChatRequest) {
    extra_as_object(req).insert(KEY_SYSTEM.into(), serde_json::json!(true));
}

/// Mark the last tool definition as cached. No-op if no tools.
pub fn mark_tools_cached(req: &mut ChatRequest) {
    extra_as_object(req).insert(KEY_TOOLS.into(), serde_json::json!(true));
}

/// Read the system-cache flag.
pub fn is_system_cached(req: &ChatRequest) -> bool {
    req.extra
        .as_object()
        .and_then(|o| o.get(KEY_SYSTEM))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Read the tools-cache flag.
pub fn is_tools_cached(req: &ChatRequest) -> bool {
    req.extra
        .as_object()
        .and_then(|o| o.get(KEY_TOOLS))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Strip every cache marker from `extra` and return the values that
/// were present. Used by the Anthropic provider after consuming the
/// markers — keeps them out of the wire request.
pub struct ConsumedCacheMarkers {
    pub breakpoints: Vec<u32>,
    pub cache_system: bool,
    pub cache_tools: bool,
}

pub fn consume_markers(req: &mut ChatRequest) -> ConsumedCacheMarkers {
    let breakpoints = get_breakpoints(req);
    let cache_system = is_system_cached(req);
    let cache_tools = is_tools_cached(req);
    if let Some(map) = req.extra.as_object_mut() {
        map.remove(KEY_BREAKPOINTS);
        map.remove(KEY_SYSTEM);
        map.remove(KEY_TOOLS);
    }
    // If the extra object is now empty, normalise back to null so the
    // serialised wire body matches the no-extras shape exactly.
    if let Some(map) = req.extra.as_object() {
        if map.is_empty() {
            req.extra = serde_json::Value::Null;
        }
    }
    ConsumedCacheMarkers {
        breakpoints,
        cache_system,
        cache_tools,
    }
}

/// Remove every cache marker from `extra`. Used when copying a
/// request for retry (cached prefix may have invalidated).
pub fn clear_markers(req: &mut ChatRequest) {
    let _ = consume_markers(req);
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/prompt/caching.rs"
    ));
}
