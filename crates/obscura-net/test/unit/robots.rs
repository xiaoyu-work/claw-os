use super::*;

#[test]
fn test_parse_basic_robots() {
    let body = "User-agent: *\nDisallow: /private/\nDisallow: /admin\nAllow: /admin/public\n";
    let cache = RobotsCache::new();
    cache.parse_and_store("example.com", body, "Obscura");
    assert!(cache.is_allowed("example.com", "/"));
    assert!(cache.is_allowed("example.com", "/page"));
    assert!(!cache.is_allowed("example.com", "/private/secret"));
    assert!(!cache.is_allowed("example.com", "/admin"));
    assert!(cache.is_allowed("example.com", "/admin/public"));
}

#[test]
fn test_no_rules_means_allowed() {
    let cache = RobotsCache::new();
    assert!(cache.is_allowed("unknown.com", "/anything"));
}

#[test]
fn test_disallow_all() {
    let body = "User-agent: *\nDisallow: /\n";
    let cache = RobotsCache::new();
    cache.parse_and_store("blocked.com", body, "Obscura");
    assert!(!cache.is_allowed("blocked.com", "/"));
    assert!(!cache.is_allowed("blocked.com", "/page"));
}
