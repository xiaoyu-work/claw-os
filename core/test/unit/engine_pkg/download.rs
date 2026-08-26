use super::*;
use std::io::Read as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(super) static TEST_ALLOW_INSECURE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

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
    let (url, _h) = spawn_blob_server(b"oops".to_vec(), "HTTP/1.1 500 Internal Server Error").await;
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
