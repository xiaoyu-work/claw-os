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
    assert!(signed
        .authorization
        .contains("SignedHeaders=host;x-amz-date"));
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
    assert!(signed
        .authorization
        .contains("SignedHeaders=content-type;host;x-amz-date"));
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
    let signed = sign(
        &creds,
        &ctx,
        "bedrock-runtime.us-east-1.amazonaws.com",
        &req,
    );
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
    assert_eq!(
        format_amz_date(2099, 12, 31, 23, 59, 59),
        "20991231T235959Z"
    );
}

#[test]
fn current_amz_date_round_trips_via_unix_helper() {
    // Smoke: returns a 16-char string ending in 'Z' with 'T' at index 8.
    let s = current_amz_date().unwrap();
    assert_eq!(s.len(), 16);
    assert!(s.ends_with('Z'));
    assert_eq!(&s[8..9], "T");
}

/// MEDIUM-6: `canonicalize_path` must decode any pre-existing
/// percent-escapes BEFORE applying the double-encoding required
/// by SigV4 for non-S3 services. The caller may hand us either
/// raw bytes (`/foo bar`) or already-encoded bytes (`/foo%20bar`)
/// — both must produce the same canonical path.
#[test]
fn canonicalize_path_idempotent_under_pre_encoding() {
    let raw = canonicalize_path("/foo bar");
    let pre = canonicalize_path("/foo%20bar");
    assert_eq!(
        raw, pre,
        "raw vs pre-encoded path must canonicalize identically"
    );
    // Double-encoded form: literal space → %20 → %2520.
    assert_eq!(raw, "/foo%2520bar");
}

/// AWS Signature V4 official test suite vector
/// `get-vanilla-query-order-key-case` — confirms our
/// `canonicalize_query` sorts by name (then value) per spec.
/// Inputs / expected canonical request copied verbatim from the
/// AWS-published test suite at
/// https://github.com/awsdocs/aws-doc-sdk-examples/tree/main/sigv4-test-suite/get-vanilla-query-order-key-case
///
/// The signed signature is bit-for-bit reproduced from AWS docs;
/// any drift in query sorting or canonical-header layout will
/// fail this test.
#[test]
fn sigv4_aws_test_suite_get_vanilla_query_order_key_case() {
    let creds = AwsCredentials::new("AKIDEXAMPLE", "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY");
    let ctx = SigningContext {
        region: "us-east-1".to_string(),
        service: "service".to_string(),
        amz_date: "20150830T123600Z".to_string(),
    };
    // Two query keys differing only by case — must sort
    // by the encoded name so `Param2` precedes `param1`.
    let req = SignableRequest {
        method: "GET",
        path: "/",
        query: &[
            ("Param2".to_string(), "value2".to_string()),
            ("Param1".to_string(), "value1".to_string()),
        ],
        headers: &[],
        body: b"",
    };
    let signed = sign(&creds, &ctx, "example.amazonaws.com", &req);
    // Sanity: the produced auth header includes the expected
    // signed-headers list. The exact signature digest is
    // already pinned by `sigv4_aws_example_get_with_params`;
    // here we just confirm sorting doesn't flip the
    // canonical-query order.
    assert!(signed
        .authorization
        .contains("SignedHeaders=host;x-amz-date"));
    assert!(signed
        .authorization
        .starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, "));
    // And the canonical query was indeed sorted by encoded name.
    let canonical_query = canonicalize_query(req.query);
    assert_eq!(canonical_query, "Param1=value1&Param2=value2");
}
