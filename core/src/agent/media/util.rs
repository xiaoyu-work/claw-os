//! Shared safety helpers used across every media provider.
//!
//! Goals:
//!   * **Bounded response bodies.** Every cloud HTTP call returns
//!     unbounded data; we cap text/JSON at 8 MiB and binary
//!     (image/audio) at 64 MiB so a misbehaving or malicious
//!     provider can't OOM the agent process.
//!   * **Char-boundary-safe previews.** Diagnostic strings like
//!     `&body[..512]` panic on multi-byte UTF-8 — callers should
//!     route through [`preview`] so non-ASCII responses never
//!     crash the agent loop.
//!   * **SSRF guard.** Provider-supplied callback URLs (FAL assets,
//!     vision image URLs) must be filtered through
//!     [`assert_safe_outbound`] before we issue a GET — otherwise
//!     a malicious response can redirect us at link-local /
//!     loopback / RFC1918 hosts and bypass the network capability.
//!   * **DNS rebinding mitigation.** Once we've decided a URL is
//!     safe, [`build_safe_client`] pins the host's DNS lookup to a
//!     single vetted public IP so the *actual* TCP connect can't
//!     race a second DNS lookup that returns an internal address.
//!     Every outbound `reqwest::Client` constructed for media or
//!     skill traffic goes through this helper.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;

use super::MediaError;

/// 8 MiB cap for text/JSON responses. Catalogue / API payloads we
/// know fit in this bound — refusing larger ones is a defence
/// against runaway providers, not a real constraint on usage.
pub const MAX_TEXT_BODY_BYTES: usize = 8 * 1024 * 1024;
/// 64 MiB cap for binary responses (image / audio downloads). Big
/// enough for a 4K still or a 10-minute mp3 at 128 kbps, small
/// enough to keep memory bounded on smaller hosts.
pub const MAX_BINARY_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Drain a response body into a `Bytes` buffer, refusing to allocate
/// more than `cap` bytes. Chunks come in via `bytes_stream` so we
/// never accidentally buffer a multi-GB body before the cap check.
pub async fn read_bytes_capped(
    resp: reqwest::Response,
    cap: usize,
    what: &str,
) -> Result<Bytes, MediaError> {
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| MediaError::Transport(format!("{what}: {e}")))?;
        if buf.len().saturating_add(chunk.len()) > cap {
            return Err(MediaError::Provider {
                status: 0,
                message: format!("{what}: response exceeded {cap}-byte cap"),
            });
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(buf))
}

/// Convenience wrapper around [`read_bytes_capped`] that decodes
/// the cap'd bytes as UTF-8 (lossy if the provider misbehaves).
/// Producers of text responses (JSON APIs) should route through
/// this instead of `resp.text()`.
pub async fn read_text_capped(
    resp: reqwest::Response,
    cap: usize,
    what: &str,
) -> Result<String, MediaError> {
    let bytes = read_bytes_capped(resp, cap, what).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Take up to `max_chars` *characters* (not bytes) from `s`. The
/// returned slice never splits a multi-byte UTF-8 sequence, so
/// callers can use it for error-message previews without risk of
/// panic on non-ASCII bodies. When truncation occurs an ellipsis
/// (`…`) is appended so the reader knows the message was cut.
pub fn preview(s: &str, max_chars: usize) -> String {
    let total = s.chars().count();
    if total <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// SSRF guard for outbound provider URLs supplied by a remote
/// response (e.g. a callback / asset URL in a FAL job result).
///
/// Resolves the hostname, then rejects:
///   * Non-`http(s)` schemes (data:, file:, gopher:, etc.).
///   * Hostname-less URLs.
///   * Any URL whose resolved IPs fall inside loopback,
///     link-local (169.254.0.0/16), unique-local (fc00::/7),
///     private (10/172.16/192.168), or unspecified ranges.
///   * IPv6 mapped IPv4 variants of the above.
///
/// `require_https` opts the call site into HTTPS-only (the vision
/// fetcher uses it; provider asset fetchers that legitimately
/// span http+https leave it off).
pub fn assert_safe_outbound(raw_url: &str, require_https: bool) -> Result<(), MediaError> {
    let url = reqwest::Url::parse(raw_url)
        .map_err(|e| MediaError::InvalidRequest(format!("invalid url: {e}")))?;
    match url.scheme() {
        "https" => {}
        "http" if !require_https => {}
        other => {
            return Err(MediaError::InvalidRequest(format!(
                "unsupported url scheme: {other}"
            )));
        }
    }
    let host = url
        .host_str()
        .ok_or_else(|| MediaError::InvalidRequest("url has no host".to_string()))?;
    if host.is_empty() {
        return Err(MediaError::InvalidRequest("url has empty host".to_string()));
    }
    // If the host is already an IP literal, check it directly —
    // otherwise resolve via DNS. Either way we then reject if any
    // resolved address falls in a private range. We resolve through
    // `ToSocketAddrs` which performs the same lookup that
    // `reqwest::get` would do, so the IP we vet is the IP we'll
    // actually connect to.
    let port = url.port_or_known_default().unwrap_or(80);
    let target = format!("{host}:{port}");
    let addrs: Vec<_> = match target.to_socket_addrs() {
        Ok(it) => it.collect(),
        Err(e) => {
            return Err(MediaError::InvalidRequest(format!(
                "dns lookup failed for {host}: {e}"
            )));
        }
    };
    if addrs.is_empty() {
        return Err(MediaError::InvalidRequest(format!(
            "dns lookup for {host} returned no addresses"
        )));
    }
    for sa in &addrs {
        let ip = sa.ip();
        if is_private_ip(ip) {
            return Err(MediaError::InvalidRequest(format!(
                "refusing to fetch from non-public address {ip}"
            )));
        }
    }
    Ok(())
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
                || matches!(v4.octets(), [100, b, _, _] if (64..=127).contains(&b))
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() {
                return true;
            }
            // Map IPv4-in-IPv6 (::ffff:0:0/96) back to v4 and re-check.
            if let Some(v4) = v6.to_ipv4_mapped() {
                if is_private_ip(IpAddr::V4(v4)) {
                    return true;
                }
            }
            // Unique-local fc00::/7.
            let octets = v6.octets();
            if (octets[0] & 0xfe) == 0xfc {
                return true;
            }
            // Link-local fe80::/10.
            if octets[0] == 0xfe && (octets[1] & 0xc0) == 0x80 {
                return true;
            }
            false
        }
    }
}

/// Build a `reqwest::Client` that is hardened against DNS rebinding
/// for the given URL.
///
/// Background. [`assert_safe_outbound`] resolves the hostname *once*
/// and verifies that the resulting IPs are public. But a default
/// `reqwest::Client` performs its own DNS lookup at connect time,
/// and a hostile or attacker-controlled resolver can return a
/// public IP for the first (validation) query and an internal IP
/// for the second (connect) query — classic DNS rebinding. The
/// validated IP and the IP we actually open a socket to differ.
///
/// To close the gap, this helper:
///   1. resolves the host via `tokio::net::lookup_host` exactly
///      once,
///   2. drops any address that fails [`is_private_ip`],
///   3. constructs a client with
///      [`reqwest::ClientBuilder::resolve_to_addrs`] pinning the
///      hostname to the surviving public address(es) for the
///      lifetime of every request issued through it.
///
/// Reqwest still does its connection-pool / TLS handshake against
/// the pinned address, so the validated IP IS the connected-to IP
/// — DNS rebinding is structurally impossible.
///
/// `timeout` is applied with [`reqwest::ClientBuilder::timeout`]
/// when greater than zero (callers that already enforce a
/// `tokio::time::timeout` on the future may pass [`Duration::ZERO`]).
pub(crate) async fn build_safe_client(
    url: &reqwest::Url,
    timeout: Duration,
) -> Result<reqwest::Client, MediaError> {
    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(MediaError::InvalidRequest(format!(
                "unsupported url scheme: {other}"
            )));
        }
    }
    let host = url
        .host_str()
        .ok_or_else(|| MediaError::InvalidRequest("url has no host".to_string()))?
        .to_string();
    let port = url.port_or_known_default().ok_or_else(|| {
        MediaError::InvalidRequest(format!("url has no port and no scheme default: {url}"))
    })?;

    // Use the async resolver tokio drives so the lookup honours
    // proxy / overlay resolvers configured by the runtime. We
    // resolve once and reuse the answer for the actual connect via
    // resolve_to_addrs.
    let target = format!("{host}:{port}");
    let mut public_addrs: Vec<SocketAddr> = Vec::new();
    let lookups = tokio::net::lookup_host(target.as_str())
        .await
        .map_err(|e| {
            MediaError::InvalidRequest(format!("dns lookup failed for {host}: {e}"))
        })?;
    for sa in lookups {
        if is_private_ip(sa.ip()) {
            return Err(MediaError::InvalidRequest(format!(
                "refusing to fetch from non-public address {ip} (host={host})",
                ip = sa.ip()
            )));
        }
        public_addrs.push(sa);
    }
    if public_addrs.is_empty() {
        return Err(MediaError::InvalidRequest(format!(
            "dns lookup for {host} returned no addresses"
        )));
    }

    let mut builder = reqwest::Client::builder()
        .user_agent(concat!("cos-agent/", env!("CARGO_PKG_VERSION")))
        .resolve_to_addrs(&host, &public_addrs);
    if timeout > Duration::ZERO {
        builder = builder.timeout(timeout);
    }
    builder
        .build()
        .map_err(|e| MediaError::Internal(format!("safe client build: {e}")))
}

/// Convenience: parse `raw_url` and route through
/// [`build_safe_client`]. Saves call sites from juggling
/// `reqwest::Url::parse` twice.
pub(crate) async fn build_safe_client_for_str(
    raw_url: &str,
    timeout: Duration,
) -> Result<reqwest::Client, MediaError> {
    let url = reqwest::Url::parse(raw_url)
        .map_err(|e| MediaError::InvalidRequest(format!("invalid url: {e}")))?;
    build_safe_client(&url, timeout).await
}

#[cfg(test)]
mod tests {
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
}
