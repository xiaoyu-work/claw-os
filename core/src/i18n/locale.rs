//! The set of locales Claw OS can render strings in, plus the global
//! current-locale cell.
//!
//! The enum is intentionally **closed** (no string variants): adding a
//! new language is a deliberate, code-level change that forces every
//! [`LocalizedStr`](super::string::LocalizedStr) author to consider
//! whether they want to provide a translation. Locales not yet wired
//! into the enum can still be referenced via their BCP-47 code in
//! configuration files; the OS will warn and fall back to
//! [`Locale::DEFAULT`].

use std::sync::OnceLock;
use std::sync::RwLock;

/// Languages Claw OS knows how to render UI strings in.
///
/// To add a new locale:
///   1. Add a variant here.
///   2. Update [`Locale::code`] and [`Locale::parse`].
///   3. Add the matching field on [`super::string::LocalizedStr`] and
///      a `with_<code>(...)` builder.
///   4. Optionally add translations to existing `LocalizedStr` constants.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Locale {
    /// English (the canonical fallback).
    En,
    // Placeholder for future locales. Adding `ZhCn`, `JaJp`, `EsEs`, …
    // here is the only schema change needed; everything else is purely
    // additive on `LocalizedStr`.
}

impl Locale {
    /// The compile-time default. Used when no `COS_LOCALE` is set and
    /// when a [`LocalizedStr`](super::string::LocalizedStr) does not
    /// have a translation for the active locale.
    pub const DEFAULT: Locale = Locale::En;

    /// BCP-47-ish short code used in configuration files and env vars.
    pub fn code(self) -> &'static str {
        match self {
            Locale::En => "en",
        }
    }

    /// Parse a BCP-47-ish locale tag (`en`, `en-US`, `zh`, `zh-CN`, …).
    /// Case-insensitive. Returns `None` for unknown locales so callers
    /// can decide whether to warn or silently fall back.
    pub fn parse(s: &str) -> Option<Self> {
        let lower = s.trim().to_ascii_lowercase();
        match lower.as_str() {
            "en" | "en-us" | "en-gb" | "en-au" | "en-ca" => Some(Locale::En),
            _ => None,
        }
    }

    /// Human-readable name of the locale **in its own language**. Useful
    /// for the language picker. Always falls back to English-spelling.
    pub fn native_name(self) -> &'static str {
        match self {
            Locale::En => "English",
        }
    }
}

impl Default for Locale {
    fn default() -> Self {
        Self::DEFAULT
    }
}

// ---------------------------------------------------------------------------
// Global current-locale cell
// ---------------------------------------------------------------------------

fn cell() -> &'static RwLock<Locale> {
    static CELL: OnceLock<RwLock<Locale>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(Locale::DEFAULT))
}

/// The locale currently in effect for the running process.
///
/// Cheap (a single `RwLock::read`). Safe to call from any thread.
pub fn current_locale() -> Locale {
    cell().read().map(|g| *g).unwrap_or(Locale::DEFAULT)
}

/// Override the global locale. Normally called once at boot by
/// [`init_locale_from_env`]; tests and the future `cos locale set` CLI
/// may also use it.
pub fn set_locale(loc: Locale) {
    if let Ok(mut g) = cell().write() {
        *g = loc;
    }
}

/// Initialize the global locale from the environment.
///
/// Resolution order:
///   1. `COS_LOCALE` env var.
///   2. `LC_ALL` / `LANG` env vars (POSIX convention).
///   3. [`Locale::DEFAULT`].
///
/// Unknown tags are ignored (fall through to the next source). Returns
/// the locale that was selected.
pub fn init_locale_from_env() -> Locale {
    let sources = ["COS_LOCALE", "LC_ALL", "LANG"];
    for key in sources {
        if let Ok(val) = std::env::var(key) {
            // POSIX LANG often looks like "en_US.UTF-8" — strip the codeset.
            let cleaned = val.split('.').next().unwrap_or(&val).replace('_', "-");
            if let Some(loc) = Locale::parse(&cleaned) {
                set_locale(loc);
                return loc;
            }
        }
    }
    set_locale(Locale::DEFAULT);
    Locale::DEFAULT
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/i18n/locale.rs"
    ));
}
