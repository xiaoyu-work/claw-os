use super::*;

#[test]
fn coredump_id_is_strict() {
    let id = "0123456789abcdef0123456789abcdef:42:1720000000000000";
    assert_eq!(
        parse_coredump_id(id).unwrap(),
        (
            "0123456789abcdef0123456789abcdef".to_string(),
            42,
            1720000000000000,
        )
    );
    assert!(parse_coredump_id("../../etc/passwd:42:1").is_err());
    assert!(parse_coredump_id("0123456789abcdef0123456789abcdef:0:1").is_err());
    assert!(parse_coredump_id("0123456789abcdef0123456789abcdef:42").is_err());
}

#[test]
fn crash_event_classification_is_specific() {
    assert_eq!(
        classify_message("Out of memory: Killed process 42 (demo)"),
        Some("oom")
    );
    assert_eq!(
        classify_message("demo[42]: segfault at 0 ip 0"),
        Some("crash")
    );
    assert_eq!(classify_message("service started successfully"), None);
}

#[test]
fn json_parser_accepts_stream_and_array() {
    assert_eq!(parse_json_records("{\"a\":1}\n{\"a\":2}\n").len(), 2);
    assert_eq!(parse_json_records("[{\"a\":1},{\"a\":2}]").len(), 2);
}
