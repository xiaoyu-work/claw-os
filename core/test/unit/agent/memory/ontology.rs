use super::*;

#[test]
fn normalizes_documented_operating_system_aliases() {
    for (entity, attribute) in [
        ("operating_system", "name"),
        ("operating-system", "distribution"),
        ("os", "base_distribution"),
        ("linux_distro", "platform"),
    ] {
        let fact = normalize_fact("environment", entity, attribute, "Ubuntu", None, None).unwrap();
        assert_eq!(fact.slot.key(), "os.distribution");
        assert_eq!(fact.lifetime, FactLifetime::Observed);
        assert_eq!(fact.ttl_days, Some(DEFAULT_OBSERVED_TTL_DAYS));
    }
}

#[test]
fn normalizes_system_agent_deployment_aliases() {
    for entity in ["claw-agent", "cos_agent", "system agent deployment"] {
        let fact = normalize_fact(
            "environment",
            entity,
            "deployment_state",
            "present",
            None,
            None,
        )
        .unwrap();
        assert_eq!(fact.slot.key(), "claw_os.installation");
        assert_eq!(fact.slot.value, "installed");
    }
}

#[test]
fn installation_state_and_version_shape_are_canonical() {
    let absent =
        normalize_fact("environment", "Python3", "installed", "missing", None, None).unwrap();
    assert_eq!(absent.slot.key(), "python.installation");
    assert_eq!(absent.slot.value, "not_found");

    let version = normalize_fact(
        "environment",
        "Python3",
        "installation",
        "3.13.1",
        None,
        None,
    )
    .unwrap();
    assert_eq!(version.slot.key(), "python.version");
    assert_eq!(version.slot.value, "3.13.1");
}

#[test]
fn lifetime_policy_keeps_durable_knowledge_and_bounds_live_state() {
    let preference = normalize_fact(
        "preference",
        "editor",
        "name",
        "helix",
        Some(FactLifetime::Observed),
        Some(1),
    )
    .unwrap();
    assert_eq!(preference.lifetime, FactLifetime::Durable);
    assert_eq!(preference.ttl_days, None);

    let mislabeled_live_state = normalize_fact(
        "preference",
        "python",
        "version",
        "3.13.1",
        Some(FactLifetime::Durable),
        None,
    )
    .unwrap();
    assert_eq!(mislabeled_live_state.slot.key(), "python.version");
    assert_eq!(mislabeled_live_state.lifetime, FactLifetime::Observed);
    assert_eq!(
        mislabeled_live_state.ttl_days,
        Some(DEFAULT_OBSERVED_TTL_DAYS)
    );

    let explicit_version_preference = normalize_fact(
        "preference",
        "python",
        "preferred_version",
        "3.13",
        Some(FactLifetime::Durable),
        None,
    )
    .unwrap();
    assert_eq!(
        explicit_version_preference.slot.key(),
        "python.preferred_version"
    );
    assert_eq!(explicit_version_preference.lifetime, FactLifetime::Durable);

    let resolution = normalize_fact(
        "resolution",
        "postgres",
        "cause",
        "pool exhausted",
        None,
        None,
    )
    .unwrap();
    assert_eq!(resolution.lifetime, FactLifetime::Durable);

    let package = normalize_fact(
        "environment",
        "ripgrep",
        "version",
        "14.1.0",
        Some(FactLifetime::Durable),
        Some(5_000),
    )
    .unwrap();
    assert_eq!(package.lifetime, FactLifetime::Observed);
    assert_eq!(package.ttl_days, Some(MAX_OBSERVED_TTL_DAYS));

    let project_convention = normalize_fact(
        "environment",
        "project",
        "test_command",
        "cargo test -p cos",
        None,
        None,
    )
    .unwrap();
    assert_eq!(project_convention.lifetime, FactLifetime::Durable);
    assert_eq!(project_convention.ttl_days, None);
}

#[test]
fn session_state_and_procedures_are_not_memory_eligible() {
    let task = normalize_fact(
        "environment",
        "task",
        "current_goal",
        "fix issue 59",
        None,
        None,
    )
    .unwrap();
    assert_eq!(task.lifetime, FactLifetime::Session);
    assert!(!task.lifetime.is_memory_eligible());

    let workflow = normalize_fact(
        "procedure",
        "release",
        "steps",
        "run build then publish",
        None,
        None,
    )
    .unwrap();
    assert_eq!(workflow.lifetime, FactLifetime::Procedure);
    assert!(!workflow.lifetime.is_memory_eligible());
}

#[test]
fn date_round_trip_and_validation_are_deterministic() {
    for (days, date) in [
        (0, "1970-01-01"),
        (18_262, "2020-01-01"),
        (19_782, "2024-02-29"),
    ] {
        assert_eq!(date_from_unix_s(days * 86_400), date);
        assert_eq!(date_to_epoch_days(date), Some(days));
    }
    assert_eq!(date_to_epoch_days("2023-02-29"), None);
    assert_eq!(date_to_epoch_days("not-a-date"), None);
}
