#[cfg(unix)]
mod unix_tests {
    use super::super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn tmpdir(label: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "cos-prov-fsec-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&p).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn lstat_reports_symlinks_without_following() {
        let dir = tmpdir("lstat");
        fs::write(dir.join("real"), b"hello").unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();
        let meta = lstat(&dir.join("link")).unwrap();
        assert!(meta.is_symlink);
        assert!(!meta.is_file);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn secure_location_rejects_world_writable_directory() {
        let dir = tmpdir("wworld");
        let child = dir.join("store");
        fs::create_dir_all(&child).unwrap();
        fs::set_permissions(&child, fs::Permissions::from_mode(0o777)).unwrap();
        let uid = effective_uid();
        let err = require_secure_location(&child, &[uid]).unwrap_err();
        assert!(format!("{err}").contains("group- or world-writable"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn secure_location_rejects_foreign_owner() {
        let dir = tmpdir("owner");
        // uid 0 is not the test user in CI; the check must refuse.
        let err = require_secure_location(&dir, &[u32::MAX - 1]).unwrap_err();
        assert!(format!("{err}").contains("not in the approved set"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_file_refuses_to_traverse_a_symlink() {
        let dir = tmpdir("nofollow");
        fs::write(dir.join("secret"), b"data").unwrap();
        std::os::unix::fs::symlink("secret", dir.join("alias")).unwrap();
        let handle = DirHandle::open(&dir).unwrap();
        assert!(handle.open_file("secret").is_ok());
        let err = handle.open_file("alias").unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ELOOP));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_file_refuses_symlinked_intermediate_directory() {
        let dir = tmpdir("nofollow-dir");
        fs::create_dir_all(dir.join("real")).unwrap();
        fs::write(dir.join("real/file"), b"x").unwrap();
        std::os::unix::fs::symlink("real", dir.join("alias")).unwrap();
        let handle = DirHandle::open(&dir).unwrap();
        assert!(handle.open_file("real/file").is_ok());
        assert!(handle.open_file("alias/file").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn entries_reports_raw_node_types() {
        let dir = tmpdir("entries");
        fs::write(dir.join("a"), b"a").unwrap();
        fs::create_dir(dir.join("b")).unwrap();
        std::os::unix::fs::symlink("a", dir.join("c")).unwrap();
        let handle = DirHandle::open(&dir).unwrap();
        let entries = handle.entries(None).unwrap();
        let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
        assert!(entries[0].1.is_file);
        assert!(entries[1].1.is_dir);
        assert!(entries[2].1.is_symlink);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_bounded_refuses_oversized_files() {
        let dir = tmpdir("bounded");
        fs::write(dir.join("big"), vec![0u8; 4096]).unwrap();
        let handle = DirHandle::open(&dir).unwrap();
        let fd = handle.open_file("big").unwrap();
        assert!(fd.read_bounded(10).is_err());
        assert_eq!(fd.read_bounded(8192).unwrap().len(), 4096);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn descriptor_survives_path_replacement() {
        let dir = tmpdir("toctou");
        fs::write(dir.join("f"), b"original").unwrap();
        let handle = DirHandle::open(&dir).unwrap();
        let fd = handle.open_file("f").unwrap();
        // Replace the path; the already-open descriptor still sees the
        // verified inode.
        fs::write(dir.join("f.new"), b"swapped").unwrap();
        fs::rename(dir.join("f.new"), dir.join("f")).unwrap();
        assert_eq!(fd.read_bounded(1024).unwrap(), b"original");
        let fresh = handle.open_file("f").unwrap();
        assert_eq!(fresh.read_bounded(1024).unwrap(), b"swapped");
        let _ = fs::remove_dir_all(&dir);
    }
}
