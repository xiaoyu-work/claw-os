use super::*;

#[test]
fn cidr_validation_is_bounded() {
    assert_eq!(normalize_cidr("192.0.2.1/24").unwrap(), "192.0.2.1/24");
    assert!(normalize_cidr("192.0.2.1/33").is_err());
    assert!(normalize_cidr("example.com/24").is_err());
}

#[test]
fn rendered_rules_use_managed_comments() {
    let state = FirewallState {
        schema: 1,
        revision: "r".to_string(),
        rules: vec![FirewallRule {
            id: "0123456789abcdef0123456789abcdef".to_string(),
            action: "deny".to_string(),
            direction: "input".to_string(),
            protocol: "tcp".to_string(),
            port: 22,
            remote: Some("192.0.2.0/24".to_string()),
            interface: Some("eth0".to_string()),
        }],
    };
    let script = render_ruleset(&state).unwrap();
    assert!(script.contains("tcp dport 22 drop"));
    assert!(script.contains("comment \"claw:0123456789abcdef0123456789abcdef\""));
}
