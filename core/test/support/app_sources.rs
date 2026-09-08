use std::path::{Path, PathBuf};

/// Resolve test inputs by declared source ownership, never by filesystem fallback.
pub fn app_dir(relative: &str) -> PathBuf {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let lock: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repository.join("packaging/apps.lock.json"))
            .expect("App source lock"),
    )
    .expect("valid App source lock");
    let id = relative.replace('/', "-");
    let migrated = lock["apps"].as_array().expect("locked App IDs");
    if !migrated.iter().any(|app| app.as_str() == Some(id.as_str())) {
        return repository.join("apps").join(relative);
    }
    let revision = lock["revision"].as_str().expect("locked App revision");
    let source = repository.join("build/app-sources").join(revision);
    assert!(
        source.is_dir(),
        "Run `python3 scripts/app_sources.py` from the OS repository before product integration tests"
    );
    for product in lock["products"].as_array().expect("locked products") {
        let product = source
            .join("products")
            .join(product.as_str().expect("product name"));
        let package: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(product.join("package.json")).expect("product package"),
        )
        .expect("valid product package");
        for app in package["apps"].as_array().expect("product App paths") {
            let app = app.as_str().expect("App path");
            if app.strip_prefix("apps/") == Some(relative) {
                let directory = product.join(app);
                assert!(
                    directory.is_dir(),
                    "locked App source missing: {directory:?}"
                );
                return directory;
            }
        }
    }
    panic!("App `{id}` is declared migrated but missing from the locked product packages");
}
