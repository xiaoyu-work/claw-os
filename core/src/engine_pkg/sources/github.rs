//! GitHub Releases API adapter.
//!
//! A thin async client over the public GitHub REST API that knows
//! just enough to:
//!   - resolve `latest` for an engine's upstream repo
//!   - fetch a specific tag
//!   - list recent releases (for channel handling later)
//!
//! Only the fields we actually consume are deserialized. Optional
//! `Authorization: Bearer <token>` covers private repos and lifts the
//! 60-req/hr unauthenticated rate limit to 5000.
//!
//! The client is base-URL-configurable so tests can point it at a
//! local mock TCP server.

use chrono::{DateTime, Utc};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct GhSpec {
    pub owner: &'static str,
    pub repo: &'static str,
}

/// Map an engine name to its upstream GitHub repo.
pub fn spec_for(engine: &str) -> Option<GhSpec> {
    match engine {
        "llama-cpp" => Some(GhSpec {
            owner: "ggml-org",
            repo: "llama.cpp",
        }),
        "ort" => Some(GhSpec {
            owner: "microsoft",
            repo: "onnxruntime",
        }),
        "ort-genai" => Some(GhSpec {
            owner: "microsoft",
            repo: "onnxruntime-genai",
        }),
        _ => None,
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GhRelease {
    pub tag_name: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub published_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub assets: Vec<GhAsset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GhAsset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub content_type: String,
    /// "sha256:<hex>" when GitHub has computed a digest, else None.
    #[serde(default)]
    pub digest: Option<String>,
}

impl GhAsset {
    /// Strip the "sha256:" prefix and return the lower-case hex hash,
    /// if GitHub has populated `digest`. Returns `None` for older
    /// assets uploaded before the digest field was added.
    pub fn sha256_hex(&self) -> Option<String> {
        self.digest
            .as_ref()
            .and_then(|d| d.strip_prefix("sha256:"))
            .map(|h| h.to_ascii_lowercase())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GhError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("github API returned {status}: {body}")]
    Status { status: u16, body: String },
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct GhClient {
    http: reqwest::Client,
    base_url: String,
    token: Option<String>,
}

impl GhClient {
    pub fn new() -> Self {
        Self::with_base("https://api.github.com".to_string())
    }

    pub fn with_base(base_url: String) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("cos/", env!("CARGO_PKG_VERSION"), " (engine-pkg)"))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            base_url,
            token: None,
        }
    }

    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.token = token.filter(|t| !t.is_empty());
        self
    }

    pub async fn latest(&self, spec: &GhSpec) -> Result<GhRelease, GhError> {
        let url = format!(
            "{}/repos/{}/{}/releases/latest",
            self.base_url, spec.owner, spec.repo
        );
        self.fetch_one(&url).await
    }

    pub async fn tag(&self, spec: &GhSpec, tag: &str) -> Result<GhRelease, GhError> {
        let url = format!(
            "{}/repos/{}/{}/releases/tags/{}",
            self.base_url, spec.owner, spec.repo, tag
        );
        self.fetch_one(&url).await
    }

    pub async fn list(&self, spec: &GhSpec, per_page: usize) -> Result<Vec<GhRelease>, GhError> {
        let url = format!(
            "{}/repos/{}/{}/releases?per_page={}",
            self.base_url,
            spec.owner,
            spec.repo,
            per_page.clamp(1, 100)
        );
        let mut req = self
            .http
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(t) = &self.token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        let resp = req.send().await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(GhError::Status {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: Vec<GhRelease> = serde_json::from_str(&body)?;
        Ok(parsed)
    }

    async fn fetch_one(&self, url: &str) -> Result<GhRelease, GhError> {
        let mut req = self
            .http
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(t) = &self.token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        let resp = req.send().await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(GhError::Status {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: GhRelease = serde_json::from_str(&body)?;
        Ok(parsed)
    }
}

impl Default for GhClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn sample_release_json(tag: &str) -> String {
        serde_json::json!({
            "tag_name": tag,
            "name": format!("Release {tag}"),
            "published_at": "2026-05-05T20:44:25Z",
            "prerelease": false,
            "draft": false,
            "assets": [
                {
                    "name": "llama-b9037-bin-win-cpu-x64.zip",
                    "browser_download_url": "https://example.invalid/llama-b9037-bin-win-cpu-x64.zip",
                    "size": 12345,
                    "content_type": "application/json; charset=utf-8",
                    "digest": "sha256:8c79a9b226de4b3cacfd1f83d24f962d0773be79f1e7b75c6af4ded7e32ae1d6",
                },
                {
                    "name": "llama-b9037-bin-win-cuda-12.4-x64.zip",
                    "browser_download_url": "https://example.invalid/llama-b9037-bin-win-cuda-12.4-x64.zip",
                    "size": 67890,
                    "content_type": "application/json; charset=utf-8",
                    "digest": null,
                }
            ]
        })
        .to_string()
    }

    /// Spawns a one-shot HTTP/1.1 server on a random localhost port
    /// that responds to *any* GET with `body` and returns the captured
    /// request bytes. Mirrors the pattern used by stt.rs tests.
    async fn spawn_one_shot(
        body: String,
        status_line: &'static str,
    ) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}");
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
            let body_bytes = body.as_bytes();
            let response = format!(
                "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body_bytes.len()
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.write_all(body_bytes).await;
            let _ = sock.shutdown().await;
            total
        });
        (url, handle)
    }

    #[tokio::test]
    async fn latest_round_trips() {
        let body = sample_release_json("b9037");
        let (base, handle) = spawn_one_shot(body, "HTTP/1.1 200 OK").await;
        let client = GhClient::with_base(base);
        let spec = GhSpec {
            owner: "ggml-org",
            repo: "llama.cpp",
        };
        let release = client.latest(&spec).await.unwrap();
        assert_eq!(release.tag_name, "b9037");
        assert_eq!(release.assets.len(), 2);
        assert_eq!(
            release.assets[0].sha256_hex().unwrap(),
            "8c79a9b226de4b3cacfd1f83d24f962d0773be79f1e7b75c6af4ded7e32ae1d6"
        );
        assert!(release.assets[1].sha256_hex().is_none());
        let req_bytes = handle.await.unwrap();
        let head = String::from_utf8_lossy(&req_bytes).to_ascii_lowercase();
        assert!(head.starts_with("get /repos/ggml-org/llama.cpp/releases/latest "));
        assert!(head.contains("user-agent: cos/"));
        assert!(!head.contains("authorization:"));
    }

    #[tokio::test]
    async fn tag_lookup_uses_correct_path() {
        let body = sample_release_json("b9000");
        let (base, handle) = spawn_one_shot(body, "HTTP/1.1 200 OK").await;
        let client = GhClient::with_base(base);
        let spec = GhSpec {
            owner: "ggml-org",
            repo: "llama.cpp",
        };
        let release = client.tag(&spec, "b9000").await.unwrap();
        assert_eq!(release.tag_name, "b9000");
        let req = String::from_utf8_lossy(&handle.await.unwrap())
            .into_owned()
            .to_ascii_lowercase();
        assert!(req.starts_with("get /repos/ggml-org/llama.cpp/releases/tags/b9000 "));
    }

    #[tokio::test]
    async fn token_attaches_authorization_header() {
        let body = sample_release_json("b9037");
        let (base, handle) = spawn_one_shot(body, "HTTP/1.1 200 OK").await;
        let client = GhClient::with_base(base).with_token(Some("ghp_secret123".into()));
        let spec = GhSpec {
            owner: "ggml-org",
            repo: "llama.cpp",
        };
        client.latest(&spec).await.unwrap();
        let req = String::from_utf8_lossy(&handle.await.unwrap())
            .into_owned()
            .to_ascii_lowercase();
        assert!(req.contains("authorization: bearer ghp_secret123"));
    }

    #[tokio::test]
    async fn empty_token_does_not_attach_header() {
        let body = sample_release_json("b9037");
        let (base, handle) = spawn_one_shot(body, "HTTP/1.1 200 OK").await;
        let client = GhClient::with_base(base).with_token(Some(String::new()));
        let spec = GhSpec {
            owner: "ggml-org",
            repo: "llama.cpp",
        };
        client.latest(&spec).await.unwrap();
        let req = String::from_utf8_lossy(&handle.await.unwrap())
            .into_owned()
            .to_ascii_lowercase();
        assert!(!req.contains("authorization:"));
    }

    #[tokio::test]
    async fn non_2xx_status_propagates() {
        let (base, _h) = spawn_one_shot("not found".into(), "HTTP/1.1 404 Not Found").await;
        let client = GhClient::with_base(base);
        let spec = GhSpec {
            owner: "ggml-org",
            repo: "llama.cpp",
        };
        let err = client.latest(&spec).await.unwrap_err();
        match err {
            GhError::Status { status, .. } => assert_eq!(status, 404),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_returns_array() {
        let body = serde_json::json!([
            {"tag_name": "b9037", "prerelease": false, "draft": false, "assets": []},
            {"tag_name": "b9036", "prerelease": false, "draft": false, "assets": []},
        ])
        .to_string();
        let (base, _h) = spawn_one_shot(body, "HTTP/1.1 200 OK").await;
        let client = GhClient::with_base(base);
        let spec = GhSpec {
            owner: "ggml-org",
            repo: "llama.cpp",
        };
        let rels = client.list(&spec, 5).await.unwrap();
        assert_eq!(rels.len(), 2);
        assert_eq!(rels[0].tag_name, "b9037");
    }

    #[test]
    fn spec_for_known_engines() {
        assert_eq!(spec_for("llama-cpp").unwrap().owner, "ggml-org");
        assert_eq!(spec_for("ort").unwrap().repo, "onnxruntime");
        assert_eq!(spec_for("ort-genai").unwrap().repo, "onnxruntime-genai");
        assert!(spec_for("nonsense").is_none());
    }
}
