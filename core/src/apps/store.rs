//! Authenticated official release metadata and package-local Web resources.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use base64::Engine;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::provenance::VerifiedPackage;

pub(crate) const REPOSITORY: &str = "xiaoyu-work/clawos-app";
pub(crate) const INDEX_URL: &str = "https://xiaoyu-work.github.io/clawos-app/store/index.json";
pub(crate) const ARCHIVE_FINGERPRINT: &str = "ADBA1957E712B6C80B4CC8736C044D6F411416AF";
pub(crate) const MAX_METADATA: usize = 8 * 1024 * 1024;
pub(crate) const MAX_DOWNLOAD: u64 = 256 * 1024 * 1024;
pub(crate) const WEB_DESCRIPTOR: &str = "web-ui.json";
pub(crate) const WEB_PROTOCOL: &str = "claw.web-app/v1";

const ARCHIVE_KEY: &str = include_str!("../../assets/app-store-archive-key.asc");

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Catalog {
    pub schema: String,
    pub repository: String,
    pub runtime_abi: u32,
    pub generation: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub apps: Vec<Record>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    pub app_id: String,
    pub app_version: String,
    pub architecture: String,
    pub artifact: Artifact,
    pub manifest: ManifestBinding,
    pub provenance: ProvenanceBinding,
    pub release_version: String,
    pub release_tag: String,
    pub display: Display,
    pub web_ui: WebBinding,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Artifact {
    pub file: String,
    pub size: u64,
    pub sha256: String,
    pub media_type: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManifestBinding {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProvenanceBinding {
    pub path: String,
    pub sha256: String,
    pub content_digest: String,
    pub publisher_key_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Display {
    pub name: String,
    pub description: String,
    pub source_kind: String,
    pub source_name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WebBinding {
    pub path: String,
    pub sha256: String,
    pub entry: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WebDescriptor {
    pub schema: String,
    pub app_id: String,
    pub source_kind: String,
    pub source_name: String,
    pub title: Value,
    pub description: Value,
    pub entry: String,
}

pub(crate) fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub(crate) fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn identifier(value: &str, underscore: bool) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || byte == b'-'
                || underscore && byte == b'_'
        })
}

pub(crate) fn architecture() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => "unsupported",
    }
}

impl Catalog {
    pub(crate) fn decode(bytes: &[u8], now: DateTime<Utc>) -> Result<Self, String> {
        if bytes.len() > MAX_METADATA {
            return Err("App Store catalog exceeds its metadata limit".into());
        }
        let catalog: Self = serde_json::from_slice(bytes)
            .map_err(|_| "The official App Store catalog has an invalid shape".to_string())?;
        if catalog.schema != "claw.app-store/v1"
            || catalog.repository != REPOSITORY
            || catalog.runtime_abi != 1
            || catalog.apps.is_empty()
            || catalog.apps.len() > 1000
            || catalog.generation.is_empty()
            || catalog.generation.starts_with('0')
            || !catalog.generation.bytes().all(|byte| byte.is_ascii_digit())
            || catalog.generation.parse::<u64>().is_err()
            || catalog.issued_at > now + chrono::Duration::minutes(5)
            || catalog.expires_at <= now
            || catalog.expires_at <= catalog.issued_at
            || catalog.expires_at - catalog.issued_at > chrono::Duration::days(14)
        {
            return Err(
                "App Store catalog identity, ABI or authenticated freshness is invalid".into(),
            );
        }
        let mut identities = BTreeSet::new();
        for record in &catalog.apps {
            record.validate()?;
            if !identities.insert((&record.app_id, &record.architecture)) {
                return Err("App Store catalog has duplicate App identities".into());
            }
        }
        Ok(catalog)
    }

    pub(crate) fn select(&self, app_id: &str, checksum: &str) -> Result<Record, String> {
        self.apps
            .iter()
            .find(|record| {
                record.app_id == app_id
                    && record.artifact.sha256 == checksum
                    && (record.architecture == "all" || record.architecture == architecture())
            })
            .cloned()
            .ok_or_else(|| {
                "The selected App release is unavailable for this architecture; refresh the Store"
                    .into()
            })
    }
}

impl Record {
    fn validate(&self) -> Result<(), String> {
        let publisher = self.provenance.publisher_key_id.strip_prefix("sha256:");
        let content = self.provenance.content_digest.strip_prefix("sha256:");
        let prefix = match self.display.source_kind.as_str() {
            "product" => "app",
            "capability" => "cap",
            _ => return Err("Unknown App source kind in Store".into()),
        };
        if !identifier(&self.app_id, true)
            || !identifier(&self.display.source_name, false)
            || !matches!(self.architecture.as_str(), "all" | "amd64" | "arm64")
            || self.app_version.is_empty()
            || self.app_version.len() > 128
            || self.release_version.is_empty()
            || self.release_version.len() > 128
            || !self
                .release_version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".+~-".contains(&byte))
            || self.release_tag
                != format!(
                    "{prefix}-{}-v{}",
                    self.display.source_name,
                    self.release_version.replace('~', "-")
                )
            || self.display.name.is_empty()
            || self.display.name.len() > 1024
            || self.display.description.len() > 16 * 1024
            || !valid_sha256(&self.artifact.sha256)
            || self.artifact.file != format!("claw-app-snapshot-{}.tar.gz", self.artifact.sha256)
            || self.artifact.size == 0
            || self.artifact.size > 1024 * 1024 * 1024
            || self.artifact.media_type != "application/gzip"
            || self.manifest.path != "app.json"
            || self.manifest.size == 0
            || self.manifest.size > MAX_METADATA as u64
            || !valid_sha256(&self.manifest.sha256)
            || self.provenance.path != ".provenance.json"
            || !valid_sha256(&self.provenance.sha256)
            || !publisher.is_some_and(valid_sha256)
            || !content.is_some_and(valid_sha256)
            || self.web_ui.path != WEB_DESCRIPTOR
            || self.web_ui.entry != "web/index.html"
            || !valid_sha256(&self.web_ui.sha256)
        {
            return Err("App Store release metadata is invalid or not package-bound".into());
        }
        Ok(())
    }

    pub(crate) fn download_url(&self) -> String {
        format!(
            "https://github.com/{REPOSITORY}/releases/download/{}/{}",
            self.release_tag, self.artifact.file
        )
    }

    pub(crate) fn verify_package(&self, package: &VerifiedPackage) -> Result<(), String> {
        let manifest = package
            .read_verified("app.json")
            .map_err(|error| error.to_string())?;
        let descriptor = package
            .read_verified(WEB_DESCRIPTOR)
            .map_err(|error| error.to_string())?;
        let envelope = std::fs::read(package.dir().join(".provenance.json"))
            .map_err(|error| error.to_string())?;
        let source = package.audit_facts();
        if package.id() != self.app_id
            || package.version() != self.app_version
            || package.content_digest() != self.provenance.content_digest
            || source.get("trust").and_then(Value::as_str) != Some("publisher")
            || source.get("publisher_key_id").and_then(Value::as_str)
                != Some(self.provenance.publisher_key_id.as_str())
            || manifest.len() as u64 != self.manifest.size
            || sha256(&manifest) != self.manifest.sha256
            || sha256(&descriptor) != self.web_ui.sha256
            || sha256(&envelope) != self.provenance.sha256
        {
            return Err("Downloaded App differs from its authenticated Store release".into());
        }
        let web = web_descriptor(package)?;
        if web.source_kind != self.display.source_kind
            || web.source_name != self.display.source_name
            || text(&web.title, package.id()) != self.display.name
            || text(&web.description, "") != self.display.description
        {
            return Err(
                "App Store presentation does not match the signed product interface".into(),
            );
        }
        Ok(())
    }
}

fn text(value: &Value, default: &str) -> String {
    value
        .as_str()
        .or_else(|| {
            value.as_object().and_then(|map| {
                map.get("en")
                    .or_else(|| map.values().next())
                    .and_then(Value::as_str)
            })
        })
        .unwrap_or(default)
        .to_string()
}

pub(crate) fn web_descriptor(package: &VerifiedPackage) -> Result<WebDescriptor, String> {
    let bytes = package
        .read_verified(WEB_DESCRIPTOR)
        .map_err(|_| "This installed App has no authenticated Web interface".to_string())?;
    if bytes.len() > 16 * 1024 {
        return Err("App Web descriptor is too large".into());
    }
    let descriptor: WebDescriptor =
        serde_json::from_slice(&bytes).map_err(|_| "App Web descriptor is invalid".to_string())?;
    if descriptor.schema != WEB_PROTOCOL
        || descriptor.app_id != package.id()
        || !matches!(descriptor.source_kind.as_str(), "product" | "capability")
        || !identifier(&descriptor.source_name, false)
        || descriptor.entry != "web/index.html"
    {
        return Err("App Web interface does not bind its owning identity".into());
    }
    package
        .read_verified(&descriptor.entry)
        .map_err(|error| format!("App Web entry is not authenticated: {error}"))?;
    Ok(descriptor)
}

pub(crate) fn web_metadata(package: &VerifiedPackage) -> Result<Value, String> {
    let descriptor = web_descriptor(package)?;
    let manifest: Value =
        serde_json::from_str(&package.manifest_text().map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let tools = manifest
        .get("mcp")
        .and_then(|mcp| mcp.get("tools"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|mut tool| {
            let description = text(tool.get("summary").unwrap_or(&Value::Null), "");
            tool["description"] = Value::String(description);
            if let Some(name) = tool.get("name").and_then(Value::as_str) {
                let command = name.rsplit('.').next().unwrap_or(name).to_string();
                tool["command"] = Value::String(command);
            }
            tool
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({
        "id": package.id(), "version": package.version(),
        "content_digest": package.content_digest(),
        "name": text(&descriptor.title, package.id()),
        "description": text(&descriptor.description, ""),
        "source_name": descriptor.source_name, "source_kind": descriptor.source_kind,
        "entry": descriptor.entry, "protocol": WEB_PROTOCOL, "tools": tools,
    }))
}

fn allowed_download(url: &url::Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && url.port_or_known_default() == Some(443)
        && matches!(
            url.host_str(),
            Some(
                "xiaoyu-work.github.io"
                    | "github.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
            )
        )
}

pub(crate) fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(180))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 || !allowed_download(attempt.url()) {
                attempt.error("App downloads must remain on the official HTTPS release hosts")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|error| error.to_string())
}

pub(crate) async fn fetch_bytes(
    client: &reqwest::Client,
    url: &str,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let parsed = url::Url::parse(url).map_err(|error| error.to_string())?;
    if !allowed_download(&parsed) {
        return Err("App download URL is not an official HTTPS endpoint".into());
    }
    let response = client
        .get(parsed)
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .send()
        .await
        .map_err(|error| format!("App release download failed: {error}"))?
        .error_for_status()
        .map_err(|error| format!("Official App release is unavailable: {error}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err("App release download exceeds its size limit".into());
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| error.to_string())?;
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err("App release download exceeds its size limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(crate) async fn verify_catalog_signature(bytes: &[u8], signature: &[u8]) -> Result<(), String> {
    verify_signature(bytes, signature, ARCHIVE_KEY, ARCHIVE_FINGERPRINT).await
}

async fn verify_signature(
    bytes: &[u8],
    signature: &[u8],
    public_key: &str,
    fingerprint: &str,
) -> Result<(), String> {
    if bytes.len() > MAX_METADATA || signature.len() > 128 * 1024 {
        return Err("App catalog signature or metadata is too large".into());
    }
    let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
    let payload = public_key
        .lines()
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with('-')
                && !line.starts_with('=')
                && !line.contains(':')
        })
        .collect::<String>();
    let key = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|_| "The compiled App archive key is invalid".to_string())?;
    let keyring = temporary.path().join("trusted.gpg");
    let catalog = temporary.path().join("index.json");
    let detached = temporary.path().join("index.json.asc");
    std::fs::write(&keyring, key).map_err(|error| error.to_string())?;
    std::fs::write(&catalog, bytes).map_err(|error| error.to_string())?;
    std::fs::write(&detached, signature).map_err(|error| error.to_string())?;
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new("/usr/bin/gpgv")
            .env_clear()
            .arg("--homedir")
            .arg(temporary.path())
            .arg("--keyring")
            .arg(&keyring)
            .arg("--status-fd")
            .arg("1")
            .arg(&detached)
            .arg(&catalog)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "App archive signature verification timed out".to_string())?
    .map_err(|error| format!("App archive verifier is unavailable: {error}"))?;
    let status = String::from_utf8_lossy(&result.stdout);
    let valid = status
        .lines()
        .filter(|line| line.starts_with("[GNUPG:] VALIDSIG "))
        .collect::<Vec<_>>();
    if !result.status.success()
        || valid.len() != 1
        || valid[0].split_whitespace().last() != Some(fingerprint)
    {
        return Err(
            "App Store catalog is not signed by the pinned official archive publisher".into(),
        );
    }
    Ok(())
}

fn check_tar_headers(source: &Path) -> Result<(), String> {
    let file = std::fs::File::open(source).map_err(|error| error.to_string())?;
    let mut stream = flate2::read::MultiGzDecoder::new(file);
    let mut count = 0_usize;
    let mut total = 0_u64;
    let mut pending_metadata = false;
    loop {
        let mut block = [0_u8; 512];
        stream
            .read_exact(&mut block)
            .map_err(|error| error.to_string())?;
        if block.iter().all(|byte| *byte == 0) {
            let mut tail = Vec::new();
            stream
                .take(10 * 1024 + 1)
                .read_to_end(&mut tail)
                .map_err(|error| error.to_string())?;
            if pending_metadata
                || tail.len() < 512
                || tail.len() > 10 * 1024
                || tail.iter().any(|byte| *byte != 0)
            {
                return Err("App archive contains truncated or trailing unindexed data".into());
            }
            return Ok(());
        }
        let header = tar::Header::from_byte_slice(&block);
        let size = header.size().map_err(|error| error.to_string())?;
        count += 1;
        total = total.checked_add(size).ok_or("App archive size overflow")?;
        if count > 40_000 || total > MAX_DOWNLOAD {
            return Err("App archive exceeds its expanded size or entry limit".into());
        }
        let kind = header.entry_type();
        if kind == tar::EntryType::XHeader {
            if pending_metadata || size > MAX_METADATA as u64 {
                return Err("App archive extended metadata is excessive or nested".into());
            }
            pending_metadata = true;
        } else if kind.is_file() || kind.is_dir() {
            pending_metadata = false;
        } else {
            return Err("App archives cannot carry links, hooks or special nodes".into());
        }
        let padded = size.div_ceil(512) * 512;
        let consumed = std::io::copy(&mut (&mut stream).take(padded), &mut std::io::sink())
            .map_err(|error| error.to_string())?;
        if consumed != padded {
            return Err("App archive has a truncated member".into());
        }
    }
}

pub(crate) fn extract_snapshot(source: &Path, destination: &Path) -> Result<(), String> {
    check_tar_headers(source)?;
    std::fs::create_dir(destination).map_err(|error| error.to_string())?;
    let file = std::fs::File::open(source).map_err(|error| error.to_string())?;
    let mut archive = tar::Archive::new(flate2::read::MultiGzDecoder::new(file));
    let mut entries = BTreeSet::new();
    let mut total = 0_u64;
    for entry in archive.entries().map_err(|error| error.to_string())? {
        let mut entry = entry.map_err(|error| error.to_string())?;
        let path = entry
            .path()
            .map_err(|error| error.to_string())?
            .into_owned();
        let relative = path.to_str().ok_or("App archive paths must be UTF-8")?;
        if relative != ".provenance.json" {
            crate::provenance::envelope::validate_tree_path(relative)
                .map_err(|error| format!("Unsafe App archive path: {error}"))?;
        }
        if let Some(extensions) = entry.pax_extensions().map_err(|error| error.to_string())? {
            let mut path_seen = false;
            for extension in extensions {
                let extension = extension.map_err(|error| error.to_string())?;
                if extension.key_bytes() != b"path" || path_seen {
                    return Err(
                        "App archives cannot override ownership, modes or unindexed metadata"
                            .into(),
                    );
                }
                path_seen = true;
            }
        }
        let folded = relative.to_lowercase();
        if !entries.insert(folded) || entries.len() > 20_000 {
            return Err("App archive contains duplicate or excessive paths".into());
        }
        if relative
            .split('/')
            .any(|part| matches!(part, ".git" | "DEBIAN"))
            || matches!(relative, "preinst" | "postinst" | "prerm" | "postrm")
        {
            return Err("App payloads cannot supply package-manager or root hooks".into());
        }
        let header = entry.header();
        let size = header.size().map_err(|error| error.to_string())?;
        let mode = header.mode().map_err(|error| error.to_string())?;
        let kind = header.entry_type();
        total = total.checked_add(size).ok_or("App archive size overflow")?;
        if total > MAX_DOWNLOAD
            || header.uid().map_err(|error| error.to_string())? != 0
            || header.gid().map_err(|error| error.to_string())? != 0
            || mode & 0o7022 != 0
            || !(kind.is_dir() || kind.is_file())
        {
            return Err("App archive has unsafe ownership, modes or node types".into());
        }
        let target = destination.join(&path);
        if kind.is_dir() {
            std::fs::create_dir(&target).map_err(|error| error.to_string())?;
        } else {
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|error| error.to_string())?;
            let written =
                std::io::copy(&mut entry, &mut output).map_err(|error| error.to_string())?;
            if written != size {
                return Err("App archive has a truncated file".into());
            }
            output.flush().map_err(|error| error.to_string())?;
            output.sync_all().map_err(|error| error.to_string())?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode))
                .map_err(|error| error.to_string())?;
        }
    }
    crate::provenance::install::assert_safe_tree(
        destination,
        &crate::provenance::install::Limits::default(),
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/apps/store.rs"
    ));
}
