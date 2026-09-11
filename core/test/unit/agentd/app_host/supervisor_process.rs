// Opt-in process test. Run its exact name as root in the private /run
// namespace documented by agentd/app_host/MODULE.md, with a freshly built
// COS_APP_HOST_COS_BIN and COS_APP_HOST_OWNER_UID (or sudo's SUDO_UID).

use super::super::*;

use std::ffi::{CString, OsString};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::activities::{
    ActivityService, ActivityState, ReceiptOutcome, ReceiptSource, ResultKind,
};
use crate::caps::{Cap, CapSet, Role, Scope, Verb};
use crate::provenance::runtime::{InstanceClass, PackageRef, ProcessIdentity};
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;

mod support {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agentd/app_host/process_support.rs"
    ));
}

use support::{ProcessContext, APP_ID, BODY, CHILD_TEST, CONTEXT_ENV, OPERATION};

const APP_SOURCE: &str = r#"
import hashlib
import json
import os
from pathlib import Path
import time
from cos_runtime import file_changes, policy

def run(command, args):
    assert command == "read" and len(args) == 1
    path = Path(args[0])
    policy.require("fs.read", path=str(path))
    policy.require("fs.write", path=str(path))
    policy.require("data.kv.write", name="host-process")
    body = path.read_text()
    metadata = path.stat()
    expected = {
        "sha256": "sha256:" + hashlib.sha256(body.encode()).hexdigest(),
        "size": metadata.st_size,
        "device": metadata.st_dev,
        "inode": metadata.st_ino,
        "mode": metadata.st_mode,
        "modified_ns": metadata.st_mtime_ns,
        "changed_ns": metadata.st_ctime_ns,
    }
    replacement = file_changes.replace_file(
        str(path), expected, (body + "updated by controlled broker\n").encode()
    )
    data = Path(os.environ["COS_DATA_DIR"])
    counter = data / "counter"
    count = int(counter.read_text()) + 1 if counter.exists() else 1
    counter.write_text(str(count))
    status = Path("/proc/self/status").read_text()
    nnp = next(line.split()[1] for line in status.splitlines()
               if line.startswith("NoNewPrivs:"))
    facts = {
        "body": body,
        "replacement": replacement,
        "invocations": count,
        "app_uid": os.getuid(),
        "app_gid": os.getgid(),
        "app_pid": os.getpid(),
        "app_nnp": nnp == "1",
        "session_id": os.environ["COS_SESSION"],
        "mount_namespace": os.readlink("/proc/self/ns/mnt"),
        "pid_namespace": os.readlink("/proc/self/ns/pid"),
        "sibling_visible": path.with_name("not-granted.txt").exists(),
    }
    stage = data / "ready.new"
    stage.write_text(json.dumps(facts))
    os.replace(stage, data / "ready.json")
    deadline = time.monotonic() + 30
    while not (data / "release").exists():
        if time.monotonic() >= deadline:
            raise RuntimeError("root test did not inspect the live App")
        time.sleep(0.02)
    return facts
"#;

struct ProcessFixture {
    context: ProcessContext,
    env: Vec<(&'static str, Option<OsString>)>,
    was_broker: bool,
}

impl ProcessFixture {
    fn new(uid: u32, gid: u32) -> Self {
        assert_private_run();
        let root = PathBuf::from("/run").join(format!("cos-ah-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&root).unwrap();
        mode(&root, 0o755);
        let context = ProcessContext {
            root,
            uid,
            gid,
            mount_namespace: std::fs::read_link("/proc/self/ns/mnt")
                .unwrap()
                .to_str()
                .unwrap()
                .to_string(),
        };
        let mut fixture = Self {
            context,
            env: Vec::new(),
            was_broker: crate::agentd::guard::is_broker_process(),
        };
        for key in [
            "COS_PROC_DATA_DIR",
            "COS_PROVENANCE_RUNTIME_DIR",
            "COS_SESSION",
            "COS_APP_ID",
            "COS_BROKER_ENDPOINT",
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "XDG_RUNTIME_DIR",
            "DBUS_SESSION_BUS_ADDRESS",
        ] {
            fixture.env.push((key, std::env::var_os(key)));
            std::env::remove_var(key);
        }
        for (key, child) in [
            ("COS_DATA_DIR", "data"),
            ("COS_CAPS_DATA_DIR", "caps"),
            ("COS_APPS_DIR", "apps"),
            ("COS_RUNTIME_DIR", "runtime"),
            ("COS_CONFIG_DIR", "config"),
            ("COS_LOG_DIR", "logs"),
            ("COS_CACHE_DIR", "cache"),
        ] {
            let path = fixture.context.root.join(child);
            std::fs::create_dir(&path).unwrap();
            mode(&path, 0o700);
            fixture.set_env(key, path);
        }
        crate::agentd::guard::mark_broker_process();
        std::fs::create_dir(fixture.context.home()).unwrap();
        chown(&fixture.context.home(), uid, gid);
        mode(&fixture.context.home(), 0o700);
        fixture
    }

    fn set_env(&mut self, key: &'static str, value: impl AsRef<std::ffi::OsStr>) {
        self.env.push((key, std::env::var_os(key)));
        std::env::set_var(key, value);
    }

    fn install(&mut self, cos_binary: &Path) {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let root = &self.context.root;
        let apps = self.context.apps();
        mode(&apps, 0o755);
        let app = apps.join(APP_ID);
        std::fs::create_dir(&app).unwrap();
        mode(&app, 0o755);
        write_public(&self.context.input(), BODY.as_bytes());
        write_public(&root.join("not-granted.txt"), b"must not be mounted");
        write_public(&app.join("main.py"), APP_SOURCE.as_bytes());
        let manifest = json!({
            "id": APP_ID, "version": "0.1.0", "name": "Controlled host process",
            "operations": {
                "read": {
                    "label": "Read exactly once",
                    "args": [{"name": "path", "kind": "path", "required": true}],
                    "needs": [
                        {"verb": "fs.read", "scope": {"kind": "from-arg", "arg": "path"}, "why": "read"},
                        {"verb": "fs.write", "scope": {"kind": "from-arg", "arg": "path"}, "why": "replace"},
                        {"verb": "data.kv.write", "scope": {"kind": "fixed", "scope": {"kind": "name", "value": APP_ID}}, "why": "count"},
                    ],
                },
            },
        });
        write_public(
            &app.join("app.json"),
            &serde_json::to_vec(&manifest).unwrap(),
        );
        let roots = self.context.trust_roots();
        std::fs::create_dir_all(&roots[0].path).unwrap();
        mode(roots[0].path.parent().unwrap(), 0o755);
        mode(&roots[0].path, 0o755);
        let key = crate::provenance::sign::SigningKeyFile::generate(Some(
            "controlled host process fixture".into(),
        ))
        .unwrap();
        write_public(
            &roots[0].path.join("test.json"),
            &serde_json::to_vec(&key.trust_entry(&[crate::provenance::PackageKind::App])).unwrap(),
        );
        crate::test_env::record_trust_state(&roots);
        self.context.install_test_trust();
        crate::provenance::sign::sign_directory(
            &app,
            &crate::provenance::sign::SignRequest {
                kind: crate::provenance::PackageKind::App,
                id: APP_ID.into(),
                version: "0.1.0".into(),
                manifest_schema: "test".into(),
                manifest_path: "app.json".into(),
                entrypoints: vec!["main.py".into()],
                resources: vec![],
            },
            &key,
        )
        .unwrap();
        // The envelope is public metadata, still root-owned and never writable by the worker.
        mode(&app.join(".provenance.json"), 0o644);

        let sdk = root.join("python");
        std::fs::create_dir(&sdk).unwrap();
        mode(&sdk, 0o755);
        for (source, package) in [
            ("cos-runtime", "cos_runtime"),
            ("claw-os-sdk", "claw_os_sdk"),
        ] {
            copy_python(
                &repo.join(source).join("python").join("src").join(package),
                &sdk.join(package),
            );
        }
        let bin = root.join("bin");
        std::fs::create_dir(&bin).unwrap();
        mode(&bin, 0o755);
        let cos = bin.join("cos");
        let tests = bin.join("unit-tests");
        std::fs::copy(cos_binary, &cos).unwrap();
        std::fs::copy(std::env::current_exe().unwrap(), &tests).unwrap();
        mode(&cos, 0o555);
        mode(&tests, 0o555);
        let context = root.join("context.json");
        write_public(&context, &serde_json::to_vec(&self.context).unwrap());
        let wrapper = bin.join("worker");
        let script = format!(
            "#!/bin/sh\nexport {CONTEXT_ENV}={}\nexport COS_SDK_PYTHON_DIR={}\n\
             exec {} --ignored --exact {} --test-threads=1 --nocapture\n",
            shell_quote(&context),
            shell_quote(&sdk),
            shell_quote(&tests),
            shell_quote(Path::new(CHILD_TEST)),
        );
        write_public(&wrapper, script.as_bytes());
        mode(&wrapper, 0o555);
        self.set_env("COS_BIN", cos);
        self.set_env("COS_AGENTD_BIN", wrapper);
    }
}

impl Drop for ProcessFixture {
    fn drop(&mut self) {
        crate::session::journal::writer::release_all();
        crate::provenance::set_trust_store(crate::provenance::TrustStore::load_default());
        if !self.was_broker {
            crate::agentd::guard::clear_broker_process_for_test();
        }
        for (key, previous) in self.env.drain(..).rev() {
            match previous {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        let _ = std::fs::remove_dir_all(&self.context.root);
    }
}

fn assert_private_run() {
    assert_eq!(unsafe { libc::geteuid() }, 0, "requires a root supervisor");
    assert_ne!(
        std::fs::read_link("/proc/self/ns/mnt").unwrap(),
        std::fs::read_link("/proc/1/ns/mnt").unwrap(),
        "never run against the machine's live /run",
    );
    let mounts = std::fs::read_to_string("/proc/self/mountinfo").unwrap();
    assert!(
        mounts.lines().any(|line| {
            let Some((mount, filesystem)) = line.split_once(" - ") else {
                return false;
            };
            mount.split_whitespace().nth(4) == Some("/run")
                && filesystem.split_whitespace().next() == Some("tmpfs")
        }),
        "requires a fresh /run tmpfs"
    );
    let marker = std::fs::symlink_metadata("/run/.cos-app-host-test")
        .expect("create /run/.cos-app-host-test only after mounting the private tmpfs");
    assert!(marker.is_file() && marker.uid() == 0 && marker.mode() & 0o022 == 0);
}

fn mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn chown(path: &Path, uid: u32, gid: u32) {
    let path = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(path.as_ptr(), uid, gid) }, 0);
}

fn write_public(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    mode(path, 0o644);
}

fn copy_python(source: &Path, target: &Path) {
    std::fs::create_dir(target).unwrap();
    mode(target, 0o755);
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "py")
        {
            let target = target.join(entry.file_name());
            std::fs::copy(entry.path(), &target).unwrap();
            mode(&target, 0o644);
        }
    }
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}

struct ProcessCleanup {
    worker: ProcessIdentity,
    app: Option<ProcessIdentity>,
}

impl Drop for ProcessCleanup {
    fn drop(&mut self) {
        for identity in self.app.iter().chain(std::iter::once(&self.worker)) {
            if identity.still_matches() {
                let pid = libc::pid_t::try_from(identity.pid).unwrap();
                let group = unsafe { libc::getpgid(pid) };
                unsafe { libc::kill(if group == pid { -pid } else { pid }, libc::SIGKILL) };
            }
        }
    }
}

fn parent_session(id: String, caps: CapSet, home: &Path) -> crate::proc::SessionInfo {
    crate::proc::SessionInfo {
        session_id: id,
        pid: std::process::id(),
        command: vec!["test task".into()],
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: Some("agent".into()),
        parent: None,
        workdir: Some(home.to_str().unwrap().into()),
        exit_code: None,
        ended_at: None,
        tier: Some(Role::AgentHost.credential_tier()),
        scope: None,
        priority: None,
        caps: Some(caps),
        transient_caps: None,
        role: Some(Role::AgentHost.name().into()),
        app_id: None,
        pending_bind: false,
        start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
    }
}

fn describe_outcome(outcome: &TaskOutcome) -> String {
    match outcome {
        TaskOutcome::Reported(outcome) => format!("{outcome:?}"),
        TaskOutcome::Cancelled => "cancelled".into(),
        TaskOutcome::Failed(message) | TaskOutcome::Retry(message) => message.clone(),
    }
}

async fn owner_row(uid: u32, home: &Path, id: &str) -> Option<crate::proc::SessionInfo> {
    crate::paths::with_user_override(uid, home.to_path_buf(), async {
        crate::proc::session_info_by_id(id)
    })
    .await
}

async fn inspect_live_app(
    context: &ProcessContext,
    task_session: &str,
    package: &PackageRef,
    worker_pid: u32,
) -> (String, ProcessIdentity, Value) {
    let ready = context.app_data().join("ready.json");
    tokio::time::timeout(Duration::from_secs(40), async {
        while !ready.is_file() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let facts: Value = serde_json::from_slice(&std::fs::read(&ready).unwrap()).unwrap();
        let session = facts["session_id"].as_str().unwrap().to_string();
        let instance = crate::provenance::runtime::instance_for(context.uid, &session)
            .unwrap()
            .expect("root persisted the bound App instance");
        assert_eq!(instance.class, InstanceClass::App);
        assert_eq!(instance.package.as_ref(), Some(package));
        let process = instance
            .process
            .expect("root persisted the exact App process");
        assert_eq!(process.uid, context.uid);
        assert_ne!(process.pid, worker_pid);
        assert!(process.still_matches());
        assert!(process.start_time_ticks.is_some());
        let kernel = crate::clawd::transport::peer::verify(crate::clawd::transport::Credentials {
            pid: process.pid,
            uid: context.uid,
            gid: context.gid,
        })
        .unwrap();
        assert_eq!(Some(kernel.start_time_ticks), process.start_time_ticks);
        let row = owner_row(context.uid, &context.home(), &session)
            .await
            .unwrap();
        assert_eq!(row.parent.as_deref(), Some(task_session));
        assert_eq!(row.app_id.as_deref(), Some(APP_ID));
        assert_eq!(row.pid, process.pid);
        assert_eq!(row.start_time_ticks, process.start_time_ticks);
        assert!(!row.pending_bind);
        let caps = row.caps.unwrap();
        assert_eq!(caps.len(), 4);
        assert!(caps.covers(&Cap::new(
            Verb::FS_READ,
            Scope::path(context.input().to_string_lossy())
        )));
        assert!(!caps.covers(&Cap::new(
            Verb::FS_READ,
            Scope::path(context.root.join("not-granted.txt").to_string_lossy())
        )));
        for path in [
            crate::provenance::runtime::state_path_for(context.uid),
            PathBuf::from("/run/cos/caps")
                .join(context.uid.to_string())
                .join("proc/registry.json"),
        ] {
            let metadata = std::fs::symlink_metadata(path).unwrap();
            assert_eq!(metadata.uid(), 0);
            assert_eq!(metadata.mode() & 0o022, 0);
        }
        assert_eq!(facts["app_uid"], context.uid);
        assert_eq!(facts["app_gid"], context.gid);
        assert_eq!(facts["app_nnp"], true);
        assert_eq!(facts["sibling_visible"], false);
        assert_eq!(facts["body"], BODY);
        assert_eq!(facts["invocations"], 1);
        assert_ne!(
            facts["mount_namespace"].as_str(),
            Some(context.mount_namespace.as_str())
        );
        assert_ne!(
            facts["pid_namespace"].as_str(),
            std::fs::read_link("/proc/self/ns/pid").unwrap().to_str(),
        );
        (session, process, facts)
    })
    .await
    .expect("App never became live in its real sandbox")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires root/private /run, COS_APP_HOST_COS_BIN, an owner UID, and working bubblewrap"]
async fn controlled_host_runs_one_real_app_and_records_one_receipt() {
    let _env = crate::test_env::lock_env();
    assert_private_run();
    let binary = PathBuf::from(std::env::var_os("COS_APP_HOST_COS_BIN").expect("fresh cos binary"))
        .canonicalize()
        .unwrap();
    let uid: u32 = std::env::var("COS_APP_HOST_OWNER_UID")
        .or_else(|_| std::env::var("SUDO_UID"))
        .expect("set COS_APP_HOST_OWNER_UID to an existing non-root owner")
        .parse()
        .unwrap();
    let mut identity = spawn::resolve_identity(uid).unwrap();
    let mut fixture = ProcessFixture::new(uid, identity.gid);
    fixture.install(&binary);
    let context = fixture.context.clone();
    // Isolate HOME without changing the real passwd-derived uid/gid.
    identity.home = context.home();
    crate::storage::ensure_owner_agent_state_dir(uid, identity.gid).unwrap();
    let availability = crate::worker::availability();
    assert!(availability.is_available(), "{availability:?}");
    let app = crate::apps::find_verified(&context.apps(), APP_ID).unwrap();
    let package = PackageRef::of(app.require_verified().unwrap());
    let caps = CapSet::from_caps([
        Cap::new(Verb::AGENT_INVOKE, Scope::name(APP_ID)),
        Cap::new(
            Verb::FS_READ,
            Scope::path(context.input().to_string_lossy()),
        ),
        Cap::new(
            Verb::FS_WRITE,
            Scope::path(context.input().to_string_lossy()),
        ),
        Cap::new(Verb::DATA_KV_WRITE, Scope::name(APP_ID)),
    ]);
    let session = crate::paths::with_user_override(uid, context.home(), async {
        crate::session::create("controlled App process regression").unwrap()
    })
    .await;
    crate::session::set_caps(&session, &caps).unwrap();
    crate::session::update_meta(&session, |meta| {
        meta.owner_uid = Some(uid);
        meta.role = Some(Role::AgentHost);
        meta.origin = Some(crate::session::SessionOrigin::SystemAgentTask);
    })
    .unwrap();
    let service = crate::activities::open_default().unwrap();
    let activity = service
        .create(
            uid,
            serde_json::from_value(json!({
                "title": "Controlled App", "goal": "Read one exact file",
            }))
            .unwrap(),
        )
        .unwrap();
    let store = Store::with_root(context.root.join("jobs")).unwrap();
    let submitted = store
        .submit_with_activity(
            "one App operation".into(),
            None,
            None,
            Some(session.to_string()),
            Some(1),
            false,
            Some(uid),
            Some(context.home().to_str().unwrap().into()),
            Some(activity.id.clone()),
        )
        .unwrap();
    let job = store.claim_one().unwrap().unwrap();
    assert_eq!(job.id, submitted.id);
    let task_session = parent_session(session.to_string(), caps, &context.home());
    let spawned = spawn::spawn_worker(&identity, &job.id).unwrap();
    let spawn::SpawnedWorker {
        mut child,
        channel,
        pid,
        start_time_ticks,
    } = spawned;
    let mut cleanup = ProcessCleanup {
        worker: ProcessIdentity::of_process(uid, pid).unwrap(),
        app: None,
    };
    let peer = crate::clawd::transport::peer::verify(crate::clawd::transport::Credentials {
        pid,
        uid,
        gid: identity.gid,
    })
    .unwrap();
    assert_eq!(Some(peer.start_time_ticks), start_time_ticks);
    let job = store.bind_worker(&job.id, pid, start_time_ticks).unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let logs = tokio::spawn(async move {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut stdout = stdout.take(256 * 1024);
        let mut stderr = stderr.take(256 * 1024);
        let (out_read, err_read) =
            tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err),);
        out_read.unwrap();
        err_read.unwrap();
        (out, err)
    });
    let config = SupervisorConfig {
        lease: Duration::from_secs(60),
        heartbeat_grace: Duration::from_secs(20),
        ..SupervisorConfig::default()
    };
    let lease = Lease {
        task_id: job.id.clone(),
        session_id: job.session_id.clone(),
        owner_uid: uid,
        owner_gid: identity.gid,
        worker_pid: pid,
        worker_start_time_ticks: start_time_ticks,
        deadline: Instant::now() + config.lease,
        receipts_authorized: false,
        app_host_authorized: false,
    };
    let signer = GrantSigner::from_secret([61; 32]);
    let shutdown = Arc::new(AtomicBool::new(false));
    let services = BrokerServices {
        state: crate::clawd::state::DaemonState::new().unwrap(),
        admission: crate::clawd::transport::limits::Admission::new(
            crate::clawd::transport::limits::Limits::default(),
        ),
    };
    let (outcome, app_session, app_identity, app_facts) = {
        let run = pump(
            &store,
            &signer,
            &config,
            &shutdown,
            std::process::id(),
            &job,
            Some(task_session),
            lease,
            channel,
            &mut child,
            services,
        );
        tokio::pin!(run);
        let (app_session, app_identity, app_facts) = tokio::select! {
            observed = inspect_live_app(&context, session.as_str(), &package, pid) => observed,
            outcome = &mut run => panic!("worker ended before live App inspection: {}", describe_outcome(&outcome)),
        };
        cleanup.app = Some(app_identity.clone());
        write_public(&context.app_data().join("release"), b"continue");
        let outcome = tokio::time::timeout(Duration::from_secs(60), &mut run)
            .await
            .expect("controlled host did not return a result");
        (outcome, app_session, app_identity, app_facts)
    };
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
    let (stdout, stderr) = logs.await.unwrap();
    assert!(
        status.success(),
        "{}{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    let outcome = match outcome {
        TaskOutcome::Reported(outcome) => outcome,
        outcome => panic!("controlled host failed: {}", describe_outcome(&outcome)),
    };
    let run = match outcome.as_ref() {
        WorkerOutcome::Ok(run) => run,
        outcome => panic!("App execution failed: {outcome:?}"),
    };
    let result: Value = serde_json::from_str(&run.response).unwrap();
    assert_eq!(result["output"], app_facts);
    assert_eq!(result["worker_pid"], pid);
    assert_eq!(result["worker_start_time_ticks"], start_time_ticks.unwrap());
    assert_eq!(result["worker_uid"], uid);
    assert_eq!(result["worker_gid"], identity.gid);
    assert_eq!(result["worker_nnp"], true);
    assert_eq!(result["receipt_calls"], 2);
    assert!(result["app_controls"].as_u64().unwrap() >= 6);
    let finished = store.finish(job, (*outcome).into()).unwrap();
    assert_eq!(finished.owner_uid, Some(uid));
    assert_eq!(finished.activity_id.as_deref(), Some(activity.id.as_str()));
    assert_eq!(finished.worker_pid, Some(pid));
    assert_eq!(finished.status, crate::agent::service::JobStatus::Ok);

    let receipts = service.receipts(uid, &activity.id, 100).unwrap();
    assert_eq!(
        receipts.len(),
        1,
        "record-only retry must not duplicate storage"
    );
    let receipt = &receipts[0];
    assert_eq!(receipt.id, result["receipt_id"].as_str().unwrap());
    assert_eq!(receipt.owner_uid, uid);
    assert_eq!(receipt.activity_id, activity.id);
    assert_eq!(receipt.source, ReceiptSource::CallerReported);
    assert_eq!(receipt.report.app_id, APP_ID);
    assert_eq!(receipt.report.operation, OPERATION);
    assert_eq!(receipt.report.package_digest, package.content_digest);
    assert_eq!(receipt.report.outcome, ReceiptOutcome::Returned);
    assert_eq!(
        receipt.report.result.as_ref().unwrap().kind,
        ResultKind::Json
    );
    assert!(receipt
        .report
        .result
        .as_ref()
        .unwrap()
        .preview
        .contains("controlled App process body"));
    assert!(receipt.declaration.is_some());
    assert!(receipt.declaration_error.is_none());
    assert!(service.receipts(0, &activity.id, 100).is_err());
    assert_eq!(
        service.get(uid, &activity.id).unwrap().state,
        ActivityState::Active
    );
    assert_eq!(
        std::fs::metadata(context.root.join("data/activities.db"))
            .unwrap()
            .uid(),
        0
    );
    let (_, events) = store.read_stream_events(&finished.id, 0).unwrap();
    let links: Vec<_> = events
        .iter()
        .filter(|event| event["progress"]["kind"] == "activity_receipt")
        .collect();
    assert_eq!(links.len(), 2);
    for link in links {
        assert_eq!(link["progress"]["activity_id"], activity.id);
        assert_eq!(link["progress"]["receipt_id"], receipt.id);
        assert_eq!(link["progress"]["source"], "caller_reported");
    }
    assert_eq!(
        std::fs::read_to_string(context.app_data().join("counter")).unwrap(),
        "1"
    );
    let replaced = format!("{BODY}updated by controlled broker\n");
    assert_eq!(std::fs::read_to_string(context.input()).unwrap(), replaced);
    assert_eq!(std::fs::metadata(context.input()).unwrap().uid(), 0);
    assert_eq!(result["output"]["replacement"]["changed"], true);
    assert_eq!(result["output"]["replacement"]["bytes"], replaced.len());
    assert_eq!(
        result["output"]["replacement"]["sha256"],
        format!("sha256:{}", crate::crypto::sha256_hex(replaced.as_bytes()))
    );
    assert!(crate::provenance::runtime::instance_for(uid, &app_session)
        .unwrap()
        .is_none());
    assert!(owner_row(uid, &context.home(), &app_session)
        .await
        .is_none());
    assert!(!app_identity.still_matches());
    assert!(!cleanup.worker.still_matches());
}
