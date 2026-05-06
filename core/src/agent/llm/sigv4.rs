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
    let credential_scope = format!(
        "{}/{}/{}/aws4_request",
        date_stamp, ctx.region, ctx.service
    );
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
/// * Each segment is URI-encoded with the unreserved set, then
///   re-encoded a *second* time (SigV4 explicitly requires double
///   encoding for non-S3 services). Bedrock is non-S3, so we double
///   encode.
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
                let once = uri_encode(segment, false);
                out.push_str(&uri_encode(&once, false));
            }
        } else {
            out.push('/');
            let once = uri_encode(segment, false);
            out.push_str(&uri_encode(&once, false));
        }
    }
    if out.is_empty() {
        out.push('/');
    }
    out
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
    use super::*;

    /// AWS Signature V4 reference implementation cross-check, regression
    /// vector. The `service` literal here matches AWS's example-page
    /// convention. Input/output were independently computed via PowerShell's
    /// `System.Security.Cryptography.HMACSHA256` to confirm:
    ///
    ///   GET /?Param1=value1
    ///   host: example.amazonaws.com
    ///   x-amz-date: 20150830T123600Z
    ///
    /// → Signature: a67d582fa61cc504c4bae71f336f98b97f1ea3c7a6bfe1b6e45aec72011b9aeb
    ///
    /// Fixture must stay byte-stable: any drift in canonicalization,
    /// HMAC chaining, or scope formatting will break this.
    #[test]
    fn sigv4_aws_example_get_with_params() {
        let creds = AwsCredentials::new("AKIDEXAMPLE", "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY");
        let ctx = SigningContext {
            region: "us-east-1".to_string(),
            service: "service".to_string(),
            amz_date: "20150830T123600Z".to_string(),
        };
        let req = SignableRequest {
            method: "GET",
            path: "/",
            query: &[("Param1".to_string(), "value1".to_string())],
            headers: &[],
            body: b"",
        };
        let signed = sign(&creds, &ctx, "example.amazonaws.com", &req);
        assert!(
            signed.authorization.contains(
                "Signature=a67d582fa61cc504c4bae71f336f98b97f1ea3c7a6bfe1b6e45aec72011b9aeb"
            ),
            "got authorization: {}",
            signed.authorization
        );
        // SignedHeaders order must be lexicographic.
        assert!(signed.authorization.contains("SignedHeaders=host;x-amz-date"));
    }

    /// Same idea as the GET vector but with a body and a Content-Type
    /// header that the caller wants signed. Cross-checked via the same
    /// PowerShell HMACSHA256 path:
    ///
    ///   POST / (body: "Param1=value1")
    ///   content-type: application/x-www-form-urlencoded; charset=utf-8
    ///   host: example.amazonaws.com
    ///   x-amz-date: 20150830T123600Z
    ///
    /// → Signature: 2f3b42f35f135abf9c562afcbbc44fc03df96dcfd4332ecebad8b39a7d4b6125
    #[test]
    fn sigv4_aws_example_post_with_content_type_header() {
        let creds = AwsCredentials::new("AKIDEXAMPLE", "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY");
        let ctx = SigningContext {
            region: "us-east-1".to_string(),
            service: "service".to_string(),
            amz_date: "20150830T123600Z".to_string(),
        };
        let req = SignableRequest {
            method: "POST",
            path: "/",
            query: &[],
            headers: &[(
                "content-type".to_string(),
                "application/x-www-form-urlencoded; charset=utf-8".to_string(),
            )],
            body: b"Param1=value1",
        };
        let signed = sign(&creds, &ctx, "example.amazonaws.com", &req);
        assert!(
            signed.authorization.contains(
                "Signature=2f3b42f35f135abf9c562afcbbc44fc03df96dcfd4332ecebad8b39a7d4b6125"
            ),
            "got authorization: {}",
            signed.authorization
        );
        assert!(
            signed
                .authorization
                .contains("SignedHeaders=content-type;host;x-amz-date")
        );
    }

    #[test]
    fn signed_headers_pairs_include_security_token_when_present() {
        let creds = AwsCredentials::new("AKID", "secret").with_session_token("FwoG...");
        let ctx = SigningContext {
            region: "us-east-1".to_string(),
            service: "bedrock".to_string(),
            amz_date: "20240101T000000Z".to_string(),
        };
        let req = SignableRequest {
            method: "POST",
            path: "/model/foo/invoke",
            query: &[],
            headers: &[],
            body: b"{}",
        };
        let signed = sign(&creds, &ctx, "bedrock-runtime.us-east-1.amazonaws.com", &req);
        let pairs = signed.as_header_pairs();
        let names: Vec<&str> = pairs.iter().map(|(k, _)| *k).collect();
        assert!(names.contains(&"x-amz-security-token"));
        assert!(names.contains(&"Authorization"));
        assert!(names.contains(&"x-amz-date"));
        assert!(names.contains(&"x-amz-content-sha256"));
    }

    #[test]
    fn signed_headers_pairs_omit_security_token_when_absent() {
        let creds = AwsCredentials::new("AKID", "secret");
        let ctx = SigningContext {
            region: "us-east-1".to_string(),
            service: "bedrock".to_string(),
            amz_date: "20240101T000000Z".to_string(),
        };
        let req = SignableRequest {
            method: "POST",
            path: "/",
            query: &[],
            headers: &[],
            body: b"",
        };
        let signed = sign(&creds, &ctx, "host.example", &req);
        let names: Vec<&str> = signed.as_header_pairs().iter().map(|(k, _)| *k).collect();
        assert!(!names.contains(&"x-amz-security-token"));
    }

    /// Two requests with identical bodies + ctx but different session
    /// tokens MUST produce different signatures — the token is a
    /// signed header.
    #[test]
    fn session_token_changes_signature() {
        let ctx = SigningContext {
            region: "us-east-1".to_string(),
            service: "bedrock".to_string(),
            amz_date: "20240101T000000Z".to_string(),
        };
        let req = SignableRequest {
            method: "POST",
            path: "/model/foo/invoke",
            query: &[],
            headers: &[],
            body: b"{\"x\":1}",
        };
        let no_tok = sign(
            &AwsCredentials::new("AKID", "secret"),
            &ctx,
            "host.example",
            &req,
        );
        let with_tok = sign(
            &AwsCredentials::new("AKID", "secret").with_session_token("TOK"),
            &ctx,
            "host.example",
            &req,
        );
        assert_ne!(no_tok.authorization, with_tok.authorization);
    }

    #[test]
    fn body_change_changes_signature() {
        let creds = AwsCredentials::new("AKID", "secret");
        let ctx = SigningContext {
            region: "us-east-1".to_string(),
            service: "bedrock".to_string(),
            amz_date: "20240101T000000Z".to_string(),
        };
        let mk = |body: &'static [u8]| {
            sign(
                &creds,
                &ctx,
                "host.example",
                &SignableRequest {
                    method: "POST",
                    path: "/",
                    query: &[],
                    headers: &[],
                    body,
                },
            )
        };
        let a = mk(b"a");
        let b = mk(b"b");
        assert_ne!(a.authorization, b.authorization);
        assert_ne!(a.x_amz_content_sha256, b.x_amz_content_sha256);
    }

    #[test]
    fn date_change_changes_signature() {
        let creds = AwsCredentials::new("AKID", "secret");
        let req = SignableRequest {
            method: "POST",
            path: "/",
            query: &[],
            headers: &[],
            body: b"{}",
        };
        let mk = |amz: &str| {
            sign(
                &creds,
                &SigningContext {
                    region: "us-east-1".to_string(),
                    service: "bedrock".to_string(),
                    amz_date: amz.to_string(),
                },
                "host.example",
                &req,
            )
        };
        assert_ne!(
            mk("20240101T000000Z").authorization,
            mk("20240102T000000Z").authorization
        );
    }

    // === Pure helper unit tests ===

    #[test]
    fn uri_encode_unreserved_passes_through() {
        assert_eq!(uri_encode("AbZ-_.~09", false), "AbZ-_.~09");
    }

    #[test]
    fn uri_encode_slash_default_encoded() {
        assert_eq!(uri_encode("/foo/bar", false), "%2Ffoo%2Fbar");
    }

    #[test]
    fn uri_encode_keep_slash_preserves_slash() {
        assert_eq!(uri_encode("/foo/bar", true), "/foo/bar");
    }

    #[test]
    fn uri_encode_special_chars() {
        // Space, +, =, & all need percent-encoding.
        assert_eq!(uri_encode("a b+c=d&e", false), "a%20b%2Bc%3Dd%26e");
    }

    #[test]
    fn canonicalize_path_double_encodes_for_non_s3() {
        // `%` itself becomes `%25` after the second pass.
        let p = canonicalize_path("/foo bar/baz");
        // first: "foo bar" → "foo%20bar"; second pass on that segment:
        // "%" → "%25", "20" stays, so "foo%2520bar".
        assert_eq!(p, "/foo%2520bar/baz");
    }

    #[test]
    fn canonicalize_path_empty_to_slash() {
        assert_eq!(canonicalize_path(""), "/");
    }

    #[test]
    fn canonicalize_query_sorts_lexicographically() {
        let q = canonicalize_query(&[
            ("z".to_string(), "1".to_string()),
            ("a".to_string(), "2".to_string()),
            ("m".to_string(), "3".to_string()),
        ]);
        assert_eq!(q, "a=2&m=3&z=1");
    }

    #[test]
    fn canonicalize_query_encodes_special_chars() {
        let q = canonicalize_query(&[("name=tag".to_string(), "value+1".to_string())]);
        assert_eq!(q, "name%3Dtag=value%2B1");
    }

    #[test]
    fn trim_header_value_collapses_whitespace() {
        assert_eq!(trim_header_value("  hello   world  "), "hello world");
    }

    #[test]
    fn unix_to_ymdhms_known_dates() {
        // 0 → 1970-01-01T00:00:00Z
        assert_eq!(unix_to_ymdhms(0), (1970, 1, 1, 0, 0, 0));
        // 1577836800 → 2020-01-01T00:00:00Z
        assert_eq!(unix_to_ymdhms(1_577_836_800), (2020, 1, 1, 0, 0, 0));
        // 1704067199 → 2023-12-31T23:59:59Z
        assert_eq!(unix_to_ymdhms(1_704_067_199), (2023, 12, 31, 23, 59, 59));
        // 1672531200 → 2023-01-01T00:00:00Z (leap year boundary check)
        assert_eq!(unix_to_ymdhms(1_672_531_200), (2023, 1, 1, 0, 0, 0));
        // 1582934400 → 2020-02-29T00:00:00Z (leap day)
        assert_eq!(unix_to_ymdhms(1_582_934_400), (2020, 2, 29, 0, 0, 0));
    }

    #[test]
    fn format_amz_date_is_compact_iso8601() {
        assert_eq!(format_amz_date(2024, 5, 1, 12, 0, 0), "20240501T120000Z");
        assert_eq!(format_amz_date(2099, 12, 31, 23, 59, 59), "20991231T235959Z");
    }

    #[test]
    fn current_amz_date_round_trips_via_unix_helper() {
        // Smoke: returns a 16-char string ending in 'Z' with 'T' at index 8.
        let s = current_amz_date().unwrap();
        assert_eq!(s.len(), 16);
        assert!(s.ends_with('Z'));
        assert_eq!(&s[8..9], "T");
    }
}
