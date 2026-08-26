use super::*;

#[test]
fn wild_covers_anything() {
    assert!(Scope::Wild.covers(&Scope::path("/etc/passwd")));
    assert!(Scope::Wild.covers(&Scope::host("evil.com")));
    assert!(Scope::Wild.covers(&Scope::Wild));
}

#[test]
fn different_kinds_never_cover() {
    let p = Scope::path("/etc/**");
    let h = Scope::host("*.github.com");
    assert!(!p.covers(&h));
    assert!(!h.covers(&p));
}

#[test]
fn path_prefix_basic() {
    let granted = Scope::path("/home/jay/docs/**");
    assert!(granted.covers(&Scope::path("/home/jay/docs/a.txt")));
    assert!(granted.covers(&Scope::path("/home/jay/docs/deep/sub/a.txt")));
    assert!(!granted.covers(&Scope::path("/home/jay/other/a.txt")));
}

#[test]
fn path_single_star_is_one_segment() {
    let granted = Scope::path("/home/*/docs");
    assert!(granted.covers(&Scope::path("/home/jay/docs")));
    assert!(granted.covers(&Scope::path("/home/alice/docs")));
    assert!(!granted.covers(&Scope::path("/home/jay/sub/docs")));
}

#[test]
fn path_dotdot_cannot_escape() {
    let granted = Scope::path("/home/jay/docs/**");
    // ../ resolves to /home/jay/secret which is outside the scope.
    assert!(!granted.covers(&Scope::path("/home/jay/docs/../secret/x")));
}

#[test]
fn host_label_wildcard() {
    let granted = Scope::host("*.github.com");
    assert!(granted.covers(&Scope::host("api.github.com")));
    assert!(!granted.covers(&Scope::host("github.com"))); // * needs a label
    assert!(!granted.covers(&Scope::host("api.gitlab.com")));
}

#[test]
fn host_port_match() {
    let granted = Scope::host("*.github.com:443");
    assert!(granted.covers(&Scope::host("api.github.com:443")));
    assert!(!granted.covers(&Scope::host("api.github.com:80")));
    // Granted without port covers any port.
    let any_port = Scope::host("*.github.com");
    assert!(any_port.covers(&Scope::host("api.github.com:443")));
    assert!(any_port.covers(&Scope::host("api.github.com:80")));
}

#[test]
fn name_glob() {
    let granted = Scope::name("openai/*");
    assert!(granted.covers(&Scope::name("openai/api-key")));
    assert!(!granted.covers(&Scope::name("anthropic/api-key")));

    let deep = Scope::name("billing/**");
    assert!(deep.covers(&Scope::name("billing/2026/q1/invoices")));
}

#[test]
fn self_ref_is_literal() {
    let granted = Scope::self_ref("self.children.*");
    // Literal equality — no glob magic on SelfRef.
    assert!(granted.covers(&Scope::self_ref("self.children.*")));
    assert!(!granted.covers(&Scope::self_ref("self.children.123")));
}

#[test]
fn is_wildcard_flags_dangerous_scopes() {
    assert!(Scope::Wild.is_wildcard());
    assert!(Scope::path("/").is_wildcard());
    assert!(Scope::path("**").is_wildcard());
    assert!(Scope::host("*").is_wildcard());
    assert!(!Scope::path("~/Documents/**").is_wildcard());
    assert!(!Scope::host("*.github.com").is_wildcard());
}

#[test]
fn serde_round_trip_each_kind() {
    for s in [
        Scope::path("/a/**"),
        Scope::host("*.example.com:443"),
        Scope::name("ns/*"),
        Scope::self_ref("self"),
        Scope::Wild,
    ] {
        let j = serde_json::to_string(&s).unwrap();
        let back: Scope = serde_json::from_str(&j).unwrap();
        assert_eq!(back, s);
    }
}

#[test]
fn env_vars_are_not_expanded() {
    // We deliberately removed `$VAR` expansion from `Scope::path`
    // (audit: caps/scope.rs HIGH) because untrusted manifest /
    // SDK callers shouldn't be able to read process env. The
    // literal `$COS_TEST_PFX` segment must be matched as-is.
    std::env::set_var("COS_TEST_PFX", "/var/tmp/cos-test");
    let granted = Scope::path("$COS_TEST_PFX/**");
    assert!(!granted.covers(&Scope::path("/var/tmp/cos-test/file.txt")));
    // It DOES still match a literal `$COS_TEST_PFX` directory.
    assert!(granted.covers(&Scope::path("$COS_TEST_PFX/file.txt")));
    std::env::remove_var("COS_TEST_PFX");
}

/// Audit fix (caps/scope.rs HIGH "path scope rejects symlink
/// outside"): a request whose path resolves — via symlink — to
/// somewhere outside the granted prefix must be denied. Before
/// the fix, `Scope::path("/safe/**").covers(&Scope::path(s))`
/// did string-prefix comparison on the *unresolved* path, so a
/// symlink `/safe/back -> /etc` granted read access to
/// `/etc/passwd` via the path `/safe/back/passwd`.
#[test]
#[cfg(unix)]
fn path_scope_rejects_symlink_outside() {
    use std::os::unix::fs::symlink;
    let root = std::env::temp_dir().join(format!(
        "cos-scope-symlink-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let safe = root.join("safe");
    let outside = root.join("outside");
    std::fs::create_dir_all(&safe).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let secret = outside.join("secret.txt");
    std::fs::write(&secret, b"top secret").unwrap();
    let backdoor = safe.join("back");
    // Plant `safe/back -> outside` symlink.
    let _ = std::fs::remove_file(&backdoor);
    symlink(&outside, &backdoor).unwrap();

    let granted = Scope::path(&format!("{}/**", safe.display()));
    let attack = Scope::path(&format!("{}/secret.txt", backdoor.display()));

    assert!(
        !granted.covers(&attack),
        "scope `{granted:?}` must NOT cover `{attack:?}` — symlink escapes the granted prefix",
    );

    // Cleanup.
    let _ = std::fs::remove_dir_all(&root);
}
