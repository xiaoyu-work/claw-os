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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/engine_pkg/download.rs"
    ));
}
