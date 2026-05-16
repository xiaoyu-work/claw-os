// Minimal stand-in after the Launchpad-style redesign — the original "groups"
// feature was removed in favour of a single flat search + grid of all apps.
//
// `AppLibraryConfig` is kept only as a persisted (empty) cosmic-config record
// for backwards-compatibility with on-disk state. All filtering is now just
// "name contains the search string" against the full app list.

use std::sync::Arc;

use cosmic::cosmic_config::cosmic_config_derive::CosmicConfigEntry;
use cosmic::cosmic_config::{
    CosmicConfigEntry, {self},
};
use cosmic::desktop::DesktopEntryData;
use serde::{Deserialize, Serialize};

use crate::config::APP_ID;

#[derive(Default, Serialize, Deserialize, CosmicConfigEntry, Clone, Debug, PartialEq, Eq)]
#[version = 1]
pub struct AppLibraryConfig {}

impl AppLibraryConfig {
    pub fn helper() -> Option<cosmic_config::Config> {
        cosmic_config::Config::new(APP_ID, Self::VERSION).ok()
    }

    /// Filter the supplied app list by a free-form search string. The search is
    /// case-insensitive and matches against the app name and freedesktop
    /// categories.
    pub fn filtered(
        &self,
        input_value: &str,
        entries: &[Arc<DesktopEntryData>],
    ) -> Vec<Arc<DesktopEntryData>> {
        let needle = input_value.trim().to_lowercase();
        if needle.is_empty() {
            return entries.to_vec();
        }
        entries
            .iter()
            .filter(|de| {
                de.name.to_lowercase().contains(&needle)
                    || de
                        .categories
                        .iter()
                        .any(|acat| acat.to_lowercase() == needle)
            })
            .cloned()
            .collect()
    }
}
