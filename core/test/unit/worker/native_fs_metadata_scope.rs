use super::*;

#[test]
fn metadata_root_names_are_literal_and_cannot_escape_or_normalize() {
    for relative in ["", "Projects/review", "任意/批准"] {
        assert!(
            MetadataRoot::validate_relative(Path::new(relative)).is_ok(),
            "{relative:?}"
        );
    }
    for relative in [
        "/outside",
        "../outside",
        "Projects/../Other",
        "Projects/./review",
        "Projects//review",
        "Projects/",
        "Projects/*",
        "Projects/?",
        "Projects/[ab]",
        "Projects/\0",
    ] {
        assert!(
            MetadataRoot::validate_relative(Path::new(relative)).is_err(),
            "{relative:?}"
        );
    }
}

#[test]
fn metadata_root_name_limits_fail_closed_without_truncating_the_scope() {
    let allowed_depth = std::iter::repeat_n("a", 64).collect::<Vec<_>>().join("/");
    let excessive_depth = std::iter::repeat_n("a", 65).collect::<Vec<_>>().join("/");
    assert!(MetadataRoot::validate_relative(Path::new(&allowed_depth)).is_ok());
    assert!(MetadataRoot::validate_relative(Path::new(&excessive_depth)).is_err());
    assert!(MetadataRoot::validate_relative(Path::new(&"a".repeat(4096))).is_ok());
    assert!(MetadataRoot::validate_relative(Path::new(&"a".repeat(4097))).is_err());
}

struct DirectoryFixture(PathBuf);

impl DirectoryFixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let base = std::env::current_dir().unwrap().join("build");
        std::fs::create_dir_all(&base).unwrap();
        let path = base.join(format!(
            "metadata-anchor-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        let home = path.join("home");
        std::fs::create_dir_all(home.join("Projects/review")).unwrap();
        std::fs::write(
            home.join("Projects/review/leaf"),
            b"retained original bytes",
        )
        .unwrap();
        Self(path)
    }

    fn home(&self) -> PathBuf {
        self.0.join("home")
    }

    fn source(&self, relative: &str) -> MetadataRoot {
        let home = self.home();
        let owner = std::fs::metadata(&home).unwrap().uid();
        MetadataRoot::pin_as_owner(owner, &home, Path::new(relative)).unwrap()
    }
}

impl Drop for DirectoryFixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn revalidate_test_directory(source: &MetadataRoot) -> Result<(), String> {
    // Only the credential/NSS boundary is omitted; the same FD walk and latch run.
    source.revalidate_with(|| {
        MetadataRoot::pin_as_owner(source.owner, &source.home_path, &source.relative)
    })
}

#[test]
fn metadata_root_rename_fences_retained_handles_and_clones_after_path_restore() {
    use std::io::Read;
    let fixture = DirectoryFixture::new();
    let home = fixture.home();
    let source = fixture.source("Projects/review");
    let cloned = source.try_clone().unwrap();
    let directory = source.root.try_clone().unwrap();
    let mut file = File::open(source.path().join("leaf")).unwrap();
    let entries = std::fs::read_dir(source.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    revalidate_test_directory(&source).unwrap();
    std::fs::rename(home.join("Projects"), home.join("detached")).unwrap();
    std::fs::create_dir_all(home.join("Projects/review")).unwrap();
    std::fs::write(home.join("Projects/review/leaf"), b"unapproved replacement").unwrap();
    assert!(directory.metadata().unwrap().is_dir());
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"retained original bytes");
    assert!(entries.iter().any(|entry| entry == "leaf"));
    assert!(revalidate_test_directory(&source).is_err());
    std::fs::remove_dir_all(home.join("Projects")).unwrap();
    std::fs::rename(home.join("detached"), home.join("Projects")).unwrap();
    assert!(revalidate_test_directory(&source).is_err());
    assert!(revalidate_test_directory(&cloned).is_err());
    drop((file, directory));
}

#[test]
fn metadata_root_ancestor_permission_loss_fences_full_home_and_subroot_views() {
    use std::os::unix::fs::PermissionsExt;
    for relative in ["", "Projects/review"] {
        let fixture = DirectoryFixture::new();
        let home = fixture.home();
        let source = fixture.source(relative);
        let before = source.identities().unwrap();
        let directory = source.root.try_clone().unwrap();
        let file = File::open(home.join("Projects/review/leaf")).unwrap();
        let ancestor = if relative.is_empty() {
            home.clone()
        } else {
            home.join("Projects")
        };
        let original_mode = std::fs::metadata(&ancestor).unwrap().permissions();
        std::fs::set_permissions(&ancestor, std::fs::Permissions::from_mode(0o000)).unwrap();
        let invalid = revalidate_test_directory(&source).is_err();
        std::fs::set_permissions(&ancestor, original_mode).unwrap();
        assert!(
            invalid,
            "retained subroot FDs must not bypass owner ancestor access"
        );
        assert_eq!(source.identities().unwrap(), before);
        assert!(directory.metadata().unwrap().is_dir());
        assert!(file.metadata().unwrap().is_file());
        assert!(
            revalidate_test_directory(&source).is_err(),
            "restored permissions cannot renew fenced consent"
        );
        drop((file, directory));
    }
}

#[test]
fn metadata_root_symlink_ancestor_fences_retained_handles_after_restore() {
    use std::os::unix::fs::symlink;
    let fixture = DirectoryFixture::new();
    let home = fixture.home();
    let source = fixture.source("Projects/review");
    let directory = source.root.try_clone().unwrap();
    let file = File::open(source.path().join("leaf")).unwrap();
    std::fs::rename(home.join("Projects"), home.join("alias-target")).unwrap();
    symlink("alias-target", home.join("Projects")).unwrap();
    assert!(revalidate_test_directory(&source).is_err());
    std::fs::remove_file(home.join("Projects")).unwrap();
    std::fs::rename(home.join("alias-target"), home.join("Projects")).unwrap();
    assert!(directory.metadata().unwrap().is_dir());
    assert!(file.metadata().unwrap().is_file());
    assert!(revalidate_test_directory(&source).is_err());
    drop((file, directory));
}

#[test]
fn metadata_root_protected_alias_fences_retained_handles_and_clones_after_restore() {
    let fixture = DirectoryFixture::new();
    let home = fixture.home();
    let source = fixture.source("Projects/review");
    let cloned = source.try_clone().unwrap();
    let before = source.identities().unwrap();
    let directory = source.root.try_clone().unwrap();
    let file = File::open(source.path().join("leaf")).unwrap();
    std::os::unix::fs::symlink("Projects/review", home.join(".ssh")).unwrap();
    assert!(revalidate_test_directory(&source).is_err());
    std::fs::remove_file(home.join(".ssh")).unwrap();
    assert_eq!(source.identities().unwrap(), before);
    assert!(directory.metadata().unwrap().is_dir());
    assert!(file.metadata().unwrap().is_file());
    assert!(revalidate_test_directory(&source).is_err());
    assert!(revalidate_test_directory(&cloned).is_err());
    drop((file, directory));
}
