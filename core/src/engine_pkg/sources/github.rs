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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/engine_pkg/sources/github.rs"
    ));
}
