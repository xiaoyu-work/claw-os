//! GitHub-backed skill hub — discovery half.
//!
//! A "hub" is a GitHub repository that publishes a `hub.json`
//! catalogue plus one tarball per skill version on its latest
//! release. The kernel's discovery surface is small:
//!
//! 1. `latest_catalogue()` — fetch the catalogue from the latest
//!    release of the configured repo via [`crate::engine_pkg::sources::github::GhClient`].
//! 2. `find(id)` — look up one entry by skill id.
//!
//! Actual extraction into `<data>/skills/<id>/<version>/` reuses
//! the same zip / tar helpers that the engine package manager
//! already ships ([`crate::engine_pkg::download`] +
//! [`crate::engine_pkg::install_local`]); wiring the install path
//! here would duplicate that logic and is left for the matching
//! `cos skill install` CLI surface (Phase 7). For now this module
//! gives the agent the metadata it needs to recommend / display
//! installable skills, and a documented contract for the hub
//! authors to publish against.
//!
//! ## hub.json schema
//!
//! ```json
//! {
//!   "schema": 1,
//!   "skills": [
//!     {
//!       "id": "pdf-extract",
//!       "name": "PDF extract",
//!       "description": "Extract text + tables from PDFs.",
//!       "version": "0.2.1",
//!       "asset": "pdf-extract-0.2.1.tar.gz",
//!       "sha256": "8f1c...",
//!       "homepage": "https://...",
//!       "license": "MIT",
//!       "tags": ["productivity"]
//!     }
//!   ]
//! }
//! ```
//!
//! `asset` must match a [`GhAsset.name`] on the same release;
//! `sha256` is the lower-case hex digest used to authenticate the
//! tarball. We refuse to surface entries whose asset isn't present
//! on the release — partial publishes silently disappear from
//! `latest_catalogue()` rather than handing the user a 404 later.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::agent::media::util::build_safe_client;
use crate::engine_pkg::sources::github::{GhAsset, GhClient, GhError, GhSpec};

/// Where to fetch the hub from. Default points at the canonical
/// open-source skills hub but every field is overridable.
#[derive(Debug, Clone)]
pub struct HubConfig {
    pub owner: String,
    pub repo: String,
    /// Asset name to look for on the latest release. Defaults to
    /// `hub.json`.
    pub catalogue_asset: String,
    /// Optional GitHub PAT for private hubs / higher rate limits.
    pub token: Option<String>,
}

impl HubConfig {
    pub fn new(owner: impl Into<String>, repo: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            repo: repo.into(),
            catalogue_asset: "hub.json".into(),
            token: None,
        }
    }

    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.token = token.filter(|t| !t.is_empty());
        self
    }

    pub fn with_catalogue_asset(mut self, name: impl Into<String>) -> Self {
        self.catalogue_asset = name.into();
        self
    }

    fn gh_spec(&self) -> GhSpec {
        // GhSpec needs &'static str. Hub configs are typically
        // process-lifetime so we leak once per call into a static
        // arena. Tests construct/drop hubs hundreds of times so we
        // cache the leaked pair on the SkillsHub instead — see
        // SkillsHub::new.
        GhSpec {
            owner: Box::leak(self.owner.clone().into_boxed_str()),
            repo: Box::leak(self.repo.clone().into_boxed_str()),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HubError {
    #[error("github: {0}")]
    Gh(#[from] GhError),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("status {status} fetching {what}: {body}")]
    Status {
        status: u16,
        what: String,
        body: String,
    },
    #[error("catalogue asset '{asset}' not found on release '{tag}'")]
    CatalogueMissing { asset: String, tag: String },
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("schema mismatch: expected {expected}, got {got}")]
    Schema { expected: u32, got: u32 },
    /// Asset download URL resolved to a non-public IP (loopback,
    /// link-local, RFC1918) or used a disallowed scheme. The hub
    /// can't be trusted to publish URLs we'll then connect to
    /// blindly: this is the SSRF / DNS-rebinding gate.
    #[error("unsafe asset url: {0}")]
    UnsafeUrl(String),
}

/// One skill listed in the hub catalogue.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HubSkill {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub version: String,
    /// Asset filename on the release that holds the skill tarball.
    pub asset: String,
    /// Lower-case hex sha256 digest of the tarball.
    pub sha256: String,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubCatalogue {
    pub schema: u32,
    /// Hub release tag the catalogue was published from.
    #[serde(default)]
    pub release_tag: Option<String>,
    pub skills: Vec<HubSkill>,
}

/// Schema version this kernel build understands.
pub const HUB_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct ResolvedSkill {
    pub entry: HubSkill,
    /// Resolved download URL for the skill asset.
    pub download_url: String,
    /// Asset size in bytes, as reported by the GitHub API.
    pub size: u64,
}

pub struct SkillsHub {
    cfg: HubConfig,
    spec: GhSpec,
    gh: GhClient,
}

impl SkillsHub {
    pub fn new(cfg: HubConfig) -> Self {
        let token = cfg.token.clone();
        let gh = GhClient::new().with_token(token);
        let spec = cfg.gh_spec();
        // No long-lived `http` client here: see `fetch_asset_text`
        // for the per-request DNS-pinned builder.
        Self { cfg, spec, gh }
    }

    /// Allow tests to inject a custom-base GhClient (e.g. wiremock).
    #[cfg(test)]
    pub(crate) fn with_gh_client(cfg: HubConfig, gh: GhClient) -> Self {
        let spec = cfg.gh_spec();
        Self { cfg, spec, gh }
    }

    /// Fetch the hub catalogue from the latest release of the
    /// configured repo. Skills whose `asset` field doesn't appear on
    /// the release are dropped (with a tracing warn) so callers
    /// never see broken entries.
    pub async fn latest_catalogue(&self) -> Result<HubCatalogue, HubError> {
        let release = self.gh.latest(&self.spec).await?;
        let asset = release
            .assets
            .iter()
            .find(|a| a.name == self.cfg.catalogue_asset)
            .ok_or_else(|| HubError::CatalogueMissing {
                asset: self.cfg.catalogue_asset.clone(),
                tag: release.tag_name.clone(),
            })?;
        let body = self.fetch_asset_text(asset).await?;
        let mut cat: HubCatalogue = serde_json::from_str(&body)?;
        if cat.schema != HUB_SCHEMA_VERSION {
            return Err(HubError::Schema {
                expected: HUB_SCHEMA_VERSION,
                got: cat.schema,
            });
        }
        if cat.release_tag.is_none() {
            cat.release_tag = Some(release.tag_name.clone());
        }
        // Index assets present on the release for cheap lookup.
        let assets: BTreeMap<&str, &GhAsset> = release
            .assets
            .iter()
            .map(|a| (a.name.as_str(), a))
            .collect();
        cat.skills.retain(|s| {
            let present = assets.contains_key(s.asset.as_str());
            if !present {
                tracing::warn!(
                    skill = %s.id,
                    asset = %s.asset,
                    "hub catalogue references missing asset, skipping"
                );
            }
            present
        });
        Ok(cat)
    }

    /// Resolve one skill by id against the latest catalogue.
    pub async fn resolve(&self, id: &str) -> Result<Option<ResolvedSkill>, HubError> {
        let release = self.gh.latest(&self.spec).await?;
        let cat_asset = release
            .assets
            .iter()
            .find(|a| a.name == self.cfg.catalogue_asset)
            .ok_or_else(|| HubError::CatalogueMissing {
                asset: self.cfg.catalogue_asset.clone(),
                tag: release.tag_name.clone(),
            })?;
        let body = self.fetch_asset_text(cat_asset).await?;
        let cat: HubCatalogue = serde_json::from_str(&body)?;
        if cat.schema != HUB_SCHEMA_VERSION {
            return Err(HubError::Schema {
                expected: HUB_SCHEMA_VERSION,
                got: cat.schema,
            });
        }
        let entry = match cat.skills.into_iter().find(|s| s.id == id) {
            Some(e) => e,
            None => return Ok(None),
        };
        let asset = match release.assets.iter().find(|a| a.name == entry.asset) {
            Some(a) => a,
            None => return Ok(None),
        };
        Ok(Some(ResolvedSkill {
            download_url: asset.browser_download_url.clone(),
            size: asset.size,
            entry,
        }))
    }

    async fn fetch_asset_text(&self, asset: &GhAsset) -> Result<String, HubError> {
        // Build a per-call client pinned to a single vetted public
        // IP for the asset host. The hub publishes the download URL
        // so we can't assume it's safe — between DNS resolution and
        // connect, a malicious or compromised host could rebind to
        // a private address. `build_safe_client` resolves once and
        // pins the client to those addresses, which neutralises the
        // rebinding race.
        let asset_url = reqwest::Url::parse(&asset.browser_download_url).map_err(|e| {
            HubError::UnsafeUrl(format!(
                "asset url {} is not a valid URL: {e}",
                asset.browser_download_url
            ))
        })?;
        let client = build_safe_client(&asset_url, std::time::Duration::from_secs(60))
            .await
            .map_err(|e| HubError::UnsafeUrl(e.to_string()))?;
        let mut req = client.get(asset_url);
        if let Some(t) = &self.cfg.token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        let resp = req.send().await?;
        let status = resp.status();
        // Cap the response body so a malicious or misconfigured hub
        // can't OOM the agent by publishing a multi-GB catalogue.
        // 16 MiB is comfortably above any plausible hub.json and
        // small enough to fit on the smallest agent host.
        const MAX_CATALOGUE_BYTES: usize = 16 * 1024 * 1024;
        let body = read_capped_text(resp, MAX_CATALOGUE_BYTES, &asset.name).await?;
        if !status.is_success() {
            return Err(HubError::Status {
                status: status.as_u16(),
                what: asset.name.clone(),
                body,
            });
        }
        Ok(body)
    }
}

/// Drain a response body to a `String`, refusing to allocate more
/// than `cap` bytes. The body is read in chunks via `bytes_stream`
/// so we never buffer the whole transfer to memory before checking
/// the cap.
async fn read_capped_text(
    resp: reqwest::Response,
    cap: usize,
    what: &str,
) -> Result<String, HubError> {
    use futures_util::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if buf.len().saturating_add(chunk.len()) > cap {
            return Err(HubError::Status {
                status: 0,
                what: what.to_string(),
                body: format!("response exceeded {cap}-byte cap"),
            });
        }
        buf.extend_from_slice(&chunk);
    }
    String::from_utf8(buf).map_err(|e| HubError::Status {
        status: 0,
        what: what.to_string(),
        body: format!("response was not valid utf-8: {e}"),
    })
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/skills/hub.rs"
    ));
}
