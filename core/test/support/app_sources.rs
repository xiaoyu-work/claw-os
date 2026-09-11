use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(serde::Deserialize)]
struct SourceLock {
    version: u32,
    revision: String,
    products: Vec<String>,
    #[serde(default)]
    capabilities: Vec<String>,
    apps: Vec<String>,
}

fn valid_name(value: &str) -> bool {
    value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Resolve test inputs by declared source ownership, never by filesystem fallback.
pub fn app_dir(relative: &str) -> PathBuf {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    app_dir_in(repository, relative)
}

fn read_lock(repository: &Path) -> SourceLock {
    let lock: SourceLock = serde_json::from_str(
        &std::fs::read_to_string(repository.join("packaging/apps.lock.json"))
            .expect("App source lock"),
    )
    .expect("valid App source lock");
    assert!(
        lock.version == 1
            && lock.revision.len() == 40
            && lock
                .revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "App fixtures require a version-1 lock with a full Git revision"
    );
    assert!(
        !lock.products.is_empty() && !lock.apps.is_empty(),
        "empty App source lock"
    );
    for values in [&lock.products, &lock.capabilities, &lock.apps] {
        assert!(
            values.iter().all(|name| valid_name(name))
                && values.iter().collect::<BTreeSet<_>>().len() == values.len(),
            "invalid or duplicate App source lock identifiers"
        );
    }
    assert!(
        !lock
            .products
            .iter()
            .any(|name| lock.capabilities.contains(name)),
        "duplicate source group across products and capabilities"
    );
    lock
}

fn source_cache(repository: &Path, lock: &SourceLock) -> PathBuf {
    let source = repository.join("build/app-sources").join(&lock.revision);
    assert!(
        source.is_dir(),
        "Run `python3 scripts/app_sources.py` from the OS repository before App integration tests"
    );
    source
}

#[allow(dead_code)]
pub fn source_root() -> PathBuf {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    source_cache(repository, &read_lock(repository))
}

pub fn app_dir_in(repository: &Path, relative: &str) -> PathBuf {
    assert!(
        relative.split('/').all(valid_name),
        "invalid App fixture path"
    );
    let lock = read_lock(repository);
    let id = relative.replace('/', "-");
    assert!(
        lock.apps.contains(&id),
        "App fixture is not in the source lock: {id}"
    );
    let source = source_cache(repository, &lock);
    let mut apps = BTreeMap::new();
    for (root, kind, names) in [
        ("products", "product", &lock.products),
        (
            "capabilities",
            "shared-capability-client",
            &lock.capabilities,
        ),
    ] {
        for name in names {
            let group = source.join(root).join(name);
            let package: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(group.join("package.json")).unwrap_or_else(|error| {
                    panic!("locked source package missing: {group:?}: {error}")
                }),
            )
            .expect("valid source package");
            let declared_kind = package
                .get("kind")
                .map_or(Some("product"), |value| value.as_str());
            assert_eq!(declared_kind, Some(kind), "locked source kind mismatch");
            if root == "capabilities" {
                assert!(
                    !package
                        .as_object()
                        .expect("source package object")
                        .keys()
                        .any(|key| key.starts_with("native") || key == "extension"),
                    "shared capability clients cannot declare native product assets"
                );
            }
            let canonical_group = group.canonicalize().expect("locked source directory");
            assert!(
                canonical_group
                    .starts_with(source.join(root).canonicalize().expect("source kind root")),
                "locked source package escapes its declared kind"
            );
            for app in package["apps"].as_array().expect("source App paths") {
                let app = app.as_str().expect("App path");
                let layout = app.strip_prefix("apps/").expect("App source layout");
                assert!(
                    layout.split('/').all(valid_name),
                    "invalid App source layout"
                );
                let directory = group.join(app);
                let canonical = directory.canonicalize().unwrap_or_else(|error| {
                    panic!("locked App source missing: {directory:?}: {error}")
                });
                assert!(
                    canonical.starts_with(&canonical_group),
                    "App source escapes its package"
                );
                let manifest: serde_json::Value = serde_json::from_str(
                    &std::fs::read_to_string(directory.join("app.json"))
                        .expect("locked App manifest"),
                )
                .expect("valid locked App manifest");
                let app_id = layout.replace('/', "-");
                assert_eq!(
                    manifest["id"].as_str(),
                    Some(app_id.as_str()),
                    "App identity/layout mismatch"
                );
                assert!(
                    apps.insert(app_id, directory).is_none(),
                    "duplicate locked App identity"
                );
            }
        }
    }
    assert_eq!(
        apps.keys().collect::<BTreeSet<_>>(),
        lock.apps.iter().collect::<BTreeSet<_>>(),
        "App identities are missing from or unexpected in the locked source packages"
    );
    apps.remove(&id).expect("locked App source")
}
