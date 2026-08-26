use super::*;

fn r() -> Redactor {
    Redactor::default_set()
}

#[test]
fn no_secrets_passes_through_unchanged() {
    let s = "the quick brown fox 1234567890";
    assert_eq!(r().redact(s), s);
    assert!(!r().contains_secrets(s));
}

#[test]
fn openai_style_sk_key_redacted() {
    let s = "key = sk-abcdef0123456789ABCDEFXYZ123";
    let out = r().redact(s);
    assert!(out.contains("[REDACTED:api_key]"));
    assert!(!out.contains("sk-abcdef"));
}

#[test]
fn anthropic_sk_ant_key_redacted() {
    let s = "Bearer sk-ant-api03-aaaaaaaaaaaaaaaaaaaaaaaa";
    let out = r().redact(s);
    // The Bearer pattern fires first; the sk-ant token lives inside
    // the bearer token and gets redacted as bearer-token.
    assert!(out.contains("[REDACTED:bearer]") || out.contains("[REDACTED:api_key]"));
    assert!(!out.contains("sk-ant-api03-aaaaaaaa"));
}

#[test]
fn github_classic_pat_redacted() {
    let s = "use ghp_aBcD1234aBcD1234aBcD1234aBcD1234abcd";
    let out = r().redact(s);
    assert!(out.contains("[REDACTED:github_token]"));
    assert!(!out.contains("ghp_a"));
}

#[test]
fn github_fine_grained_pat_redacted() {
    let s = "token github_pat_11ABCDEFG0wxyz0123456789_aBcDeFgHiJkLmNoPqRsTuVwXyZ";
    let out = r().redact(s);
    assert!(out.contains("[REDACTED:github_token]"));
    assert!(!out.contains("github_pat_11"));
}

#[test]
fn gitlab_pat_redacted() {
    let s = "glpat-AbCdEfGhIjKlMnOp";
    let out = r().redact(s);
    assert!(out.contains("[REDACTED:gitlab_token]"));
}

#[test]
fn slack_token_redacted() {
    let s = "xoxb-1234-5678-AAAAAAAAAAAA";
    assert!(r().redact(s).contains("[REDACTED:slack_token]"));
}

#[test]
fn aws_access_key_redacted() {
    let s = "AKIAIOSFODNN7EXAMPLE";
    assert!(r().redact(s).contains("[REDACTED:aws_access_key]"));
}

#[test]
fn aws_temporary_access_key_redacted() {
    let s = "ASIAIOSFODNN7EXAMPLE";
    assert!(r().redact(s).contains("[REDACTED:aws_access_key]"));
}

#[test]
fn google_api_key_redacted() {
    let s = "key=AIzaSyA-aaaaaaaaaaaaaaaaaaaaaaaaaaaa1234";
    assert!(r().redact(s).contains("[REDACTED:google_api_key]"));
}

#[test]
fn bearer_token_redacted_keeping_surrounding_context() {
    let s = "Authorization: Bearer abcd1234efgh5678";
    let out = r().redact(s);
    assert!(out.starts_with("Authorization: Bearer "));
    assert!(out.contains("[REDACTED:bearer]"));
    assert!(!out.contains("abcd1234efgh5678"));
}

#[test]
fn bearer_case_insensitive() {
    let s = "BEARER abcd1234efgh5678";
    assert!(r().redact(s).contains("[REDACTED:bearer]"));
}

#[test]
fn url_credentials_redacted_keeping_host() {
    let s = "https://alice:hunter2@example.com/path";
    let out = r().redact(s);
    assert!(out.contains("[REDACTED:url_credential]"));
    assert!(out.contains("example.com"));
    assert!(!out.contains("alice:hunter2"));
}

#[test]
fn url_without_credentials_unchanged() {
    let s = "https://example.com/path";
    assert_eq!(r().redact(s), s);
}

#[test]
fn private_key_block_fully_redacted() {
    let s =
        "before\n-----BEGIN PRIVATE KEY-----\nMIIE...AAAA\n-----END PRIVATE KEY-----\nafter";
    let out = r().redact(s);
    assert!(out.starts_with("before"));
    assert!(out.contains("[REDACTED:private_key]"));
    assert!(out.ends_with("after"));
    assert!(!out.contains("MIIE"));
}

#[test]
fn rsa_private_key_block_redacted() {
    let s = "-----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----";
    assert!(r().redact(s).contains("[REDACTED:private_key]"));
}

#[test]
fn jwt_redacted() {
    let s = "token: eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ4eHgifQ.signature123_abc";
    assert!(r().redact(s).contains("[REDACTED:jwt]"));
}

#[test]
fn email_unchanged_by_default() {
    let s = "contact alice@example.com for info";
    let out = r().redact(s);
    assert!(out.contains("alice@example.com"));
}

#[test]
fn email_redacted_in_strict_mode() {
    let s = "contact alice@example.com for info";
    let out = Redactor::strict().redact(s);
    assert!(out.contains("[REDACTED:email]"));
    assert!(!out.contains("alice@example.com"));
}

#[test]
fn multiple_secrets_all_redacted() {
    let s = "AKIAIOSFODNN7EXAMPLE and ghp_aBcD1234aBcD1234aBcD1234aBcD1234abcd";
    let out = r().redact(s);
    assert!(out.contains("[REDACTED:aws_access_key]"));
    assert!(out.contains("[REDACTED:github_token]"));
}

#[test]
fn contains_secrets_truthy_when_present() {
    assert!(r().contains_secrets("Bearer abcd1234efgh5678"));
    assert!(!r().contains_secrets("nothing to see here"));
}

#[test]
fn placeholder_for_each_kind_is_stable() {
    for k in [
        SecretKind::ApiKey,
        SecretKind::GithubToken,
        SecretKind::GitlabToken,
        SecretKind::SlackToken,
        SecretKind::DiscordToken,
        SecretKind::AwsAccessKey,
        SecretKind::GoogleApiKey,
        SecretKind::Bearer,
        SecretKind::UrlCredential,
        SecretKind::PrivateKey,
        SecretKind::Jwt,
        SecretKind::Email,
    ] {
        let p = k.placeholder();
        assert!(p.starts_with("[REDACTED:"));
        assert!(p.ends_with("]"));
    }
}

#[test]
fn redactor_is_idempotent() {
    let s = "sk-abcdef0123456789ABCDEFXYZ123";
    let r = r();
    let once = r.redact(s);
    let twice = r.redact(&once);
    assert_eq!(once, twice);
}

#[test]
fn redactor_default_count_includes_all_patterns() {
    assert_eq!(Redactor::default_set().pattern_count(), 13);
    assert_eq!(Redactor::strict().pattern_count(), 14);
}

#[test]
fn empty_string_unchanged() {
    assert_eq!(r().redact(""), "");
    assert!(!r().contains_secrets(""));
}

#[test]
fn url_credential_does_not_match_path_with_colon() {
    // path-like 'http://example.com/foo:bar' has a ':' after the
    // host but no '@', so it must NOT trigger the URL-cred pattern.
    let s = "http://example.com/foo:bar";
    assert_eq!(r().redact(s), s);
}

/// Discord-token regex used to fire on any `<b64>.<b64>.<b64>` blob
/// (long opaque OIDC ID tokens, capability bundles, etc.). The
/// tightened pattern requires the `M/N/O` snowflake-id prefix, so
/// a non-Discord triple-dotted blob passes through unchanged.
#[test]
fn discord_rule_does_not_match_arbitrary_triple_dotted_blob() {
    // Non-Discord token (starts with `Z`, not M/N/O). Must not be
    // matched as Discord. (It may still match the JWT rule above,
    // but only when the first segment starts with `eyJ`.)
    let s = "ZGFiY2RlZmdoaWprbG1ub3BxcnN0dXY=.YWJjZGVm.dGhpc2lzbm90YWRpc2NvcmR0b2tlbmFsc28=";
    let out = r().redact(s);
    assert!(
        !out.contains("[REDACTED:discord_token]"),
        "non-Discord triple-dotted blob must not match the Discord rule, got {out}"
    );
}

/// Positive control: verify the redactor's machinery (pattern match +
/// placeholder substitution) actually does its job for the Discord
/// kind. We use a custom benign pattern (`token123`) instead of a
/// real-shaped Discord token so that GitHub push-protection / secret-
/// scanning does not flag this fixture. The production Discord regex
/// is exercised indirectly by `discord_rule_does_not_match_*` (above)
/// and by `redactor_default_count_includes_all_patterns`.
#[test]
fn discord_kind_substitutes_placeholder() {
    let custom = Redactor::with_patterns(vec![Pattern {
        re: Regex::new(r"\btoken123\b").unwrap(),
        kind: SecretKind::DiscordToken,
        group: 0,
    }]);
    let out = custom.redact("hello token123 world");
    assert!(
        out.contains("[REDACTED:discord_token]"),
        "Discord placeholder must be substituted, got {out}"
    );
    assert!(!out.contains("token123"));
}
