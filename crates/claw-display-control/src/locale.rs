//! Bounded PAM presentation values. These never identify a display or grant access.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Error;

pub const KEYS: &[&str] = &[
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_NUMERIC",
    "LC_TIME",
    "LC_COLLATE",
    "LC_MONETARY",
    "LC_MESSAGES",
    "LC_PAPER",
    "LC_NAME",
    "LC_ADDRESS",
    "LC_TELEPHONE",
    "LC_MEASUREMENT",
    "LC_IDENTIFICATION",
];
pub const MAX_VALUE: usize = 256;

#[derive(Clone, Debug, Default, Serialize)]
#[serde(transparent)]
pub struct LocaleEnvironment(BTreeMap<String, String>);

impl TryFrom<BTreeMap<String, String>> for LocaleEnvironment {
    type Error = Error;

    fn try_from(values: BTreeMap<String, String>) -> Result<Self, Error> {
        if values.len() > KEYS.len()
            || values.iter().any(|(key, value)| {
                !KEYS.contains(&key.as_str())
                    || value.len() > MAX_VALUE
                    || !value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_-.@:".contains(&byte))
            })
        {
            return Err(Error::Protocol("invalid bounded PAM locale environment"));
        }
        Ok(Self(values))
    }
}

impl<'de> Deserialize<'de> for LocaleEnvironment {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(BTreeMap::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl LocaleEnvironment {
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/locale.rs"));
}
