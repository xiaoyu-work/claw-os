use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// An app manifest loaded from app.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub commands: BTreeMap<String, String>,
    #[serde(default)]
    pub dependencies: serde_json::Value,
}

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

/// Scan `apps_dir` for subdirectories containing a valid app.json.
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
        let manifest: AppManifest = match serde_json::from_str(&data) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let name = manifest.name.clone();
        apps.insert(
            name,
            App {
                manifest,
                dir: path,
            },
        );
    }

    apps
}
