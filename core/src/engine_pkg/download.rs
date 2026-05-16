//! Stream-download to a temp file with on-the-fly SHA-256 hashing.
//!
//! Used by `cos engine update` (and any other engine_pkg consumer) to
//! fetch a release archive from a URL into a `NamedTempFile` that
//! `install_local::install_from_archive` can then unpack atomically.
//!
//! Streaming (chunked) download avoids loading multi-hundred-MB
//! archives into memory. The same byte stream is fed both to disk
//! and to `Sha256Stream`, so we never need a second pass.
//!
//! GitHub release downloads land at `objects.githubusercontent.com`
//! after a 302; reqwest's default policy follows up to 10 redirects
//! which is plenty.

use std::io::Write as _;

use futures_util::StreamExt;

use crate::engine_pkg::install_local::Sha256Stream;

#[derive(Debug)]
pub struct DownloadOpts<'a> {
    pub url: &'a str,
    pub headers: &'a [(&'a str, &'a str)],
    /// Lower-case hex SHA-256. If present, mismatch is a hard failure.
    pub expected_sha256: Option<&'a str>,
    /// User-visible label for progress / error messages.
    pub label: &'a str,
}

#[derive(Debug)]
pub struct DownloadResult {
    pub temp_file: tempfile::NamedTempFile,
    pub bytes: u64,
    pub sha256_hex: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("download {label} returned {status}")]
    Status { label: String, status: u16 },
    #[error("checksum mismatch for {label}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        label: String,
        expected: String,
        actual: String,
    },
    #[error("invalid header value: {0}")]
    BadHeader(String),
    #[error("refusing to download over insecure scheme: {0} (https:// required)")]
    InsecureScheme(String),
    #[error("invalid url: {0}")]
    BadUrl(String),
}

pub async fn stream_to_temp(opts: &DownloadOpts<'_>) -> Result<DownloadResult, DownloadError> {
    // Refuse non-HTTPS URLs unconditionally. Engine downloads carry
    // native code into our process via libloading later, so the
    // transport must be encrypted + authenticated end-to-end.
    let parsed = reqwest::Url::parse(opts.url)
        .map_err(|e| DownloadError::BadUrl(format!("{}: {e}", opts.url)))?;
    if parsed.scheme() != "https" && !allow_insecure_for_tests() {
        return Err(DownloadError::InsecureScheme(opts.url.to_string()));
    }

    let client = reqwest::Client::builder()
        .user_agent(concat!("cos/", env!("CARGO_PKG_VERSION"), " (engine-pkg)"))
        .timeout(std::time::Duration::from_secs(60 * 30))
        // Refuse any redirect that leaves https — a redirect to http://
        // would defeat the scheme guard above.
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 {
                return attempt.error("too many redirects");
            }
            if attempt.url().scheme() != "https" && !allow_insecure_for_tests() {
                return attempt.error("redirect to non-https scheme");
            }
            attempt.follow()
        }))
        .build()?;

    let mut req = client.get(opts.url);
    for (k, v) in opts.headers {
        req = req.header(*k, *v);
    }

    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(DownloadError::Status {
            label: opts.label.to_string(),
            status: status.as_u16(),
        });
    }

    let suffix = url_extension(opts.url).unwrap_or_else(|| ".bin".to_string());
    let mut tmp = tempfile::Builder::new()
        .prefix("cos-engine-dl-")
        .suffix(&suffix)
        .tempfile()?;
    let mut hasher = Sha256Stream::new();
    let mut total: u64 = 0;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        hasher.update(&chunk);
        tmp.as_file_mut().write_all(&chunk)?;
        total = total.saturating_add(chunk.len() as u64);
    }
    tmp.as_file_mut().sync_all()?;

    let actual = hasher.finalize_hex();
    if let Some(expected) = opts.expected_sha256 {
        let expected = expected.to_ascii_lowercase();
        if expected != actual {
            return Err(DownloadError::ChecksumMismatch {
                label: opts.label.to_string(),
                expected,
                actual,
            });
        }
    }

    Ok(DownloadResult {
        temp_file: tmp,
        bytes: total,
        sha256_hex: actual,
    })
}

/// Extract a file extension from a URL like `.../foo-1.2.3.tar.gz` or
/// `.../foo.zip?token=abc`. Returns the leading-dot suffix (`".tar.gz"`,
/// `".zip"`) so it can be passed straight to `tempfile::Builder::suffix`.
fn url_extension(url: &str) -> Option<String> {
    let path_part = url.split('?').next().unwrap_or(url);
    let path_part = path_part.split('#').next().unwrap_or(path_part);
    let last_seg = path_part.rsplit('/').next().unwrap_or(path_part);
    let lower = last_seg.to_ascii_lowercase();
    for ext in [
        ".tar.gz", ".tgz", ".tar.bz2", ".tar.xz", ".zip", ".gz", ".7z",
    ] {
        if lower.ends_with(ext) {
            return Some(ext.to_string());
        }
    }
    None
}

/// Test-only hook: cfg(test) builds may flip
/// [`TEST_ALLOW_INSECURE`] to exercise the streaming/checksum logic
/// against a local plaintext HTTP fixture. The flag is **never**
/// honored in release builds — the scheme guard is unconditional
/// outside `cfg(test)`.
#[cfg(test)]
pub(crate) static TEST_ALLOW_INSECURE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn allow_insecure_for_tests() -> bool {
    #[cfg(test)]
    {
        TEST_ALLOW_INSECURE.load(std::sync::atomic::Ordering::Relaxed)
    }
    #[cfg(not(test))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Acquire a guard that pins [`TEST_ALLOW_INSECURE`] for the
    /// duration of one test. The mutex serializes tests that need
    /// opposite settings so the parallel-test runner can't observe
    /// torn state.
    async fn allow_http_guard() -> InsecureGuard {
        InsecureGuard::set(true).await
    }

    async fn reject_http_guard() -> InsecureGuard {
        InsecureGuard::set(false).await
    }

    struct InsecureGuard {
        prev: bool,
        _lock: tokio::sync::MutexGuard<'static, ()>,
    }

    impl InsecureGuard {
        async fn set(allow: bool) -> Self {
            use std::sync::OnceLock;
            static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
            let lock = LOCK
                .get_or_init(|| tokio::sync::Mutex::new(()))
                .lock()
                .await;
            let prev = TEST_ALLOW_INSECURE.swap(allow, std::sync::atomic::Ordering::Relaxed);
            Self { prev, _lock: lock }
        }
    }

    impl Drop for InsecureGuard {
        fn drop(&mut self) {
            TEST_ALLOW_INSECURE.store(self.prev, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Tiny one-shot HTTP/1.1 server that serves `body` once.
    async fn spawn_blob_server(
        body: Vec<u8>,
        status_line: &'static str,
    ) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/blob.zip");
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 16 * 1024];
            let mut total = Vec::new();
            loop {
                let n = sock.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                total.extend_from_slice(&buf[..n]);
                if total.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let response = format!(
                "{status_line}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.write_all(&body).await;
            let _ = sock.shutdown().await;
            total
        });
        (url, handle)
    }

    fn sha256_known(s: &str) -> String {
        // SHA-256 of literal "abc" is a fixed published vector.
        match s {
            "abc" => "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_string(),
            "" => "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
            _ => panic!("no precomputed hash for input"),
        }
    }

    #[tokio::test]
    async fn download_succeeds_and_hashes() {
        let _g = allow_http_guard().await;
        let (url, _h) = spawn_blob_server(b"abc".to_vec(), "HTTP/1.1 200 OK").await;
        let result = stream_to_temp(&DownloadOpts {
            url: &url,
            headers: &[],
            expected_sha256: None,
            label: "test",
        })
        .await
        .unwrap();
        assert_eq!(result.bytes, 3);
        assert_eq!(result.sha256_hex, sha256_known("abc"));
        let mut buf = Vec::new();
        std::fs::File::open(result.temp_file.path())
            .unwrap()
            .read_to_end(&mut buf)
            .unwrap();
        assert_eq!(buf, b"abc");
    }

    #[tokio::test]
    async fn checksum_match_passes() {
        let _g = allow_http_guard().await;
        let (url, _h) = spawn_blob_server(b"abc".to_vec(), "HTTP/1.1 200 OK").await;
        let r = stream_to_temp(&DownloadOpts {
            url: &url,
            headers: &[],
            expected_sha256: Some(&sha256_known("abc")),
            label: "test",
        })
        .await
        .unwrap();
        assert_eq!(r.bytes, 3);
    }

    #[tokio::test]
    async fn checksum_mismatch_fails() {
        let _g = allow_http_guard().await;
        let (url, _h) = spawn_blob_server(b"abc".to_vec(), "HTTP/1.1 200 OK").await;
        let err = stream_to_temp(&DownloadOpts {
            url: &url,
            headers: &[],
            expected_sha256: Some(&"0".repeat(64)),
            label: "test",
        })
        .await
        .unwrap_err();
        match err {
            DownloadError::ChecksumMismatch { actual, .. } => {
                assert_eq!(actual, sha256_known("abc"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_2xx_propagates_status() {
        let _g = allow_http_guard().await;
        let (url, _h) =
            spawn_blob_server(b"oops".to_vec(), "HTTP/1.1 500 Internal Server Error").await;
        let err = stream_to_temp(&DownloadOpts {
            url: &url,
            headers: &[],
            expected_sha256: None,
            label: "test",
        })
        .await
        .unwrap_err();
        match err {
            DownloadError::Status { status, .. } => assert_eq!(status, 500),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn extra_headers_are_sent() {
        let _g = allow_http_guard().await;
        let (url, handle) = spawn_blob_server(b"abc".to_vec(), "HTTP/1.1 200 OK").await;
        stream_to_temp(&DownloadOpts {
            url: &url,
            headers: &[
                ("Authorization", "Bearer t1"),
                ("Accept", "application/zip"),
            ],
            expected_sha256: None,
            label: "test",
        })
        .await
        .unwrap();
        let req = String::from_utf8_lossy(&handle.await.unwrap())
            .into_owned()
            .to_ascii_lowercase();
        assert!(req.contains("authorization: bearer t1"));
        assert!(req.contains("accept: application/zip"));
    }

    #[tokio::test]
    async fn temp_file_uses_url_extension() {
        let _g = allow_http_guard().await;
        let (url, _h) = spawn_blob_server(b"PK\x03\x04 fake zip".to_vec(), "HTTP/1.1 200 OK").await;
        let r = stream_to_temp(&DownloadOpts {
            url: &url,
            headers: &[],
            expected_sha256: None,
            label: "test",
        })
        .await
        .unwrap();
        let p = r.temp_file.path().to_string_lossy().to_string();
        assert!(
            p.ends_with(".zip"),
            "expected temp file path to end with .zip, got {p}"
        );
    }

    #[test]
    fn url_extension_strips_query_string() {
        assert_eq!(
            url_extension("https://example.com/foo/bar.zip?token=abc"),
            Some(".zip".to_string())
        );
        assert_eq!(
            url_extension("https://example.com/foo-1.2.tar.gz"),
            Some(".tar.gz".to_string())
        );
        assert_eq!(
            url_extension("https://example.com/foo.tgz#fragment"),
            Some(".tgz".to_string())
        );
        assert_eq!(url_extension("https://example.com/no-ext"), None);
    }

    /// New: without the test escape hatch, an `http://` URL is rejected
    /// before any network IO is attempted. This protects engine
    /// installs against MITM substitution of the native libraries we
    /// later `dlopen`.
    #[tokio::test]
    async fn http_scheme_rejected_in_release_mode() {
        let _g = reject_http_guard().await;
        let err = stream_to_temp(&DownloadOpts {
            url: "http://example.com/engine.zip",
            headers: &[],
            expected_sha256: None,
            label: "test",
        })
        .await
        .unwrap_err();
        assert!(
            matches!(err, DownloadError::InsecureScheme(_)),
            "expected InsecureScheme, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn ftp_scheme_rejected() {
        let _g = reject_http_guard().await;
        let err = stream_to_temp(&DownloadOpts {
            url: "ftp://example.com/engine.zip",
            headers: &[],
            expected_sha256: None,
            label: "test",
        })
        .await
        .unwrap_err();
        assert!(matches!(err, DownloadError::InsecureScheme(_)));
    }

    #[tokio::test]
    async fn malformed_url_rejected() {
        let _g = reject_http_guard().await;
        let err = stream_to_temp(&DownloadOpts {
            url: "not a url",
            headers: &[],
            expected_sha256: None,
            label: "test",
        })
        .await
        .unwrap_err();
        assert!(matches!(err, DownloadError::BadUrl(_)));
    }
}
