use super::*;
use std::sync::Once;

static INIT: Once = Once::new();

fn test_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("cos-test-shared-{}", std::process::id()));
    INIT.call_once(|| {
        let _ = fs::create_dir_all(&dir);
    });
    dir
}

#[test]
fn write_and_read() {
    let path = test_dir().join("filelock-wr.json");
    write_locked(&path, r#"{"hello":"world"}"#).unwrap();
    let data = read_locked(&path).unwrap().unwrap();
    assert_eq!(data, r#"{"hello":"world"}"#);
}

#[test]
fn read_nonexistent() {
    let path = test_dir().join("filelock-nonexistent.json");
    assert!(read_locked(&path).unwrap().is_none());
}

#[test]
fn append_creates_and_appends() {
    let path = test_dir().join("filelock-append.jsonl");
    let _ = fs::remove_file(&path);
    append_locked(&path, "line1").unwrap();
    append_locked(&path, "line2").unwrap();
    let data = fs::read_to_string(&path).unwrap();
    assert_eq!(data.lines().count(), 2);
}

#[test]
fn write_atomic_no_leftover_tmp() {
    let path = test_dir().join("filelock-atomic.json");
    write_locked(&path, "first").unwrap();
    write_locked(&path, "second").unwrap();
    let data = read_locked(&path).unwrap().unwrap();
    assert_eq!(data, "second");
    assert!(!path.with_extension("tmp").exists());
}

#[test]
fn update_locked_creates_when_missing() {
    let path = test_dir().join("filelock-update-create.txt");
    let _ = fs::remove_file(&path);
    let _ = fs::remove_file(lock_sentinel_path(&path));
    update_locked::<_, std::convert::Infallible>(&path, |cur| {
        assert!(cur.is_none(), "expected missing file -> None");
        Ok("created".to_string())
    })
    .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "created");
}

#[test]
fn update_locked_serializes_concurrent_rmw() {
    // Two threads each increment a counter in a JSON file 200 times.
    // With the pre-fix read_locked -> write_locked pattern these would
    // race and the final count would be < 400. With update_locked the
    // RMW is atomic so we must see exactly 400.
    let path = test_dir().join("filelock-update-rmw.json");
    let _ = fs::remove_file(&path);
    let _ = fs::remove_file(lock_sentinel_path(&path));
    fs::write(&path, "0").unwrap();

    let increments_per_thread = 200;
    let path_a = path.clone();
    let path_b = path.clone();
    let h1 = std::thread::spawn(move || {
        for _ in 0..increments_per_thread {
            update_locked::<_, std::convert::Infallible>(&path_a, |cur| {
                let n: u64 = cur.as_deref().unwrap_or("0").trim().parse().unwrap();
                Ok((n + 1).to_string())
            })
            .unwrap();
        }
    });
    let h2 = std::thread::spawn(move || {
        for _ in 0..increments_per_thread {
            update_locked::<_, std::convert::Infallible>(&path_b, |cur| {
                let n: u64 = cur.as_deref().unwrap_or("0").trim().parse().unwrap();
                Ok((n + 1).to_string())
            })
            .unwrap();
        }
    });
    h1.join().unwrap();
    h2.join().unwrap();

    let final_value: u64 = fs::read_to_string(&path).unwrap().trim().parse().unwrap();
    assert_eq!(
        final_value,
        2 * increments_per_thread,
        "lost updates: concurrent RMW must serialize"
    );
}

#[test]
fn update_locked_propagates_transform_error() {
    let path = test_dir().join("filelock-update-err.txt");
    let _ = fs::remove_file(&path);
    let _ = fs::remove_file(lock_sentinel_path(&path));
    let err = update_locked::<_, &'static str>(&path, |_| Err("boom"));
    match err {
        Err(UpdateLockError::Transform(msg)) => assert_eq!(msg, "boom"),
        other => panic!("expected Transform error, got {other:?}"),
    }
    assert!(
        !path.exists(),
        "file must not be created when closure fails"
    );
}
