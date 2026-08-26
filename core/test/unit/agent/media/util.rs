use super::*;

#[test]
fn preview_does_not_split_codepoint() {
    // 6-char emoji-ish string, each char up to 4 bytes.
    let s = "héllo🦀world";
    let p = preview(s, 6);
    // 6 chars (+ ellipsis since input is longer) and never panics —
    // byte slicing would have.
    assert_eq!(p.chars().count(), 7);
    assert!(p.ends_with('…'));
}

#[test]
fn preview_passes_through_short_strings() {
    assert_eq!(preview("ok", 100), "ok");
}

#[test]
fn ssrf_blocks_loopback() {
    for url in &[
        "http://127.0.0.1/x",
        "http://localhost/x",
        "http://[::1]/x",
        "http://[::]/x",
    ] {
        let err = assert_safe_outbound(url, false).unwrap_err();
        assert!(matches!(err, MediaError::InvalidRequest(_)), "url={url}");
    }
}

#[test]
fn ssrf_blocks_private_v4() {
    for url in &[
        "http://10.0.0.1/x",
        "http://172.16.0.1/x",
        "http://192.168.1.1/x",
        "http://169.254.169.254/x", // AWS IMDS
    ] {
        let err = assert_safe_outbound(url, false).unwrap_err();
        assert!(matches!(err, MediaError::InvalidRequest(_)), "url={url}");
    }
}

#[test]
fn ssrf_blocks_data_and_file_schemes() {
    for url in &[
        "file:///etc/passwd",
        "data:text/plain;base64,aGk=",
        "gopher://evil/x",
    ] {
        let err = assert_safe_outbound(url, false).unwrap_err();
        assert!(matches!(err, MediaError::InvalidRequest(_)), "url={url}");
    }
}

#[test]
fn ssrf_requires_https_when_asked() {
    // http rejected when require_https.
    let err = assert_safe_outbound("http://example.com/x", true).unwrap_err();
    assert!(matches!(err, MediaError::InvalidRequest(_)));
}

#[tokio::test]
async fn build_safe_client_rejects_loopback_host() {
    // `localhost` resolves to a loopback IP — must be refused.
    let err = build_safe_client_for_str("http://localhost/x", Duration::ZERO)
        .await
        .unwrap_err();
    assert!(matches!(err, MediaError::InvalidRequest(_)));
}

#[tokio::test]
async fn build_safe_client_rejects_loopback_ip_literal() {
    let err = build_safe_client_for_str("http://127.0.0.1/x", Duration::ZERO)
        .await
        .unwrap_err();
    assert!(matches!(err, MediaError::InvalidRequest(_)));
    let err = build_safe_client_for_str("http://[::1]/x", Duration::ZERO)
        .await
        .unwrap_err();
    assert!(matches!(err, MediaError::InvalidRequest(_)));
}

#[tokio::test]
async fn build_safe_client_rejects_link_local_imds() {
    // AWS IMDS link-local — must be refused.
    let err = build_safe_client_for_str("http://169.254.169.254/x", Duration::ZERO)
        .await
        .unwrap_err();
    assert!(matches!(err, MediaError::InvalidRequest(_)));
}

#[tokio::test]
async fn build_safe_client_rejects_unsupported_scheme() {
    let err = build_safe_client_for_str("file:///etc/passwd", Duration::ZERO)
        .await
        .unwrap_err();
    assert!(matches!(err, MediaError::InvalidRequest(_)));
}
