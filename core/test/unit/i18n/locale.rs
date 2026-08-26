use super::*;

#[test]
fn default_is_english() {
    assert_eq!(Locale::DEFAULT, Locale::En);
    assert_eq!(Locale::default(), Locale::En);
}

#[test]
fn parse_accepts_common_english_tags() {
    assert_eq!(Locale::parse("en"), Some(Locale::En));
    assert_eq!(Locale::parse("EN"), Some(Locale::En));
    assert_eq!(Locale::parse("en-US"), Some(Locale::En));
    assert_eq!(Locale::parse(" en-gb "), Some(Locale::En));
}

#[test]
fn parse_rejects_unknown_tags() {
    assert_eq!(Locale::parse(""), None);
    assert_eq!(Locale::parse("xx"), None);
    assert_eq!(Locale::parse("klingon"), None);
}

#[test]
fn set_and_get_round_trip() {
    set_locale(Locale::En);
    assert_eq!(current_locale(), Locale::En);
}

#[test]
fn code_matches_parse() {
    for loc in [Locale::En] {
        assert_eq!(Locale::parse(loc.code()), Some(loc));
    }
}
