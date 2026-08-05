// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use cosmic::cosmic_config::{
    self, Config, CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry,
};
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
pub const APP_ID: &str = "com.clawos.AppList";
const DEFAULT_FAVORITES: &[&str] = &[
    "com.clawos.Agent",
    "thunderbird",
    "chromium",
    "com.clawos.Term",
    "com.clawos.Files",
    "com.clawos.Edit",
    "com.clawos.Player",
    "com.clawos.Store",
    "com.clawos.Settings",
];

#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
pub enum ToplevelFilter {
    #[default]
    ActiveWorkspace,
    ConfiguredOutput,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, CosmicConfigEntry)]
#[version = 1]
pub struct AppListConfig {
    pub filter_top_levels: Option<ToplevelFilter>,
    pub favorites: Vec<String>,
    pub enable_drag_source: bool,
}

impl Default for AppListConfig {
    fn default() -> Self {
        Self {
            filter_top_levels: None,
            favorites: DEFAULT_FAVORITES
                .iter()
                .map(|id| (*id).to_string())
                .collect(),
            enable_drag_source: true,
        }
    }
}

impl AppListConfig {
    pub fn add_pinned(&mut self, id: String, config: &Config) {
        if !self.favorites.contains(&id) {
            self.favorites.push(id);
            let _ = self.write_entry(config);
        }
    }

    pub fn remove_pinned(&mut self, id: &str, config: &Config) {
        if let Some(pos) = self.favorites.iter().position(|e| e == id) {
            self.favorites.remove(pos);
            let _ = self.write_entry(config);
        }
    }

    pub fn update_pinned(&mut self, favorites: Vec<String>, config: &Config) {
        self.favorites = favorites;
        let _ = self.write_entry(config);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn default_favorites_match_the_claw_dock_order() {
        assert_eq!(
            AppListConfig::default().favorites,
            DEFAULT_FAVORITES,
            "compiled defaults must stay aligned with data/default_schema",
        );
    }

    #[test]
    fn every_shipped_default_matches() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let schema_dir = manifest.join("../data/default_schema/com.clawos.AppList/v1");
        let favorites_path = schema_dir.join("favorites");
        let favorites: Vec<String> = ron::from_str(
            &std::fs::read_to_string(&favorites_path)
                .unwrap_or_else(|e| panic!("read {}: {e}", favorites_path.display())),
        )
        .unwrap_or_else(|e| panic!("parse {}: {e}", favorites_path.display()));
        let filter_path = schema_dir.join("filter_top_levels");
        let filter_top_levels: Option<ToplevelFilter> = ron::from_str(
            &std::fs::read_to_string(&filter_path)
                .unwrap_or_else(|e| panic!("read {}: {e}", filter_path.display())),
        )
        .unwrap_or_else(|e| panic!("parse {}: {e}", filter_path.display()));
        let drag_path = schema_dir.join("enable_drag_source");
        let enable_drag_source: bool = ron::from_str(
            &std::fs::read_to_string(&drag_path)
                .unwrap_or_else(|e| panic!("read {}: {e}", drag_path.display())),
        )
        .unwrap_or_else(|e| panic!("parse {}: {e}", drag_path.display()));
        let schema = AppListConfig {
            filter_top_levels,
            favorites,
            enable_drag_source,
        };

        let sample_path = manifest.join("src/config.ron");
        let sample: AppListConfig = ron::from_str(
            &std::fs::read_to_string(&sample_path)
                .unwrap_or_else(|e| panic!("read {}: {e}", sample_path.display())),
        )
        .unwrap_or_else(|e| panic!("parse {}: {e}", sample_path.display()));

        let compiled = AppListConfig::default();
        assert_eq!(schema, compiled);
        assert_eq!(sample, compiled);
    }
}
