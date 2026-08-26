use super::select_chromium_bin;

#[test]
fn prefers_standard_chromium_name() {
    let selected =
        select_chromium_bin(None, |candidate| matches!(candidate, "chromium")).unwrap();
    assert_eq!(selected, "chromium");
}

#[test]
fn falls_back_to_ubuntu_chromium_name() {
    let selected =
        select_chromium_bin(None, |candidate| matches!(candidate, "chromium-browser")).unwrap();
    assert_eq!(selected, "chromium-browser");
}

#[test]
fn supports_google_chrome_installations() {
    let selected =
        select_chromium_bin(None, |candidate| matches!(candidate, "google-chrome-stable"))
            .unwrap();
    assert_eq!(selected, "google-chrome-stable");
}

#[test]
fn explicit_override_does_not_silently_fall_back() {
    let error = select_chromium_bin(Some("/missing/chrome".to_string()), |_| false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("COS_CHROMIUM_BIN"));
}
