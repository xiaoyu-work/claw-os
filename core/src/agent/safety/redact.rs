//! Sensitive-data redaction.
//!
//! Scrubs API keys, bearer tokens, AWS credentials, GitHub PATs,
//! private-key blocks, URL passwords, and (optionally) email addresses
//! out of strings before they get logged, persisted, or shipped to a
//! cloud provider.
//!
//! ## Threat model
//!
//! The agent loop touches credentials in three places that each carry
//! a meaningful exfiltration risk:
//!
//!   1. **Tool results** that originate from the user's filesystem or
//!      shell — `.env` files, `aws/credentials`, error messages
//!      embedding API keys, etc. If the model includes them in its
//!      next chat request, they ship to the cloud provider.
//!   2. **Run logs / audit trails** persisted to disk. If the model
//!      ever reasoned about a secret out loud, the secret ends up in
//!      `ai.jsonl`.
//!   3. **Memory FTS** — secrets land in `memory.db` and become
//!      searchable forever.
//!
//! `Redactor` is a string-in / string-out preprocessor wired into any
//! of these sinks at the caller's discretion. We deliberately *do
//! not* hook it into the runtime by default in this commit — adding
//! it on every Provider call is a behaviour change that needs its own
//! review. Library only here.
//!
//! ## What we redact
//!
//! Built-in patterns catch the high-value, low-false-positive cases:
//!
//!   * Generic `sk-` prefix keys (OpenAI, Anthropic, xAI, DeepSeek
//!     all share the prefix today).
//!   * GitHub PATs / OAuth tokens (`ghp_`, `gho_`, `ghu_`, `ghs_`,
//!     `ghr_`, `github_pat_`).
//!   * GitLab PATs (`glpat-`).
//!   * Slack tokens (`xoxb-`, `xoxa-`, `xoxp-`, `xoxr-`, `xoxs-`).
//!   * Discord bot tokens (3 base64 segments separated by `.`).
//!   * AWS access key ids (`AKIA[0-9A-Z]{16}`) and `AKIASIA…`
//!     temporary keys.
//!   * Google API keys (`AIza...`).
//!   * `Bearer <token>` in HTTP `Authorization` headers.
//!   * URL credentials of the form `scheme://user:pass@host`.
//!   * PEM-style PRIVATE KEY blocks.
//!   * JWT-shaped tokens (`eyJ...` 3 base64-segments).
//!   * Optionally: email addresses (off by default — they're
//!     legitimate content most of the time).
//!
//! ## Output format
//!
//! Each match is replaced with `[REDACTED:<kind>]` where `<kind>` is
//! a stable identifier (`api_key`, `aws_access_key`, `bearer`, etc.).
//! The placeholder length is shorter than most secrets, so a redacted
//! string is generally smaller than the original — fine for downstream
//! token budgets.

use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Kinds of secret a redactor will substitute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretKind {
    ApiKey,
    GithubToken,
    GitlabToken,
    SlackToken,
    DiscordToken,
    AwsAccessKey,
    GoogleApiKey,
    Bearer,
    UrlCredential,
    PrivateKey,
    Jwt,
    Email,
}

impl SecretKind {
    pub fn placeholder(self) -> &'static str {
        match self {
            SecretKind::ApiKey => "[REDACTED:api_key]",
            SecretKind::GithubToken => "[REDACTED:github_token]",
            SecretKind::GitlabToken => "[REDACTED:gitlab_token]",
            SecretKind::SlackToken => "[REDACTED:slack_token]",
            SecretKind::DiscordToken => "[REDACTED:discord_token]",
            SecretKind::AwsAccessKey => "[REDACTED:aws_access_key]",
            SecretKind::GoogleApiKey => "[REDACTED:google_api_key]",
            SecretKind::Bearer => "[REDACTED:bearer]",
            SecretKind::UrlCredential => "[REDACTED:url_credential]",
            SecretKind::PrivateKey => "[REDACTED:private_key]",
            SecretKind::Jwt => "[REDACTED:jwt]",
            SecretKind::Email => "[REDACTED:email]",
        }
    }
}

/// One pattern: a regex + the kind of secret it identifies + an
/// optional capture-group index. When the group is `Some(idx)`, only
/// the captured substring is redacted (used for `Bearer <token>`,
/// `scheme://user:pass@host`, etc., where we want to keep the
/// surrounding context).
struct Pattern {
    re: Regex,
    kind: SecretKind,
    /// Capture group to redact within the match. `0` = whole match.
    group: usize,
}

/// Configurable redactor. Construct with [`Redactor::default_set`] for
/// the production pattern list, or [`Redactor::with_patterns`] to roll
/// your own (for tests or stricter modes).
pub struct Redactor {
    patterns: Vec<Pattern>,
    redact_emails: bool,
}

static EMAIL_RE: OnceLock<Regex> = OnceLock::new();

fn email_regex() -> &'static Regex {
    EMAIL_RE
        .get_or_init(|| Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}").unwrap())
}

impl Redactor {
    /// Production pattern set. Email redaction is OFF by default —
    /// emails are legitimate content most of the time and high-FP if
    /// blanket-redacted.
    pub fn default_set() -> Self {
        Self {
            patterns: default_patterns(),
            redact_emails: false,
        }
    }

    /// Same as [`Self::default_set`] but with email redaction enabled.
    pub fn strict() -> Self {
        let mut r = Self::default_set();
        r.redact_emails = true;
        r
    }

    fn with_patterns(patterns: Vec<Pattern>) -> Self {
        Self {
            patterns,
            redact_emails: false,
        }
    }

    /// True if `s` contains any redactable secret.
    pub fn contains_secrets(&self, s: &str) -> bool {
        for p in &self.patterns {
            if p.re.is_match(s) {
                return true;
            }
        }
        if self.redact_emails && email_regex().is_match(s) {
            return true;
        }
        false
    }

    /// Return a redacted copy of `s`. If nothing matched, the input is
    /// echoed unchanged.
    pub fn redact(&self, s: &str) -> String {
        let mut out = s.to_string();
        for p in &self.patterns {
            // Collect spans first so we don't have iterator-invalidation
            // issues when replacing in-place. Iterate in reverse so
            // earlier offsets stay valid.
            let mut spans: Vec<(usize, usize)> = Vec::new();
            for caps in p.re.captures_iter(&out) {
                let m = match caps.get(p.group) {
                    Some(m) => m,
                    None => continue,
                };
                spans.push((m.start(), m.end()));
            }
            for (start, end) in spans.into_iter().rev() {
                let placeholder = p.kind.placeholder();
                out.replace_range(start..end, placeholder);
            }
        }
        if self.redact_emails {
            let mut spans: Vec<(usize, usize)> = Vec::new();
            for m in email_regex().find_iter(&out) {
                spans.push((m.start(), m.end()));
            }
            for (start, end) in spans.into_iter().rev() {
                out.replace_range(start..end, SecretKind::Email.placeholder());
            }
        }
        out
    }

    pub fn pattern_count(&self) -> usize {
        self.patterns.len() + if self.redact_emails { 1 } else { 0 }
    }
}

impl Default for Redactor {
    fn default() -> Self {
        Self::default_set()
    }
}

fn pat(re: &str, kind: SecretKind, group: usize) -> Pattern {
    Pattern {
        re: Regex::new(re).unwrap_or_else(|e| panic!("invalid redact regex {re:?}: {e}")),
        kind,
        group,
    }
}

/// Build the canonical production pattern list. Order matters when
/// patterns overlap (more specific first wins on the redacted output).
fn default_patterns() -> Vec<Pattern> {
    vec![
        // PEM-style private key blocks. Multiline.
        pat(
            r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
            SecretKind::PrivateKey,
            0,
        ),
        // GitHub family. github_pat_ has a longer prefix; check before ghp_.
        pat(r"github_pat_[A-Za-z0-9_]{20,}", SecretKind::GithubToken, 0),
        pat(r"gh[opusr]_[A-Za-z0-9]{30,}", SecretKind::GithubToken, 0),
        // GitLab.
        pat(r"glpat-[A-Za-z0-9_\-]{10,}", SecretKind::GitlabToken, 0),
        // Slack.
        pat(r"xox[baprs]-[A-Za-z0-9\-]{10,}", SecretKind::SlackToken, 0),
        // AWS access key id (canonical 20-char `AKIA…` form).
        pat(r"AKIA[0-9A-Z]{16}", SecretKind::AwsAccessKey, 0),
        pat(r"ASIA[0-9A-Z]{16}", SecretKind::AwsAccessKey, 0),
        // Google API key. Always 39 chars, fixed `AIza` prefix.
        pat(r"AIza[0-9A-Za-z_\-]{35}", SecretKind::GoogleApiKey, 0),
        // Generic `sk-…` keys (OpenAI / Anthropic / xAI / DeepSeek
        // currently all use this prefix). Permits the various
        // mid-prefix subforms (sk-proj-, sk-ant-, etc.) by accepting
        // any non-whitespace continuation of length >= 20.
        pat(r"sk-[A-Za-z0-9_\-]{20,}", SecretKind::ApiKey, 0),
        // JWTs — three base64url segments separated by '.'.
        pat(
            r"eyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+",
            SecretKind::Jwt,
            0,
        ),
        // Discord bot token: 3 base64-ish segments separated by '.'.
        // First segment ~24 chars (snowflake id b64), then 6, then 27+.
        //
        // The shape `<b64>.<b64>.<b64>` also matches any opaque
        // triple-dotted blob — JWTs, capability bundles, signed URLs —
        // so we anchor the first segment to the Discord-specific
        // `M` / `N` / `O` snowflake-id prefix. Real-world Discord
        // bot tokens always start with one of these (the snowflake
        // id base64-encodes a 64-bit integer < 2^63). The JWT rule
        // (`eyJ…`) above runs first and is preferred for that shape.
        pat(
            r"\b[MNO][A-Za-z0-9_\-]{23,}\.[A-Za-z0-9_\-]{6}\.[A-Za-z0-9_\-]{27,}\b",
            SecretKind::DiscordToken,
            0,
        ),
        // Bearer token in Authorization header. Capture group 1 = token.
        pat(
            r"(?i)bearer\s+([A-Za-z0-9_\-\.=]{8,})",
            SecretKind::Bearer,
            1,
        ),
        // URL with credentials: scheme://user:pass@host
        // Capture group 1 = `user:pass`.
        pat(
            r"(?i)\b[a-z][a-z0-9+.\-]*://([^\s/@]+:[^\s/@]+)@",
            SecretKind::UrlCredential,
            1,
        ),
    ]
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/safety/redact.rs"
    ));
}
