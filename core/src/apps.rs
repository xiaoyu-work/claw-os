use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// An app manifest loaded from `app.json`. There is one manifest format
/// — [`caps::manifest::Manifest`](crate::caps::manifest::Manifest) — and
/// the loader rejects anything that doesn't validate.
pub type AppManifest = crate::caps::manifest::Manifest;

/// Discovered app: manifest + path on disk.
#[derive(Debug, Clone)]
pub struct App {
    pub manifest: AppManifest,
    pub dir: PathBuf,
}

impl App {
    pub fn main_py(&self) -> PathBuf {
        self.dir.join("main.py")
    }
}

/// Scan `apps_dir` for subdirectories containing a valid `app.json`.
///
/// Apps whose manifest fails to parse or validate are silently skipped
/// — keeping a broken app installed must never poison discovery for
/// the rest of the system. Operators can run `cos app doctor` (TBD) to
/// see why a specific app is missing.
pub fn discover(apps_dir: &Path) -> BTreeMap<String, App> {
    let mut apps = BTreeMap::new();

    let entries = match fs::read_dir(apps_dir) {
        Ok(e) => e,
        Err(_) => return apps,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("app.json");
        if !manifest_path.is_file() {
            continue;
        }
        let data = match fs::read_to_string(&manifest_path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let manifest = match AppManifest::from_json(&data) {
            Ok(m) => m,
            Err(_) => continue,
        };
        // The directory name is the canonical lookup key; we require it
        // to match the manifest's declared id so authors can't ship a
        // mismatched pair that confuses routing.
        let dir_name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if dir_name != manifest.id {
            continue;
        }
        apps.insert(
            manifest.id.clone(),
            App {
                manifest,
                dir: path,
            },
        );
    }

    apps
}
