use crate::{CosmicPanelBackground, CosmicPanelConfig, CosmicPanelOuput};
use cosmic_config::cosmic_config_derive::CosmicConfigEntry;
use cosmic_config::{Config, ConfigGet, ConfigSet, CosmicConfigEntry};
use serde::{Deserialize, Serialize};
use tracing::warn;
use xdg_shell_wrapper_config::{Layer, WrapperConfig, WrapperOutput};

#[derive(Default, Debug, Deserialize, Serialize, Clone, PartialEq, CosmicConfigEntry)]
#[version = 1]
#[serde(deny_unknown_fields)]
pub struct CosmicPanelContainerConfigEntry {
    pub entries: Vec<String>,
}

/// Config structure for the cosmic panel
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct CosmicPanelContainerConfig {
    pub config_list: Vec<CosmicPanelConfig>,
}

impl WrapperConfig for CosmicPanelContainerConfig {
    fn outputs(&self) -> WrapperOutput {
        self.config_list.iter().fold(WrapperOutput::Name(vec![]), |mut acc, c| {
            let c_output = c.outputs();
            if matches!(acc, WrapperOutput::All) || matches!(c_output, WrapperOutput::All) {
                return WrapperOutput::All;
            } else if let (WrapperOutput::Name(mut new_n), WrapperOutput::Name(acc_vec)) =
                (c_output, &mut acc)
            {
                acc_vec.append(&mut new_n);
            }
            acc
        })
    }

    fn name(&self) -> &str {
        "Cosmic Panel Config"
    }
}

pub const NAME: &str = "com.clawos.Panel";
pub const VERSION: u64 = 1;

impl CosmicPanelContainerConfig {
    /// load config with the provided name
    pub fn load() -> Result<Self, (Vec<cosmic_config::Error>, Self)> {
        let config = match Self::cosmic_config() {
            Ok(config) => config,
            Err(e) => {
                warn!("Falling back to default panel configuration");
                return Err((vec![e], Self::default()));
            },
        };
        Self::load_from_config(&config, false)
    }

    pub fn load_from_config(
        config: &Config,
        system: bool,
    ) -> Result<Self, (Vec<cosmic_config::Error>, Self)> {
        let entry_names = match config.get::<Vec<String>>("entries") {
            Ok(names) => names,
            Err(e) => {
                warn!("Falling back to default panel configuration");
                return Err((vec![e], Self::default()));
            },
        };
        let mut config_list = Vec::new();
        let mut entry_errors = Vec::new();

        for name in entry_names {
            let config = match if system {
                Config::system(format!("{}.{}", NAME, name).as_str(), VERSION)
            } else {
                Config::new(format!("{}.{}", NAME, name).as_str(), VERSION)
            } {
                Ok(config) => config,
                Err(e) => {
                    entry_errors.push(e);
                    continue;
                },
            };
            match CosmicPanelConfig::get_entry(&config) {
                Ok(entry) => {
                    config_list.push(entry);
                },
                Err((mut errors, entry)) => {
                    config_list.push(entry);
                    entry_errors.append(&mut errors);
                },
            };
        }
        if entry_errors.is_empty() {
            Ok(Self { config_list })
        } else {
            Err((entry_errors, Self { config_list }))
        }
    }

    pub fn configs_for_output(&self, output_name: &str) -> Vec<&CosmicPanelConfig> {
        let mut configs: Vec<_> = self
            .config_list
            .iter()
            .filter(|c| match &c.output {
                CosmicPanelOuput::All => true,
                CosmicPanelOuput::Name(n) => n == output_name,
                _ => false,
            })
            .collect();
        configs.sort_by_key(|b| std::cmp::Reverse(b.get_priority()));
        configs
    }

    pub fn cosmic_config() -> Result<Config, cosmic_config::Error> {
        Config::new(NAME, VERSION)
    }

    pub fn write_entries(&self) -> Result<(), cosmic_config::Error> {
        let config = Self::cosmic_config()?;
        let entry_names = self.config_list.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        config.set("entries", entry_names)?;
        for entry in &self.config_list {
            let config = Config::new(format!("{}.{}", NAME, entry.name).as_str(), VERSION)?;
            entry.write_entry(&config)?;
        }
        Ok(())
    }
}

impl Default for CosmicPanelContainerConfig {
    /// Mirrors `data/default_schema`, which is what a fresh install
    /// actually boots with. These compiled-in values only apply when
    /// that schema is missing, so any drift between the two renders a
    /// visibly different desktop depending on how ClawOS was installed.
    fn default() -> Self {
        Self {
            config_list: vec![
                CosmicPanelConfig {
                    name: "Panel".to_string(),
                    anchor: crate::PanelAnchor::Top,
                    anchor_gap: false,
                    layer: Layer::Top,
                    keyboard_interactivity:
                        xdg_shell_wrapper_config::KeyboardInteractivity::OnDemand,
                    size: crate::PanelSize::XS,
                    output: CosmicPanelOuput::All,
                    background: CosmicPanelBackground::ThemeDefault,
                    plugins_wings: Some((
                        vec![
                            "com.clawos.PanelBrandButton".to_string(),
                            "com.clawos.AppletWorkspaces".to_string(),
                        ],
                        vec![
                            "com.clawos.PanelLauncherButton".to_string(),
                            "com.clawos.PanelCalendarButton".to_string(),
                            "com.clawos.AppletApprovalGate".to_string(),
                            "com.clawos.AppletNetwork".to_string(),
                            "com.clawos.AppletAudio".to_string(),
                            "com.clawos.AppletBattery".to_string(),
                            "com.clawos.AppletTime".to_string(),
                        ],
                    )),
                    plugins_center: Some(vec!["com.clawos.AppletAgentActivity".to_string()]),
                    size_wings: None,
                    size_center: None,
                    expand_to_edges: true,
                    padding: 4,
                    spacing: 4,
                    border_radius: 0,
                    exclusive_zone: true,
                    autohide: None,
                    margin: 0,
                    opacity: 0.6,
                    autohover_delay_ms: Some(500),
                    padding_overlap: 0.5,
                },
                CosmicPanelConfig {
                    name: "Dock".to_string(),
                    anchor: crate::PanelAnchor::Left,
                    anchor_gap: true,
                    layer: Layer::Top,
                    keyboard_interactivity:
                        xdg_shell_wrapper_config::KeyboardInteractivity::OnDemand,
                    size: crate::PanelSize::M,
                    output: CosmicPanelOuput::All,
                    background: CosmicPanelBackground::ThemeDefault,
                    plugins_wings: None,
                    plugins_center: Some(vec![
                        "com.clawos.AppList".to_string(),
                        "com.clawos.PanelDockDivider".to_string(),
                        "com.clawos.PanelAppButton".to_string(),
                    ]),
                    size_wings: None,
                    size_center: None,
                    expand_to_edges: false,
                    padding: 2,
                    spacing: 0,
                    border_radius: 32,
                    exclusive_zone: true,
                    autohide: None,
                    margin: 24,
                    opacity: 0.6,
                    autohover_delay_ms: Some(500),
                    padding_overlap: 0.5,
                },
                CosmicPanelConfig {
                    name: "WidgetRail".to_string(),
                    anchor: crate::PanelAnchor::Right,
                    anchor_gap: true,
                    layer: Layer::Top,
                    keyboard_interactivity:
                        xdg_shell_wrapper_config::KeyboardInteractivity::OnDemand,
                    size: crate::PanelSize::Custom(328),
                    output: CosmicPanelOuput::All,
                    background: CosmicPanelBackground::Color([0.0, 0.0, 0.0]),
                    plugins_wings: None,
                    plugins_center: Some(vec!["com.clawos.AppletWidgetRail".to_string()]),
                    size_wings: None,
                    size_center: None,
                    expand_to_edges: false,
                    padding: 0,
                    spacing: 0,
                    border_radius: 0,
                    exclusive_zone: false,
                    autohide: None,
                    margin: 24,
                    opacity: 0.0,
                    autohover_delay_ms: None,
                    padding_overlap: 0.0,
                },
            ],
        }
    }
}
