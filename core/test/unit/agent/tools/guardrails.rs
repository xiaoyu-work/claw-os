use super::*;
use crate::agent::tools::registry::builtin_only_registry;

fn r() -> ToolRegistry {
    builtin_only_registry()
}

#[test]
fn permissive_default_allows_everything() {
    let g = Guardrails::default();
    assert_eq!(g.decide("echo"), Decision::Allow);
    assert_eq!(g.decide("anything"), Decision::Allow);
    assert!(g.permits("echo"));
}

#[test]
fn deny_all_denies_everything() {
    let g = Guardrails::deny_all();
    assert!(matches!(g.decide("echo"), Decision::Deny(_)));
    assert!(matches!(g.decide("xyz"), Decision::Deny(_)));
}

#[test]
fn allowlist_only_permits_listed() {
    let g = Guardrails::default().with_allow(Some(["echo"]));
    assert_eq!(g.decide("echo"), Decision::Allow);
    assert!(matches!(g.decide("now"), Decision::Deny(_)));
}

#[test]
fn denylist_blocks_listed() {
    let g = Guardrails::default().with_deny(["echo"]);
    assert!(matches!(g.decide("echo"), Decision::Deny(_)));
    assert_eq!(g.decide("now"), Decision::Allow);
}

#[test]
fn deny_wins_over_allow() {
    let g = Guardrails::default()
        .with_allow(Some(["echo", "now"]))
        .with_deny(["echo"]);
    assert!(matches!(g.decide("echo"), Decision::Deny(_)));
    assert_eq!(g.decide("now"), Decision::Allow);
}

#[test]
fn allow_tool_initialises_set_when_none() {
    let g = Guardrails::default().allow_tool("echo");
    assert!(g.allow.is_some());
    assert_eq!(g.decide("echo"), Decision::Allow);
    assert!(matches!(g.decide("now"), Decision::Deny(_)));
}

#[test]
fn allow_tool_appends_to_existing_set() {
    let g = Guardrails::default()
        .with_allow(Some(["echo"]))
        .allow_tool("now");
    assert_eq!(g.decide("echo"), Decision::Allow);
    assert_eq!(g.decide("now"), Decision::Allow);
}

#[test]
fn deny_tool_appends_to_existing_set() {
    let g = Guardrails::default().deny_tool("echo").deny_tool("now");
    assert!(matches!(g.decide("echo"), Decision::Deny(_)));
    assert!(matches!(g.decide("now"), Decision::Deny(_)));
}

#[test]
fn empty_allowlist_denies_all() {
    let g = Guardrails::default().with_allow(Some(Vec::<String>::new()));
    assert!(matches!(g.decide("echo"), Decision::Deny(_)));
}

#[test]
fn deny_decision_includes_tool_name() {
    let g = Guardrails::default().with_deny(["echo"]);
    match g.decide("echo") {
        Decision::Deny(reason) => assert!(reason.contains("echo")),
        Decision::Allow => panic!("expected deny"),
    }
}

#[test]
fn filter_llm_tools_keeps_only_permitted() {
    let registry = r();
    let g = Guardrails::default().with_allow(Some(["echo"]));
    let tools = filter_llm_tools(&registry, &g);
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");
}

#[test]
fn filter_llm_tools_permissive_returns_all() {
    let registry = r();
    let g = Guardrails::default();
    let tools = filter_llm_tools(&registry, &g);
    assert_eq!(tools.len(), registry.len());
}

#[test]
fn filter_llm_tools_deny_all_returns_none() {
    let registry = r();
    let g = Guardrails::deny_all();
    let tools = filter_llm_tools(&registry, &g);
    assert!(tools.is_empty());
}

#[test]
fn get_filtered_returns_none_for_denied() {
    let registry = r();
    let g = Guardrails::default().with_deny(["echo"]);
    assert!(get_filtered(&registry, &g, "echo").is_none());
    assert!(get_filtered(&registry, &g, "now").is_some());
}

#[test]
fn get_filtered_returns_none_for_unknown_tool() {
    let registry = r();
    let g = Guardrails::default();
    assert!(get_filtered(&registry, &g, "nonexistent").is_none());
}

#[test]
fn get_filtered_returns_arc_for_allowed() {
    let registry = r();
    let g = Guardrails::default();
    let t = get_filtered(&registry, &g, "echo").expect("echo allowed");
    assert_eq!(t.name(), "echo");
}

#[test]
fn permitted_names_returns_sorted_subset() {
    let registry = r();
    let g = Guardrails::default().with_deny(["echo"]);
    let names = permitted_names(&registry, &g);
    assert_eq!(names, vec!["now"]);
}

#[test]
fn permitted_names_empty_when_deny_all() {
    let registry = r();
    let g = Guardrails::deny_all();
    assert!(permitted_names(&registry, &g).is_empty());
}

#[test]
fn permits_helper_matches_decide() {
    let g = Guardrails::default().with_deny(["x"]);
    assert!(g.permits("y"));
    assert!(!g.permits("x"));
}

#[test]
fn guardrails_clone_independent() {
    let g1 = Guardrails::default().with_deny(["x"]);
    let g2 = g1.clone().with_deny(["y"]);
    assert!(matches!(g1.decide("x"), Decision::Deny(_)));
    assert_eq!(g1.decide("y"), Decision::Allow);
    assert_eq!(g2.decide("x"), Decision::Allow);
    assert!(matches!(g2.decide("y"), Decision::Deny(_)));
}
