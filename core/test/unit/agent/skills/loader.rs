use super::*;
use std::fs;
use tempfile::tempdir;

fn write_skill(root: &Path, id: &str, contents: &str) {
    let dir = root.join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SKILL.md"), contents).unwrap();
}

fn minimal(id: &str) -> String {
    format!("---\nname: {id}\n---\nbody for {id}\n")
}

#[test]
fn missing_root_returns_empty() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().join("does-not-exist");
    let r = load_dir(&root, &LoadOptions::default());
    assert!(r.is_empty());
}

#[test]
fn empty_root_returns_empty() {
    let tmp = tempdir().unwrap();
    let r = load_dir(tmp.path(), &LoadOptions::default());
    assert!(r.is_empty());
}

#[test]
fn loads_single_skill() {
    let tmp = tempdir().unwrap();
    write_skill(tmp.path(), "pdf", &minimal("pdf"));
    let r = load_dir(tmp.path(), &LoadOptions::default());
    assert_eq!(r.loaded_count(), 1);
    let s = r.skills.get("pdf").unwrap();
    assert_eq!(s.id, "pdf");
    assert_eq!(s.manifest.name, "pdf");
    assert_eq!(s.body, "body for pdf\n");
    assert_eq!(s.body_bytes, "body for pdf\n".len());
    assert_eq!(s.manifest_path.file_name().unwrap(), "SKILL.md");
    assert_eq!(s.origin, SkillOrigin::Local);
}

#[test]
fn loads_multiple_skills_alphabetised() {
    let tmp = tempdir().unwrap();
    write_skill(tmp.path(), "zebra", &minimal("zebra"));
    write_skill(tmp.path(), "alpha", &minimal("alpha"));
    write_skill(tmp.path(), "mango", &minimal("mango"));
    let r = load_dir(tmp.path(), &LoadOptions::default());
    let names: Vec<&str> = r.skills.keys().map(String::as_str).collect();
    assert_eq!(names, vec!["alpha", "mango", "zebra"]);
}

#[test]
fn skill_id_is_directory_name_not_manifest_name() {
    let tmp = tempdir().unwrap();
    // Manifest declares a different "name" than the dir.
    write_skill(
        tmp.path(),
        "dirname",
        "---\nname: human-friendly-label\n---\n",
    );
    let r = load_dir(tmp.path(), &LoadOptions::default());
    let s = r.skills.get("dirname").expect("loaded by dir name");
    assert_eq!(s.id, "dirname");
    assert_eq!(s.manifest.name, "human-friendly-label");
}

#[test]
fn missing_skill_md_recorded_in_errors() {
    let tmp = tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("orphan")).unwrap();
    let r = load_dir(tmp.path(), &LoadOptions::default());
    assert_eq!(r.loaded_count(), 0);
    assert!(r.errors.contains_key("orphan"));
    let msg = &r.errors["orphan"];
    assert!(msg.contains("SKILL.md"), "got: {msg}");
}

#[test]
fn malformed_manifest_recorded_in_errors_and_others_still_load() {
    let tmp = tempdir().unwrap();
    write_skill(tmp.path(), "good", &minimal("good"));
    write_skill(tmp.path(), "bad", "no frontmatter at all\n");
    let r = load_dir(tmp.path(), &LoadOptions::default());
    assert!(r.skills.contains_key("good"));
    assert!(r.errors.contains_key("bad"));
}

#[test]
fn red_teaming_disabled_by_default() {
    let tmp = tempdir().unwrap();
    write_skill(tmp.path(), "red-teaming", &minimal("red-teaming"));
    write_skill(tmp.path(), "red-teaming-attacker", &minimal("rt-attacker"));
    write_skill(tmp.path(), "godmode", &minimal("godmode"));
    let r = load_dir(tmp.path(), &LoadOptions::default());
    assert!(r.skills.is_empty());
    assert!(r.disabled.contains_key("red-teaming"));
    assert!(r.disabled.contains_key("red-teaming-attacker"));
    assert!(r.disabled.contains_key("godmode"));
}

#[test]
fn yuanbao_disabled_by_default() {
    let tmp = tempdir().unwrap();
    write_skill(tmp.path(), "yuanbao", &minimal("yuanbao"));
    write_skill(tmp.path(), "yuanbao-tools", &minimal("yuanbao-tools"));
    let r = load_dir(tmp.path(), &LoadOptions::default());
    assert!(r.disabled.contains_key("yuanbao"));
    assert!(r.disabled.contains_key("yuanbao-tools"));
}

#[test]
fn deny_match_is_case_insensitive() {
    let tmp = tempdir().unwrap();
    write_skill(tmp.path(), "Godmode", &minimal("Godmode"));
    let r = load_dir(tmp.path(), &LoadOptions::default());
    assert!(r.disabled.contains_key("Godmode"));
}

#[test]
fn deny_prefix_does_not_match_unrelated_skills() {
    let tmp = tempdir().unwrap();
    // `red-team-strategy` would match `red-team-` if we used a
    // raw startswith; ensure the boundary check holds: only
    // `red-teaming` (exact) and `red-teaming-...` are denied.
    write_skill(
        tmp.path(),
        "red-team-strategy",
        &minimal("red-team-strategy"),
    );
    let r = load_dir(tmp.path(), &LoadOptions::default());
    assert!(r.skills.contains_key("red-team-strategy"));
    assert!(!r.disabled.contains_key("red-team-strategy"));
}

#[test]
fn empty_deny_list_loads_everything() {
    let tmp = tempdir().unwrap();
    write_skill(tmp.path(), "godmode", &minimal("godmode"));
    let opts = LoadOptions {
        deny_list: Vec::new(),
        ..LoadOptions::default()
    };
    let r = load_dir(tmp.path(), &opts);
    assert!(r.skills.contains_key("godmode"));
    assert!(r.disabled.is_empty());
}

#[test]
fn oversize_manifest_rejected() {
    let tmp = tempdir().unwrap();
    let huge = format!(
        "---\nname: huge\ndescription: |\n  {}\n---\n",
        "x".repeat(2048)
    );
    write_skill(tmp.path(), "huge", &huge);
    let opts = LoadOptions {
        max_manifest_bytes: 100,
        ..LoadOptions::default()
    };
    let r = load_dir(tmp.path(), &opts);
    assert!(r.skills.is_empty());
    let msg = r.errors.get("huge").expect("error recorded");
    assert!(msg.contains("exceeds"), "got: {msg}");
}

#[test]
fn dotfiles_and_files_at_root_ignored() {
    let tmp = tempdir().unwrap();
    write_skill(tmp.path(), ".hidden", &minimal("hidden"));
    fs::write(tmp.path().join("loose.md"), "stray file").unwrap();
    write_skill(tmp.path(), "ok", &minimal("ok"));
    let r = load_dir(tmp.path(), &LoadOptions::default());
    assert_eq!(r.loaded_count(), 1);
    assert!(r.skills.contains_key("ok"));
}

#[test]
fn skill_md_as_directory_errors_out() {
    let tmp = tempdir().unwrap();
    let dir = tmp.path().join("weird");
    fs::create_dir_all(dir.join("SKILL.md")).unwrap();
    let r = load_dir(tmp.path(), &LoadOptions::default());
    let msg = r.errors.get("weird").expect("error");
    assert!(msg.contains("not a regular file"), "got: {msg}");
}

#[cfg(unix)]
#[test]
fn symlinked_skill_md_is_rejected() {
    use std::os::unix::fs::symlink;

    let tmp = tempdir().unwrap();
    let outside = tmp.path().join("outside.md");
    fs::write(&outside, minimal("linked")).unwrap();
    let dir = tmp.path().join("linked");
    fs::create_dir_all(&dir).unwrap();
    symlink(&outside, dir.join("SKILL.md")).unwrap();

    let result = load_dir(tmp.path(), &LoadOptions::default());

    assert!(result.errors["linked"].contains("not a regular file"));
}

#[test]
fn missing_name_field_recorded_in_errors() {
    let tmp = tempdir().unwrap();
    write_skill(tmp.path(), "noname", "---\ndescription: x\n---\n");
    let r = load_dir(tmp.path(), &LoadOptions::default());
    let msg = r.errors.get("noname").expect("error");
    assert!(msg.contains("name"), "got: {msg}");
}

#[test]
fn allowed_tools_round_trip() {
    let tmp = tempdir().unwrap();
    write_skill(
        tmp.path(),
        "with-tools",
        "---\nname: with-tools\nallowed-tools: [cos_fs, cos_exec]\n---\n",
    );
    let r = load_dir(tmp.path(), &LoadOptions::default());
    let s = &r.skills["with-tools"];
    assert_eq!(s.manifest.allowed_tools, vec!["cos_fs", "cos_exec"]);
}

#[test]
fn load_default_does_not_panic_on_missing_dir() {
    // Don't mess with COS_DATA_DIR (parallel-test safety); just
    // make sure the function survives the default path being
    // absent. If the system happens to have skills, we still
    // get a valid LoadResult.
    let _ = load_default();
}

#[test]
fn layered_load_includes_builtin_and_user_skills() {
    let system = tempdir().unwrap();
    let user = tempdir().unwrap();
    write_skill(system.path(), "claw-os", &minimal("claw-os"));
    write_skill(user.path(), "my-skill", &minimal("my-skill"));

    let result = load_layered(system.path(), user.path(), &LoadOptions::default());

    assert_eq!(result.loaded_count(), 2);
    assert_eq!(result.skills["claw-os"].origin, SkillOrigin::BuiltIn);
    assert_eq!(result.skills["my-skill"].origin, SkillOrigin::User);
}

#[test]
fn layered_load_prevents_user_shadowing_builtin_skill() {
    let system = tempdir().unwrap();
    let user = tempdir().unwrap();
    write_skill(system.path(), "claw-os", &minimal("vendor-claw-os"));
    write_skill(user.path(), "claw-os", &minimal("user-claw-os"));

    let result = load_layered(system.path(), user.path(), &LoadOptions::default());

    assert_eq!(result.loaded_count(), 1);
    assert_eq!(result.skills["claw-os"].manifest.name, "vendor-claw-os");
    assert_eq!(result.skills["claw-os"].origin, SkillOrigin::BuiltIn);
    assert!(result.errors.contains_key("claw-os (user shadow)"));
}

#[test]
fn metadata_only_load_drops_instruction_bodies() {
    let system = tempdir().unwrap();
    let user = tempdir().unwrap();
    write_skill(system.path(), "claw-os", &minimal("claw-os"));
    let options = LoadOptions {
        include_body: false,
        ..LoadOptions::default()
    };

    let result = load_layered(system.path(), user.path(), &options);

    assert_eq!(result.skills["claw-os"].manifest.name, "claw-os");
    assert!(result.skills["claw-os"].body.is_empty());
    assert_eq!(
        result.skills["claw-os"].body_bytes,
        "body for claw-os\n".len()
    );
    let hydrated = hydrate(&result.skills["claw-os"], &LoadOptions::default()).unwrap();
    assert_eq!(hydrated.body, "body for claw-os\n");
}
