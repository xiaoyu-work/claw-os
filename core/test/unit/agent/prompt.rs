use super::*;
use std::io::Write;

#[test]
fn scaffold_is_returned_when_no_extra() {
    let p = build_system_prompt(None);
    assert!(p.contains("ClawOS"));
    assert!(p.contains("You are Claw,"));
    assert!(p.contains("cos_"));
    assert!(!p.contains("kernel-resident"));
    assert!(p.contains("does not imply that the host operating system is ClawOS"));
    assert!(p.contains("`claw_os: true`"));
}

#[test]
fn scaffold_steers_gui_launches_through_launcher() {
    // GUI-app launches must route through `cos_app_launcher`
    // (the cap-gated AppID launcher), not `cos_app_exec`. The
    // scaffold has to spell this out: without it the model picks
    // `exec start cosmic-files` for "open the file manager",
    // bypassing `desktop.launch` and the user's installed
    // `.desktop` entries.
    let p = build_system_prompt(None);
    assert!(
        p.contains("cos_app_launcher"),
        "scaffold should mention the launcher tool"
    );
    assert!(
        p.contains("cos_app_exec"),
        "scaffold should explicitly contrast with cos_app_exec"
    );
    assert!(
        p.contains("desktop.launch"),
        "scaffold should name the cap that gates the launcher path"
    );
}

#[test]
fn scaffold_requires_runtime_evidence_citations() {
    let prompt = build_system_prompt(None);
    assert!(prompt.contains("[evidence:<tool_call_id>"));
    assert!(prompt.contains("confidence=<0.00-1.00>"));
    assert!(prompt.contains("Use only tool call IDs from this trajectory"));
}

#[test]
fn scaffold_stops_on_non_retryable_auth_errors() {
    let prompt = build_system_prompt(None);
    assert!(prompt.contains("`auth_required: true`"));
    assert!(prompt.contains("stop retrying credential/catalog/filesystem tools"));
    assert!(prompt.contains("Never ask the user to paste"));
}

#[test]
fn extra_file_appended_when_provided() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("cos-prompt-{}.md", std::process::id()));
    let mut f = fs::File::create(&path).unwrap();
    writeln!(f, "EXTRA_BLOCK").unwrap();
    let p = build_system_prompt(Some(&path));
    assert!(p.contains("EXTRA_BLOCK"));
    let _ = fs::remove_file(&path);
}

#[test]
fn missing_extra_file_is_silent() {
    let p = build_system_prompt(Some(Path::new("/nonexistent/cos-prompt.md")));
    assert!(p.contains("ClawOS"));
}

#[test]
fn no_due_nudges_means_no_due_block() {
    // Without writing any nudges to the data dir, the
    // DUE_NUDGES block must be absent. (NudgeStore returns
    // Vec::new() for missing or unparseable files.)
    let p = build_system_prompt(None);
    assert!(!p.contains("<DUE_NUDGES>"));
}
