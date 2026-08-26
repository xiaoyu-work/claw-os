use super::*;

#[test]
fn test_set_and_get_cookie() {
    let jar = CookieJar::new();
    let url = Url::parse("https://example.com/path").unwrap();
    jar.set_cookie("session=abc123; Path=/; Secure; HttpOnly", &url);

    let header = jar.get_cookie_header(&url);
    assert!(header.contains("session=abc123"));
}

#[test]
fn test_cookie_domain_matching() {
    let jar = CookieJar::new();
    let url = Url::parse("https://www.example.com/").unwrap();
    jar.set_cookie("token=xyz; Domain=example.com", &url);

    let header = jar.get_cookie_header(&url);
    assert!(header.contains("token=xyz"));

    let sub_url = Url::parse("https://api.example.com/").unwrap();
    let header2 = jar.get_cookie_header(&sub_url);
    assert!(header2.contains("token=xyz"));

    let other_url = Url::parse("https://other.com/").unwrap();
    let header3 = jar.get_cookie_header(&other_url);
    assert!(header3.is_empty());
}

#[test]
fn rejects_cookie_for_unrelated_domain() {
    let jar = CookieJar::new();
    let attacker = Url::parse("https://attacker.example/").unwrap();
    jar.set_cookie("session=fixed; Domain=victim.example; Path=/", &attacker);

    let victim = Url::parse("https://victim.example/").unwrap();
    assert!(jar.get_cookie_header(&victim).is_empty());
}

#[test]
fn rejects_cookie_for_public_suffix() {
    let jar = CookieJar::new();
    let url = Url::parse("https://shop.example.co.uk/").unwrap();
    jar.set_cookie("session=fixed; Domain=co.uk; Path=/", &url);

    assert!(jar.get_cookie_header(&url).is_empty());
}

#[test]
fn host_only_cookie_is_not_sent_to_subdomains() {
    let jar = CookieJar::new();
    let apex = Url::parse("https://example.com/").unwrap();
    jar.set_cookie("host_only=yes; Path=/", &apex);

    let subdomain = Url::parse("https://api.example.com/").unwrap();
    assert!(jar.get_cookie_header(&subdomain).is_empty());
    assert!(jar.get_cookie_header(&apex).contains("host_only=yes"));
}

#[test]
fn same_name_cookies_on_different_paths_coexist() {
    let jar = CookieJar::new();
    let url = Url::parse("https://example.com/account/login").unwrap();
    jar.set_cookie("session=root; Path=/", &url);
    jar.set_cookie("session=account; Path=/account", &url);

    let header = jar.get_cookie_header(&url);
    assert_eq!(header, "session=account; session=root");
}

#[test]
fn cookie_path_matching_respects_segment_boundaries() {
    let jar = CookieJar::new();
    let url = Url::parse("https://example.com/foo/index").unwrap();
    jar.set_cookie("scoped=yes; Path=/foo", &url);

    let sibling = Url::parse("https://example.com/foobar").unwrap();
    assert!(jar.get_cookie_header(&sibling).is_empty());
}

#[test]
fn default_cookie_path_is_request_directory() {
    let jar = CookieJar::new();
    let source = Url::parse("https://example.com/account/login").unwrap();
    jar.set_cookie("default_path=yes", &source);

    let sibling = Url::parse("https://example.com/account/profile").unwrap();
    let outside = Url::parse("https://example.com/settings").unwrap();
    assert!(jar.get_cookie_header(&sibling).contains("default_path=yes"));
    assert!(jar.get_cookie_header(&outside).is_empty());
}

#[test]
fn test_cdp_cookie_with_leading_dot_domain_matches_requests() {
    let jar = CookieJar::new();
    jar.set_cookies_from_cdp(vec![CookieInfo {
        name: "token".to_string(),
        value: "xyz".to_string(),
        domain: ".example.com".to_string(),
        path: "/".to_string(),
        secure: false,
        http_only: false,
    }]);

    let apex_url = Url::parse("https://example.com/").unwrap();
    let apex_header = jar.get_cookie_header(&apex_url);
    assert!(apex_header.contains("token=xyz"));

    let subdomain_url = Url::parse("https://api.example.com/").unwrap();
    let subdomain_header = jar.get_cookie_header(&subdomain_url);
    assert!(subdomain_header.contains("token=xyz"));

    let other_url = Url::parse("https://other.com/").unwrap();
    let other_header = jar.get_cookie_header(&other_url);
    assert!(other_header.is_empty());
}

#[test]
fn test_secure_cookie_not_sent_over_http() {
    let jar = CookieJar::new();
    let https_url = Url::parse("https://example.com/").unwrap();
    jar.set_cookie("secure_token=secret; Secure", &https_url);

    let http_url = Url::parse("http://example.com/").unwrap();
    let header = jar.get_cookie_header(&http_url);
    assert!(header.is_empty());
}

#[test]
fn test_max_age_zero_deletes_cookie() {
    let jar = CookieJar::new();
    let url = Url::parse("https://example.com/").unwrap();
    jar.set_cookie("session=abc", &url);
    assert!(jar.get_cookie_header(&url).contains("session=abc"));

    jar.set_cookie("session=abc; Max-Age=0", &url);
    assert!(jar.get_cookie_header(&url).is_empty());
}

#[test]
fn test_max_age_sets_expiry() {
    let jar = CookieJar::new();
    let url = Url::parse("https://example.com/").unwrap();
    jar.set_cookie("token=xyz; Max-Age=3600", &url);
    assert!(jar.get_cookie_header(&url).contains("token=xyz"));
}

#[test]
fn test_expired_cookie_not_sent() {
    let jar = CookieJar::new();
    let url = Url::parse("https://example.com/").unwrap();
    jar.set_cookie("old=gone; Expires=Thu, 01 Jan 2020 00:00:00 GMT", &url);
    assert!(jar.get_cookie_header(&url).is_empty());
}

#[test]
fn test_samesite_parsed() {
    let jar = CookieJar::new();
    let url = Url::parse("https://example.com/").unwrap();
    jar.set_cookie("strict_cookie=val; SameSite=Strict", &url);
    assert!(jar.get_cookie_header(&url).contains("strict_cookie=val"));
}

#[test]
fn test_clear_cookies() {
    let jar = CookieJar::new();
    let url = Url::parse("https://example.com/").unwrap();
    jar.set_cookie("a=1", &url);
    assert!(!jar.get_cookie_header(&url).is_empty());

    jar.clear();
    assert!(jar.get_cookie_header(&url).is_empty());
}
