use super::is_settings_entry;

#[test]
fn settings_category_uses_app_id_or_desktop_metadata() {
    assert!(is_settings_entry("com.yuos.Settings", &["ClawOS"]));
    assert!(is_settings_entry(
        "com.yuos.Settings.Appearance",
        &["ClawOS"]
    ));
    assert!(is_settings_entry(
        "org.example.ControlCenter",
        &["Settings"]
    ));
    assert!(!is_settings_entry("org.example.Editor", &["Utility"]));
}
