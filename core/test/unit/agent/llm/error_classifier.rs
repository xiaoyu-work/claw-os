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
