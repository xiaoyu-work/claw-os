use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

use crate::clawd::wire::bounded::{Text, TextList, Token};

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct Arguments(TextList<128, 4096>);

impl Arguments {
    pub fn as_slice(&self) -> &[String] {
        self.0.as_slice()
    }
}

impl<'de> Deserialize<'de> for Arguments {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let args = TextList::deserialize(deserializer)?;
        crate::bridge::gui_args::validate_args(args.as_slice())
            .map_err(serde::de::Error::custom)?;
        Ok(Self(args))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PresentationKey {
    #[serde(rename = "LANG")]
    Lang,
    #[serde(rename = "LANGUAGE")]
    Language,
    #[serde(rename = "LC_ALL")]
    LocaleAll,
    #[serde(rename = "LC_MESSAGES")]
    LocaleMessages,
    #[serde(rename = "LC_CTYPE")]
    LocaleCharacterType,
    #[serde(rename = "LC_NUMERIC")]
    LocaleNumeric,
    #[serde(rename = "LC_TIME")]
    LocaleTime,
    #[serde(rename = "LC_COLLATE")]
    LocaleCollation,
    #[serde(rename = "LC_MONETARY")]
    LocaleMonetary,
    #[serde(rename = "LC_PAPER")]
    LocalePaper,
    #[serde(rename = "LC_NAME")]
    LocaleName,
    #[serde(rename = "LC_ADDRESS")]
    LocaleAddress,
    #[serde(rename = "LC_TELEPHONE")]
    LocaleTelephone,
    #[serde(rename = "LC_MEASUREMENT")]
    LocaleMeasurement,
    #[serde(rename = "LC_IDENTIFICATION")]
    LocaleIdentification,
    #[serde(rename = "TZ")]
    Timezone,
    #[serde(rename = "COSMIC_PANEL_NAME")]
    PanelName,
    #[serde(rename = "COSMIC_PANEL_OUTPUT")]
    PanelOutput,
    #[serde(rename = "COSMIC_PANEL_SPACING")]
    PanelSpacing,
    #[serde(rename = "COSMIC_PANEL_ANCHOR")]
    PanelAnchor,
    #[serde(rename = "COSMIC_PANEL_BACKGROUND")]
    PanelBackground,
    #[serde(rename = "COSMIC_PANEL_PADDING_OVERLAP")]
    PanelPaddingOverlap,
    #[serde(rename = "COSMIC_PANEL_SIZE")]
    PanelSize,
}

impl PresentationKey {
    pub const ALL: [Self; 23] = [
        Self::Lang,
        Self::Language,
        Self::LocaleAll,
        Self::LocaleMessages,
        Self::LocaleCharacterType,
        Self::LocaleNumeric,
        Self::LocaleTime,
        Self::LocaleCollation,
        Self::LocaleMonetary,
        Self::LocalePaper,
        Self::LocaleName,
        Self::LocaleAddress,
        Self::LocaleTelephone,
        Self::LocaleMeasurement,
        Self::LocaleIdentification,
        Self::Timezone,
        Self::PanelName,
        Self::PanelOutput,
        Self::PanelSpacing,
        Self::PanelAnchor,
        Self::PanelBackground,
        Self::PanelPaddingOverlap,
        Self::PanelSize,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lang => "LANG",
            Self::Language => "LANGUAGE",
            Self::LocaleAll => "LC_ALL",
            Self::LocaleMessages => "LC_MESSAGES",
            Self::LocaleCharacterType => "LC_CTYPE",
            Self::LocaleNumeric => "LC_NUMERIC",
            Self::LocaleTime => "LC_TIME",
            Self::LocaleCollation => "LC_COLLATE",
            Self::LocaleMonetary => "LC_MONETARY",
            Self::LocalePaper => "LC_PAPER",
            Self::LocaleName => "LC_NAME",
            Self::LocaleAddress => "LC_ADDRESS",
            Self::LocaleTelephone => "LC_TELEPHONE",
            Self::LocaleMeasurement => "LC_MEASUREMENT",
            Self::LocaleIdentification => "LC_IDENTIFICATION",
            Self::Timezone => "TZ",
            Self::PanelName => "COSMIC_PANEL_NAME",
            Self::PanelOutput => "COSMIC_PANEL_OUTPUT",
            Self::PanelSpacing => "COSMIC_PANEL_SPACING",
            Self::PanelAnchor => "COSMIC_PANEL_ANCHOR",
            Self::PanelBackground => "COSMIC_PANEL_BACKGROUND",
            Self::PanelPaddingOverlap => "COSMIC_PANEL_PADDING_OVERLAP",
            Self::PanelSize => "COSMIC_PANEL_SIZE",
        }
    }

    pub fn is_panel(self) -> bool {
        matches!(
            self,
            Self::PanelName
                | Self::PanelOutput
                | Self::PanelSpacing
                | Self::PanelAnchor
                | Self::PanelBackground
                | Self::PanelPaddingOverlap
                | Self::PanelSize
        )
    }
}

pub type Presentation = BTreeMap<PresentationKey, Text<1024>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchRequest {
    pub session_id: Token,
    pub handle: Token,
    pub args: Arguments,
    #[serde(default)]
    pub presentation: Presentation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceRequest {
    pub session_id: Token,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturedOutput {
    pub text: String,
    pub truncated: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub stdout: CapturedOutput,
    pub stderr: CapturedOutput,
}

impl Outcome {
    pub(crate) fn failed(error: String) -> Self {
        Self {
            exit_code: None,
            error: Some(error),
            stdout: CapturedOutput {
                text: String::new(),
                truncated: false,
            },
            stderr: CapturedOutput {
                text: String::new(),
                truncated: false,
            },
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchReply {
    pub started: bool,
    pub layer_shell: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitReply {
    pub finished: bool,
    pub outcome: Option<Outcome>,
    pub retiring: Option<bool>,
    pub retirement_error: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StopReply {
    pub retired: bool,
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/gui/inputs.rs"
    ));
}
