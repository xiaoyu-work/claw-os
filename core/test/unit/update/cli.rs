use super::*;

use crate::update::tests::{manifest_bytes, scratch_root, ManifestSpec};

fn args(raw: &[&str]) -> Vec<String> {
    raw.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn options_reject_an_unknown_flag_rather_than_ignoring_it() {
    assert!(parse_options(&args(&["--nope", "x"])).is_err());
}

#[test]
fn options_require_a_value_for_every_flag() {
    assert!(parse_options(&args(&["--package"])).is_err());
}

#[test]
fn installed_versions_can_be_supplied_explicitly() {
    let options = parse_options(&args(&[
        "--installed",
        "claw-os-base=0.2.0",
        "--installed",
        "claw-os-desktop=",
    ]))
    .unwrap();
    assert!(options.installed_given);
    assert_eq!(options.installed["claw-os-base"], "0.2.0");
    assert!(!options.installed.contains_key("claw-os-desktop"));
}

#[test]
fn an_apt_v2_plan_names_the_archives_of_gated_packages() {
    let input = "VERSION 2\nAPT::Architecture=amd64\n\n\
                 claw-os-agent 0.2.0 < 0.1.0 /var/cache/apt/archives/claw-os-agent_0.1.0_amd64.deb\n\
                 vim - < 9.0 /var/cache/apt/archives/vim.deb\n";
    let plan = parse_apt_plan(input).unwrap();
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].package, "claw-os-agent");
    assert_eq!(plan[0].version, "0.1.0");
    assert_eq!(
        plan[0].archive.as_deref(),
        Some(std::path::Path::new(
            "/var/cache/apt/archives/claw-os-agent_0.1.0_amd64.deb"
        ))
    );
}

#[test]
fn an_apt_v3_plan_is_parsed_with_its_wider_fields() {
    let input = "VERSION 3\nAPT::Architecture=amd64\n\n\
                 claw-os-agent:amd64 0.2.0 amd64 same < 0.1.0 amd64 same \
                 /var/cache/apt/archives/claw-os-agent_0.1.0_amd64.deb\n";
    let plan = parse_apt_plan(input).unwrap();
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].package, "claw-os-agent");
    assert_eq!(plan[0].version, "0.1.0");
}

#[test]
fn removals_and_configures_carry_no_archive_to_inspect() {
    let input = "VERSION 2\n\n\
                 claw-os-agent 0.2.0 > - **REMOVE**\n\
                 claw-os-base 0.2.0 = 0.2.0 **CONFIGURE**\n";
    assert!(parse_apt_plan(input).unwrap().is_empty());
}

#[test]
fn the_version_1_protocol_is_reported_as_unsupported_not_parsed() {
    // Version 1 is a bare list of filenames: no versions at all, so it
    // cannot answer the question this hook asks.
    let error =
        parse_apt_plan("/var/cache/apt/archives/claw-os-agent_0.1.0_amd64.deb\n").unwrap_err();
    assert!(
        matches!(&error, PlanError::Unsupported(reason) if reason.contains("version 1")),
        "{error:?}"
    );
}

#[test]
fn an_unknown_apt_protocol_is_unsupported() {
    let error = parse_apt_plan("VERSION 9\n\nclaw-os-agent 1 < 2 /x.deb\n").unwrap_err();
    assert!(matches!(error, PlanError::Unsupported(_)), "{error:?}");
    let error = parse_apt_plan("VERSION x\n\n").unwrap_err();
    assert!(matches!(error, PlanError::Unsupported(_)), "{error:?}");
}

#[test]
fn a_malformed_apt_record_is_reported_separately_from_the_protocol() {
    let error = parse_apt_plan("VERSION 2\n\nclaw-os-agent 0.2.0 <\n").unwrap_err();
    assert!(matches!(error, PlanError::Malformed(_)), "{error:?}");
    // No blank line at all: the configuration block never ended.
    let error = parse_apt_plan("VERSION 2\nAPT::Architecture=amd64\n").unwrap_err();
    assert!(matches!(error, PlanError::Malformed(_)), "{error:?}");
}

#[test]
fn a_relative_archive_path_is_refused() {
    let error =
        parse_apt_plan("VERSION 2\n\nclaw-os-agent 0.2.0 < 0.1.0 ../../evil.deb\n").unwrap_err();
    assert!(matches!(error, PlanError::Malformed(_)), "{error:?}");
}

#[test]
fn the_apt_plan_parser_is_bounded() {
    let mut input = String::from("VERSION 2\n\n");
    input.push_str("claw-os-agent 0.2.0 < 0.1.0 /a.deb\n");
    input.push_str(&format!(
        "claw-os-agent 0.2.0 < 0.1.0 /{}\n",
        "a".repeat(MAX_HOOK_LINE_BYTES)
    ));
    let error = parse_apt_plan(&input).unwrap_err();
    assert!(matches!(error, PlanError::Malformed(_)), "{error:?}");

    let mut flood = String::from("VERSION 2\n\n");
    for _ in 0..(MAX_HOOK_LINES + 2) {
        flood.push_str("vim - < 9.0 /vim.deb\n");
    }
    assert!(matches!(
        parse_apt_plan(&flood).unwrap_err(),
        PlanError::Malformed(_)
    ));
}

#[test]
fn measuring_declared_components_refuses_a_mismatched_install() {
    let root = scratch_root("cli-measure");
    let installed = root.join("usr/local/bin/clawd");
    std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
    std::fs::write(&installed, b"the real clawd").unwrap();

    let good = manifest_bytes(&ManifestSpec {
        component_digest: crate::crypto::sha256_hex(b"the real clawd"),
        ..ManifestSpec::default()
    });
    let manifest = Manifest::parse(&good).unwrap();
    assert!(measure_declared_components(&root, &manifest).is_ok());

    std::fs::write(&installed, b"a different clawd").unwrap();
    let error = measure_declared_components(&root, &manifest).unwrap_err();
    assert!(error.contains("does not match"), "{error}");
}

#[test]
fn measuring_declared_components_refuses_a_missing_file() {
    let root = scratch_root("cli-measure-missing");
    let manifest = Manifest::parse(&manifest_bytes(&ManifestSpec::default())).unwrap();
    let error = measure_declared_components(&root, &manifest).unwrap_err();
    assert!(error.contains("not installed correctly"), "{error}");
}

#[test]
fn the_policy_command_prints_the_compiled_epoch() {
    assert_eq!(policy_command().unwrap(), EXIT_ALLOWED);
}

#[test]
fn an_unknown_command_is_a_usage_error_not_a_silent_success() {
    assert_eq!(run(&args(&["not-a-command"])).unwrap(), EXIT_USAGE);
}

#[test]
fn show_reports_an_uninitialized_floor() {
    let root = scratch_root("cli-show");
    let code = run(&args(&["show", "--root", root.to_str().unwrap()])).unwrap();
    assert_eq!(code, EXIT_ALLOWED);
}

#[test]
fn check_candidate_refuses_an_older_release_against_a_seeded_floor() {
    let root = scratch_root("cli-check");
    let store = FloorStore::under_root(&root);
    store.ensure_dir().unwrap();
    let newer = Manifest::parse(&manifest_bytes(&ManifestSpec {
        version: "1:0.2.0+git200.gbbbbbbbbbbbb",
        ..ManifestSpec::default()
    }))
    .unwrap();
    let floor = Floor::bootstrap(
        &newer,
        Default::default(),
        Default::default(),
        chrono::Utc::now(),
    );
    store.commit(&floor, "seed").unwrap();

    let manifest_path = root.join("candidate.json");
    std::fs::write(
        &manifest_path,
        manifest_bytes(&ManifestSpec {
            version: "1:0.2.0+git100.gaaaaaaaaaaaa",
            ..ManifestSpec::default()
        }),
    )
    .unwrap();

    let code = run(&args(&[
        "check-candidate",
        "--root",
        root.to_str().unwrap(),
        "--package",
        "claw-os-agent",
        "--version",
        "1:0.2.0+git100.gaaaaaaaaaaaa",
        "--manifest",
        manifest_path.to_str().unwrap(),
        "--installed",
        "claw-os-agent=1:0.2.0+git200.gbbbbbbbbbbbb",
    ]))
    .unwrap();
    assert_eq!(code, EXIT_REFUSED);
}

#[test]
fn check_incoming_refuses_an_older_version_before_unpack() {
    let root = scratch_root("cli-incoming");
    let store = FloorStore::under_root(&root);
    store.ensure_dir().unwrap();
    let newer = Manifest::parse(&manifest_bytes(&ManifestSpec {
        version: "1:0.2.0+git200.gbbbbbbbbbbbb",
        ..ManifestSpec::default()
    }))
    .unwrap();
    store
        .commit(
            &Floor::bootstrap(
                &newer,
                Default::default(),
                Default::default(),
                chrono::Utc::now(),
            ),
            "seed",
        )
        .unwrap();

    assert_eq!(
        run(&args(&[
            "check-incoming",
            "--root",
            root.to_str().unwrap(),
            "--package",
            "claw-os-agent",
            "--version",
            "1:0.2.0+git100.gaaaaaaaaaaaa",
        ]))
        .unwrap(),
        EXIT_REFUSED
    );
    assert_eq!(
        run(&args(&[
            "check-incoming",
            "--root",
            root.to_str().unwrap(),
            "--package",
            "claw-os-agent",
            "--version",
            "1:0.2.0+git900.gzzzzzzzzzzzz",
        ]))
        .unwrap(),
        EXIT_ALLOWED
    );
}

#[test]
fn commit_seeds_the_floor_and_then_refuses_to_walk_it_back() {
    let root = scratch_root("cli-commit");
    let binary = root.join("usr/local/bin/clawd");
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    std::fs::write(&binary, b"clawd bytes").unwrap();

    let spec = ManifestSpec {
        component_digest: crate::crypto::sha256_hex(b"clawd bytes"),
        ..ManifestSpec::default()
    };
    let manifest_path = root.join("manifest.json");
    std::fs::write(&manifest_path, manifest_bytes(&spec)).unwrap();

    assert_eq!(
        run(&args(&[
            "commit",
            "--root",
            root.to_str().unwrap(),
            "--package",
            "claw-os-agent",
            "--version",
            spec.version,
            "--manifest",
            manifest_path.to_str().unwrap(),
            "--installed",
            "claw-os-agent=1:0.2.0+git100.gaaaaaaaaaaaa",
        ]))
        .unwrap(),
        EXIT_ALLOWED
    );

    let older = ManifestSpec {
        version: "1:0.2.0+git50.gddddddddddd0",
        component_digest: crate::crypto::sha256_hex(b"clawd bytes"),
        ..ManifestSpec::default()
    };
    let older_path = root.join("older.json");
    std::fs::write(&older_path, manifest_bytes(&older)).unwrap();
    assert_eq!(
        run(&args(&[
            "check-candidate",
            "--root",
            root.to_str().unwrap(),
            "--package",
            "claw-os-agent",
            "--version",
            older.version,
            "--manifest",
            older_path.to_str().unwrap(),
            "--installed",
            "claw-os-agent=1:0.2.0+git100.gaaaaaaaaaaaa",
        ]))
        .unwrap(),
        EXIT_REFUSED
    );
}

#[test]
fn commit_refuses_when_the_unpacked_files_do_not_match_the_manifest() {
    let root = scratch_root("cli-commit-mismatch");
    let binary = root.join("usr/local/bin/clawd");
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    std::fs::write(&binary, b"something else").unwrap();

    let spec = ManifestSpec {
        component_digest: crate::crypto::sha256_hex(b"clawd bytes"),
        ..ManifestSpec::default()
    };
    let manifest_path = root.join("manifest.json");
    std::fs::write(&manifest_path, manifest_bytes(&spec)).unwrap();

    let code = run(&args(&[
        "commit",
        "--root",
        root.to_str().unwrap(),
        "--package",
        "claw-os-agent",
        "--version",
        spec.version,
        "--manifest",
        manifest_path.to_str().unwrap(),
        "--installed",
        "claw-os-agent=1:0.2.0+git100.gaaaaaaaaaaaa",
    ]));
    assert!(
        code.is_err(),
        "a mismatched unpack must not advance the floor"
    );
    assert_eq!(
        FloorStore::under_root(&root).load().unwrap(),
        crate::update::floor::FloorState::Uninitialized
    );
}

#[test]
fn a_recovery_authorization_cannot_be_recorded_without_a_terminal() {
    // The test process has no controlling terminal on stdin, which is
    // exactly the condition an agent, App or MCP session would be in.
    assert!(require_operator_terminal().is_err());
}

#[test]
fn service_gate_refuses_an_incompatible_installed_set() {
    let root = scratch_root("cli-service-gate");
    let manifest_path = root.join("manifest.json");
    std::fs::write(&manifest_path, manifest_bytes(&ManifestSpec::default())).unwrap();
    assert_eq!(
        run(&args(&[
            "service-gate",
            "--root",
            root.to_str().unwrap(),
            "--package",
            "claw-os-agent",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "--installed",
            "claw-os-base=0.1.0",
        ]))
        .unwrap(),
        EXIT_REFUSED
    );
    assert_eq!(
        run(&args(&[
            "service-gate",
            "--root",
            root.to_str().unwrap(),
            "--package",
            "claw-os-agent",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "--installed",
            "claw-os-base=1:0.2.0",
        ]))
        .unwrap(),
        EXIT_ALLOWED
    );
}
