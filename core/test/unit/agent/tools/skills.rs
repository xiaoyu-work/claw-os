use super::*;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap().filter_map(Result::ok) {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let meta = fs::symlink_metadata(&from).unwrap();
        if meta.is_dir() {
            copy_tree(&from, &to);
        } else if meta.is_file() {
            fs::copy(&from, &to).unwrap();
        }
    }
}

fn write_builtin(root: &Path, body: &str) {
    let dir = root.join("claw-os");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: claw-os\ndescription: Claw system operations\n---\n{body}\n"),
    )
    .unwrap();
    fs::write(dir.join("network.md"), "NETWORK_RESOURCE").unwrap();
    crate::test_env::sign_test_package(&dir, crate::provenance::PackageKind::Skill, "claw-os");
}

#[tokio::test]
async fn list_discloses_metadata_without_body() {
    let system = tempdir().unwrap();
    let user = tempdir().unwrap();
    write_builtin(system.path(), "INSTRUCTION_BODY");
    let tool = SkillDisclosure::with_roots(system.path(), user.path());

    let result = tool.exec(json!({"command": "list"})).await;

    assert!(!result.is_error);
    assert!(result.content.contains("source=skills_catalog"));
    assert!(result.content.contains("trust=extension-metadata"));
    assert!(result.content.contains("Claw system operations"));
    assert!(!result.content.contains("INSTRUCTION_BODY"));
}

#[tokio::test]
async fn read_then_resource_progressively_discloses_content() {
    let system = tempdir().unwrap();
    let user = tempdir().unwrap();
    write_builtin(system.path(), "INSTRUCTION_BODY");
    let tool = SkillDisclosure::with_roots(system.path(), user.path());

    let instructions = tool.exec(json!({"command": "read", "id": "claw-os"})).await;
    assert!(!instructions.is_error);
    assert!(instructions.content.contains("INSTRUCTION_BODY"));
    assert!(instructions.content.contains("network.md"));
    assert!(!instructions.content.contains("NETWORK_RESOURCE"));

    let resource = tool
        .exec(json!({
            "command": "resource",
            "id": "claw-os",
            "path": "network.md"
        }))
        .await;
    assert!(!resource.is_error);
    assert!(resource.content.contains("NETWORK_RESOURCE"));

    let usage = fs::read_to_string(user.path().join(".skills-usage.jsonl")).unwrap();
    let records = usage
        .lines()
        .map(|line| serde_json::from_str::<UsageRecord>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 2);
    assert_eq!(records[1].resource_path.as_deref(), Some("network.md"));
}

#[tokio::test]
async fn unknown_command_is_an_error() {
    let system = tempdir().unwrap();
    let user = tempdir().unwrap();
    let tool = SkillDisclosure::with_roots(system.path(), user.path());

    let result = tool.exec(json!({"command": "everything"})).await;

    assert!(result.is_error);
    assert!(result.content.contains("unknown command"));
}

#[tokio::test]
async fn list_supports_bounded_pagination() {
    let system = tempdir().unwrap();
    let user = tempdir().unwrap();
    for id in ["alpha", "beta", "gamma"] {
        let dir = system.path().join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {id}\ndescription: {id}\n---\nBODY_{id}\n"),
        )
        .unwrap();
        crate::test_env::sign_test_package(&dir, crate::provenance::PackageKind::Skill, id);
    }
    let tool = SkillDisclosure::with_roots(system.path(), user.path());

    let result = tool
        .exec(json!({"command": "list", "offset": 1, "limit": 1}))
        .await;

    assert!(!result.is_error);
    assert!(result.content.contains("\"id\": \"beta\""));
    assert!(!result.content.contains("\"id\": \"alpha\""));
    assert!(result.content.contains("\"next_offset\": 2"));
}

#[tokio::test]
async fn user_skill_instructions_remain_inside_untrusted_boundary() {
    let system = tempdir().unwrap();
    let user = tempdir().unwrap();
    let dir = user.path().join("community");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        "---\nname: community\ndescription: Community skill\n---\nDO_SOMETHING\n",
    )
    .unwrap();
    crate::test_env::sign_test_package(&dir, crate::provenance::PackageKind::Skill, "community");
    let tool = SkillDisclosure::with_roots(system.path(), user.path());

    let result = tool
        .exec(json!({"command": "read", "id": "community"}))
        .await;

    assert!(!result.is_error);
    assert!(result.content.contains("source=skill_instructions:"));
    assert!(result.content.contains("trust=extension-metadata"));
    assert!(result.content.contains("DO_SOMETHING"));
}

#[tokio::test]
async fn overridden_system_root_remains_local_and_untrusted() {
    let system = tempdir().unwrap();
    let user = tempdir().unwrap();
    write_builtin(system.path(), "OVERRIDDEN_SYSTEM_INSTRUCTIONS");
    let tool = SkillDisclosure::with_paths(
        system.path().to_path_buf(),
        user.path().to_path_buf(),
        user.path().join("usage.jsonl"),
        SkillOrigin::Local,
    );

    let result = tool
        .exec(json!({"command": "read", "id": "claw-os"}))
        .await;

    assert!(!result.is_error);
    assert!(result.content.contains("source=skill_instructions:"));
    assert!(result.content.contains("trust=extension-metadata"));
    assert!(result.content.contains("OVERRIDDEN_SYSTEM_INSTRUCTIONS"));
}

#[tokio::test]
async fn repository_builtin_skill_is_readable_progressively() {
    // The repository checkout is not an approved package root, so the
    // in-tree skill is staged into a signed copy first — the same shape
    // the Debian package produces under /usr/lib/cos/skills.
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repository root")
        .join("skills")
        .join("claw-os");
    let staged = tempdir().unwrap();
    let system_root = staged.path().to_path_buf();
    copy_tree(&source, &system_root.join("claw-os"));
    crate::test_env::sign_test_package(
        &system_root.join("claw-os"),
        crate::provenance::PackageKind::Skill,
        "claw-os",
    );
    let user = tempdir().unwrap();
    let tool = SkillDisclosure::with_roots(&system_root, user.path());

    let list = tool.exec(json!({"command": "list"})).await;
    assert!(!list.is_error);
    assert!(list.content.contains("\"id\": \"claw-os\""));
    assert!(!list.content.contains("System Diagnosis Protocol"));

    let instructions = tool.exec(json!({"command": "read", "id": "claw-os"})).await;
    assert!(!instructions.is_error);
    assert!(instructions.content.contains("Claw System Agent"));
    assert!(instructions.content.contains("diagnostics.md"));
    assert!(!instructions.content.contains("Establish the symptom"));

    let resource = tool
        .exec(json!({
            "command": "resource",
            "id": "claw-os",
            "path": "diagnostics.md"
        }))
        .await;
    assert!(!resource.is_error);
    assert!(resource.content.contains("Establish the symptom"));
}
