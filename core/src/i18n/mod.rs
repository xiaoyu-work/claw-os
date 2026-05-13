//! OS-wide internationalization.
//!
//! Claw OS is a multi-language system. Every user-facing string in the
//! kernel, built-in apps, and third-party apps flows through this module
//! so a single locale switch redrives the whole UI.
//!
//! Design principles:
//!
//! 1. **Locale is global, set once at boot.** Read from the `COS_LOCALE`
//!    env var first, then `$config_dir/locale`, then fall back to the
//!    compile-time default (`Locale::DEFAULT`, currently English). Code
//!    elsewhere asks [`current_locale`]; the value is cheap to read.
//!
//! 2. **English is always present.** Every [`LocalizedStr`] carries an
//!    English string. Other locales are `Option<&str>` — missing entries
//!    silently fall back to English. This makes adding a new locale a
//!    non-blocking, additive change.
//!
//! 3. **Strings live with the thing they describe.** Rather than a single
//!    giant message catalog file, each subsystem (caps, roles, scopes,
//!    error kinds, …) declares its own `LocalizedStr` constants next to
//!    the data they label. This keeps translation work co-located with
//!    feature work.
//!
//! 4. **Compile-time string tables only — no runtime loaders yet.** We
//!    can add `.po` / `.ftl` loading later behind a feature flag without
//!    changing call sites. Today's `&'static str` storage is enough for
//!    the OS-internal surface area.
//!
//! Usage:
//!
//! ```ignore
//! use crate::i18n::{LocalizedStr, current_locale};
//!
//! static GREETING: LocalizedStr = LocalizedStr::new("Hello");
//!
//! fn greet() -> &'static str {
//!     GREETING.current()
//! }
//! ```

pub mod locale;
pub mod string;

pub use locale::{current_locale, init_locale_from_env, set_locale, Locale};
pub use string::LocalizedStr;
