use super::*;
use std::io::Write;

struct EnvVarGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &Path) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

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

#[test]
fn skill_catalog_is_metadata_only_and_traced() {
    let system = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    let dir = system.path().join("claw-os");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        "---\nname: claw-os\ndescription: Claw system operations\n---\nHIDDEN_BODY\n",
    )
    .unwrap();
    let skills = crate::agent::skills::loader::load_layered(
        system.path(),
        user.path(),
        &crate::agent::skills::loader::LoadOptions::default(),
    );
    let mut prompt = String::new();
    let mut segments = Vec::new();

    append_skill_catalog(&mut prompt, &mut segments, &skills);

    assert!(prompt.contains("Claw system operations"));
    assert!(prompt.contains("cos_skill"));
    assert!(!prompt.contains("HIDDEN_BODY"));
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].source, INJECTED_SOURCE_SKILLS_CATALOG);
    assert!(prompt.ends_with(&segments[0].content));
}

#[test]
fn traced_prompt_builder_discovers_configured_system_skills() {
    let system = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let dir = system.path().join("configured-skill");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        "---\nname: configured-skill\ndescription: DISCOVERED_METADATA\n---\nHIDDEN_BODY\n",
    )
    .unwrap();
    let _system_guard = EnvVarGuard::set("COS_SYSTEM_SKILLS_DIR", system.path());
    let _data_guard = EnvVarGuard::set("COS_DATA_DIR", data.path());
    let _user_data_guard = EnvVarGuard::set("COS_USER_DATA_DIR", data.path());

    let (prompt, segments) = build_system_prompt_traced(None, Some("use configured skill"));

    assert!(prompt.contains("DISCOVERED_METADATA"));
    assert!(!prompt.contains("HIDDEN_BODY"));
    assert!(segments
        .iter()
        .any(|segment| segment.source == INJECTED_SOURCE_SKILLS_CATALOG));
}
