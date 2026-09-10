use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::caps::{Cap, Scope, Verb};
use crate::clawd::wire::bounded::{Text, Token};

pub const LOCALE_KEYS: [&str; 10] = [
    "LANG",
    "LC_ADDRESS",
    "LC_IDENTIFICATION",
    "LC_MEASUREMENT",
    "LC_MONETARY",
    "LC_NAME",
    "LC_NUMERIC",
    "LC_PAPER",
    "LC_TELEPHONE",
    "LC_TIME",
];

#[derive(Clone, Debug, Serialize)]
#[serde(transparent)]
pub struct Request(pub(super) Body);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Body {
    SystemLocale {
        session: Token,
        lang: Text<128>,
        region: Text<128>,
    },
    OwnerLanguage {
        session: Token,
        languages: Text<4096>,
    },
    StaticHostname {
        session: Token,
        hostname: Text<64>,
    },
}

impl<'de> Deserialize<'de> for Request {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let request = Self(Body::deserialize(deserializer)?);
        request.validate().map_err(serde::de::Error::custom)?;
        Ok(request)
    }
}

impl Request {
    pub fn decode(value: Value) -> Result<Self, String> {
        serde_json::from_value(value)
            .map_err(|error| format!("invalid regional settings request: {error}"))
    }

    pub fn session(&self) -> &str {
        match &self.0 {
            Body::SystemLocale { session, .. }
            | Body::OwnerLanguage { session, .. }
            | Body::StaticHostname { session, .. } => session.as_str(),
        }
    }

    pub fn action(&self) -> &'static str {
        match self.0 {
            Body::SystemLocale { .. } => "system_locale",
            Body::OwnerLanguage { .. } => "owner_language",
            Body::StaticHostname { .. } => "static_hostname",
        }
    }

    pub fn required_cap(&self) -> Cap {
        let (verb, scope) = match self.0 {
            Body::SystemLocale { .. } => (Verb::SYS_LOCALE, "system"),
            Body::OwnerLanguage { .. } => (Verb::SYS_LANGUAGE, "self"),
            Body::StaticHostname { .. } => (Verb::SYS_HOSTNAME, "static"),
        };
        Cap::new(verb, Scope::name(scope))
    }

    fn validate(&self) -> Result<(), &'static str> {
        match &self.0 {
            Body::SystemLocale { lang, region, .. } => {
                validate_locale(lang.as_str())?;
                validate_locale(region.as_str())
            }
            Body::OwnerLanguage { languages, .. } => {
                let mut count = 0;
                for language in languages.as_str().split(':') {
                    validate_locale(language)?;
                    count += 1;
                    if count > 64 {
                        return Err("language preference exceeds 64 entries");
                    }
                }
                Ok(())
            }
            Body::StaticHostname { hostname, .. } => {
                let value = hostname.as_str();
                if value.is_empty()
                    || !value.is_ascii()
                    || !value.split('.').all(|label| {
                        !label.is_empty()
                            && label.len() <= 63
                            && label.as_bytes()[0].is_ascii_alphanumeric()
                            && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                            && label
                                .bytes()
                                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                    })
                {
                    return Err("hostname must contain bounded ASCII hostname labels");
                }
                Ok(())
            }
        }
    }
}

fn validate_locale(value: &str) -> Result<(), &'static str> {
    static GRAMMAR: OnceLock<Regex> = OnceLock::new();
    let grammar = GRAMMAR.get_or_init(|| Regex::new(
        r"^(?:C|POSIX|[A-Za-z]{2,3}(?:_[A-Za-z0-9]{2,8})?)(?:\.[A-Za-z0-9][A-Za-z0-9_-]{0,31})?(?:@[A-Za-z0-9][A-Za-z0-9_-]{0,31})?$"
    ).expect("fixed POSIX locale grammar"));
    if value.len() > 128 || !grammar.is_match(value) {
        return Err("locale must be a bounded ASCII POSIX locale name");
    }
    Ok(())
}

pub(super) fn locale_values(lang: &str, region: &str) -> Vec<String> {
    LOCALE_KEYS
        .iter()
        .map(|key| format!("{key}={}", if *key == "LANG" { lang } else { region }))
        .collect()
}

pub(super) fn locale_confirmed(observed: &[String], lang: &str, region: &str) -> bool {
    if observed.len() > 16 {
        return false;
    }
    let mut values = std::collections::BTreeMap::new();
    for value in observed {
        let Some((key, value)) = value.split_once('=') else {
            return false;
        };
        if value.len() > 128 || key.len() > 32 || values.insert(key, value).is_some() {
            return false;
        }
    }
    if values.get("LANG") != Some(&lang) || values.contains_key("LC_ALL") {
        return false;
    }
    // localed may omit LC_* values equal to LANG. Confirm effective defaults,
    // then return the explicit ten-key projection rather than unrelated state.
    LOCALE_KEYS[1..]
        .iter()
        .all(|key| values.get(key).or_else(|| values.get("LANG")) == Some(&region))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/regional_settings/request.rs"
    ));
}
