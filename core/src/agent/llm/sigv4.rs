//! AWS Signature V4 signing — pure Rust, zero AWS deps.
//!
//! Used by the Bedrock provider to authenticate requests against
//! `bedrock-runtime.<region>.amazonaws.com` without pulling
//! `aws-sigv4` (~5MB of transitive surface). The algorithm is
//! documented at:
//!   <https://docs.aws.amazon.com/general/latest/gr/sigv4-signed-request-examples.html>
//!
//! The signing flow:
//!
//! 1. Build a *canonical request* string from
//!    `METHOD\nURI\nQUERY\nHEADERS\nSIGNED_HEADERS\nPAYLOAD_HASH`.
//! 2. Build a *string to sign* from
//!    `AWS4-HMAC-SHA256\nDATETIME\nSCOPE\nHEX(SHA256(canonical))`.
//! 3. Derive a *signing key* via four chained HMACs:
//!    `kDate = HMAC("AWS4"+secret, date)` →
//!    `kRegion = HMAC(kDate, region)` →
//!    `kService = HMAC(kRegion, service)` →
//!    `kSigning = HMAC(kService, "aws4_request")`.
//! 4. The signature = `HEX(HMAC(kSigning, string_to_sign))`.
//! 5. Emit an `Authorization` header:
//!    `AWS4-HMAC-SHA256 Credential=<access>/<scope>, SignedHeaders=<list>, Signature=<hex>`.
//!
//! Callers always include `host` and `x-amz-date` in the canonical
//! header set, plus `x-amz-security-token` if a session token is
//! provided (STS / SSO / IAM role credentials).
//!
//! ## Scope
//!
//! This module signs in-memory request payloads only — it does not
//! mutate `reqwest::Request` directly because reqwest's API doesn't
//! give us reliable access to the canonical query/header form before
//! send. Instead, [`sign`] returns the headers the caller should
//! attach.

use crate::crypto::{hmac_sha256, hmac_sha256_hex, sha256_hex};
use std::collections::BTreeMap;

/// Long-lived AWS credentials. `session_token` is set when using
/// temporary credentials from STS, IAM roles, or SSO.
#[derive(Debug, Clone)]
pub struct AwsCredentials {
    pub access_key: String,
    pub secret_key: String,
    pub session_token: Option<String>,
}

impl AwsCredentials {
    pub fn new(access_key: impl Into<String>, secret_key: impl Into<String>) -> Self {
        Self {
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            session_token: None,
        }
    }

    pub fn with_session_token(mut self, token: impl Into<String>) -> Self {
        self.session_token = Some(token.into());
        self
    }
}

/// Per-request signing context — region (e.g. `us-east-1`),
/// service (e.g. `bedrock`), and the wall-clock instant the request
/// is being signed at (UTC; only the second-precision matters).
#[derive(Debug, Clone)]
pub struct SigningContext {
    pub region: String,
    pub service: String,
    /// Compact ISO-8601 form: `YYYYMMDDTHHMMSSZ` (e.g. `20240101T120000Z`).
    /// Use [`format_amz_date`] to derive this from a `time::OffsetDateTime`
    /// or system clock.
    pub amz_date: String,
}

/// The signable request — what HTTP method + path + query + headers
/// + body the caller is about to send.
#[derive(Debug, Clone)]
pub struct SignableRequest<'a> {
    pub method: &'a str,
    /// Path portion of the URL, percent-encoded (e.g. `/model/foo/invoke`).
    /// Must start with `/`. Empty path is normalized to `/`.
    pub path: &'a str,
    /// Already-parsed query params. SigV4 sorts these lexicographically
    /// after URI-encoding both names and values (RFC 3986).
    pub query: &'a [(String, String)],
    /// Headers the caller will include in the actual HTTP request.
    /// `host` and `x-amz-date` (and `x-amz-security-token` when
    /// applicable) are auto-injected by [`sign`] if not already
    /// present, so callers don't have to think about them.
    pub headers: &'a [(String, String)],
    pub body: &'a [u8],
}

/// Output of [`sign`] — the headers the caller MUST attach to the
/// outbound HTTP request for AWS to accept the signature.
#[derive(Debug, Clone)]
pub struct SignedHeaders {
    pub authorization: String,
    pub x_amz_date: String,
    pub x_amz_content_sha256: String,
    pub x_amz_security_token: Option<String>,
}

impl SignedHeaders {
    /// Convenience: yield `(name, value)` pairs in the order that
    /// reqwest's `RequestBuilder::header()` expects.
    pub fn as_header_pairs(&self) -> Vec<(&'static str, String)> {
        let mut out = vec![
            ("Authorization", self.authorization.clone()),
            ("x-amz-date", self.x_amz_date.clone()),
            ("x-amz-content-sha256", self.x_amz_content_sha256.clone()),
        ];
        if let Some(t) = &self.x_amz_security_token {
            out.push(("x-amz-security-token", t.clone()));
        }
        out
    }
}

/// Sign an in-memory request. Returns the headers to attach.
///
/// `host` MUST be passed because the caller knows the actual host
/// reqwest will hit (it depends on region / endpoint override).
/// We do NOT extract it from `headers` because the canonical
/// request must include the exact host that the signature was
/// computed over.
///
/// `x-amz-content-sha256` is always returned in [`SignedHeaders`]
/// (callers attach it to the wire request — required by some
/// services like Bedrock) but it is NOT auto-injected into the
/// signed-headers set unless the caller explicitly passes it in
/// `req.headers`. This matches AWS's reference test vectors and
/// gives callers control over whether the body hash header is
/// part of the signature scope.
pub fn sign(
    creds: &AwsCredentials,
    ctx: &SigningContext,
    host: &str,
    req: &SignableRequest<'_>,
) -> SignedHeaders {
    // Step 0: payload hash. AWS docs call this "x-amz-content-sha256".
    let payload_hash = sha256_hex(req.body);

    // Build the canonical-headers map. We always inject host + amz-date
    // (mandatory for SigV4) and x-amz-security-token if present
    // (so that token rotations can't replay an old signature).
    // x-amz-content-sha256 is NOT injected automatically — it stays
    // optional, matching AWS's reference test vectors.
    let mut canonical_headers: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in req.headers {
        canonical_headers.insert(k.to_ascii_lowercase(), trim_header_value(v));
    }
    canonical_headers.insert("host".to_string(), host.to_string());
    canonical_headers.insert("x-amz-date".to_string(), ctx.amz_date.clone());
    if let Some(token) = &creds.session_token {
        canonical_headers.insert("x-amz-security-token".to_string(), token.clone());
    }

    // Step 1: build canonical request.
    let canonical_uri = canonicalize_path(req.path);
    let canonical_query = canonicalize_query(req.query);

    let mut canonical_headers_block = String::new();
    let mut signed_headers_list: Vec<&str> = Vec::with_capacity(canonical_headers.len());
    for (k, v) in &canonical_headers {
        canonical_headers_block.push_str(k);
        canonical_headers_block.push(':');
        canonical_headers_block.push_str(v);
        canonical_headers_block.push('\n');
        signed_headers_list.push(k);
    }
    let signed_headers = signed_headers_list.join(";");

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        req.method.to_ascii_uppercase(),
        canonical_uri,
        canonical_query,
        canonical_headers_block,
        signed_headers,
        payload_hash,
    );

    // Step 2: string to sign.
    let date_stamp = &ctx.amz_date[..8]; // YYYYMMDD
    let credential_scope = format!("{}/{}/{}/aws4_request", date_stamp, ctx.region, ctx.service);
    let canonical_request_hash = sha256_hex(canonical_request.as_bytes());
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        ctx.amz_date, credential_scope, canonical_request_hash
    );

    // Step 3: derive signing key (4 chained HMACs).
    let k_secret = format!("AWS4{}", creds.secret_key);
    let k_date = hmac_sha256(k_secret.as_bytes(), date_stamp.as_bytes());
    let k_region = hmac_sha256(&k_date, ctx.region.as_bytes());
    let k_service = hmac_sha256(&k_region, ctx.service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"aws4_request");

    // Step 4: signature.
    let signature = hmac_sha256_hex(&k_signing, string_to_sign.as_bytes());

    // Step 5: Authorization header.
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
        creds.access_key, credential_scope, signed_headers, signature,
    );

    SignedHeaders {
        authorization,
        x_amz_date: ctx.amz_date.clone(),
        x_amz_content_sha256: payload_hash,
        x_amz_security_token: creds.session_token.clone(),
    }
}

/// Format a system time in SigV4's compact form: `YYYYMMDDTHHMMSSZ`.
///
/// Takes (year, month, day, hour, minute, second). All UTC. We don't
/// pull `chrono` for this — the format is fixed and trivial.
pub fn format_amz_date(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> String {
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        year, month, day, hour, minute, second
    )
}

/// Format the current UTC wall clock as an SigV4 amz-date.
///
/// Uses `std::time::SystemTime` (`UNIX_EPOCH` math) so we don't pull
/// `chrono`. Returns `Err` if the system clock is before 1970.
pub fn current_amz_date() -> Result<String, &'static str> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock before unix epoch")?
        .as_secs();
    let (y, m, d, h, mi, s) = unix_to_ymdhms(secs);
    Ok(format_amz_date(y, m, d, h, mi, s))
}

/// Convert seconds-since-epoch to (year, month, day, hour, minute, second) UTC.
///
/// Uses the algorithm from <http://howardhinnant.github.io/date_algorithms.html>
/// (civil_from_days). Correct for any year ≥ 1970 (year 9999 still works).
pub(crate) fn unix_to_ymdhms(secs: u64) -> (u16, u8, u8, u8, u8, u8) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let hour = (rem / 3600) as u8;
    let minute = ((rem % 3600) / 60) as u8;
    let second = (rem % 60) as u8;

    // civil_from_days: adapted from H. Hinnant.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y0 = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8;
    let y = (y0 + if m <= 2 { 1 } else { 0 }) as u16;

    (y, m, d, hour, minute, second)
}

/// Trim leading/trailing whitespace and collapse internal runs of
/// spaces to a single space, per the SigV4 header-value normalization
/// rules. Tabs are not part of the spec — header values are treated
/// as token strings here.
fn trim_header_value(v: &str) -> String {
    let trimmed = v.trim();
    let mut out = String::with_capacity(trimmed.len());
    let mut prev_space = false;
    for ch in trimmed.chars() {
        if ch == ' ' {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out
}

/// Canonicalize the URI path per SigV4 rules:
///
/// * Empty path → `/`.
/// * For non-S3 services, AWS requires: decode any pre-existing
///   percent-escapes, then URI-encode the segment, then encode the
///   result a *second* time (double-encoding). Decoding first is
///   important — a caller that already URL-encoded the path (e.g.
///   `/foo%20bar`) would otherwise be double-encoded into
///   `/foo%2520bar` then `/foo%252520bar`, breaking signatures.
fn canonicalize_path(path: &str) -> String {
    if path.is_empty() {
        return "/".to_string();
    }
    let mut out = String::with_capacity(path.len() + 8);
    let mut first = true;
    for segment in path.split('/') {
        if first {
            // path starts with '/', so first split yields ""
            first = false;
            if !segment.is_empty() {
                out.push('/');
                let decoded = percent_decode_lossy(segment);
                let once = uri_encode(&decoded, false);
                out.push_str(&uri_encode(&once, false));
            }
        } else {
            out.push('/');
            let decoded = percent_decode_lossy(segment);
            let once = uri_encode(&decoded, false);
            out.push_str(&uri_encode(&once, false));
        }
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}

/// Decode `%XX` escapes back to raw bytes. Anything that isn't a
/// valid `%XX` triplet is passed through literally — this is
/// intentional because hostile inputs shouldn't break signing; the
/// double-encode pass downstream will percent-encode the literal
/// `%` again.
fn percent_decode_lossy(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    // Lossy: if the decoded bytes don't form valid UTF-8 (signing
    // input is URL path text, so this is rare), fall back to the
    // original string — the encode step below handles raw bytes
    // either way via the `bytes()` iterator.
    String::from_utf8(out).unwrap_or_else(|_| input.to_string())
}

/// Canonicalize the query string per SigV4 rules:
///
/// 1. URI-encode each name and value separately.
/// 2. Sort by encoded name (then by encoded value).
/// 3. Join with `&`.
fn canonicalize_query(params: &[(String, String)]) -> String {
    let mut encoded: Vec<(String, String)> = params
        .iter()
        .map(|(k, v)| (uri_encode(k, false), uri_encode(v, false)))
        .collect();
    encoded.sort();
    let mut out = String::new();
    for (i, (k, v)) in encoded.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        out.push_str(k);
        out.push('=');
        out.push_str(v);
    }
    out
}

/// RFC 3986 percent-encoding for SigV4. Unreserved set:
/// `A-Z a-z 0-9 - _ . ~`. Slash is encoded EXCEPT in object keys
/// (S3-only path), so we accept a flag for that — Bedrock callers
/// pass `false`.
fn uri_encode(input: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        let unreserved = byte.is_ascii_alphanumeric()
            || byte == b'-'
            || byte == b'_'
            || byte == b'.'
            || byte == b'~';
        if unreserved || (keep_slash && byte == b'/') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{:02X}", byte));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/llm/sigv4.rs"
    ));
}
