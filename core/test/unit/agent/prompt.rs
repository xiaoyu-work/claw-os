use super::*;

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
    // GUI launches route through the generic app gateway's launcher app, not
    // the exec app. Typed per-app proxies are intentionally absent by default.
    let p = build_system_prompt(None);
    assert!(
        p.contains("`cos_app_run` with `app=\"launcher\"`"),
        "scaffold should route launch discovery through the generic app runner"
    );
    assert!(
        p.contains("Never start GUI binaries through `app=\"exec\"`"),
        "scaffold should explicitly contrast with the exec app"
    );
    assert!(
        p.contains("desktop.launch"),
        "scaffold should name the cap that gates the launcher path"
    );
}

#[test]
fn scaffold_explains_progressive_app_disclosure() {
    let prompt = build_system_prompt(None);
    assert!(prompt.contains("`cos_app_catalog search`"));
    assert!(prompt.contains("then invoke it through `cos_app_run`"));
    assert!(prompt.contains("Do not guess unavailable `cos_app_<id>` tool names"));
}

#[test]
fn scaffold_requires_recursive_cli_discovery_before_claiming_unsupported() {
    let prompt = build_system_prompt(None);
    assert!(prompt.contains("call `cos_help` with `path=[]`"));
    assert!(prompt.contains("one level at a time"));
    assert!(prompt.contains("Never claim that a Claw capability is unsupported"));
    assert!(prompt.contains("never executes them"));
}

#[test]
fn scaffold_requires_runtime_evidence_citations() {
    let prompt = build_system_prompt(None);
    assert!(prompt.contains("[evidence:<tool_call_id>"));
    assert!(prompt.contains("confidence=<0.00-1.00>"));
    assert!(prompt.contains("Use only tool call IDs from this trajectory"));
}

#[test]
fn scaffold_orchestrates_supported_oauth_setup() {
    let prompt = build_system_prompt(None);
    assert!(prompt.contains("`auth_required: true`"));
    assert!(prompt.contains("`setup.agent_action`"));
    assert!(prompt.contains("`cos_oauth_login`"));
    assert!(prompt.contains("retry the original App operation once"));
    assert!(prompt.contains("Never ask the user to paste"));
}

#[test]
fn owner_writable_extra_file_is_prelude_data_not_policy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("preface.md");
    fs::write(&path, "EXTRA_BLOCK\n").unwrap();

    // The temp dir belongs to the test user, so ownership verification
    // refuses to promote it into the policy channel.
    let policy = build_system_prompt(Some(&path));
    assert!(!policy.contains("EXTRA_BLOCK"));

    let skills = crate::agent::skills::loader::LoadResult::default();
    let notes = crate::agent::memory::notes::NotesStore::at(dir.path());
    let projection = build_projection(Some(&path), None, &skills, &notes);
    assert!(projection.channels_are_separated());
    let prelude = projection
        .prelude_segments()
        .iter()
        .find(|segment| segment.kind() == crate::agent::trust::SourceKind::OperatorPromptFile)
        .expect("owner-writable prompt file is prelude data");
    assert_eq!(prelude.content(), "EXTRA_BLOCK");
    assert_eq!(
        prelude.class(),
        crate::agent::trust::TrustClass::UserControlledContext
    );
}

#[test]
fn missing_extra_file_is_silent() {
    let p = build_system_prompt(Some(Path::new("/nonexistent/cos-prompt.md")));
    assert!(p.contains("ClawOS"));
}

#[test]
fn no_due_nudges_means_no_due_block() {
    assert!(build_turn_context_segments().is_empty());
}

#[test]
fn due_nudges_are_request_local_not_canonical_system_prompt() {
    let data = tempfile::tempdir().unwrap();
    let _data_guard = EnvVarGuard::set("COS_DATA_DIR", data.path());
    let _user_data_guard = EnvVarGuard::set("COS_USER_DATA_DIR", data.path());
    let store = crate::agent::nudge::NudgeStore::new(crate::paths::agent_nudges_path());
    store
        .add(crate::agent::nudge::Nudge {
            id: "due-now".into(),
            message: "check the backup".into(),
            due_at_epoch_s: 0,
            repeat_secs: None,
            tag: None,
            last_fired_epoch_s: None,
        })
        .unwrap();

    let canonical = build_system_prompt(None);
    let turn_segments = build_turn_context_segments();

    assert!(!canonical.contains("check the backup"));
    assert_eq!(turn_segments.len(), 1);
    assert_eq!(turn_segments[0].source(), INJECTED_SOURCE_DUE_NUDGES);
    assert_eq!(
        turn_segments[0].class(),
        crate::agent::trust::TrustClass::UserControlledContext
    );
    assert!(turn_segments[0].content.contains("check the backup"));
    // Owner-authored reminders are data, not operator rules.
    assert!(crate::agent::trust::envelope::looks_enveloped(
        &turn_segments[0].content
    ));
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
    crate::test_env::sign_test_package(&dir, crate::provenance::PackageKind::Skill, "claw-os");
    let skills = crate::agent::skills::loader::load_layered(
        system.path(),
        user.path(),
        &crate::agent::skills::loader::LoadOptions::default(),
    );
    let notes = crate::agent::memory::notes::NotesStore::at(
        tempfile::tempdir().unwrap().path().to_path_buf(),
    );

    let (prompt, segments) = build_system_prompt_traced_with(None, None, &skills, &notes);

    // The catalogue is extension metadata, so it is *not* in the policy
    // channel — it is prelude data carried in a fenced user message.
    assert!(!prompt.contains("Claw system operations"));
    assert!(!prompt.contains("HIDDEN_BODY"));
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].source(), INJECTED_SOURCE_SKILLS_CATALOG);
    assert!(segments[0].content.contains("Claw system operations"));
    assert!(segments[0].content.contains("cos_skill"));
    assert!(!segments[0].content.contains("HIDDEN_BODY"));
    // A Skill's own metadata is extension metadata, never policy, even
    // when the package is vendor-signed.
    assert_eq!(
        segments[0].class(),
        crate::agent::trust::TrustClass::ExtensionMetadata
    );
    assert!(crate::agent::trust::envelope::looks_enveloped(
        &segments[0].content
    ));

    let projection = build_projection(None, None, &skills, &notes);
    assert!(projection.channels_are_separated());
    assert_eq!(projection.prelude_segments().len(), 1);
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
    crate::test_env::sign_test_package(
        &dir,
        crate::provenance::PackageKind::Skill,
        "configured-skill",
    );
    let _system_guard = EnvVarGuard::set("COS_SYSTEM_SKILLS_DIR", system.path());
    let _data_guard = EnvVarGuard::set("COS_DATA_DIR", data.path());
    let _user_data_guard = EnvVarGuard::set("COS_USER_DATA_DIR", data.path());

    let (prompt, segments) = build_system_prompt_traced(None, Some("use configured skill"));

    // Discovered Skill metadata is prelude data, never policy.
    assert!(!prompt.contains("DISCOVERED_METADATA"));
    assert!(!prompt.contains("HIDDEN_BODY"));
    let catalog = segments
        .iter()
        .find(|segment| segment.source() == INJECTED_SOURCE_SKILLS_CATALOG)
        .expect("skill catalogue traced");
    assert!(catalog.content.contains("DISCOVERED_METADATA"));
    assert!(!catalog.content.contains("HIDDEN_BODY"));
}
