use super::*;
use std::fs;
use tempfile::tempdir;

fn write_skill(root: &Path, id: &str, description: &str, body: &str) {
    let dir = root.join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {id}\ndescription: {description}\n---\n{body}\n"),
    )
    .unwrap();
}

fn layered<'a>(system: &'a Path, user: &'a Path) -> super::super::loader::LoadResult {
    super::super::loader::load_layered(system, user, &super::super::loader::LoadOptions::default())
}

#[test]
fn prompt_catalog_contains_metadata_but_not_skill_body() {
    let system = tempdir().unwrap();
    let user = tempdir().unwrap();
    write_skill(
        system.path(),
        "claw-os",
        "Operate the Claw system layer",
        "SECRET_INSTRUCTION_BODY",
    );
    let load = layered(system.path(), user.path());

    let catalog = render_prompt_catalog(&load).expect("catalog");

    assert!(catalog.contains("\"id\": \"claw-os\""));
    assert!(catalog.contains("Operate the Claw system layer"));
    assert!(catalog.contains("\"source\": \"builtin\""));
    assert!(catalog.contains("command=read"));
    assert!(catalog.contains("command=resource"));
    assert!(!catalog.contains("SECRET_INSTRUCTION_BODY"));
}

#[test]
fn instruction_disclosure_returns_body_and_resource_names_only() {
    let system = tempdir().unwrap();
    let user = tempdir().unwrap();
    write_skill(
        system.path(),
        "claw-os",
        "Operate the Claw system layer",
        "Use the matching playbook.",
    );
    fs::write(
        system.path().join("claw-os").join("diagnostics.md"),
        "PRIVATE_RESOURCE_CONTENT",
    )
    .unwrap();
    let load = layered(system.path(), user.path());

    let value = disclose_instructions(&load.skills["claw-os"]).expect("instructions");

    assert_eq!(value["disclosure_level"], "instructions");
    assert!(value["instructions"]
        .as_str()
        .is_some_and(|body| body.contains("matching playbook")));
    assert_eq!(value["resources"][0]["path"], "diagnostics.md");
    assert!(!value.to_string().contains("PRIVATE_RESOURCE_CONTENT"));
}

#[test]
fn resource_disclosure_reads_one_requested_file() {
    let system = tempdir().unwrap();
    let user = tempdir().unwrap();
    write_skill(system.path(), "claw-os", "Claw", "Read details as needed.");
    fs::create_dir_all(system.path().join("claw-os").join("playbooks")).unwrap();
    fs::write(
        system
            .path()
            .join("claw-os")
            .join("playbooks")
            .join("network.md"),
        "NETWORK_PLAYBOOK",
    )
    .unwrap();
    let load = layered(system.path(), user.path());

    let value =
        disclose_resource(&load.skills["claw-os"], "playbooks/network.md").expect("resource");

    assert_eq!(value["disclosure_level"], "resource");
    assert_eq!(value["path"], "playbooks/network.md");
    assert_eq!(value["content"], "NETWORK_PLAYBOOK");
}

#[test]
fn resource_disclosure_rejects_escape_and_absolute_paths() {
    let system = tempdir().unwrap();
    let user = tempdir().unwrap();
    write_skill(system.path(), "claw-os", "Claw", "Body");
    let skill = &layered(system.path(), user.path()).skills["claw-os"];

    assert!(disclose_resource(skill, "../secret").is_err());
    assert!(disclose_resource(skill, "/etc/passwd").is_err());
    assert!(disclose_resource(skill, "SKILL.md").is_err());
    assert!(disclose_resource(skill, ".env").is_err());
    assert!(disclose_resource(skill, ".git/config").is_err());
}

#[cfg(unix)]
#[test]
fn resource_disclosure_rejects_symlinks() {
    use std::os::unix::fs::symlink;

    let system = tempdir().unwrap();
    let user = tempdir().unwrap();
    let outside = tempdir().unwrap();
    write_skill(system.path(), "claw-os", "Claw", "Body");
    fs::write(outside.path().join("secret.md"), "SECRET").unwrap();
    symlink(
        outside.path().join("secret.md"),
        system.path().join("claw-os").join("linked.md"),
    )
    .unwrap();
    let load = layered(system.path(), user.path());

    let error =
        disclose_resource(&load.skills["claw-os"], "linked.md").expect_err("symlink rejected");
    assert!(error.contains("symlink"));
}

#[test]
fn builtin_skill_skips_third_party_guard() {
    let system = tempdir().unwrap();
    let user = tempdir().unwrap();
    write_skill(system.path(), "claw-os", "Claw", "Body");
    fs::File::create(system.path().join("claw-os").join("large.bin"))
        .unwrap()
        .set_len(6 * 1024 * 1024)
        .unwrap();
    let load = layered(system.path(), user.path());

    assert!(disclose_instructions(&load.skills["claw-os"]).is_ok());
}

#[test]
fn user_skill_still_uses_third_party_guard() {
    let system = tempdir().unwrap();
    let user = tempdir().unwrap();
    write_skill(user.path(), "community", "Community", "Body");
    fs::File::create(user.path().join("community").join("large.bin"))
        .unwrap()
        .set_len(6 * 1024 * 1024)
        .unwrap();
    let load = layered(system.path(), user.path());

    let error = disclose_instructions(&load.skills["community"]).expect_err("guard should block");
    assert!(error.contains("explicit operator review"));
}

#[test]
fn oversized_instruction_body_is_rejected() {
    let system = tempdir().unwrap();
    let user = tempdir().unwrap();
    write_skill(
        system.path(),
        "oversized",
        "Large",
        &"x".repeat(MAX_INSTRUCTION_BYTES + 1),
    );
    let load = layered(system.path(), user.path());

    let metadata = catalog_page(&load, 0, 1);
    assert_eq!(metadata[0]["disclosable"], false);
    let error =
        disclose_instructions(&load.skills["oversized"]).expect_err("body cap should apply");

    assert!(error.contains("exceeds disclosure cap"));
}
