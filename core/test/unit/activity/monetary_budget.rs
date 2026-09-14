use super::*;

#[test]
fn parses_closed_set_command_and_rejects_incomplete_body() {
    let id = "00000000-0000-4000-8000-000000000001".to_string();
    let args = vec![
        id,
        "--max-total-microusd".into(),
        "100".into(),
        "--input-microusd-per-million-tokens".into(),
        "2".into(),
        "--output-microusd-per-million-tokens".into(),
        "3".into(),
        "--max-output-tokens-per-turn".into(),
        "4".into(),
    ];
    let (route, body) = parse("set-monetary-budget", &args).unwrap();
    assert_eq!(route, Command::ActivityMonetaryBudgetSet);
    assert_eq!(body["budget"]["currency"], "USD");
    assert!(parse("set-monetary-budget", &args[..3]).is_err());
    assert!(parse(
        "set-monetary-budget",
        &[
            args[0].clone(),
            "--max-total-microusd".into(),
            "1000000000001".into(),
            "--input-microusd-per-million-tokens".into(),
            "2".into(),
            "--output-microusd-per-million-tokens".into(),
            "3".into(),
            "--max-output-tokens-per-turn".into(),
            "4".into(),
        ]
    )
    .is_err());
}

#[test]
fn parses_get_enable_and_disable_with_exact_revision() {
    let id = "00000000-0000-4000-8000-000000000001".to_string();
    let (route, body) = parse("monetary-budget", std::slice::from_ref(&id)).unwrap();
    assert_eq!(route, Command::ActivityMonetaryBudgetGet);
    assert_eq!(body["id"], id);

    for (command, expected) in [
        ("enable-monetary-budget", true),
        ("disable-monetary-budget", false),
    ] {
        let (route, body) = parse(
            command,
            &[id.clone(), "--revision".into(), "7".into()],
        )
        .unwrap();
        assert_eq!(route, Command::ActivityMonetaryBudgetEnabled);
        assert_eq!(body["expected_revision"], 7);
        assert_eq!(body["enabled"], expected);
        assert!(parse(command, std::slice::from_ref(&id)).is_err());
    }
}
