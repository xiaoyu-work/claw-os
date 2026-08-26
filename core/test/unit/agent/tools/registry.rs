use super::*;

#[test]
fn default_registry_has_builtins_and_cos_proxy() {
    let r = default_registry();
    assert!(r.get("echo").is_some());
    assert!(r.get("now").is_some());
    assert!(r.get("cos_delegate").is_some());
    assert!(r.get("cos_todo").is_some());
    assert!(r.get("cos_clarify").is_some());
    assert!(r.get("cos_sandbox").is_some());
    assert!(r.get("cos_sysinfo").is_some());
    assert!(r.get("cos_memory").is_some());
    assert!(r.get("cos_tts").is_some());
    assert!(r.get("cos_stt").is_some());
    assert!(r.get("cos_imagegen").is_some());
    assert!(r.get("cos_doctor").is_some());
    // Generic catalog + run are always registered, regardless of
    // whether any typed cos_app_<id> proxies were picked up from
    // $COS_APPS_DIR (which is environment-dependent at test time).
    assert!(r.get("cos_app_catalog").is_some());
    assert!(r.get("cos_app_run").is_some());

    // Lower bound: 2 builtins + cos_delegate + cos_todo + cos_clarify
    // + every cos_proxy tool (primitives + cos_memory) + cos_app_catalog
    // + cos_app_run + 3 media tools, plus optionally cos_recall and any
    // dynamic cos_app_<id> proxies discovered on disk.
    let expected_min = 5 + super::super::cos_proxy::total_count() + 2 + 3;
    assert!(
        r.len() >= expected_min,
        "expected at least {} tools, got {}",
        expected_min,
        r.len()
    );
}

#[test]
fn builtin_only_registry_has_just_builtins() {
    let r = builtin_only_registry();
    assert_eq!(r.len(), 2);
    assert!(r.get("cos_sandbox").is_none());
}

#[test]
fn names_are_sorted() {
    let r = default_registry();
    let names = r.names();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
}

#[test]
fn as_llm_tools_round_trips_schema() {
    let r = default_registry();
    let tools = r.as_llm_tools();
    assert!(tools.iter().any(|t| t.name == "echo"));
}
