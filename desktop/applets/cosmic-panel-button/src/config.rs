use rustc_hash::FxHashMap;

use cosmic_config::{CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone, CosmicConfigEntry)]
#[version = 1]
#[serde(deny_unknown_fields)]
pub struct CosmicPanelButtonConfig {
    /// configs indexed by panel name
    pub configs: FxHashMap<String, IndividualConfig>,
}

impl Default for CosmicPanelButtonConfig {
    fn default() -> Self {
        Self {
            configs: FxHashMap::from_iter([
                (
                    "Panel".to_string(),
                    IndividualConfig {
                        force_presentation: None,
                    },
                ),
                (
                    "Dock".to_string(),
                    IndividualConfig {
                        force_presentation: Some(Override::Icon),
                    },
                ),
            ]),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Default, Clone)]
pub struct IndividualConfig {
    pub force_presentation: Option<Override>,
}

#[derive(Debug, PartialEq, Clone, Copy, Serialize, Deserialize)]
pub enum Override {
    Icon,
    Text,
    /// Icon and label side by side — used for the shell's branding
    /// button, where the mark alone is not self-explanatory.
    IconAndText,
    /// Non-interactive separator used to divide Dock groups.
    Divider,
}
