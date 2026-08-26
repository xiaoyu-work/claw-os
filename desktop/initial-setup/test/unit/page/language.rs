use super::*;

#[test]
fn test_parse_locale_output_filters_c_utf8() {
    let output = "C.UTF-8\nen_US.utf8\nde_DE.utf8\n";
    let result = parse_locale_output(output);
    assert!(!result.contains(&"C.UTF-8".to_string()));
    assert_eq!(result.len(), 2);
}

#[test]
fn test_parse_locale_output_handles_empty_input() {
    let output = "";
    let result = parse_locale_output(output);
    assert_eq!(result.len(), 0);
}

#[test]
fn test_parse_locale_output_preserves_locale_strings() {
    let output = "en_US.utf8\nde_DE.utf8\nfr_FR.utf8\n";
    let result = parse_locale_output(output);
    assert_eq!(result.len(), 3);
    assert!(result.contains(&"en_US.utf8".to_string()));
}

#[test]
fn test_build_locale_settings_includes_all_lc_variables() {
    let lang = "en_US.UTF-8";
    let region = "de_DE.UTF-8";
    let settings = build_locale_settings(lang, region);

    assert_eq!(settings.len(), 10);
    assert!(settings.contains(&format!("LANG={}", lang)));
    assert!(settings.contains(&format!("LC_ADDRESS={}", region)));
    assert!(settings.contains(&format!("LC_IDENTIFICATION={}", region)));
    assert!(settings.contains(&format!("LC_MEASUREMENT={}", region)));
    assert!(settings.contains(&format!("LC_MONETARY={}", region)));
    assert!(settings.contains(&format!("LC_NAME={}", region)));
    assert!(settings.contains(&format!("LC_NUMERIC={}", region)));
    assert!(settings.contains(&format!("LC_PAPER={}", region)));
    assert!(settings.contains(&format!("LC_TELEPHONE={}", region)));
    assert!(settings.contains(&format!("LC_TIME={}", region)));
}

#[test]
fn test_build_locale_settings_uses_correct_values() {
    let lang = "fr_FR.UTF-8";
    let region = "en_GB.UTF-8";
    let settings = build_locale_settings(lang, region);

    assert!(settings.iter().any(|s| s == "LANG=fr_FR.UTF-8"));
    assert!(settings.iter().any(|s| s == "LC_TIME=en_GB.UTF-8"));
}

#[test]
fn test_parse_locale_output_filters_any_c_posix_variant() {
    let output = "C\nC.utf8\nC.UTF-8\nPOSIX\nPOSIX.utf8\nC.iso88591\nen_US.utf8\n";
    let result = parse_locale_output(output);

    // Should filter out all C and POSIX variants
    assert!(!result.contains(&"C".to_string()));
    assert!(!result.contains(&"C.utf8".to_string()));
    assert!(!result.contains(&"C.UTF-8".to_string()));
    assert!(!result.contains(&"POSIX".to_string()));
    assert!(!result.contains(&"POSIX.utf8".to_string()));
    assert!(!result.contains(&"C.iso88591".to_string()));

    // Should keep real locales
    assert!(result.contains(&"en_US.utf8".to_string()));
    assert_eq!(result.len(), 1);
}

#[test]
fn test_parse_locale_output_accepts_only_utf8_locales() {
    let output = "en_US.utf8\nen_US.UTF-8\nde_DE.iso88591\nfr_FR\nes_ES.utf8\n";
    let result = parse_locale_output(output);

    // Should accept UTF-8 encoded locales (case insensitive)
    assert!(result.contains(&"en_US.utf8".to_string()));
    assert!(result.contains(&"en_US.UTF-8".to_string()));
    assert!(result.contains(&"es_ES.utf8".to_string()));

    // Should reject non-UTF-8 encodings
    assert!(!result.contains(&"de_DE.iso88591".to_string()));

    // Should reject locales without explicit encoding
    assert!(!result.contains(&"fr_FR".to_string()));

    assert_eq!(result.len(), 3);
}

#[test]
fn test_parse_locale_output_comprehensive_filtering() {
    // Test comprehensive scenario matching cosmic-settings PR #1961
    let output = concat!(
        "C\n",
        "C.utf8\n",
        "C.UTF-8\n",
        "POSIX\n",
        "POSIX.utf8\n",
        "C.iso88591\n",
        "en_US.utf8\n",
        "en_US.UTF-8\n",
        "de_DE.utf8\n",
        "fr_FR.UTF-8\n",
        "es_ES.iso88591\n",
        "ca_ES.utf8@valencia\n", // Locale with modifier
        "ar_IN\n",               // No encoding specified
        "\n",                    // Empty line
    );
    let result = parse_locale_output(output);

    // Should filter all C and POSIX variants
    assert!(!result.iter().any(|s| s.starts_with("C")));
    assert!(!result.iter().any(|s| s.starts_with("POSIX")));

    // Should accept UTF-8 locales (case insensitive)
    assert!(result.contains(&"en_US.utf8".to_string()));
    assert!(result.contains(&"en_US.UTF-8".to_string()));
    assert!(result.contains(&"de_DE.utf8".to_string()));
    assert!(result.contains(&"fr_FR.UTF-8".to_string()));

    // Should accept UTF-8 locales with modifiers
    assert!(result.contains(&"ca_ES.utf8@valencia".to_string()));

    // Should reject non-UTF-8 encodings and locales without encoding
    assert!(!result.contains(&"es_ES.iso88591".to_string()));
    assert!(!result.contains(&"ar_IN".to_string()));

    // Should handle empty lines
    assert!(!result.contains(&"".to_string()));

    assert_eq!(result.len(), 5);
}

#[test]
fn test_parse_locale_output_filters_pseudo_locales() {
    let output = "C\nC.utf8\nC.UTF-8\nPOSIX\nen_US.utf8\nde_DE.UTF-8\n";
    let result = parse_locale_output(output);

    // Should filter out all C and POSIX variants
    assert!(!result.contains(&"C".to_string()));
    assert!(!result.contains(&"C.utf8".to_string()));
    assert!(!result.contains(&"C.UTF-8".to_string()));
    assert!(!result.contains(&"POSIX".to_string()));

    // Should keep actual locales
    assert!(result.contains(&"en_US.utf8".to_string()));
    assert!(result.contains(&"de_DE.UTF-8".to_string()));
    assert_eq!(result.len(), 2);
}

#[test]
fn test_parse_locale_output_handles_whitespace() {
    let output = " en_US.utf8 \n\t de_DE.UTF-8\t\n fr_FR.utf8 \n";
    let result = parse_locale_output(output);

    // Should handle leading/trailing whitespace
    assert!(result.contains(&"en_US.utf8".to_string()));
    assert!(result.contains(&"de_DE.UTF-8".to_string()));
    assert!(result.contains(&"fr_FR.utf8".to_string()));
    assert_eq!(result.len(), 3);
}

#[test]
fn test_parse_locale_output_handles_empty_lines() {
    let output = "en_US.utf8\n\n\nde_DE.UTF-8\n\n";
    let result = parse_locale_output(output);

    // Should skip empty lines
    assert!(result.contains(&"en_US.utf8".to_string()));
    assert!(result.contains(&"de_DE.UTF-8".to_string()));
    assert_eq!(result.len(), 2);
}

#[test]
fn test_parse_locale_output_catalan_not_filtered_as_pseudo() {
    let output = "C\nca_ES.UTF-8\nca_ES.utf8\ncs_CZ.UTF-8\nen_US.utf8\n";
    let result = parse_locale_output(output);

    // Should filter out C but not Catalan (ca_*) or Czech (cs_*)
    assert!(!result.contains(&"C".to_string()));
    assert!(result.contains(&"ca_ES.UTF-8".to_string()));
    assert!(result.contains(&"ca_ES.utf8".to_string()));
    assert!(result.contains(&"cs_CZ.UTF-8".to_string()));
    assert_eq!(result.len(), 4);
}

#[test]
fn test_parse_locale_output_handles_locale_modifiers() {
    let output = "en_US.UTF-8@euro\nca_ES.UTF-8@valencia\nde_DE.utf8\n";
    let result = parse_locale_output(output);

    // Locales with modifiers should be accepted
    assert!(result.contains(&"en_US.UTF-8@euro".to_string()));
    assert!(result.contains(&"ca_ES.UTF-8@valencia".to_string()));
    assert!(result.contains(&"de_DE.utf8".to_string()));
    assert_eq!(result.len(), 3);
}

#[test]
fn test_parse_locale_output_case_variations() {
    let output = "en_US.UTF-8\nen_US.utf-8\nen_US.utf8\nen_US.UTF8\nde_DE.Utf8\n";
    let result = parse_locale_output(output);

    // All case variations should be accepted (case-insensitive regex)
    assert!(result.contains(&"en_US.UTF-8".to_string()));
    assert!(result.contains(&"en_US.utf-8".to_string()));
    assert!(result.contains(&"en_US.utf8".to_string()));
    assert!(result.contains(&"en_US.UTF8".to_string()));
    assert!(result.contains(&"de_DE.Utf8".to_string()));
    assert_eq!(result.len(), 5);
}
