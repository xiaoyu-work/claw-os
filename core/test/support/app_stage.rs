use std::path::{Path, PathBuf};
use std::process::Command;

pub fn capability(source: &Path, group: &str, app_id: &str, root: &Path) -> PathBuf {
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .arg("-C")
            .arg(source)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let lock: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repository.join("packaging/apps.lock.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        git(&["rev-parse", "HEAD"]),
        lock["revision"].as_str().unwrap()
    );
    let dirty = git(&["status", "--porcelain", "--untracked-files=all"]);
    assert!(
        dirty.is_empty(),
        "locked App fixture cache is modified: {dirty}"
    );
    let pinned = PathBuf::from(git(&["rev-parse", "--show-toplevel"]));
    let output = Command::new("python3")
        .arg(pinned.join("tools/stage.py"))
        .args([group, "--kind", "capability", "--apps", app_id, "--root"])
        .arg(root)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!([app_id])
    );
    let app = root.join("usr/lib/cos/apps").join(app_id);
    assert_eq!(
        std::fs::read(app.join("app.json")).unwrap(),
        std::fs::read(source.join("app.json")).unwrap()
    );
    app
}

pub fn shared_python(apps: &Path) -> PathBuf {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let destination = apps.join("_shared");
    let output = Command::new("cp")
        .arg("-a")
        .arg(repository.join("apps/_shared"))
        .arg(&destination)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    destination
}
