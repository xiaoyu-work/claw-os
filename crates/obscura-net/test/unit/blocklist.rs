use super::*;

#[test]
fn test_exact_match() {
    assert!(is_blocked("google-analytics.com"));
    assert!(is_blocked("doubleclick.net"));
}

#[test]
fn test_subdomain_match() {
    assert!(is_blocked("www.google-analytics.com"));
    assert!(is_blocked("ssl.google-analytics.com"));
}

#[test]
fn test_not_blocked() {
    assert!(!is_blocked("google.com"));
    assert!(!is_blocked("example.com"));
    assert!(!is_blocked("github.com"));
}

#[test]
fn test_pgl_domains() {
    assert!(is_blocked("adnxs.com"));
    assert!(is_blocked("criteo.com"));
}

#[test]
fn test_blocklist_size() {
    assert!(blocklist().len() > 3500);
}
