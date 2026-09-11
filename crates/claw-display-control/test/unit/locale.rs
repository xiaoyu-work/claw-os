use super::*;

#[test]
fn only_bounded_pam_locale_values_are_projected() {
    let environment: LocaleEnvironment = serde_json::from_str(
        r#"{"LANG":"fr_CA.UTF-8","LANGUAGE":"fr:en","LC_TIME":"C.UTF-8","LC_ALL":""}"#,
    )
    .unwrap();
    assert_eq!(environment.iter().count(), 4);
    for invalid in [
        r#"{"LD_PRELOAD":"/tmp/library.so"}"#,
        r#"{"LOCPATH":"/tmp/locales"}"#,
        r#"{"LANG":"../locale"}"#,
        r#"{"LANG":"en_US\nDISPLAY=:1"}"#,
        r#"{"LANG":"en_US\u0000.UTF-8"}"#,
    ] {
        assert!(
            serde_json::from_str::<LocaleEnvironment>(invalid).is_err(),
            "{invalid}"
        );
    }
    assert!(LocaleEnvironment::try_from(BTreeMap::from([(
        "LANG".to_string(),
        "a".repeat(MAX_VALUE + 1)
    ),]))
    .is_err());
    assert_eq!(LocaleEnvironment::default().iter().count(), 0);
}
