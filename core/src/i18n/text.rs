//! An owned, runtime-loaded counterpart to [`LocalizedStr`].
//!
//! [`LocalizedStr`](super::string::LocalizedStr) carries `&'static str`s
//! and is used for compile-time-baked strings (catalog labels, role
//! descriptions, kernel error templates).  When a string is loaded
//! from disk — a manifest, a config file, an inbox entry — it cannot
//! be `&'static`, so we use [`LocalizedText`] instead.
//!
//! The JSON shape is intentionally permissive:
//!
//! ```json
//! "name": "Files"
//! ```
//!
//! is treated as `{ "en": "Files" }`, so authors writing v2 manifests
//! who only have English can drop the wrapper object. Either form
//! deserializes into the same `LocalizedText`.
//!
//! Lookup falls back to English when the requested locale is missing.
//! Authors are required to provide at least an English entry — the
//! [`validate`](LocalizedText::validate) helper enforces this and is
//! invoked by the manifest parser.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

use super::locale::{current_locale, Locale};

/// A runtime-loaded, locale-aware string. Internally a sparse map
/// from locale code (e.g. `"en"`, `"zh-CN"`) to translation.
///
/// English (`"en"`) is required; other locales are optional. The
/// [`get`](Self::get) and [`current`](Self::current) accessors fall
/// back to English when a translation is missing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct LocalizedText {
    entries: BTreeMap<String, String>,
}

impl LocalizedText {
    /// Construct from an English-only string.
    pub fn en(s: impl Into<String>) -> Self {
        let mut entries = BTreeMap::new();
        entries.insert("en".to_string(), s.into());
        Self { entries }
    }

    /// Add (or replace) a translation for the given locale code.
    pub fn with(mut self, locale_code: impl Into<String>, text: impl Into<String>) -> Self {
        self.entries.insert(locale_code.into(), text.into());
        self
    }

    /// Translation for `locale`, falling back to English. Returns an
    /// empty string only if even the English entry is missing — which
    /// [`validate`](Self::validate) is supposed to prevent.
    pub fn get(&self, locale: Locale) -> &str {
        self.entries
            .get(locale.code())
            .map(String::as_str)
            .or_else(|| self.entries.get("en").map(String::as_str))
            .unwrap_or("")
    }

    /// Translation for the process-wide current locale.
    pub fn current(&self) -> &str {
        self.get(current_locale())
    }

    /// English translation. Panic-free; returns `""` if absent (use
    /// [`validate`](Self::validate) early to avoid this).
    pub fn en_str(&self) -> &str {
        self.entries.get("en").map(String::as_str).unwrap_or("")
    }

    /// True if the English entry is present and non-empty.
    pub fn has_english(&self) -> bool {
        self.entries
            .get("en")
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    }

    /// Validate that the minimum content is present. Manifest parsers
    /// call this so misconfigured apps fail loudly rather than
    /// rendering as blank text in the UI.
    pub fn validate(&self) -> Result<(), String> {
        if !self.has_english() {
            return Err("missing required English (`en`) translation".into());
        }
        Ok(())
    }

    /// Iterate `(locale_code, text)` pairs in alphabetical order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// True if no entries at all.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<'de> Deserialize<'de> for LocalizedText {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Accept either a bare string ("Files") or a map ({"en": "Files"}).
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Bare(String),
            Map(BTreeMap<String, String>),
        }
        match Repr::deserialize(d)? {
            Repr::Bare(s) => Ok(LocalizedText::en(s)),
            Repr::Map(m) => Ok(LocalizedText { entries: m }),
        }
    }
}

impl std::fmt::Display for LocalizedText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.current())
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/i18n/text.rs"
    ));
}
