//! A compile-time, locale-aware string.
//!
//! [`LocalizedStr`] is the single primitive used everywhere a string is
//! displayed to a human. It always carries an English translation
//! (`Locale::DEFAULT`) and may carry additional translations as new
//! locales come online.
//!
//! ```ignore
//! use crate::i18n::LocalizedStr;
//!
//! pub const FS_READ_LABEL: LocalizedStr = LocalizedStr::new("View your files");
//! // Later, when Chinese is wired in:
//! //   pub const FS_READ_LABEL: LocalizedStr =
//! //       LocalizedStr::new("View your files").with_zh_cn("查看你的文件");
//!
//! let text = FS_READ_LABEL.current(); // honours the global current_locale()
//! ```
//!
//! The struct is `const`-constructible so every cap, role, error kind,
//! etc. can declare its translations as a `static` and pay zero
//! per-call cost.

use super::locale::{current_locale, Locale};

#[derive(Copy, Clone, Debug)]
pub struct LocalizedStr {
    /// English translation. Always present — every other locale falls
    /// back to this string if its own translation is missing.
    en: &'static str,
    // Add `Option<&'static str>` fields here as new locales are wired
    // into `Locale`. The `get()` matcher must gain a corresponding arm.
    //
    // Example (kept commented until ZhCn lands in the Locale enum):
    //
    //     zh_cn: Option<&'static str>,
}

impl LocalizedStr {
    /// Build a `LocalizedStr` with only the English translation. Use the
    /// `with_<locale>` builders to add more languages.
    pub const fn new(en: &'static str) -> Self {
        Self { en }
    }

    /// Translation for `locale`, falling back to English.
    ///
    /// Matching `Locale::En` directly (rather than going through the
    /// `Option` path) keeps the common case branch-free.
    pub fn get(&self, locale: Locale) -> &'static str {
        match locale {
            Locale::En => self.en,
        }
    }

    /// Translation for the process-wide
    /// [`current_locale`](super::locale::current_locale). This is the
    /// call site every UI path should use; tests can use [`get`] with
    /// an explicit locale.
    pub fn current(&self) -> &'static str {
        self.get(current_locale())
    }

    /// Always return the English string regardless of the current
    /// locale. Useful for stable log lines and audit records that must
    /// not change wording when the user switches languages.
    pub fn en(&self) -> &'static str {
        self.en
    }
}

impl std::fmt::Display for LocalizedStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.current())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::locale::set_locale;

    #[test]
    fn english_round_trip() {
        let s = LocalizedStr::new("hello");
        assert_eq!(s.get(Locale::En), "hello");
        assert_eq!(s.en(), "hello");
    }

    #[test]
    fn current_honours_global_locale() {
        let s = LocalizedStr::new("hello");
        set_locale(Locale::En);
        assert_eq!(s.current(), "hello");
    }

    #[test]
    fn display_renders_current_locale() {
        let s = LocalizedStr::new("hi");
        assert_eq!(format!("{s}"), "hi");
    }

    #[test]
    fn const_construction_in_static() {
        static MSG: LocalizedStr = LocalizedStr::new("constexpr-friendly");
        assert_eq!(MSG.en(), "constexpr-friendly");
    }
}
