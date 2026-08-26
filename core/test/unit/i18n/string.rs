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
