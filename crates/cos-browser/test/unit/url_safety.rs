use super::*;

#[test]
fn rejects_localhost_literal() {
    assert!(validate_navigable_url("http://localhost/").is_err());
    assert!(validate_navigable_url("http://localhost:8080/x").is_err());
    assert!(validate_navigable_url("http://ip6-localhost/").is_err());
}

#[test]
fn rejects_loopback_ipv4() {
    assert!(validate_navigable_url("http://127.0.0.1/").is_err());
    assert!(validate_navigable_url("http://127.5.5.5/").is_err());
}

#[test]
fn rejects_loopback_ipv6() {
    assert!(validate_navigable_url("http://[::1]/").is_err());
}

#[test]
fn rejects_private_ranges() {
    assert!(validate_navigable_url("http://10.0.0.1/").is_err());
    assert!(validate_navigable_url("http://192.168.1.1/").is_err());
    assert!(validate_navigable_url("http://172.16.0.1/").is_err());
}

#[test]
fn rejects_link_local_and_imds() {
    // 169.254.0.0/16 — both AWS/GCP IMDS (169.254.169.254) and
    // generic link-local addresses live here.
    assert!(validate_navigable_url("http://169.254.169.254/").is_err());
    assert!(validate_navigable_url("http://169.254.0.1/").is_err());
}

#[test]
fn rejects_cgnat() {
    // 100.64.0.0/10
    assert!(validate_navigable_url("http://100.64.0.1/").is_err());
    assert!(validate_navigable_url("http://100.127.255.255/").is_err());
}

#[test]
fn rejects_ipv4_mapped_ipv6() {
    // ::ffff:10.0.0.1 should be caught by the v4 ruleset.
    assert!(validate_navigable_url("http://[::ffff:10.0.0.1]/").is_err());
    assert!(validate_navigable_url("http://[::ffff:127.0.0.1]/").is_err());
}

#[test]
fn rejects_non_http_schemes() {
    assert!(validate_navigable_url("file:///etc/passwd").is_err());
    assert!(validate_navigable_url("ftp://example.com/").is_err());
    assert!(validate_navigable_url("javascript:alert(1)").is_err());
}

#[test]
fn rejects_unique_local_ipv6() {
    assert!(validate_navigable_url("http://[fc00::1]/").is_err());
    assert!(validate_navigable_url("http://[fd00::1]/").is_err());
    assert!(validate_navigable_url("http://[fe80::1]/").is_err());
}
