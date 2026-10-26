use super::*;
use std::os::unix::fs::symlink;

fn owner_file(store: &Store, path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(name.as_ptr(), store.owner, 65534) }, 0);
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

pub(super) fn fixture() -> (tempfile::TempDir, Store) {
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "run this ignored test in a private root test process"
    );
    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o711)).unwrap();
    let home = temp.path().join("home");
    fs::create_dir(&home).unwrap();
    let name = std::ffi::CString::new(home.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(name.as_ptr(), 65534, 65534) }, 0);
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    let root = temp.path().join("state");
    private(&root).unwrap();
    private(&root.join("sessions")).unwrap();
    private(&root.join("uploads")).unwrap();
    (
        temp,
        Store {
            root,
            owner: 65534,
            home,
        },
    )
}

#[test]
#[ignore = "requires root; uses only generated temporary files and no radio"]
fn nearby_files_pin_sources_enforce_the_exact_limit_and_snapshot_read_only() {
    let (_temp, store) = fixture();
    let source = store.home.join("a.txt");
    owner_file(&store, &source, b"abc");
    assert_eq!(store.source(source.to_str().unwrap()).unwrap(), source);
    let window = uuid::Uuid::new_v4().to_string();
    let transfer = uuid::Uuid::new_v4().to_string();
    let (assigned, metadata) = store
        .snapshot(
            &window,
            &transfer,
            &[Source {
                path: source.clone(),
                name: "a.txt".into(),
            }],
            false,
        )
        .unwrap();
    assert_eq!(assigned, vec![format!("/data/outgoing/{transfer}/a.txt")]);
    assert_eq!(metadata[0].bytes, Some(3));
    let staged = store
        .root
        .join("sessions")
        .join(&window)
        .join("outgoing")
        .join(&transfer)
        .join("a.txt");
    assert_eq!(fs::read(&staged).unwrap(), b"abc");
    assert_eq!(fs::metadata(staged).unwrap().mode() & 0o222, 0);
    store.retire_send(&window, &transfer).unwrap();
    let file = File::options().write(true).open(&source).unwrap();
    file.set_len(MAX_FILE_BYTES).unwrap();
    store.source(source.to_str().unwrap()).unwrap();
    file.set_len(MAX_FILE_BYTES + 1).unwrap();
    assert!(store.source(source.to_str().unwrap()).is_err());
    symlink("/etc/passwd", store.home.join("foreign")).unwrap();
    assert!(store
        .source(store.home.join("foreign").to_str().unwrap())
        .is_err());
    store.retire_session(&window).unwrap();
}

#[test]
#[ignore = "requires root; uses only generated temporary files and no radio"]
fn nearby_import_never_overwrites_and_reports_partial_owner_files() {
    let (_temp, store) = fixture();
    let window = uuid::Uuid::new_v4().to_string();
    let transfer = uuid::Uuid::new_v4().to_string();
    let incoming = store.session(&window).unwrap().join("incoming");
    owner_file(&store, &incoming.join("a.txt"), b"abc");
    let files = vec![
        SharedFile {
            name: "a.txt".into(),
            bytes: Some(3),
        },
        SharedFile {
            name: "missing.txt".into(),
            bytes: Some(3),
        },
    ];
    let failed = match store.import(
        &window,
        &transfer,
        &files,
        &["a.txt".into(), "missing.txt".into()],
    ) {
        Err(error) => error,
        Ok(_) => panic!("missing second file must not report complete"),
    };
    assert_eq!(failed.paths.len(), 1, "{}", failed.message);
    assert_eq!(fs::read(&failed.paths[0]).unwrap(), b"abc");
    assert_eq!(fs::metadata(&failed.paths[0]).unwrap().uid(), store.owner);
    owner_file(&store, &incoming.join("a.txt"), b"new");
    assert!(store
        .import(&window, &transfer, &files[..1], &["a.txt".into()])
        .is_err());
    assert_eq!(fs::read(&failed.paths[0]).unwrap(), b"abc");
    let wrong_size = vec![SharedFile {
        name: "a.txt".into(),
        bytes: Some(4),
    }];
    let received = match store.import(
        &window,
        &uuid::Uuid::new_v4().to_string(),
        &wrong_size,
        &["a.txt".into()],
    ) {
        Ok(received) => received,
        Err(error) => panic!("{}", error.message),
    };
    assert_eq!(
        received.bytes, 3,
        "sender size is advisory, persisted bytes are observed"
    );
}

#[test]
#[ignore = "requires root; uses only generated temporary files and no radio"]
fn nearby_upload_deletion_recovers_without_hiding_orphaned_bytes() {
    let (_temp, store) = fixture();
    let id = uuid::Uuid::new_v4().to_string();
    fs::write(store.upload_path(&id).unwrap(), b"abc").unwrap();
    store
        .save_upload(&UploadView {
            id: id.clone(),
            name: "a.txt".into(),
            bytes: 3,
            written: 3,
            ready: true,
        })
        .unwrap();
    fs::rename(
        store.upload_path(&id).unwrap().with_extension("json"),
        store.upload_path(&id).unwrap().with_extension("deleting"),
    )
    .unwrap();
    store.recover_uploads().unwrap();
    assert!(store.uploads().unwrap().is_empty());
    assert!(!store.upload_path(&id).unwrap().exists());
    let orphan = uuid::Uuid::new_v4().to_string();
    fs::write(store.upload_path(&orphan).unwrap(), b"uncommitted").unwrap();
    store.recover_uploads().unwrap();
    assert!(!store.upload_path(&orphan).unwrap().exists());
}
