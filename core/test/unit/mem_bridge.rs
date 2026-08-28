use super::*;

/// A stand-in that never authorizes, so these tests exercise argument
/// validation without touching the owner's store.
struct DenyAll;

impl MemoryAuthority for DenyAll {
    fn allow(&self, _verb: Verb, _scope: Scope) -> Result<(), String> {
        Err("denied".to_string())
    }
}

#[test]
fn remember_requires_json_arg() {
    let err = remember(&DenyAll, &[]).unwrap_err();
    assert!(err.contains("missing --json"));
}

#[test]
fn list_rejects_unknown_flag() {
    let err = list(&DenyAll, &["--bogus".into()]).unwrap_err();
    assert!(err.contains("unexpected"));
}

#[test]
fn search_requires_query() {
    let err = search(&DenyAll, &[]).unwrap_err();
    assert!(err.contains("missing <query>"));
}

#[test]
fn forget_requires_exactly_one_target() {
    let err = forget(&DenyAll, &[]).unwrap_err();
    assert!(err.contains("exactly one"));
    let err = forget(
        &DenyAll,
        &[
            "--source".into(),
            "expense-tracker".into(),
            "--row".into(),
            "1".into(),
        ],
    )
    .unwrap_err();
    assert!(err.contains("exactly one"));
}

#[test]
fn run_unknown_command() {
    let err = run("nope", &[]).unwrap_err();
    assert!(err.contains("unknown internal memory command"));
}

#[test]
fn a_launch_authority_answers_from_its_own_capability_set() {
    let caps = CapSet::from_caps(vec![Cap::new(
        Verb::MEMORY_WRITE,
        Scope::self_ref("expense-tracker"),
    )]);
    let authority = LaunchAuthority::new(caps);
    assert!(authority
        .allow(Verb::MEMORY_WRITE, Scope::self_ref("expense-tracker"))
        .is_ok());
    // Another App's source, and a read the launch was never granted,
    // are refused by the same set the kernel would have consulted.
    assert!(authority
        .allow(Verb::MEMORY_WRITE, Scope::self_ref("calendar"))
        .is_err());
    assert!(authority
        .allow(Verb::MEMORY_READ, Scope::self_ref("expense-tracker"))
        .is_err());
}

/// A store seeded with one row that belongs to somebody else.
#[cfg(unix)]
fn foreign_row() -> (tempdir::Dir, i64) {
    let data = tempdir::Dir::new("memory-oracle");
    std::env::set_var("COS_DATA_DIR", data.path());
    let db = open_db().expect("open db");
    let store = app_memory::open_default_store();
    let outcome = runtime()
        .block_on(app_memory::remember(
            &db,
            store.as_ref(),
            AppMemoryEntry {
                source: "neighbour".to_string(),
                text: "the neighbour's secret".to_string(),
                kind: None,
                entity_id: None,
                tags: Vec::new(),
                link: None,
            },
            false,
        ))
        .expect("seed row");
    (data, outcome.row_id)
}

#[cfg(unix)]
mod tempdir {
    use std::path::{Path, PathBuf};

    pub struct Dir(PathBuf);

    impl Dir {
        pub fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "cos-{label}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&path).expect("temp dir");
            Self(path)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(unix)]
#[test]
fn a_foreign_row_reads_exactly_like_a_missing_one() {
    let (_data, row_id) = foreign_row();
    let authority = LaunchAuthority::new(CapSet::from_caps(vec![
        Cap::new(Verb::MEMORY_READ, Scope::self_ref("expense-tracker")),
        Cap::new(Verb::MEMORY_WRITE, Scope::self_ref("expense-tracker")),
    ]));

    let foreign = show(&authority, &[row_id.to_string()]).expect("show foreign");
    let absent = show(&authority, &[(row_id + 100_000).to_string()]).expect("show absent");
    assert_eq!(foreign, absent, "the row id space is an existence oracle");
    assert_eq!(foreign["row"], Value::Null);

    let forgot_foreign = forget(&authority, &["--row".into(), row_id.to_string()])
        .expect("forget foreign");
    let forgot_absent = forget(
        &authority,
        &["--row".into(), (row_id + 100_000).to_string()],
    )
    .expect("forget absent");
    assert_eq!(forgot_foreign["removed"], forgot_absent["removed"]);
    assert_eq!(forgot_foreign["removed"], serde_json::json!(0));

    // And the refused delete really did not happen.
    let db = open_db().expect("reopen");
    assert!(app_memory::show(&db, row_id).expect("lookup").is_some());
    std::env::remove_var("COS_DATA_DIR");
}

#[cfg(unix)]
#[test]
fn the_owning_source_still_reads_and_deletes_its_own_row() {
    let (_data, row_id) = foreign_row();
    let authority = LaunchAuthority::new(CapSet::from_caps(vec![
        Cap::new(Verb::MEMORY_READ, Scope::self_ref("neighbour")),
        Cap::new(Verb::MEMORY_WRITE, Scope::self_ref("neighbour")),
    ]));

    let seen = show(&authority, &[row_id.to_string()]).expect("show own");
    assert_eq!(seen["row"]["source"], "neighbour");

    let removed = forget(&authority, &["--row".into(), row_id.to_string()]).expect("forget own");
    assert_eq!(removed["removed"], serde_json::json!(1));
    let db = open_db().expect("reopen");
    assert!(app_memory::show(&db, row_id).expect("lookup").is_none());
    std::env::remove_var("COS_DATA_DIR");
}
