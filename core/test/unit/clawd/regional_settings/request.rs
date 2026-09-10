use super::*;
use serde_json::json;

fn examples() -> [Value; 3] {
    [
        json!({"session":"regional-test","action":"system_locale","lang":"en_US.UTF-8","region":"de_DE.utf8@euro"}),
        json!({"session":"regional-test","action":"owner_language","languages":"de_DE:de:en_US:en"}),
        json!({"session":"regional-test","action":"static_hostname","hostname":"desk.example"}),
    ]
}

#[test]
fn closed_actions_preserve_values_and_request_separate_exact_caps() {
    for (value, verb, scope) in [
        (examples()[0].clone(), Verb::SYS_LOCALE, "system"),
        (examples()[1].clone(), Verb::SYS_LANGUAGE, "self"),
        (examples()[2].clone(), Verb::SYS_HOSTNAME, "static"),
    ] {
        let request = Request::decode(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap(), value);
        assert_eq!(request.required_cap(), Cap::new(verb, Scope::name(scope)));
        assert_eq!(request.session(), "regional-test");
    }
}

#[test]
fn locale_projection_has_only_the_original_ten_keys_and_separate_values() {
    let values = locale_values("fr_FR.UTF-8", "en_GB.UTF-8");
    assert_eq!(values.len(), 10);
    assert_eq!(values[0], "LANG=fr_FR.UTF-8");
    assert_eq!(
        values[1..],
        [
            "LC_ADDRESS=en_GB.UTF-8",
            "LC_IDENTIFICATION=en_GB.UTF-8",
            "LC_MEASUREMENT=en_GB.UTF-8",
            "LC_MONETARY=en_GB.UTF-8",
            "LC_NAME=en_GB.UTF-8",
            "LC_NUMERIC=en_GB.UTF-8",
            "LC_PAPER=en_GB.UTF-8",
            "LC_TELEPHONE=en_GB.UTF-8",
            "LC_TIME=en_GB.UTF-8",
        ]
    );
}

#[test]
fn locale_readback_confirms_lang_inheritance_but_not_missing_or_conflicting_values() {
    let lang = "en_US.UTF-8";
    assert!(locale_confirmed(&[format!("LANG={lang}")], lang, lang));
    assert!(!locale_confirmed(
        &[format!("LANG={lang}")],
        lang,
        "de_DE.UTF-8"
    ));
    assert!(!locale_confirmed(&[], lang, lang));
    assert!(!locale_confirmed(
        &[format!("LANG={lang}"), "LANG=fr_FR.UTF-8".into()],
        lang,
        lang
    ));
    assert!(!locale_confirmed(
        &[format!("LANG={lang}"), "LC_ALL=fr_FR.UTF-8".into()],
        lang,
        lang
    ));
}

#[test]
fn invalid_fields_owners_commands_paths_or_keys_fail_during_deserialization() {
    for original in examples() {
        for (key, value) in [
            ("owner_uid", json!(0)),
            ("uid", json!(1000)),
            ("app_id", json!("cosmic-settings")),
            ("path", json!("/etc/locale.conf")),
            ("command", json!("/bin/sh")),
            ("bus", json!("unix:path=/untrusted")),
            ("LC_ALL", json!("C")),
            ("capabilities", json!(["*"])),
            ("approved", json!(true)),
        ] {
            let mut input = original.clone();
            input[key] = value;
            assert!(Request::decode(input).is_err(), "{key}");
        }
    }
    assert!(Request::decode(json!({"session":"test","action":"set_keyboard"})).is_err());
}

#[test]
fn invalid_locale_and_language_values_fail_before_dispatch() {
    for bad in [
        "",
        " en_US",
        "en_US\n",
        "LANG=en_US",
        "../en_US",
        "en_US:de_DE",
        "en_US;id",
        "en_US\0",
        "en_US.UTF-8@x/y",
        "en_US.UTF-8=1",
    ] {
        let mut input = examples()[0].clone();
        input["lang"] = json!(bad);
        assert!(Request::decode(input).is_err(), "{bad:?}");
    }
    for bad in ["", "en:", ":en", "en::de", "en:../de", "en:de\n"] {
        let mut input = examples()[1].clone();
        input["languages"] = json!(bad);
        assert!(Request::decode(input).is_err(), "{bad:?}");
    }
    let mut input = examples()[1].clone();
    input["languages"] = json!(vec!["en"; 64].join(":"));
    Request::decode(input.clone()).unwrap();
    input["languages"] = json!(vec!["en"; 65].join(":"));
    assert!(Request::decode(input).is_err());
}

#[test]
fn hostname_labels_and_total_size_are_bounded_without_normalizing_input() {
    for bad in [
        "", "-desk", "desk-", "desk.", ".desk", "a..b", "a/b", "a b", "desk\n", "a_thing", "\u{e9}",
    ] {
        let mut input = examples()[2].clone();
        input["hostname"] = json!(bad);
        assert!(Request::decode(input).is_err(), "{bad:?}");
    }
    for (value, valid) in [
        ("a".repeat(63), true),
        ("a".repeat(64), false),
        (format!("{}.a", "a".repeat(62)), true),
        (format!("{}.ab", "a".repeat(62)), false),
    ] {
        let mut input = examples()[2].clone();
        input["hostname"] = json!(value);
        assert_eq!(Request::decode(input).is_ok(), valid);
    }
}
