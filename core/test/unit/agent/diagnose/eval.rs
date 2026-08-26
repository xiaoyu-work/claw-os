use super::*;
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
struct Scenario {
    name: String,
    args: Vec<String>,
    responses: BTreeMap<String, Value>,
    expected_domain: String,
    expected_status: String,
    expected_findings: Vec<String>,
    #[serde(default)]
    forbidden_findings: Vec<String>,
}

#[test]
fn system_diagnosis_scenario_corpus() {
    let scenarios: Vec<Scenario> =
        serde_json::from_str(include_str!("scenarios.json")).expect("valid diagnosis scenarios");
    assert!(
        scenarios.len() >= 8,
        "the corpus must cover every major diagnosis domain"
    );

    for scenario in scenarios {
        let report = diagnose_with(&scenario.args, |command, args| {
            let key = response_key(command, args);
            Ok(scenario
                .responses
                .get(&key)
                .cloned()
                .unwrap_or_else(|| json!({})))
        })
        .unwrap_or_else(|error| panic!("scenario `{}` failed: {error}", scenario.name));

        assert_eq!(
            report["domain"], scenario.expected_domain,
            "scenario `{}` routed to the wrong domain",
            scenario.name
        );
        assert_eq!(
            report["status"], scenario.expected_status,
            "scenario `{}` returned the wrong status",
            scenario.name
        );

        let finding_codes = report["findings"]
            .as_array()
            .expect("findings array")
            .iter()
            .filter_map(|finding| finding["code"].as_str())
            .collect::<BTreeSet<_>>();
        for expected in &scenario.expected_findings {
            assert!(
                finding_codes.contains(expected.as_str()),
                "scenario `{}` missing finding `{expected}`; got {finding_codes:?}",
                scenario.name
            );
        }
        for forbidden in &scenario.forbidden_findings {
            assert!(
                !finding_codes.contains(forbidden.as_str()),
                "scenario `{}` unexpectedly emitted `{forbidden}`",
                scenario.name
            );
        }

        let evidence_ids = report["evidence"]
            .as_array()
            .expect("evidence array")
            .iter()
            .filter_map(|item| item["id"].as_str())
            .collect::<BTreeSet<_>>();
        for finding in report["findings"].as_array().unwrap() {
            for evidence_id in finding["evidence"].as_array().unwrap() {
                assert!(
                    evidence_ids.contains(evidence_id.as_str().unwrap()),
                    "scenario `{}` emitted an unbound evidence reference",
                    scenario.name
                );
            }
        }
    }
}

fn response_key(command: &str, args: &[String]) -> String {
    if command != "top" {
        return command.to_string();
    }
    let by = args
        .windows(2)
        .find(|pair| pair[0] == "--by")
        .map(|pair| pair[1].as_str())
        .unwrap_or("cpu");
    format!("top:{by}")
}
