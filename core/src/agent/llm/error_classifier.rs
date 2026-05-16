//! Map provider HTTP responses to a `FailureClass` for credential-pool feedback.
//!
//! Providers all return errors with subtly different shapes (Anthropic's
//! `{type: "error", error: {type, message}}`, OpenAI's `{error: {code,
//! message, type}}`, Gemini's `{error: {status, message}}`, etc.). This
//! module gives every provider a single helper to call so that the
//! credential pool can react uniformly:
//!
//! - **CooldownWorthy**: auth/quota — the *key* is bad. Skip it for the
//!   cooldown window.
//! - **Transient**: 5xx / network — retry on the same key (or any key).
//! - **CallerError**: 4xx that's our fault (validation, model not found).
//!   Don't blame the key.
//!
//! The classifier intentionally does NOT take Anthropic-specific JSON shapes
//! — providers extract a `(status: u16, message: &str)` and let this module
//! decide. Keyword matches use lowercase substring search; for unknown 4xx
//! it errs toward `CallerError` (don't punish the key for bad requests we
//! sent it).

use crate::agent::llm::credential_pool::FailureClass;

/// Decide what kind of failure this HTTP response represents.
///
/// `status` is the HTTP status code from the provider response, `message`
/// is the human-readable error string (anywhere from "" to a multi-line
/// stack trace — we only look at lowercased substrings).
///
/// Rules, in priority order:
///
/// 1. **401 / 403** → `CooldownWorthy` (auth)
/// 2. **429** → `CooldownWorthy` (rate limit / quota — the key is hot)
/// 3. **5xx** → `Transient`
/// 4. **4xx with quota/billing/credit/permission keywords** → `CooldownWorthy`
///    (some providers return 400/402 for over-quota or expired card)
/// 5. **4xx (default)** → `CallerError`
/// 6. **2xx**: caller probably shouldn't be here, but → `CallerError`
///    (we don't have a "this isn't an error" variant — caller should
///    only invoke `classify` after deciding the request failed)
pub fn classify(status: u16, message: &str) -> FailureClass {
    if status >= 500 {
        return FailureClass::Transient;
    }

    // Auth-class statuses are unambiguous.
    if status == 401 || status == 403 {
        return FailureClass::CooldownWorthy;
    }

    // 429 = rate limit / quota. Always cooldown — we don't know if it's
    // a brief burst or a daily quota cap, but the key is hot either way.
    if status == 429 {
        return FailureClass::CooldownWorthy;
    }

    // CALLER-SIDE 4xx short-circuit. Some upstream error bodies on
    // these statuses contain words that overlap with our quota
    // vocabulary ("context length exceeded", "model not found",
    // "model deprecated / expired") but are NOT key-quota issues
    // — they're our request's fault. Punishing the key for them
    // would cause innocent keys to enter cooldown on every bad
    // prompt. Detect the unambiguous caller-side cases first.
    if is_caller_side_4xx(status, message) {
        return FailureClass::CallerError;
    }

    // 4xx with billing/quota signals: providers occasionally return 400
    // or 402 instead of 429 when a key is over its hard limit or the
    // account is suspended.
    if (400..500).contains(&status) && message_indicates_quota(message) {
        return FailureClass::CooldownWorthy;
    }

    // Everything else: treat as caller error so we don't punish the key
    // for our bad request body / unknown model / wrong tool schema.
    FailureClass::CallerError
}

/// Recognise 4xx responses that come from a *caller-side* problem
/// (request shape, model identifier, content length) rather than the
/// key's entitlement. Specifically catches the false-positive overlaps
/// between quota keyword heuristics and these legitimate caller errors:
///
/// * `400 context_length_exceeded` — the word "exceeded" used to match
///   `QUOTA_KEYWORDS` and cool the key down.
/// * `404 model_not_found` — caller asked for a model that doesn't exist.
/// * `410 model_deprecated / model expired` — the word "expired" used
///   to match `QUOTA_KEYWORDS`.
fn is_caller_side_4xx(status: u16, message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    match status {
        400 => {
            m.contains("context_length_exceeded")
                || m.contains("context length exceeded")
                || m.contains("maximum context length")
                || m.contains("string too long")
                || m.contains("invalid_request_error")
                || m.contains("invalid request")
        }
        404 => {
            m.contains("model_not_found")
                || m.contains("model not found")
                || m.contains("not_found_error")
        }
        410 => {
            m.contains("model_deprecated")
                || m.contains("model deprecated")
                || m.contains("model expired")
                || m.contains("model has expired")
                || m.contains("deprecated")
        }
        _ => false,
    }
}

/// Network-level failure (connect/timeout/dns) before any HTTP response.
/// Always transient — try again, or try a different key.
#[inline]
pub fn classify_network_error() -> FailureClass {
    FailureClass::Transient
}

fn message_indicates_quota(message: &str) -> bool {
    if message.is_empty() {
        return false;
    }
    let m = message.to_ascii_lowercase();
    QUOTA_KEYWORDS.iter().any(|kw| m.contains(kw))
}

/// Keywords providers use across error messages to signal the caller
/// has hit a quota / billing / permission cap on this key. Lowercased.
///
/// IMPORTANT: keep these narrow enough that they only fire for genuine
/// account-state problems, not transient caller errors. Words like
/// `exceeded` / `expired` / `out of` used to be in this list but
/// matched too aggressively (context-length-exceeded, model-expired)
/// — those caller-side cases now short-circuit via `is_caller_side_4xx`
/// before this set is consulted.
const QUOTA_KEYWORDS: &[&str] = &[
    "quota exceeded",
    "quota_exceeded",
    "rate_limit",
    "rate limit",
    "ratelimit",
    "billing",
    "credit",
    "credits",
    "insufficient_quota",
    "insufficient quota",
    "over_capacity",
    "over capacity",
    "suspended",
    "permission_denied",
    "permission denied",
    "hard_limit",
    "hard limit",
    "monthly_budget",
    "monthly budget",
    "weekly_limit",
    "weekly limit",
    "spending limit",
    "payment required",
    "plan exceeded",
    "plan_exceeded",
    "account_deactivated",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_hundred_is_transient() {
        assert_eq!(classify(500, "internal error"), FailureClass::Transient);
        assert_eq!(classify(502, "bad gateway"), FailureClass::Transient);
        assert_eq!(classify(503, ""), FailureClass::Transient);
        assert_eq!(classify(504, "timeout"), FailureClass::Transient);
        assert_eq!(classify(599, "weird"), FailureClass::Transient);
    }

    #[test]
    fn auth_codes_are_cooldown_worthy() {
        assert_eq!(
            classify(401, "invalid api key"),
            FailureClass::CooldownWorthy
        );
        assert_eq!(classify(403, "forbidden"), FailureClass::CooldownWorthy);
    }

    #[test]
    fn rate_limit_is_cooldown_worthy() {
        assert_eq!(classify(429, ""), FailureClass::CooldownWorthy);
        assert_eq!(
            classify(429, "Too many requests"),
            FailureClass::CooldownWorthy
        );
    }

    #[test]
    fn quota_in_400_message_triggers_cooldown() {
        assert_eq!(
            classify(400, "You exceeded your current quota. quota exceeded"),
            FailureClass::CooldownWorthy
        );
        assert_eq!(
            classify(402, "Payment required"),
            FailureClass::CooldownWorthy
        );
        assert_eq!(
            classify(400, "billing details required"),
            FailureClass::CooldownWorthy
        );
        assert_eq!(
            classify(403, "permission_denied: api key suspended"),
            FailureClass::CooldownWorthy
        );
    }

    #[test]
    fn quota_keyword_case_insensitive() {
        assert_eq!(
            classify(400, "INSUFFICIENT_QUOTA"),
            FailureClass::CooldownWorthy
        );
        assert_eq!(
            classify(400, "Out Of Credits — billing required"),
            FailureClass::CooldownWorthy
        );
    }

    /// Regression: callers used to leak into the keyword path because
    /// of the bare `"exceeded"` / `"expired"` / `"out of"` keywords.
    /// These are caller-side errors, NOT key-quota issues — the pool
    /// must not cool a key down on a context-length-exceeded prompt
    /// or a model-deprecated misconfig, because all keys share the
    /// same fate (and the key itself is fine).
    #[test]
    fn caller_side_4xx_not_quota() {
        // OpenAI 400 / context_length_exceeded.
        assert_eq!(
            classify(400, "This model's maximum context length is 128000 tokens; context_length_exceeded"),
            FailureClass::CallerError
        );
        assert_eq!(
            classify(400, "Request too long: context length exceeded"),
            FailureClass::CallerError
        );
        // OpenAI / Anthropic / xAI 404 / model_not_found.
        assert_eq!(
            classify(404, "model_not_found: gpt-99-turbo"),
            FailureClass::CallerError
        );
        assert_eq!(
            classify(404, "The model `gpt-99-turbo` was not found"),
            FailureClass::CallerError
        );
        // Anthropic 410 / model_deprecated.
        assert_eq!(
            classify(410, "model_deprecated: claude-instant-v1 expired 2024-07-21"),
            FailureClass::CallerError
        );
        assert_eq!(
            classify(410, "this model has expired and is no longer available"),
            FailureClass::CallerError
        );
        // 400 invalid_request_error must not trip the legacy keywords.
        assert_eq!(
            classify(400, "invalid_request_error: malformed tool schema"),
            FailureClass::CallerError
        );
    }

    #[test]
    fn plain_400_is_caller_error() {
        assert_eq!(
            classify(400, "invalid model: gpt-99"),
            FailureClass::CallerError
        );
        assert_eq!(classify(404, "model not found"), FailureClass::CallerError);
        assert_eq!(
            classify(422, "validation failed"),
            FailureClass::CallerError
        );
        assert_eq!(classify(400, ""), FailureClass::CallerError);
    }

    #[test]
    fn empty_message_doesnt_match_quota() {
        assert_eq!(classify(400, ""), FailureClass::CallerError);
    }

    #[test]
    fn network_error_is_transient() {
        assert_eq!(classify_network_error(), FailureClass::Transient);
    }

    #[test]
    fn two_xx_falls_through_to_caller_error() {
        // shouldn't be called on 2xx, but defined behavior
        assert_eq!(classify(200, "ok"), FailureClass::CallerError);
        assert_eq!(classify(204, ""), FailureClass::CallerError);
    }

    #[test]
    fn anthropic_overloaded_529_is_transient() {
        // Anthropic returns 529 for "overloaded" — not the caller's fault,
        // not specifically the key's fault either; retry.
        assert_eq!(classify(529, "Overloaded"), FailureClass::Transient);
    }

    #[test]
    fn weekly_limit_keyword_caught() {
        assert_eq!(
            classify(400, "weekly_limit reached"),
            FailureClass::CooldownWorthy
        );
        assert_eq!(
            classify(429, "weekly limit reached"),
            FailureClass::CooldownWorthy
        );
    }

    #[test]
    fn classify_doesnt_panic_on_unicode() {
        // Confirm to_ascii_lowercase doesn't choke on non-ASCII (it's
        // intentionally ASCII-only — Unicode-fold is overkill here).
        assert_eq!(classify(400, "配额不足"), FailureClass::CallerError);
        assert_eq!(
            classify(400, "配额不足 quota exceeded"),
            FailureClass::CooldownWorthy
        );
    }

    #[test]
    fn whitespace_only_message_is_caller_error() {
        // " " is non-empty but contains no keywords.
        assert_eq!(classify(400, "   "), FailureClass::CallerError);
    }
}
