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
    http: reqwest::Client,
}

impl SkillsHub {
    pub fn new(cfg: HubConfig) -> Self {
        let token = cfg.token.clone();
        let gh = GhClient::new().with_token(token);
        let spec = cfg.gh_spec();
        let http = reqwest::Client::builder()
            .user_agent(concat!("cos/", env!("CARGO_PKG_VERSION"), " (skills-hub)"))
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { cfg, spec, gh, http }
    }

    /// Allow tests to inject a custom-base GhClient (e.g. wiremock).
    #[cfg(test)]
    pub(crate) fn with_gh_client(cfg: HubConfig, gh: GhClient) -> Self {
        let spec = cfg.gh_spec();
        let http = reqwest::Client::builder()
            .user_agent("cos-test (skills-hub)")
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { cfg, spec, gh, http }
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
        let mut req = self.http.get(&asset.browser_download_url);
        if let Some(t) = &self.cfg.token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        let resp = req.send().await?;
        let status = resp.status();
        let body = resp.text().await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_filters_empty_token() {
        let c = HubConfig::new("o", "r").with_token(Some(String::new()));
        assert!(c.token.is_none());
        let c = HubConfig::new("o", "r").with_token(Some("abc".into()));
        assert_eq!(c.token.as_deref(), Some("abc"));
    }

    #[test]
    fn config_default_catalogue_asset_name() {
        let c = HubConfig::new("a", "b");
        assert_eq!(c.catalogue_asset, "hub.json");
        let c = c.with_catalogue_asset("registry.json");
        assert_eq!(c.catalogue_asset, "registry.json");
    }

    #[test]
    fn catalogue_round_trips_via_serde() {
        let body = r#"{
            "schema": 1,
            "release_tag": "v0.1.0",
            "skills": [{
                "id": "pdf-extract",
                "name": "PDF Extract",
                "description": "extract pdfs",
                "version": "0.2.1",
                "asset": "pdf-extract-0.2.1.tar.gz",
                "sha256": "deadbeef",
                "homepage": "https://example.com",
                "license": "MIT",
                "tags": ["productivity", "office"]
            }]
        }"#;
        let cat: HubCatalogue = serde_json::from_str(body).unwrap();
        assert_eq!(cat.schema, 1);
        assert_eq!(cat.skills.len(), 1);
        assert_eq!(cat.skills[0].id, "pdf-extract");
        assert_eq!(cat.skills[0].tags, vec!["productivity", "office"]);
    }

    #[test]
    fn skill_serializes_camel_round_trip() {
        let s = HubSkill {
            id: "x".into(),
            name: "X".into(),
            description: None,
            version: "1.0.0".into(),
            asset: "x-1.0.0.tar.gz".into(),
            sha256: "abc".into(),
            homepage: None,
            license: None,
            tags: vec![],
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["id"], "x");
        // Optional empty fields must round-trip even when None /
        // empty, otherwise older catalogue files break.
        let back: HubSkill = serde_json::from_value(v).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn schema_version_constant_is_one() {
        assert_eq!(HUB_SCHEMA_VERSION, 1);
    }

    #[test]
    fn hub_error_messages_are_actionable() {
        let e = HubError::CatalogueMissing {
            asset: "hub.json".into(),
            tag: "v1".into(),
        };
        assert!(e.to_string().contains("hub.json"));
        assert!(e.to_string().contains("v1"));
        let e = HubError::Schema {
            expected: 1,
            got: 7,
        };
        assert!(e.to_string().contains('1') && e.to_string().contains('7'));
    }

    // The full integration path (release → catalogue → asset
    // download) goes through reqwest + a live HTTP server. We have
    // no embedded mock server in the workspace right now; the
    // GhClient/asset_select tests in engine_pkg already exercise
    // the GitHub-API half against canned bodies, so we keep this
    // module's tests focused on data shapes + config knobs.
}
