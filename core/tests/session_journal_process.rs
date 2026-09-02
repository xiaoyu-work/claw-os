//! Cross-process behaviour of the session event journal.
//!
//! The unit tests prove the chain rules inside one process. The
//! properties in this file only exist *between* processes, so they are
//! tested with a real second process and a real filesystem:
//!
//! * two daemons must not both hold the writer lease;
//! * a bracket a crashed daemon left open must be found by the next one,
//!   from disk alone, and must keep refusing its own replay across any
//!   number of restarts and from a *different* process;
//! * only an explicit operator resolution may end that refusal;
//! * a journal the daemon cannot write to must refuse mutations while
//!   remaining readable;
//! * deleting the committed head must preserve the chain rather than
//!   erase it;
//! * a session that predates the journal must be adopted rather than
//!   fail closed forever.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::Command;

use cos::session::journal::{
    self, EventSource, JournalEvent, Label, MutationStart, Partition, RecoverySource, Resolution,
};

/// Set by the parent when it re-executes this binary as the "crashed
/// daemon" half of the restart tests.
const CHILD_DATA_DIR: &str = "COS_JOURNAL_TEST_DATA_DIR";
const CHILD_ROUTE: &str = "system.package.install";
const CHILD_REQUEST: &str = "req-crash-1";

fn temp_data_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

fn journal_root(data_dir: &Path) -> PathBuf {
    data_dir.join("journal")
}

fn partition() -> Partition {
    Partition::Owner(unsafe { libc::geteuid() } as u32)
}

fn owner_uid() -> u32 {
    unsafe { libc::geteuid() as u32 }
}

/// Run one `#[ignore]`d test in a fresh process, with `COS_DATA_DIR`
/// pointed at `data_dir`.
fn run_child(name: &str, data_dir: &Path) -> std::process::Output {
    Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", "--ignored", "--nocapture", name])
        .env("COS_DATA_DIR", data_dir)
        .env(CHILD_DATA_DIR, data_dir)
        .output()
        .expect("spawn child")
}

fn probe() -> JournalEvent {
    JournalEvent::ToolStarted {
        turn: 0,
        tool: Label::new("cos_todo"),
        tool_use_id: Label::new("t-1"),
        known: true,
    }
}

#[test]
fn a_second_process_cannot_take_the_writer_lease() {
    let data_dir = temp_data_dir();
    std::env::set_var("COS_DATA_DIR", data_dir.path());
    let root = journal_root(data_dir.path());
    std::fs::create_dir_all(&root).expect("root");
    let lock = root.join("writer.lock");
    std::fs::write(&lock, b"").expect("lock sentinel");

    // A real second process holding the same advisory lock is exactly
    // what a second daemon would be.
    let mut holder = Command::new("/usr/bin/flock")
        .args([
            "-x",
            lock.to_str().expect("lock path"),
            "-c",
            "printf held; sleep 10",
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn flock");

    wait_for_held(&mut holder);

    let error = journal::lease().expect_err("the second writer must be refused");
    assert!(
        error.to_string().contains("already holds"),
        "unexpected error: {error}"
    );

    let _ = holder.kill();
    let _ = holder.wait();
    std::env::remove_var("COS_DATA_DIR");
}

fn wait_for_held(child: &mut std::process::Child) {
    use std::io::Read;

    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut buffer = [0u8; 4];
    let mut read = 0;
    while read < buffer.len() {
        match stdout.read(&mut buffer[read..]) {
            Ok(0) => panic!("flock exited before taking the lock"),
            Ok(n) => read += n,
            Err(error) => panic!("failed to read from flock: {error}"),
        }
    }
    assert_eq!(&buffer, b"held");
}

#[test]
fn an_unresolved_mutation_refuses_its_replay_across_processes_and_restarts() {
    let data_dir = temp_data_dir();

    // A first client starts a mutation whose effect is unknown, then
    // dies with its bracket open. Everything up to the resolution runs
    // in child processes: the writer lease is exclusive per machine, so
    // "a different process" has to mean a process this one is not
    // holding the lease against.
    let output = run_child(
        "child_opens_a_bracket_then_exits_without_closing_it",
        data_dir.path(),
    );
    assert!(
        output.status.success(),
        "child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // A different process — different pid, different start time — must
    // recognise the same durable operation from disk alone, and must
    // keep recognising it across further restarts.
    for _ in 0..3 {
        let retry = run_child("child_retries_the_same_operation", data_dir.path());
        assert!(
            retry.status.success(),
            "retry child failed: {}",
            String::from_utf8_lossy(&retry.stderr)
        );
    }

    // Now this process takes over as the daemon.
    std::env::set_var("COS_DATA_DIR", data_dir.path());
    let report = journal::startup_recovery(RecoverySource::DaemonStart)
        .expect("recovery runs on a chain another process wrote");
    assert_eq!(report.orphans.len(), 1, "{report:?}");
    let orphan = report.orphans[0].clone();
    assert_eq!(orphan.route, CHILD_ROUTE);
    assert_eq!(
        orphan.opened_in_epoch, 1,
        "the crashed daemon held the first epoch"
    );
    assert!(report.quarantined.is_empty());

    let projection = journal::projection::build(&partition(), owner_uid()).expect("projection");
    assert_eq!(
        projection
            .mutations
            .iter()
            .find(|entry| entry.operation == orphan.operation)
            .expect("in the timeline")
            .status,
        "orphaned",
        "a crash must never look like a success"
    );
    assert!(journal::replays_unresolved(
        &partition(),
        CHILD_ROUTE,
        CHILD_REQUEST
    ));

    // Only an explicit resolution ends the refusal, and it grants
    // nothing: it records what a human concluded.
    journal::resolve_mutation(
        &partition(),
        owner_uid(),
        &orphan.operation,
        Resolution::Abandoned,
        0,
    )
    .expect("root records the outcome");
    assert!(!journal::replays_unresolved(
        &partition(),
        CHILD_ROUTE,
        CHILD_REQUEST
    ));

    let report = journal::startup_recovery(RecoverySource::DaemonStart).expect("recovery");
    assert!(
        report.orphans.is_empty(),
        "the resolution survives a restart"
    );
    let projection = journal::projection::build(&partition(), owner_uid()).expect("projection");
    let entry = projection
        .mutations
        .iter()
        .find(|entry| entry.operation == orphan.operation)
        .expect("still in the timeline");
    assert_eq!(entry.status, "resolved-abandoned");
    std::env::remove_var("COS_DATA_DIR");
}

/// The "crashed daemon". Opens a bracket, never closes it, exits.
#[test]
#[ignore = "spawned by an_unresolved_mutation_refuses_its_replay_across_processes_and_restarts"]
fn child_opens_a_bracket_then_exits_without_closing_it() {
    let data_dir = std::env::var(CHILD_DATA_DIR).expect("parent must name the data dir");
    std::env::set_var("COS_DATA_DIR", &data_dir);

    let bracket = journal::begin_mutation(MutationStart {
        partition: partition(),
        owner_uid: owner_uid(),
        route: CHILD_ROUTE,
        request_key: CHILD_REQUEST,
        grant: None,
        session_mutation: None,
        context_ingest: false,
    })
    .expect("bracket opens");
    assert_eq!(bracket.start_seq(), 1);
    std::mem::forget(bracket);
    // Leave the way a killed daemon does: no unwinding, no close.
    std::process::exit(0);
}

/// A different process retrying the same durable operation.
#[test]
#[ignore = "spawned by an_unresolved_mutation_refuses_its_replay_across_processes_and_restarts"]
fn child_retries_the_same_operation() {
    let data_dir = std::env::var(CHILD_DATA_DIR).expect("parent must name the data dir");
    std::env::set_var("COS_DATA_DIR", &data_dir);

    journal::startup_recovery(RecoverySource::DaemonStart).expect("recovery");
    assert!(
        journal::replays_unresolved(&partition(), CHILD_ROUTE, CHILD_REQUEST),
        "a new process must recognise the same operation from disk"
    );
    // An unrelated operation is not a replay, and proceeds normally.
    assert!(!journal::replays_unresolved(
        &partition(),
        CHILD_ROUTE,
        "some-other-operation"
    ));
    journal::begin_mutation(MutationStart {
        partition: partition(),
        owner_uid: owner_uid(),
        route: CHILD_ROUTE,
        request_key: "some-other-operation",
        grant: None,
        session_mutation: None,
        context_ingest: false,
    })
    .expect("an unrelated operation proceeds")
    .commit()
    .expect("and closes");
    std::process::exit(0);
}

#[test]
fn a_journal_the_daemon_cannot_write_refuses_mutations_but_still_reads() {
    if unsafe { libc::geteuid() } == 0 {
        // Mode bits do not constrain root, so this property cannot be
        // demonstrated here.
        return;
    }
    let data_dir = temp_data_dir();
    std::env::set_var("COS_DATA_DIR", data_dir.path());

    journal::record(&partition(), owner_uid(), EventSource::Kernel, probe()).expect("first append");

    let lease = journal::lease().expect("lease");
    let anchor = lease
        .load_anchor(&partition(), owner_uid())
        .expect("anchor");
    let chain = anchor.active_path(&journal_root(data_dir.path()), &partition());
    let restore = std::fs::metadata(&chain).unwrap().permissions();
    std::fs::set_permissions(
        &chain,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o400),
    )
    .expect("make the chain unwritable");

    let error = journal::begin_mutation(MutationStart {
        partition: partition(),
        owner_uid: owner_uid(),
        route: "system.service.control",
        request_key: "req-ro",
        grant: None,
        session_mutation: None,
        context_ingest: false,
    })
    .expect_err("a mutation that cannot be recorded must be refused");
    assert!(matches!(error, journal::JournalError::Io { .. }), "{error}");

    // Diagnostics stay available while the chain cannot be extended.
    let projection = journal::projection::build(&partition(), owner_uid()).expect("still readable");
    assert!(projection.health.is_verified(), "{:?}", projection.health);

    std::fs::set_permissions(&chain, restore).expect("restore");
    std::env::remove_var("COS_DATA_DIR");
}

#[test]
fn deleting_the_committed_head_preserves_the_chain_and_fails_mutations_closed() {
    let data_dir = temp_data_dir();
    std::env::set_var("COS_DATA_DIR", data_dir.path());

    journal::record(&partition(), owner_uid(), EventSource::Kernel, probe()).expect("append");
    let lease = journal::lease().expect("lease");
    let anchor = lease
        .load_anchor(&partition(), owner_uid())
        .expect("anchor");
    let chain_path = anchor.active_path(&journal_root(data_dir.path()), &partition());
    let bytes = std::fs::read(&chain_path).expect("chain");
    assert!(!bytes.is_empty());

    std::fs::remove_file(partition().anchor_path(&journal_root(data_dir.path())))
        .expect("delete the head");

    let error = journal::begin_mutation(MutationStart {
        partition: partition(),
        owner_uid: owner_uid(),
        route: "system.service.control",
        request_key: "req-headless",
        grant: None,
        session_mutation: None,
        context_ingest: false,
    })
    .expect_err("a missing head must fail closed");
    assert!(
        matches!(error, journal::JournalError::AnchorMissing { .. }),
        "{error}"
    );
    assert_eq!(
        std::fs::read(&chain_path).expect("chain"),
        bytes,
        "the committed evidence must survive for an operator"
    );

    // Recovery quarantines it rather than adopting or erasing it.
    let report = journal::startup_recovery(RecoverySource::DaemonStart).expect("recovery");
    assert_eq!(report.quarantined, vec![partition().key()]);
    assert_eq!(std::fs::read(&chain_path).expect("chain"), bytes);
    std::env::remove_var("COS_DATA_DIR");
}

#[test]
fn a_session_that_predates_the_journal_is_adopted_read_only() {
    let data_dir = temp_data_dir();
    std::env::set_var("COS_DATA_DIR", data_dir.path());

    // A legacy session directory: conversation and mutation logs, and no
    // chain at all.
    let sid = "ses_0000000000001_000000000001";
    let legacy = data_dir.path().join("sessions").join(sid);
    std::fs::create_dir_all(legacy.join("files")).expect("legacy session");
    std::fs::write(
        legacy.join("meta.json"),
        serde_json::json!({
            "id": sid,
            "purpose": "legacy",
            "status": "done",
            "created_at": "2024-01-01T00:00:00Z"
        })
        .to_string(),
    )
    .expect("meta");
    std::fs::write(
        legacy.join("turns.jsonl"),
        "{\"seq\":0,\"at\":\"2024-01-01T00:00:00Z\",\"role\":\"user\",\"content\":\"hi\"}\n",
    )
    .expect("turns");

    // Recovery must not fail on it, and must not invent history for it.
    let report = journal::startup_recovery(RecoverySource::DaemonStart).expect("recovery runs");
    assert_eq!(
        report.partitions, 0,
        "a legacy session has no chain to verify"
    );
    assert!(report.quarantined.is_empty());

    // Work that starts now opens a chain from genesis rather than
    // pretending the legacy records were signed.
    let session = Partition::Session(sid.parse().expect("session id"));
    let bracket = journal::begin_mutation(MutationStart {
        partition: session.clone(),
        owner_uid: owner_uid(),
        route: "system.service.control",
        request_key: "req-legacy",
        grant: None,
        session_mutation: Some(0),
        context_ingest: false,
    })
    .expect("a legacy session may still be journalled from now on");
    assert_eq!(bracket.start_seq(), 1);
    bracket.commit().expect("commit");

    let projection = journal::projection::build(&session, owner_uid()).expect("projection");
    assert!(projection.health.is_verified());
    assert_eq!(projection.mutations.len(), 1);
    assert_eq!(projection.mutations[0].status, "committed");
    std::env::remove_var("COS_DATA_DIR");
}
