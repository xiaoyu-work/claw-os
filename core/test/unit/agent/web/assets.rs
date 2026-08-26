use super::*;
use base64::Engine;
use sha2::{Digest, Sha256};

#[test]
fn static_assets_set_browser_xss_defenses() {
    let response = serve_file("index.html");
    let headers = response.headers();
    let csp = headers
        .get("content-security-policy")
        .unwrap()
        .to_str()
        .unwrap();
    let script_src = csp
        .split("; ")
        .find(|directive| directive.starts_with("script-src "))
        .unwrap();

    assert!(script_src.contains("'self'"));
    assert!(script_src.contains("'sha256-"));
    assert!(!script_src.contains("'unsafe-inline'"));
    assert!(csp.contains("script-src-attr 'none'"));
    assert_eq!(headers.get("referrer-policy").unwrap(), "no-referrer");
    assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(headers.get("x-frame-options").unwrap(), "DENY");
}

#[test]
fn csp_hash_matches_the_inline_theme_script() {
    let html = std::str::from_utf8(UI_DIR.get_file("index.html").unwrap().contents()).unwrap();
    let script = html
        .split_once("<script>")
        .and_then(|(_, rest)| rest.split_once("</script>"))
        .map(|(script, _)| script)
        .unwrap();
    let digest = Sha256::digest(script.as_bytes());
    let source = format!(
        "'sha256-{}'",
        base64::engine::general_purpose::STANDARD.encode(digest)
    );

    assert!(CONTENT_SECURITY_POLICY.contains(&source));
}
