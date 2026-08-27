use super::*;

#[test]
fn effective_host_scope_always_includes_the_effective_port() {
    for (input, expected) in [
        ("http://example.com/path", "example.com:80"),
        ("https://example.com/path", "example.com:443"),
        ("http://example.com:80/path", "example.com:80"),
        ("https://example.com:443/path", "example.com:443"),
        ("https://example.com:8443/path", "example.com:8443"),
        ("http://[2001:0db8::1]/path", "[2001:db8::1]:80"),
        ("https://[2001:db8::1]:9443/path", "[2001:db8::1]:9443"),
    ] {
        let url = Url::parse(input).unwrap();
        assert_eq!(effective_host_scope(&url).unwrap(), expected, "{input}");
    }
}

#[test]
fn initial_and_redirect_urls_use_the_same_scope_rule() {
    let hops = [
        Url::parse("http://origin.example/start").unwrap(),
        Url::parse("https://redirect.example/next").unwrap(),
        Url::parse("https://redirect.example:9443/final").unwrap(),
    ];
    assert_eq!(
        hops.iter()
            .map(|url| effective_host_scope(url).unwrap())
            .collect::<Vec<_>>(),
        [
            "origin.example:80",
            "redirect.example:443",
            "redirect.example:9443"
        ]
    );
}
