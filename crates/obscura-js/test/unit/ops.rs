#[test]
fn fetch_policy_scope_uses_effective_ports_and_ipv6_brackets() {
    for (input, expected) in [
        ("http://example.com/", "example.com:80"),
        ("https://example.com/", "example.com:443"),
        ("https://example.com:443/", "example.com:443"),
        ("https://example.com:8443/", "example.com:8443"),
        ("http://[2001:db8::1]/", "[2001:db8::1]:80"),
        ("https://[2001:db8::1]:9443/", "[2001:db8::1]:9443"),
    ] {
        let url = url::Url::parse(input).unwrap();
        assert_eq!(
            obscura_net::effective_host_scope(&url).unwrap(),
            expected,
            "{input}"
        );
    }
}
