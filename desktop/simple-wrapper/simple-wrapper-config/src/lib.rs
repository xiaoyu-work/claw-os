// SPDX-License-Identifier: MPL-2.0-only

use std::collections::HashMap;
use std::fs::File;
use std::ops::Range;

use anyhow::Result;
use wayland_protocols::wlr::unstable::layer_shell::v1::client::{
    zwlr_layer_shell_v1, zwlr_layer_surface_v1,
};
use serde::{Deserialize, Serialize};
use xdg::BaseDirectories;
use xdg_shell_wrapper_config::{KeyboardInteractivity, Layer, WrapperConfig};

/// Edge to which the panel is anchored
#[derive(Debug, Deserialize, Serialize, Copy, Clone)]
pub enum Anchor {
    /// anchored to left edge
    Left,
    /// anchored to right edge
    Right,
    /// anchored to top edge
    Top,
    /// anchored to bottom edge
    Bottom,
    ///
    Center,
    ///
    TopLeft,
    ///
    TopRight,
    ///
    BottomLeft,
    ///
    BottomRight,
}

impl Default for Anchor {
    fn default() -> Self {
        Anchor::Top
    }
}

impl From<zwlr_layer_surface_v1::Anchor> for Anchor {
    fn from(align: zwlr_layer_surface_v1::Anchor) -> Self {
        if align.contains(zwlr_layer_surface_v1::Anchor::Left) {
            Self::Left
        } else if align.contains(zwlr_layer_surface_v1::Anchor::Right) {
            Self::Right
        } else if align.contains(zwlr_layer_surface_v1::Anchor::Top) {
            Self::Top
        } else if align.contains(zwlr_layer_surface_v1::Anchor::Bottom) {
            Self::Bottom
        } else if align.contains(zwlr_layer_surface_v1::Anchor::Top)
            && align.contains(zwlr_layer_surface_v1::Anchor::Left)
        {
            Self::TopLeft
        } else if align.contains(zwlr_layer_surface_v1::Anchor::Top)
            && align.contains(zwlr_layer_surface_v1::Anchor::Right)
        {
            Self::TopRight
        } else if align.contains(zwlr_layer_surface_v1::Anchor::Bottom)
            && align.contains(zwlr_layer_surface_v1::Anchor::Left)
        {
            Self::BottomLeft
        } else if align.contains(zwlr_layer_surface_v1::Anchor::Bottom)
            && align.contains(zwlr_layer_surface_v1::Anchor::Right)
        {
            Self::BottomRight
        } else {
            Self::Center
        }
    }
}

impl Into<zwlr_layer_surface_v1::Anchor> for Anchor {
    fn into(self) -> zwlr_layer_surface_v1::Anchor {
        let mut anchor = zwlr_layer_surface_v1::Anchor::empty();
        match self {
            Self::Left => {
                anchor.insert(zwlr_layer_surface_v1::Anchor::Left);
            }
            Self::Right => {
                anchor.insert(zwlr_layer_surface_v1::Anchor::Right);
            }
            Self::Top => {
                anchor.insert(zwlr_layer_surface_v1::Anchor::Top);
            }
            Self::Bottom => {
                anchor.insert(zwlr_layer_surface_v1::Anchor::Bottom);
            }
            Anchor::TopLeft => {
                anchor.insert(zwlr_layer_surface_v1::Anchor::Top);
                anchor.insert(zwlr_layer_surface_v1::Anchor::Left)
            }
            Anchor::TopRight => {
                anchor.insert(zwlr_layer_surface_v1::Anchor::Top);
                anchor.insert(zwlr_layer_surface_v1::Anchor::Right)
            }
            Anchor::BottomLeft => {
                anchor.insert(zwlr_layer_surface_v1::Anchor::Bottom);
                anchor.insert(zwlr_layer_surface_v1::Anchor::Left)
            }
            Anchor::BottomRight => {
                anchor.insert(zwlr_layer_surface_v1::Anchor::Bottom);
                anchor.insert(zwlr_layer_surface_v1::Anchor::Right)
            }
            Anchor::Center => {}
        };
        anchor
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SimpleWrapperConfig {
    pub name: String,
    pub anchor: Anchor,
    pub layer: Layer,
    pub keyboard_interactivity: KeyboardInteractivity,
    pub width_range: Option<Range<u32>>,
    pub height_range: Option<Range<u32>>,
    pub output: Option<String>,
    pub exec: String,
}

impl Default for SimpleWrapperConfig {
    fn default() -> Self {
        Self {
            name: "".to_string(),
            anchor: Anchor::Center,
            layer: Layer::Top,
            keyboard_interactivity: KeyboardInteractivity::OnDemand,
            width_range: None,
            height_range: None,
            output: None,
            exec: "".into(),
        }
    }
}

impl SimpleWrapperConfig {
    pub fn load(name: &str) -> Self {
        match Self::configs().remove(name.into()) {
            Some(c) => c,
            _ => Self::default(),
        }
    }

    pub fn write(&self, name: &str) -> Result<()> {
        let mut configs = Self::configs();
        configs.insert(name.into(), SimpleWrapperConfig::default());
        let xdg = BaseDirectories::new()?;
        let f = xdg.place_config_file("simple-wrapper/config.ron").unwrap();
        let f = File::create(f)?;
        ron::ser::to_writer_pretty(&f, &configs, ron::ser::PrettyConfig::default())?;
        return Ok(());
    }

    fn configs() -> HashMap<String, Self> {
        match BaseDirectories::new()
            .map(|dirs| dirs.find_config_file("simple-wrapper/config.ron"))
            .map(|c| c.map(|c| File::open(c)))
            .map(|file| {
                file.map(|file| {
                    ron::de::from_reader::<_, HashMap<String, SimpleWrapperConfig>>(file?)
                })
            }) {
            Ok(Some(Ok(c))) => c,
            _ => HashMap::new(),
        }
    }

    /// get constraints for the thickness of the panel bar
    pub fn dimensions(&self) -> (Option<Range<u32>>, Option<Range<u32>>) {
        (self.width_range.clone(), self.height_range.clone())
    }

    pub fn applet(&self) -> String {
        self.exec.clone()
    }
}

impl WrapperConfig for SimpleWrapperConfig {
    fn output(&self) -> Option<String> {
        self.output.clone()
    }

    fn layer(&self) -> zwlr_layer_shell_v1::Layer {
        self.layer.into()
    }

    fn keyboard_interactivity(&self) -> zwlr_layer_surface_v1::KeyboardInteractivity {
        self.keyboard_interactivity.into()
    }

    fn name(&self) -> &str {
        &self.name
    }
}
