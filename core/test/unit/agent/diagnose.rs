use super::*;

#[test]
fn routes_chinese_and_english_symptoms() {
    assert_eq!(infer_domain("网络为什么很慢"), DiagnosticDomain::Network);
    assert_eq!(
        infer_domain("the service keeps crashing"),
        DiagnosticDomain::Crash
    );
    assert_eq!(infer_domain("磁盘空间不够"), DiagnosticDomain::Storage);
    assert_eq!(
        infer_domain("computer is slow"),
        DiagnosticDomain::Performance
    );
}

#[test]
fn explicit_domain_overrides_inference() {
    let options = parse_options(&[
        "--domain".into(),
        "service".into(),
        "the computer is slow".into(),
    ])
    .unwrap();
    assert_eq!(options.domain, DiagnosticDomain::Service);
}

#[test]
fn quick_network_plan_skips_sampled_rate() {
    let options = Options {
        symptom: "network slow".into(),
        domain: DiagnosticDomain::Network,
        quick: true,
        path: None,
    };
    let ids = plan_for(&options)
        .into_iter()
        .map(|probe| probe.id)
        .collect::<Vec<_>>();
    assert!(ids.contains(&"network"));
    assert!(!ids.contains(&"network-rate"));
}

#[test]
fn findings_reference_supporting_evidence() {
    let evidence = vec![
        ok(
            "resources",
            json!({"memory":{"total_mb":1000,"available_mb":50},"disk":{"total_mb":1000,"free_mb":50}}),
        ),
        ok("load", json!({"load_per_core_1min": 2.5})),
        ok("failed-units", json!({"count": 2})),
    ];
    let findings = analyze(&evidence);
    assert!(findings.iter().any(|item| item.code == "memory-critical"));
    assert!(findings.iter().any(|item| item.code == "disk-critical"));
    assert!(findings.iter().any(|item| item.code == "load-critical"));
    assert!(findings
        .iter()
        .find(|item| item.code == "failed-systemd-units")
        .unwrap()
        .evidence
        .contains(&"failed-units".to_string()));
}

#[test]
fn report_surfaces_partial_probe_failure() {
    let args = vec!["computer slow".to_string()];
    let report = diagnose_with(&args, |command, _| {
        if command == "resources" {
            Err("permission denied: token sk-abcdefghijklmnopqrstuvwxyz".into())
        } else {
            Ok(json!({}))
        }
    })
    .unwrap();
    assert_eq!(report["status"], "warn");
    assert!(report["coverage"]["failed"].as_u64().unwrap() >= 1);
    let error = report["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == "resources")
        .unwrap()["error"]
        .as_str()
        .unwrap();
    assert!(!error.contains("sk-abcdefghijklmnopqrstuvwxyz"));
}

#[test]
fn report_never_references_an_uncollected_probe() {
    let report = diagnose_with(&["general issue".into()], |command, _| match command {
        "loadavg" => Ok(json!({"load_per_core_1min": 3.0})),
        _ => Ok(json!({})),
    })
    .unwrap();
    let ids = report["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect::<BTreeSet<_>>();
    for finding in report["findings"].as_array().unwrap() {
        for evidence_id in finding["evidence"].as_array().unwrap() {
            assert!(ids.contains(evidence_id.as_str().unwrap()));
        }
    }

}

#[test]
fn detects_apparmor_denial_inside_raw_kernel_text() {
    let findings = analyze(&[ok(
        "kernel-log",
        json!({"entries": [{"raw": "audit: apparmor=\"DENIED\" operation=\"open\""}]}),
    )]);
    assert!(findings
        .iter()
        .any(|finding| finding.code == "apparmor-denial"));
}

#[test]
fn rejects_unknown_flags_and_missing_symptom() {
    assert!(parse_options(&["--bogus".into(), "x".into()]).is_err());
    assert!(parse_options(&[]).is_err());
}

fn ok(id: &str, data: Value) -> Evidence {
    Evidence {
        id: id.to_string(),
        command: id.to_string(),
        description: String::new(),
        status: "ok",
        latency_ms: 0,
        data: Some(data),
        error: None,
    }
}
