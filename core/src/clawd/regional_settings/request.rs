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
    Status {
        session: Token,
    },
    TimeStatus {
        session: Token,
    },
    Timezone {
        session: Token,
        timezone: Text<128>,
    },
    Ntp {
        session: Token,
        enabled: bool,
    },
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
    SystemKeyboard {
        session: Token,
        layout: Text<256>,
        model: Text<128>,
        variant: Text<256>,
        options: Text<2048>,
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
            Body::Status { session }
            | Body::TimeStatus { session }
            | Body::Timezone { session, .. }
            | Body::Ntp { session, .. }
            | Body::SystemLocale { session, .. }
            | Body::OwnerLanguage { session, .. }
            | Body::StaticHostname { session, .. }
            | Body::SystemKeyboard { session, .. } => session.as_str(),
        }
    }

    pub fn action(&self) -> &'static str {
        match self.0 {
            Body::Status { .. } => "status",
            Body::TimeStatus { .. } => "time_status",
            Body::Timezone { .. } => "timezone",
            Body::Ntp { .. } => "ntp",
            Body::SystemLocale { .. } => "system_locale",
            Body::OwnerLanguage { .. } => "owner_language",
            Body::StaticHostname { .. } => "static_hostname",
            Body::SystemKeyboard { .. } => "system_keyboard",
        }
    }

    pub fn required_cap(&self) -> Cap {
        let (verb, scope) = match self.0 {
            Body::Status { .. } | Body::TimeStatus { .. } => (Verb::SYS_OBSERVE, "regional"),
            Body::Timezone { .. } | Body::Ntp { .. } => {
                return Cap::new(Verb::SYS_TIME, Scope::Wild)
            }
            Body::SystemLocale { .. } => (Verb::SYS_LOCALE, "system"),
            Body::OwnerLanguage { .. } => (Verb::SYS_LANGUAGE, "self"),
            Body::StaticHostname { .. } => (Verb::SYS_HOSTNAME, "static"),
            Body::SystemKeyboard { .. } => (Verb::SYS_LOCALE, "keyboard"),
        };
        Cap::new(verb, Scope::name(scope))
    }

    fn validate(&self) -> Result<(), &'static str> {
        match &self.0 {
            Body::Status { .. } | Body::TimeStatus { .. } | Body::Ntp { .. } => Ok(()),
            Body::Timezone { timezone, .. } => {
                if timezone.as_str().split('/').any(|part| {
                    part.is_empty()
                        || matches!(part, "." | "..")
                        || !part.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric()
                                || matches!(byte, b'_' | b'-' | b'+' | b'.')
                        })
                }) {
                    return Err("timezone must be a bounded IANA name, not a path or command");
                }
                Ok(())
            }
            Body::SystemKeyboard {
                layout,
                model,
                variant,
                options,
                ..
            } => {
                let token = |value: &str| {
                    value.len() <= 64
                        && value.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric()
                                || matches!(byte, b'_' | b'-' | b'+' | b':')
                        })
                };
                let layouts: Vec<_> = layout.as_str().split(',').collect();
                if layouts.len() > 16
                    || layouts
                        .iter()
                        .any(|value| value.is_empty() || !token(value))
                    || !token(model.as_str())
                    || variant.as_str().split(',').count() > layouts.len()
                    || !variant.as_str().split(',').all(token)
                    || !options.as_str().split(',').all(token)
                {
                    return Err(
                        "keyboard defaults require bounded XKB names, not paths or commands",
                    );
                }
                Ok(())
            }
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

pub(super) fn validate_locale(value: &str) -> Result<(), &'static str> {
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
