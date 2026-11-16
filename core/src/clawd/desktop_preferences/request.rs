use crate::caps::{Cap, Scope, Verb};
use crate::clawd::wire::bounded::Token;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

pub(super) const MAX_VALUE: usize = 64 * 1024;
pub(super) const MAX_REQUEST: usize = 256 * 1024;
pub(super) const MAX_ENTRIES: usize = 128;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Namespace {
    Compositor,
    Workspaces,
    Toolkit,
    ThemeMode,
    ThemeLight,
    ThemeDark,
    ThemeBuilderLight,
    ThemeBuilderDark,
    Panel,
    Dock,
    PanelContainer,
    Idle,
    Audio,
    Daemon,
    Time,
    Shortcuts,
    WindowRules,
    Background,
    BackgroundState,
    Regional,
    KeyboardCatalog,
}

impl Namespace {
    pub(super) fn config_id(self) -> Option<&'static str> {
        Some(match self {
            Self::Compositor => "com.clawos.Comp",
            Self::Workspaces => "com.clawos.Workspaces",
            Self::Toolkit => "com.clawos.Tk",
            Self::ThemeMode => "com.clawos.Theme.Mode",
            Self::ThemeLight => "com.clawos.Theme.Light",
            Self::ThemeDark => "com.clawos.Theme.Dark",
            Self::ThemeBuilderLight => "com.clawos.Theme.Light.Builder",
            Self::ThemeBuilderDark => "com.clawos.Theme.Dark.Builder",
            Self::Panel => "com.clawos.Panel.Panel",
            Self::Dock => "com.clawos.Panel.Dock",
            Self::PanelContainer => "com.clawos.Panel",
            Self::Idle => "com.clawos.Idle",
            Self::Audio => "com.clawos.Audio",
            Self::Daemon => "com.clawos.SettingsDaemon",
            Self::Time => "com.clawos.AppletTime",
            Self::Shortcuts => "com.clawos.Settings.Shortcuts",
            Self::WindowRules => "com.clawos.Settings.WindowRules",
            Self::Background | Self::BackgroundState => "com.clawos.Background",
            Self::Regional => "com.clawos.Settings",
            Self::KeyboardCatalog => return None,
        })
    }

    pub(super) fn resource(self) -> &'static str {
        match self {
            Self::Compositor => "desktop.preferences.compositor",
            Self::Workspaces => "desktop.preferences.workspaces",
            Self::Toolkit
            | Self::ThemeMode
            | Self::ThemeLight
            | Self::ThemeDark
            | Self::ThemeBuilderLight
            | Self::ThemeBuilderDark => "desktop.preferences.appearance",
            Self::Panel | Self::Dock | Self::PanelContainer => "desktop.preferences.panels",
            Self::Idle => "desktop.preferences.idle",
            Self::Audio => "desktop.preferences.audio",
            Self::Daemon => "desktop.preferences.accessibility",
            Self::Time => "desktop.preferences.clock",
            Self::Shortcuts => "desktop.preferences.shortcuts",
            Self::WindowRules => "desktop.preferences.window-rules",
            Self::Background | Self::BackgroundState => "desktop.preferences.background",
            Self::Regional => "desktop.preferences.regional",
            Self::KeyboardCatalog => "desktop.preferences.keyboard-catalog",
        }
    }

    pub(super) fn admits_key(self, key: &str) -> bool {
        !matches!(self, Self::KeyboardCatalog)
            && valid_key(key)
            && (!matches!(self, Self::Regional) || key == "system_locales")
            && (!matches!(self, Self::Daemon) || key == "mono_sound")
    }
}

pub(super) fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        && !key.starts_with('.')
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Entry {
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Action {
    Read,
    Write { entries: Vec<Entry> },
}

#[derive(Clone, Debug, Serialize)]
pub struct Request {
    pub(super) session: Token,
    pub(super) namespace: Namespace,
    pub(super) version: u8,
    #[serde(flatten)]
    pub(super) action: Action,
}

impl<'de> Deserialize<'de> for Request {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Fields {
            session: Token,
            namespace: Namespace,
            version: u8,
            #[serde(flatten)]
            rest: std::collections::BTreeMap<String, Value>,
        }
        let fields = Fields::deserialize(deserializer)?;
        let allowed: &[&str] = if fields.rest.get("action").and_then(Value::as_str) == Some("write") {
            &["action", "entries"]
        } else {
            &["action"]
        };
        if fields.rest.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err(serde::de::Error::custom("unknown desktop preferences request field"));
        }
        let action: Action =
            serde_json::from_value(Value::Object(fields.rest.into_iter().collect()))
                .map_err(serde::de::Error::custom)?;
        let request = Self {
            session: fields.session,
            namespace: fields.namespace,
            version: fields.version,
            action,
        };
        request.validate().map_err(serde::de::Error::custom)?;
        Ok(request)
    }
}

impl Request {
    pub fn decode(value: Value) -> Result<Self, String> {
        serde_json::from_value(value)
            .map_err(|error| format!("invalid desktop preferences request: {error}"))
    }

    pub fn session(&self) -> &str {
        self.session.as_str()
    }

    pub fn required_cap(&self) -> Cap {
        Cap::new(
            match self.action {
                Action::Read => Verb::IPC_SUBSCRIBE,
                Action::Write { .. } => Verb::IPC_INVOKE,
            },
            Scope::name(self.namespace.resource()),
        )
    }

    pub fn required_caps(&self) -> Vec<Cap> {
        let mut caps = vec![self.required_cap()];
        if matches!(self.namespace, Namespace::Daemon) && matches!(self.action, Action::Write { .. }) {
            caps.push(Cap::new(Verb::DEVICE_AUDIO, Scope::name("output")));
        }
        caps
    }

    fn validate(&self) -> Result<(), &'static str> {
        if self.version != 1 {
            return Err("unsupported desktop preferences version");
        }
        if let Action::Write { entries } = &self.action {
            if matches!(self.namespace, Namespace::BackgroundState | Namespace::KeyboardCatalog) {
                return Err("this desktop metadata resource is read-only");
            }
            if entries.is_empty() || entries.len() > MAX_ENTRIES {
                return Err("desktop preferences require 1 to 128 entries");
            }
            let mut keys = std::collections::BTreeSet::new();
            let mut bytes = 0;
            for entry in entries {
                if !self.namespace.admits_key(&entry.key) || !keys.insert(&entry.key) {
                    return Err("desktop preferences contain an invalid or repeated key");
                }
                if entry.value.len() > MAX_VALUE || entry.value.contains('\0') {
                    return Err("desktop preference value exceeds its bound or contains NUL");
                }
                bytes += entry.key.len() + entry.value.len();
            }
            if bytes > MAX_REQUEST / 2 {
                return Err("desktop preferences exceed the combined value bound");
            }
        }
        Ok(())
    }
}
