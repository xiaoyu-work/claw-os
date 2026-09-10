//! Presentation records, not durable notification state or authorization.
//!
//! Any App implementing this contract can present notifications. A display label
//! is not producer identity, a numeric handle is connection-local, and a muted
//! presentation does not change OS delivery/DND policy.

use serde::{Deserialize, Serialize};

#[cfg(feature = "dbus")]
pub mod proxy;

pub const VERSION: u32 = 1;
pub const INTERFACE: &str = "com.clawos.NotificationPresentation1";
pub const OBJECT_PATH: &str = "/com/clawos/NotificationPresentation1";
pub const PANEL_FD_ENV: &str = "PANEL_NOTIFICATIONS_FD";
pub const PRESENTER_FD_ENV: &str = "DAEMON_NOTIFICATIONS_FD";
pub const APPLET_FD_ENV: &str = "COSMIC_NOTIFICATIONS";
pub const MAX_PACKET_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_IMAGE_BYTES: usize = 1024 * 1024;
pub const MAX_IMAGE_DIMENSION: u32 = 512;
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_RUNS: usize = 4096;
pub const MAX_PREFERENCES_BYTES: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid notification presentation: {0}")]
    Invalid(&'static str),
    #[error("invalid notification presentation JSON: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub version: u32,
    pub muted: bool,
}

impl Preferences {
    pub fn new(muted: bool) -> Self {
        Self {
            version: VERSION,
            muted,
        }
    }

    pub fn validate(&self) -> Result<(), Error> {
        validate_version(self.version)
    }

    pub fn from_json(value: &str) -> Result<Self, Error> {
        if value.len() > MAX_PREFERENCES_BYTES {
            return Err(Error::Invalid("preferences exceed the byte limit"));
        }
        let preferences: Self = serde_json::from_str(value)?;
        preferences.validate()?;
        Ok(preferences)
    }

    pub fn to_json(&self) -> Result<String, Error> {
        self.validate()?;
        Ok(serde_json::to_string(self)?)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextRun {
    pub text: String,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub underline: bool,
    #[serde(default)]
    pub accent: bool,
}

/// Host-readable paths, URIs and arbitrary markup are deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Icon {
    Rgba {
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    },
    Mask {
        width: u32,
        height: u32,
        alpha: Vec<u8>,
    },
}

impl Icon {
    pub fn validate(&self) -> Result<(), Error> {
        match self {
            Self::Rgba {
                width,
                height,
                pixels,
            } => validate_pixels(*width, *height, pixels.len(), 4),
            Self::Mask {
                width,
                height,
                alpha,
            } => validate_pixels(*width, *height, alpha.len(), 1),
        }
    }
}

fn validate_pixels(width: u32, height: u32, length: usize, channels: u64) -> Result<(), Error> {
    let pixels = u64::from(width) * u64::from(height);
    if width == 0
        || height == 0
        || width > MAX_IMAGE_DIMENSION
        || height > MAX_IMAGE_DIMENSION
        || pixels > (MAX_IMAGE_BYTES / 4) as u64
        || pixels.checked_mul(channels) != Some(length as u64)
    {
        return Err(Error::Invalid(
            "invalid or excessive decoded image dimensions",
        ));
    }
    Ok(())
}

/// A rendering projection supplied by a presenter, never a durable record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Card {
    pub version: u32,
    pub id: u32,
    pub source_label: String,
    pub title: String,
    pub body: Vec<TextRun>,
    pub activation: Option<String>,
    pub icon: Option<Icon>,
    pub group_icon: Option<Icon>,
}

impl Card {
    pub fn validate(&self) -> Result<(), Error> {
        validate_version(self.version)?;
        validate_handle(self.id)?;
        if self.source_label.len() > 1024 || self.title.len() > MAX_TEXT_BYTES {
            return Err(Error::Invalid(
                "display label or title exceeds the byte limit",
            ));
        }
        if self.body.len() > MAX_RUNS
            || self.body.iter().map(|run| run.text.len()).sum::<usize>() > MAX_TEXT_BYTES
        {
            return Err(Error::Invalid("body exceeds the text/run limit"));
        }
        if let Some(action) = &self.activation {
            validate_action(self.id, action)?;
        }
        for icon in [&self.icon, &self.group_icon].into_iter().flatten() {
            icon.validate()?;
        }
        Ok(())
    }

    pub fn from_json(value: &str) -> Result<Self, Error> {
        if value.len() > MAX_PACKET_BYTES {
            return Err(Error::Invalid("record exceeds the byte limit"));
        }
        let card: Self = serde_json::from_str(value)?;
        card.validate()?;
        Ok(card)
    }

    pub fn to_json(&self) -> Result<String, Error> {
        self.validate()?;
        let value = serde_json::to_string(self)?;
        if value.len() > MAX_PACKET_BYTES {
            return Err(Error::Invalid("record exceeds the byte limit"));
        }
        Ok(value)
    }
}

pub fn validate_handle(id: u32) -> Result<(), Error> {
    if id == 0 {
        Err(Error::Invalid("presentation handle must be nonzero"))
    } else {
        Ok(())
    }
}

pub fn validate_action(id: u32, action: &str) -> Result<(), Error> {
    validate_handle(id)?;
    if action.len() > 4096 {
        return Err(Error::Invalid("action exceeds the byte limit"));
    }
    Ok(())
}

fn validate_version(version: u32) -> Result<(), Error> {
    if version == VERSION {
        Ok(())
    } else {
        Err(Error::Invalid("unsupported protocol version"))
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/lib.rs"));
}
