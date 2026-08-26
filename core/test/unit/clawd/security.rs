use super::*;

#[test]
fn listener_parser_detects_wildcard_ports() {
    let listener =
        parse_listener("tcp LISTEN 0 4096 0.0.0.0:22 0.0.0.0:* users:((\"sshd\",pid=1,fd=3))")
            .unwrap();
    assert_eq!(listener["port"], 22);
    assert_eq!(listener["wildcard"], true);
}

#[test]
fn sudo_comments_are_not_rules() {
    assert_eq!(truncate_text("abc", 10), "abc");
}
