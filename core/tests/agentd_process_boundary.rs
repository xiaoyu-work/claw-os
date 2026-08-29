//! Real process-boundary checks for the `agentd` worker split.
//!
//! These spawn an actual `claw-agentd` child through the same
//! `agentd::spawn` path `clawd` uses and inspect the kernel's view of
//! it — `/proc/<pid>/status`, `/proc/<pid>/fd`, `/proc/<pid>/environ` —
//! rather than trusting a helper's return value.
//!
//! The suite runs unprivileged, so the uid/gid drop itself is a no-op
//! here (the child is already the test account). What it does verify on
//! any account is the part that is identical either way: descriptor
//! isolation, the rebuilt environment, `PR_SET_NO_NEW_PRIVS`, the grant
//! binding, and fail-closed behaviour on a protocol or identity
//! mismatch. `agentd::spawn`'s unit tests cover the ordering and
//! verification of the privileged drop.

#![cfg(all(unix, target_os = "linux"))]

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use cos::agentd::grant::{GrantClaims, GrantSigner, SignedGrant, GRANT_AUDIENCE, GRANT_VERSION};
use cos::agentd::protocol::{
    self, Assignment, BrokerFrame, FrameReader, JobSpec, WorkerFrame, WorkerOutcome,
};
use cos::agentd::spawn::{self, SpawnedWorker, WorkerIdentity};
use tokio::io::{AsyncWriteExt, BufReader};

const WORKER_BIN: &str = env!("CARGO_BIN_EXE_claw-agentd");
const EXTENSION_HOST_BIN: &str = env!("CARGO_BIN_EXE_claw-extension-host");
/// Marker placed in the *parent's* environment. The worker rebuilds its
/// environment from an allowlist, so this must never appear in the
/// child.
const LEAK_MARKER: &str = "COS_AGENTD_TEST_BROKER_SECRET";

struct Harness {
    _home: tempfile::TempDir,
    _data: tempfile::TempDir,
    leaked_path: PathBuf,
    leaked_fd: i32,
    identity: WorkerIdentity,
}

impl Drop for Harness {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.leaked_fd);
        }
        std::env::remove_var(LEAK_MARKER);
        std::env::remove_var("COS_EXTENSION_HOST_BIN");
        std::env::remove_var("COS_RUNTIME_DIR");
    }
}

fn harness() -> Option<Harness> {
    let uid = unsafe { libc::getuid() } as u32;
    if uid == 0 {
        // Running as root would exercise the privileged drop, but the
        // harness cannot then supply a second account to drop *to*.
        return None;
    }
    let identity = spawn::resolve_identity(uid).ok()?;

    let home = tempfile::tempdir().ok()?;
    let data = tempfile::tempdir().ok()?;
    std::env::set_var("COS_AGENTD_BIN", WORKER_BIN);
    std::env::set_var("COS_EXTENSION_HOST_BIN", EXTENSION_HOST_BIN);
    std::env::set_var("COS_DATA_DIR", data.path());
    std::env::set_var("COS_RUNTIME_DIR", data.path().join("run"));
    std::env::set_var(LEAK_MARKER, "broker-only-value");

    // A descriptor the broker holds without `O_CLOEXEC`, standing in
    // for a queue lock or audit handle. The worker must not inherit it.
    let leaked_path = home.path().join("broker-held.bin");
    let mut file = std::fs::File::create(&leaked_path).ok()?;
    file.write_all(b"broker").ok()?;
    drop(file);
    let c_path = std::ffi::CString::new(leaked_path.to_string_lossy().as_bytes()).ok()?;
    let leaked_fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY) };
    assert!(leaked_fd >= 0, "failed to open the leak probe");

    Some(Harness {
        _home: home,
        _data: data,
        leaked_path,
        leaked_fd,
        identity,
    })
}

fn assignment(
    grant: SignedGrant,
    protocol_version: u32,
    task_id: &str,
    owner_uid: u32,
    home: &std::path::Path,
) -> Assignment {
    Assignment {
        protocol: protocol_version,
        grant,
        job: JobSpec {
            id: task_id.to_string(),
            prompt: "process boundary probe".to_string(),
            context: None,
            branch_context: None,
            session_id: None,
            max_turns: Some(1),
            owner_uid,
            owner_home: home.to_string_lossy().into_owned(),
        },
        consent_context: cos::caps::ConsentContext::Attended,
        session: None,
        presence: None,
        extension: None,
    }
}

fn grant_for(
    signer: &GrantSigner,
    task_id: &str,
    owner: &WorkerIdentity,
    worker_pid: u32,
    start_time_ticks: Option<u64>,
) -> SignedGrant {
    let issued_at_ms = cos::agentd::grant::now_ms();
    signer.issue(GrantClaims {
        v: GRANT_VERSION,
        audience: GRANT_AUDIENCE.to_string(),
        broker_pid: std::process::id(),
        task_id: task_id.to_string(),
        session_id: None,
        owner_uid: owner.uid,
        client: cos::session::SessionClient::default(),
        presence: None,
        capability_generation: cos::agent::tools::exposure::capability_generation(
            &cos::caps::CapSet::new(),
        ),
        extension: None,
        owner_gid: owner.gid,
        worker_pid,
        worker_start_time_ticks: start_time_ticks,
        issued_at_ms,
        expires_at_ms: issued_at_ms + 120_000,
        routes: protocol::worker_routes(),
    })
}

async fn send(worker: &mut SpawnedWorker, frame: &BrokerFrame) {
    let encoded = protocol::encode(frame).expect("encode");
    worker
        .channel
        .write_all(encoded.as_bytes())
        .await
        .expect("write frame");
    worker.channel.flush().await.expect("flush");
}

fn proc_field(pid: u32, field: &str) -> Option<String> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix(field))
        .map(|value| value.trim().to_string())
}

fn open_descriptor_targets(pid: u32) -> Vec<PathBuf> {
    let mut targets = Vec::new();
    let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
        return targets;
    };
    for entry in entries.flatten() {
        if let Ok(target) = std::fs::read_link(entry.path()) {
            targets.push(target);
        }
    }
    targets
}

fn descriptor_flags(pid: u32, fd: i32) -> Option<u32> {
    let info = std::fs::read_to_string(format!("/proc/{pid}/fdinfo/{fd}")).ok()?;
    let encoded = info
        .lines()
        .find_map(|line| line.strip_prefix("flags:"))?
        .trim();
    u32::from_str_radix(encoded, 8).ok()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worker_inherits_no_broker_descriptor_environment_or_privilege() {
    let Some(harness) = harness() else {
        eprintln!("skipping: no usable unprivileged account for the worker harness");
        return;
    };
    let signer = GrantSigner::generate().expect("signer");
    let mut worker = spawn::spawn_worker(&harness.identity, "task-boundary").expect("spawn");
    let grant = grant_for(
        &signer,
        "task-boundary",
        &harness.identity,
        worker.pid,
        worker.start_time_ticks,
    );
    send(
        &mut worker,
        &BrokerFrame::Assign(Box::new(assignment(
            grant,
            protocol::PROTOCOL_VERSION,
            "task-boundary",
            harness.identity.uid,
            &harness.identity.home,
        ))),
    )
    .await;

    let SpawnedWorker {
        mut child,
        channel,
        pid,
        ..
    } = worker;
    let (reader, _writer) = channel.into_split();
    let mut frames = FrameReader::new(BufReader::new(reader));
    let hello = tokio::time::timeout(Duration::from_secs(30), frames.next_frame::<WorkerFrame>())
        .await
        .expect("handshake timed out")
        .expect("read handshake")
        .expect("a handshake frame");
    let WorkerFrame::Hello(hello) = hello else {
        panic!("the worker must handshake before anything else");
    };

    assert_eq!(hello.protocol, protocol::PROTOCOL_VERSION);
    assert_eq!(hello.pid, pid);
    assert_eq!(hello.uid, harness.identity.uid);
    assert_eq!(hello.euid, harness.identity.uid);
    assert_ne!(hello.uid, 0, "the agent runtime must never run as root");
    assert!(hello.no_new_privs, "the worker reported no NNP");

    // Kernel's own view, not the worker's self-report.
    assert_eq!(
        proc_field(pid, "NoNewPrivs:").as_deref(),
        Some("1"),
        "PR_SET_NO_NEW_PRIVS is not set on the worker process"
    );
    assert_eq!(
        proc_field(pid, "Uid:")
            .and_then(|uid| uid.split_whitespace().next().map(str::to_string))
            .as_deref(),
        Some(harness.identity.uid.to_string().as_str())
    );

    let descriptors = open_descriptor_targets(pid);
    assert!(
        !descriptors.is_empty(),
        "expected to be able to inspect the worker's descriptors"
    );
    assert!(
        !descriptors.contains(&harness.leaked_path),
        "the worker inherited a broker-held descriptor: {descriptors:?}"
    );
    assert_ne!(
        descriptor_flags(pid, protocol::CHANNEL_FD).expect("inspect worker channel flags")
            & libc::O_CLOEXEC as u32,
        0,
        "the adopted worker channel must close across descendant exec"
    );

    let environ = std::fs::read(format!("/proc/{pid}/environ")).expect("read environ");
    let environ = String::from_utf8_lossy(&environ);
    assert!(
        !environ.contains(LEAK_MARKER),
        "the worker inherited the broker's environment"
    );
    // `/proc/<pid>/environ` exposes the exec-time environment block and may
    // retain a key after libc removes it. The worker unit test checks the live
    // environment; the descendant exec test checks that it is not propagated.
    assert!(!environ.contains(protocol::TASK_HINT_ENV));

    // The task still round-trips a result even with no provider
    // configured, which is what proves the queue's outcome now arrives
    // from outside the broker. Stream, progress and audit frames may
    // precede it.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    let mut saw_audit = false;
    loop {
        let frame = tokio::time::timeout_at(deadline, frames.next_frame::<WorkerFrame>())
            .await
            .expect("result timed out")
            .expect("read frame");
        match frame {
            Some(WorkerFrame::Result { task_id, outcome }) => {
                assert_eq!(task_id, "task-boundary");
                assert!(matches!(
                    *outcome,
                    WorkerOutcome::Ok(_) | WorkerOutcome::Error { .. }
                ));
                break;
            }
            Some(WorkerFrame::Audit { task_id, .. }) => {
                assert_eq!(task_id, "task-boundary");
                saw_audit = true;
            }
            Some(WorkerFrame::Stream { task_id, .. })
            | Some(WorkerFrame::Progress { task_id, .. })
            | Some(WorkerFrame::Heartbeat { task_id }) => {
                assert_eq!(task_id, "task-boundary");
            }
            other => panic!("expected a result frame, got {other:?}"),
        }
    }
    assert!(
        saw_audit,
        "the worker must forward its runtime audit to the broker"
    );

    let status = tokio::time::timeout(Duration::from_secs(30), child.wait())
        .await
        .expect("worker did not exit")
        .expect("wait");
    assert!(status.success(), "worker exited with {status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_task_is_reported_as_cancelled_by_the_worker() {
    let Some(harness) = harness() else {
        eprintln!("skipping: no usable unprivileged account for the worker harness");
        return;
    };
    let signer = GrantSigner::generate().expect("signer");
    let mut worker = spawn::spawn_worker(&harness.identity, "task-cancel").expect("spawn");
    let grant = grant_for(
        &signer,
        "task-cancel",
        &harness.identity,
        worker.pid,
        worker.start_time_ticks,
    );
    send(
        &mut worker,
        &BrokerFrame::Assign(Box::new(assignment(
            grant,
            protocol::PROTOCOL_VERSION,
            "task-cancel",
            harness.identity.uid,
            &harness.identity.home,
        ))),
    )
    .await;
    // Queued behind the assignment, so the worker observes it as soon
    // as it starts watching the channel — whatever the run itself does,
    // the outcome must be a cancellation.
    send(
        &mut worker,
        &BrokerFrame::Cancel {
            task_id: "task-cancel".to_string(),
        },
    )
    .await;

    let SpawnedWorker {
        mut child, channel, ..
    } = worker;
    let (reader, _writer) = channel.into_split();
    let mut frames = FrameReader::new(BufReader::new(reader));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        let frame = tokio::time::timeout_at(deadline, frames.next_frame::<WorkerFrame>())
            .await
            .expect("cancellation timed out")
            .expect("read frame");
        match frame {
            Some(WorkerFrame::Result { task_id, outcome }) => {
                assert_eq!(task_id, "task-cancel");
                assert!(
                    matches!(*outcome, WorkerOutcome::Cancelled),
                    "a cancelled task must not report a normal outcome: {outcome:?}"
                );
                break;
            }
            Some(_) => continue,
            None => panic!("the worker closed its channel without reporting a cancellation"),
        }
    }

    let status = tokio::time::timeout(Duration::from_secs(30), child.wait())
        .await
        .expect("worker did not exit")
        .expect("wait");
    assert!(status.success(), "worker exited with {status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_supervisor_runs_a_task_end_to_end_through_a_real_worker() {
    use cos::agent::service::{JobStatus, Store};
    use cos::agentd::supervisor::{run_with_store, SupervisorConfig};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let Some(harness) = harness() else {
        eprintln!("skipping: no usable unprivileged account for the worker harness");
        return;
    };
    let queue = tempfile::tempdir().expect("tempdir");
    let owner_home = tempfile::tempdir().expect("tempdir");
    let store = Store::with_root(queue.path().to_path_buf()).expect("store");
    let job = store
        .submit(
            "supervisor end-to-end probe".to_string(),
            None,
            Some(1),
            Some(harness.identity.uid),
            // A config-less home keeps the probe hermetic: the worker
            // resolves the owner's config/credentials from here, so no
            // real provider is ever contacted.
            Some(owner_home.path().to_string_lossy().into_owned()),
        )
        .expect("submit");

    let shutdown = Arc::new(AtomicBool::new(false));
    let config = SupervisorConfig {
        poll: Duration::from_millis(50),
        max_workers: 1,
        lease: Duration::from_secs(120),
        heartbeat_grace: Duration::from_secs(60),
        ..SupervisorConfig::default()
    };
    let supervisor = tokio::spawn(run_with_store(config, shutdown.clone(), store.clone()));

    // The whole production sequence runs here: claim → spawn a real
    // `claw-agentd` → handshake → assignment → result → finish.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    let finished = loop {
        if let Some((_, current)) = store.locate(&job.id).expect("locate") {
            if matches!(
                current.status,
                JobStatus::Ok | JobStatus::Error | JobStatus::Cancelled
            ) {
                break current;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the supervisor never finished the task"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    shutdown.store(true, Ordering::SeqCst);
    let _ = tokio::time::timeout(Duration::from_secs(30), supervisor).await;

    // The task was executed by a separate process, not by this one.
    let worker_pid = finished.worker_pid.expect("a worker pid");
    assert_ne!(
        worker_pid,
        std::process::id(),
        "the task must be executed outside the supervising process"
    );
    assert!(finished.started_at.is_some());
    assert!(finished.finished_at.is_some());
    // With no provider configured for the owner, the honest outcome is
    // a provider failure reported *through the worker* — which is the
    // point: the queue's result now arrives from outside the broker.
    assert_eq!(finished.status, JobStatus::Error);
    let error = finished.error.unwrap_or_default();
    assert!(
        error.contains("provider"),
        "expected a provider failure from the worker, got: {error}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_root_owned_task_never_spawns_a_worker_or_initialises_a_provider() {
    use cos::agent::service::{JobStatus, Store};
    use cos::agentd::supervisor::{run_with_store, SupervisorConfig};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let Some(_harness) = harness() else {
        eprintln!("skipping: no usable unprivileged account for the worker harness");
        return;
    };
    let queue = tempfile::tempdir().expect("tempdir");
    let probe = tempfile::tempdir().expect("tempdir");
    // A stand-in worker image that records the fact it ran. Refusing a
    // root-owned task must happen before anything is executed, so this
    // marker must never appear.
    let marker = probe.path().join("worker-ran");
    let fake_worker = probe.path().join("claw-agentd");
    std::fs::write(
        &fake_worker,
        format!("#!/bin/sh\ntouch {}\n", marker.display()),
    )
    .expect("write probe");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_worker, std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
    }
    std::env::set_var("COS_AGENTD_BIN", &fake_worker);

    let store = Store::with_root(queue.path().to_path_buf()).expect("store");
    let job = store
        .submit(
            "root-owned probe".to_string(),
            None,
            Some(1),
            Some(0),
            Some("/root".to_string()),
        )
        .expect("submit");

    let shutdown = Arc::new(AtomicBool::new(false));
    let config = SupervisorConfig {
        poll: Duration::from_millis(50),
        max_workers: 1,
        ..SupervisorConfig::default()
    };
    let supervisor = tokio::spawn(run_with_store(config, shutdown.clone(), store.clone()));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let finished = loop {
        if let Some((_, current)) = store.locate(&job.id).expect("locate") {
            if matches!(current.status, JobStatus::Error | JobStatus::Cancelled) {
                break current;
            }
            assert_ne!(
                current.status,
                JobStatus::Ok,
                "a root-owned task must never succeed"
            );
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the supervisor never resolved the root-owned task"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    shutdown.store(true, Ordering::SeqCst);
    let _ = tokio::time::timeout(Duration::from_secs(30), supervisor).await;

    std::env::set_var("COS_AGENTD_BIN", WORKER_BIN);

    assert_eq!(finished.status, JobStatus::Error);
    let error = finished.error.unwrap_or_default();
    assert!(
        error.contains("non-root"),
        "the refusal must say what to do instead: {error}"
    );
    assert!(
        !marker.exists(),
        "a worker process was started for a root-owned task"
    );
    // Never rebound to a worker, because none was ever forked.
    assert_eq!(finished.worker_pid, Some(std::process::id()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_grant_without_the_approval_route_refuses_to_start() {
    let Some(harness) = harness() else {
        eprintln!("skipping: no usable unprivileged account for the worker harness");
        return;
    };
    let signer = GrantSigner::generate().expect("signer");
    let mut worker = spawn::spawn_worker(&harness.identity, "task-noapproval").expect("spawn");
    // Every route except permission mediation. Without it the worker
    // could run but would be unable to reach consent for any denied
    // capability, so it must refuse rather than start half-blind.
    let routes: Vec<String> = protocol::worker_routes()
        .into_iter()
        .filter(|route| route != protocol::ROUTE_APPROVAL)
        .collect();
    let issued_at_ms = cos::agentd::grant::now_ms();
    let grant = signer.issue(GrantClaims {
        v: GRANT_VERSION,
        audience: GRANT_AUDIENCE.to_string(),
        broker_pid: std::process::id(),
        task_id: "task-noapproval".to_string(),
        session_id: None,
        owner_uid: harness.identity.uid,
        client: cos::session::SessionClient::default(),
        presence: None,
        capability_generation: cos::agent::tools::exposure::capability_generation(
            &cos::caps::CapSet::new(),
        ),
        extension: None,
        owner_gid: harness.identity.gid,
        worker_pid: worker.pid,
        worker_start_time_ticks: worker.start_time_ticks,
        issued_at_ms,
        expires_at_ms: issued_at_ms + 120_000,
        routes,
    });
    send(
        &mut worker,
        &BrokerFrame::Assign(Box::new(assignment(
            grant,
            protocol::PROTOCOL_VERSION,
            "task-noapproval",
            harness.identity.uid,
            &harness.identity.home,
        ))),
    )
    .await;

    let status = tokio::time::timeout(Duration::from_secs(30), worker.child.wait())
        .await
        .expect("worker did not exit")
        .expect("wait");
    assert!(
        !status.success(),
        "a grant without permission mediation must fail closed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_same_uid_sibling_without_the_channel_cannot_become_a_worker() {
    // The channel is a private descriptor, not a rendezvous point: a
    // process of the same account that simply runs the binary has no
    // job, no grant and no way to obtain either.
    let mut sibling = tokio::process::Command::new(WORKER_BIN)
        .arg("--worker")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn sibling");
    let status = tokio::time::timeout(Duration::from_secs(30), sibling.wait())
        .await
        .expect("sibling did not exit")
        .expect("wait");
    assert!(
        !status.success(),
        "a worker started outside clawd must refuse to run"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worker_refuses_a_grant_minted_for_another_process() {
    let Some(harness) = harness() else {
        eprintln!("skipping: no usable unprivileged account for the worker harness");
        return;
    };
    let signer = GrantSigner::generate().expect("signer");
    let mut worker = spawn::spawn_worker(&harness.identity, "task-stolen").expect("spawn");
    // Bound to a different pid: the worker must not run the job.
    let grant = grant_for(
        &signer,
        "task-stolen",
        &harness.identity,
        worker.pid.wrapping_add(1),
        worker.start_time_ticks,
    );
    send(
        &mut worker,
        &BrokerFrame::Assign(Box::new(assignment(
            grant,
            protocol::PROTOCOL_VERSION,
            "task-stolen",
            harness.identity.uid,
            &harness.identity.home,
        ))),
    )
    .await;

    let status = tokio::time::timeout(Duration::from_secs(30), worker.child.wait())
        .await
        .expect("worker did not exit")
        .expect("wait");
    assert!(
        !status.success(),
        "a worker must fail closed on a grant minted for another process"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_protocol_version_mismatch_fails_closed() {
    let Some(harness) = harness() else {
        eprintln!("skipping: no usable unprivileged account for the worker harness");
        return;
    };
    let signer = GrantSigner::generate().expect("signer");
    let mut worker = spawn::spawn_worker(&harness.identity, "task-mixed").expect("spawn");
    let grant = grant_for(
        &signer,
        "task-mixed",
        &harness.identity,
        worker.pid,
        worker.start_time_ticks,
    );
    send(
        &mut worker,
        &BrokerFrame::Assign(Box::new(assignment(
            grant,
            protocol::PROTOCOL_VERSION + 1,
            "task-mixed",
            harness.identity.uid,
            &harness.identity.home,
        ))),
    )
    .await;

    let status = tokio::time::timeout(Duration::from_secs(30), worker.child.wait())
        .await
        .expect("worker did not exit")
        .expect("wait");
    assert!(
        !status.success(),
        "a mixed old/new install must fail closed rather than run the task"
    );
}
