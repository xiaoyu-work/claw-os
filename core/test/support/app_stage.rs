use std::path::{Path, PathBuf};
use std::process::Command;

fn pinned_source(source: &Path) -> PathBuf {
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
    assert_eq!(git(&["rev-parse", "HEAD"]), lock["revision"].as_str().unwrap());
    let dirty = git(&["status", "--porcelain", "--untracked-files=all"]);
    assert!(
        dirty.is_empty(),
        "locked App fixture cache is modified: {dirty}"
    );
    PathBuf::from(git(&["rev-parse", "--show-toplevel"]))
}

pub fn app(source: &Path, kind: &str, group: &str, app_id: &str, root: &Path) -> PathBuf {
    let pinned = pinned_source(source);
    let source_kind = match kind {
        "product" => "products",
        "capability" => "capabilities",
        _ => panic!("unknown App fixture source kind"),
    };
    let layout = source
        .strip_prefix(pinned.join(source_kind).join(group).join("apps"))
        .expect("App fixture belongs to its declared source group");
    let output = Command::new("python3")
        .arg(pinned.join("tools/stage.py"))
        .args([group, "--kind", kind, "--apps", app_id, "--root"])
        .arg(root)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!([app_id])
    );
    let app = root.join("usr/lib/cos/apps").join(layout);
    assert_eq!(
        std::fs::read(app.join("app.json")).unwrap(),
        std::fs::read(source.join("app.json")).unwrap()
    );
    app
}

pub fn python_runtime(source: &Path, root: &Path) -> PathBuf {
    let pinned = pinned_source(source);
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let destination = root.join("usr/lib/cos/python");
    let output = Command::new("python3")
        .arg(pinned.join("tools/stage.py"))
        .args(["--shared", "--root"])
        .arg(root)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<PathBuf>(&output.stdout).unwrap(),
        destination
    );
    for relative in [
        "claw-os-sdk/python/src/claw_os_sdk",
        "cos-runtime/python/src/cos_runtime",
    ] {
        let source = repository.join(relative);
        let target = destination.join(source.file_name().unwrap());
        assert!(!target.exists(), "OS fixture runtime was already staged");
        let output = Command::new("cp")
            .arg("-a")
            .arg(&source)
            .arg(target)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    assert!(!root.join("usr/lib/cos/apps/_shared").exists());
    assert!(!root.join("usr/lib/cos/apps/gateway/_shared").exists());
    destination
}
