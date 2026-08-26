use super::*;
use crate::caps::manifest::AiBudget;

fn policy(units: u64, safety: AiSafety, origins: Vec<PromptOrigin>) -> AiPolicy {
    AiPolicy {
        budget: AiBudget { monthly_units: units },
        safety,
        origins,
        tools: Vec::new(),
    }
}

#[test]
fn missing_override_yields_manifest_verbatim() {
    let m = policy(1000, AiSafety::Standard, vec![PromptOrigin::Trusted]);
    let out = apply_to_policy(&m, None);
    assert_eq!(out.budget.monthly_units, 1000);
    assert_eq!(out.safety, AiSafety::Standard);
    assert_eq!(out.origins, vec![PromptOrigin::Trusted]);
}

#[test]
fn empty_override_yields_manifest_verbatim() {
    let m = policy(1000, AiSafety::Standard, vec![PromptOrigin::Trusted]);
    let ovr = UserOverride::default();
    let out = apply_to_policy(&m, Some(&ovr));
    assert_eq!(out.budget.monthly_units, 1000);
    assert_eq!(out.safety, AiSafety::Standard);
    assert_eq!(out.origins, vec![PromptOrigin::Trusted]);
}

#[test]
fn override_lowers_budget() {
    let m = policy(1000, AiSafety::Standard, vec![PromptOrigin::Trusted]);
    let ovr = UserOverride {
        ai: Some(AiOverride {
            budget: Some(AiBudgetOverride {
                monthly_units: Some(250),
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let out = apply_to_policy(&m, Some(&ovr));
    assert_eq!(out.budget.monthly_units, 250);
}

#[test]
fn override_cannot_raise_budget() {
    let m = policy(500, AiSafety::Standard, vec![PromptOrigin::Trusted]);
    let ovr = UserOverride {
        ai: Some(AiOverride {
            budget: Some(AiBudgetOverride {
                monthly_units: Some(10_000),
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let out = apply_to_policy(&m, Some(&ovr));
    assert_eq!(out.budget.monthly_units, 500);
}

#[test]
fn override_promotes_safety_to_stricter() {
    let m = policy(1000, AiSafety::Standard, vec![PromptOrigin::Trusted]);
    let ovr = UserOverride {
        ai: Some(AiOverride {
            safety: Some(AiSafety::Strict),
            ..Default::default()
        }),
        ..Default::default()
    };
    let out = apply_to_policy(&m, Some(&ovr));
    assert_eq!(out.safety, AiSafety::Strict);
}

#[test]
fn override_cannot_weaken_safety() {
    let m = policy(1000, AiSafety::Strict, vec![PromptOrigin::Trusted]);
    let ovr = UserOverride {
        ai: Some(AiOverride {
            safety: Some(AiSafety::Minimal),
            ..Default::default()
        }),
        ..Default::default()
    };
    let out = apply_to_policy(&m, Some(&ovr));
    assert_eq!(out.safety, AiSafety::Strict);
}

#[test]
fn override_shrinks_origins_to_intersection() {
    let m = policy(
        1000,
        AiSafety::Standard,
        vec![
            PromptOrigin::Trusted,
            PromptOrigin::UserInput,
            PromptOrigin::ExternalContent,
        ],
    );
    let ovr = UserOverride {
        ai: Some(AiOverride {
            origins: Some(vec![PromptOrigin::Trusted, PromptOrigin::UserInput]),
            ..Default::default()
        }),
        ..Default::default()
    };
    let out = apply_to_policy(&m, Some(&ovr));
    assert_eq!(
        out.origins,
        vec![PromptOrigin::Trusted, PromptOrigin::UserInput]
    );
}

#[test]
fn override_cannot_add_unallowed_origin() {
    let m = policy(1000, AiSafety::Standard, vec![PromptOrigin::Trusted]);
    let ovr = UserOverride {
        ai: Some(AiOverride {
            origins: Some(vec![
                PromptOrigin::Trusted,
                PromptOrigin::ExternalContent,
            ]),
            ..Default::default()
        }),
        ..Default::default()
    };
    let out = apply_to_policy(&m, Some(&ovr));
    assert_eq!(out.origins, vec![PromptOrigin::Trusted]);
}

#[test]
fn stricter_helper_orders_correctly() {
    use AiSafety::*;
    assert_eq!(stricter(Minimal, Standard), Standard);
    assert_eq!(stricter(Standard, Strict), Strict);
    assert_eq!(stricter(Minimal, Strict), Strict);
    assert_eq!(stricter(Strict, Standard), Strict);
    assert_eq!(stricter(Minimal, Minimal), Minimal);
}

#[test]
fn load_missing_file_returns_none() {
    let tmp = std::env::temp_dir().join(format!(
        "cos-overrides-test-missing-{}",
        std::process::id()
    ));
    let prev = std::env::var_os("COS_USER_CONFIG_DIR");
    std::env::set_var("COS_USER_CONFIG_DIR", &tmp);
    let got = load("never-existed");
    match prev {
        Some(v) => std::env::set_var("COS_USER_CONFIG_DIR", v),
        None => std::env::remove_var("COS_USER_CONFIG_DIR"),
    }
    assert_eq!(got.unwrap(), None);
}

#[test]
fn load_parses_full_shape() {
    let tmp = std::env::temp_dir().join(format!(
        "cos-overrides-test-full-{}",
        std::process::id()
    ));
    let apps = tmp.join("apps");
    fs::create_dir_all(&apps).unwrap();
    let body = r#"{
        "disabled": false,
        "ai": {
            "budget": {"monthly_units": 50},
            "safety": "strict",
            "origins": ["trusted"]
        }
    }"#;
    fs::write(apps.join("widget.json"), body).unwrap();

    let prev = std::env::var_os("COS_USER_CONFIG_DIR");
    std::env::set_var("COS_USER_CONFIG_DIR", &tmp);
    let got = load("widget");
    match prev {
        Some(v) => std::env::set_var("COS_USER_CONFIG_DIR", v),
        None => std::env::remove_var("COS_USER_CONFIG_DIR"),
    }

    let ovr = got.unwrap().unwrap();
    assert!(!ovr.disabled);
    let ai = ovr.ai.unwrap();
    assert_eq!(ai.budget.unwrap().monthly_units, Some(50));
    assert_eq!(ai.safety, Some(AiSafety::Strict));
    assert_eq!(ai.origins, Some(vec![PromptOrigin::Trusted]));

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn load_disabled_only_parses() {
    let tmp = std::env::temp_dir().join(format!(
        "cos-overrides-test-disabled-{}",
        std::process::id()
    ));
    let apps = tmp.join("apps");
    fs::create_dir_all(&apps).unwrap();
    fs::write(apps.join("widget.json"), r#"{"disabled": true}"#).unwrap();

    let prev = std::env::var_os("COS_USER_CONFIG_DIR");
    std::env::set_var("COS_USER_CONFIG_DIR", &tmp);
    let got = load("widget");
    match prev {
        Some(v) => std::env::set_var("COS_USER_CONFIG_DIR", v),
        None => std::env::remove_var("COS_USER_CONFIG_DIR"),
    }

    let ovr = got.unwrap().unwrap();
    assert!(ovr.disabled);
    assert!(ovr.ai.is_none());

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn load_malformed_file_errors() {
    let tmp = std::env::temp_dir().join(format!(
        "cos-overrides-test-bad-{}",
        std::process::id()
    ));
    let apps = tmp.join("apps");
    fs::create_dir_all(&apps).unwrap();
    fs::write(apps.join("widget.json"), "{not json").unwrap();

    let prev = std::env::var_os("COS_USER_CONFIG_DIR");
    std::env::set_var("COS_USER_CONFIG_DIR", &tmp);
    let got = load("widget");
    match prev {
        Some(v) => std::env::set_var("COS_USER_CONFIG_DIR", v),
        None => std::env::remove_var("COS_USER_CONFIG_DIR"),
    }

    assert!(got.is_err());
    let _ = fs::remove_dir_all(&tmp);
}
