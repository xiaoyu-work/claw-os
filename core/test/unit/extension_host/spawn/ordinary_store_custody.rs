use super::*;
use crate::apps::store::{self, Record};
use crate::clawd::{app_execution, app_store};
use crate::clawd::state::DaemonState;
use crate::clawd::transport::{frame::PeerStream, peer, ReadOutcome};
use crate::clawd::transport::limits::{Admission, Limits as AdmissionLimits};
use crate::clawd::wire::Request;
use crate::provenance::{sign, PackageKind, TrustStore, TrustTier};
use serde_json::{json, Value};
use std::os::fd::AsFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::process::{Child, Stdio};
use std::sync::Mutex;

const OWNER: u32 = 61990;
const EXECUTION: u32 = 61991;
const ID: &str = "ordinary-custody";
const BASE: &str = "/run/ordinary-custody";
const ACTOR: &str =
    "extension_host::spawn::task_data::tests::store_custody::ordinary_store_actor";
const FOREIGN: &str =
    "extension_host::spawn::task_data::tests::store_custody::foreign_cleanup_actor";
const MAIN: &str = r#"
import ctypes, json, os, pathlib, time

def run(command, args):
    child = None
    if command == "grandchild":
        child = os.fork()
        if child == 0:
            os.setsid()
            ctypes.CDLL(None).prctl(15, b"custody-grand", 0, 0, 0)
            while True:
                time.sleep(1)
    root = pathlib.Path(os.environ["COS_DATA_DIR"])
    (root / "ready.json").write_text(json.dumps({"ready": True, "child": child}))
    while True:
        time.sleep(1)
"#;

fn roots() -> Vec<TrustRootSpec> {
    vec![TrustRootSpec {
        path: Path::new(BASE).join("trust"),
        tier: TrustTier::System,
        allowed_uids: vec![0],
        domain: crate::provenance::state::TrustDomain::System,
    }]
}

fn install_trust() {
    let roots = roots();
    let trust = TrustStore::load_roots(&roots);
    assert!(trust.diagnostics().is_empty(), "{:?}", trust.diagnostics());
    crate::provenance::set_trust_store_for_roots(trust, roots);
}

#[test]
#[ignore = "private actor of the ordinary Root custody fixtures"]
fn ordinary_store_actor() {
    assert_eq!(std::env::var("COS_ORDINARY_CUSTODY_ACTOR").unwrap(), "1");
    assert_ne!(unsafe { libc::geteuid() }, 0);
    install_trust();
    std::io::stdin().read_exact(&mut [0]).unwrap();
    let apps = Path::new(BASE).join("apps");
    let app = crate::apps::find_verified(&apps, ID).unwrap();
    let launch = crate::bridge::AppLaunch::new(app.require_verified().unwrap().clone()).unwrap();
    let operation = std::env::var("COS_ORDINARY_CUSTODY_OPERATION").unwrap();
    let result = if std::env::var("COS_ORDINARY_CUSTODY_TASK").as_deref() == Ok("1") {
        crate::bridge::run_task_app(&launch, &operation, &[], apps.to_str().unwrap())
    } else {
        crate::bridge::run_app(
            &launch, &operation, &[], &format!("{BASE}/data"), apps.to_str().unwrap(),
        )
    };
    eprintln!("ordinary fixture Host returned after retirement: {result:?}");
    // Keep the OS Host alive while Root obtains its real cleanup acknowledgement.
    std::io::stdin().read_exact(&mut [0]).unwrap();
}

#[test]
#[ignore = "private foreign peer of the ordinary Root custody fixtures"]
fn foreign_cleanup_actor() {
    assert_eq!(std::env::var("COS_ORDINARY_CUSTODY_ACTOR").unwrap(), "1");
    std::io::stdin().read_exact(&mut [0]).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(
        std::env::var_os("COS_ORDINARY_CUSTODY_FOREIGN_SOCKET").unwrap(),
    ).unwrap();
    for mut connection in listener.incoming().flatten() {
        let _ = connection.set_read_timeout(Some(Duration::from_secs(2)));
        let mut request = [0];
        if connection.read_exact(&mut request).is_ok() {
            let _ = connection.write_all(&[1]);
        }
    }
}

fn c_path(path: &Path) -> CString {
    CString::new(path.as_os_str().as_encoded_bytes()).unwrap()
}

fn bind_mount(source: &Path, target: &Path) {
    let source = c_path(source);
    let target = c_path(target);
    assert_eq!(unsafe {
        libc::mount(source.as_ptr(), target.as_ptr(), std::ptr::null(),
            libc::MS_BIND, std::ptr::null())
    }, 0, "private bind mount: {}", std::io::Error::last_os_error());
}

fn synthetic_accounts() {
    for (name, id) in [("custody-fixture-owner", OWNER), ("custody-fixture-host", EXECUTION)] {
        assert!(unsafe { libc::getpwuid(id) }.is_null(), "fixture UID must not be a real account");
        assert!(unsafe { libc::getgrgid(id) }.is_null(), "fixture GID must not be a real group");
        let home = Path::new(BASE).join("home").join(name);
        std::fs::create_dir_all(&home).unwrap();
        chown(&home, id, id);
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut passwd = std::fs::read_to_string("/etc/passwd").unwrap();
    let mut group = std::fs::read_to_string("/etc/group").unwrap();
    for (name, id) in [("custody-fixture-owner", OWNER), ("custody-fixture-host", EXECUTION)] {
        passwd.push_str(&format!("{name}:x:{id}:{id}:private fixture:{BASE}/home/{name}:/usr/sbin/nologin\n"));
        group.push_str(&format!("{name}:x:{id}:\n"));
    }
    for (name, body) in [("passwd", passwd), ("group", group)] {
        let source = Path::new(BASE).join(name);
        std::fs::write(&source, body).unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o644)).unwrap();
        bind_mount(&source, &Path::new("/etc").join(name));
    }
}

fn sign_app(directory: &Path, version: &str, key: &sign::SigningKeyFile) {
    std::fs::create_dir_all(directory.join("web")).unwrap();
    std::fs::write(directory.join("main.py"), MAIN).unwrap();
    std::fs::write(directory.join("app.json"), json!({
        "schema_version": 2, "id": ID, "version": version, "runtime": "python",
        "name": {"en": "Ordinary custody fixture"},
        "summary": {"en": "Isolated signed App"},
        "operations": {
            "hold": {"label": {"en": "Hold"}, "needs": [], "args": []},
            "grandchild": {"label": {"en": "Grandchild"}, "needs": [], "args": []}
        }
    }).to_string()).unwrap();
    std::fs::write(directory.join("web-ui.json"), json!({
        "schema": "claw.web-app/v1", "app_id": ID,
        "source_kind": "product", "source_name": "custody-fixture",
        "title": "Ordinary custody fixture", "description": "Isolated signed App",
        "entry": "web/index.html",
    }).to_string()).unwrap();
    std::fs::write(directory.join("web/index.html"), "<!doctype html><title>Private fixture</title>").unwrap();
    sign::sign_directory(directory, &sign::SignRequest {
        kind: PackageKind::App, id: ID.into(), version: version.into(),
        manifest_schema: "cos.app/v2".into(), manifest_path: "app.json".into(),
        entrypoints: vec!["main.py".into()],
        resources: vec!["web-ui.json".into(), "web/index.html".into()],
    }, key).unwrap();
}

fn record(directory: &Path) -> Record {
    let app = crate::apps::find_verified(directory.parent().unwrap(), ID).unwrap();
    let package = app.require_verified().unwrap();
    let manifest = std::fs::read(directory.join("app.json")).unwrap();
    let descriptor = std::fs::read(directory.join("web-ui.json")).unwrap();
    let envelope = std::fs::read(directory.join(".provenance.json")).unwrap();
    let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default()));
    archive.append_dir_all("package", directory).unwrap();
    let archive = archive.into_inner().unwrap().finish().unwrap();
    let checksum = store::sha256(&archive);
    let record = Record {
        app_id: ID.into(), app_version: "2.0.0".into(), architecture: "all".into(),
        artifact: store::Artifact {
            file: format!("claw-app-snapshot-{checksum}.tar.gz"), size: archive.len() as u64,
            sha256: checksum, media_type: "application/gzip".into(),
        },
        manifest: store::ManifestBinding { path: "app.json".into(), size: manifest.len() as u64, sha256: store::sha256(&manifest) },
        provenance: store::ProvenanceBinding {
            path: ".provenance.json".into(), sha256: store::sha256(&envelope),
            content_digest: package.content_digest().into(),
            publisher_key_id: package.audit_facts()["publisher_key_id"].as_str().unwrap().into(),
        },
        release_version: "2.0.0".into(), release_tag: "app-custody-fixture-v2.0.0".into(),
        display: store::Display {
            name: "Ordinary custody fixture".into(), description: "Isolated signed App".into(),
            source_kind: "product".into(), source_name: "custody-fixture".into(),
        },
        web_ui: store::WebBinding {
            path: "web-ui.json".into(), sha256: store::sha256(&descriptor),
            entry: "web/index.html".into(),
        },
    };
    let now = chrono::Utc::now();
    let catalog = store::Catalog {
        schema: "claw.app-store/v1".into(), repository: store::REPOSITORY.into(),
        runtime_abi: 1, generation: "1".into(), issued_at: now,
        expires_at: now + chrono::Duration::hours(1), apps: vec![record.clone()],
    };
    store::Catalog::decode(&serde_json::to_vec(&catalog).unwrap(), now).unwrap();
    record.verify_package(package).unwrap();
    record
}

struct RootGroup(PathBuf);

impl Drop for RootGroup {
    fn drop(&mut self) {
        fn remove_owned(path: &Path) {
            let fd = claw_display_control::workload::open_directory(path).unwrap();
            claw_display_control::workload::check_cgroup(fd.as_fd()).unwrap();
            claw_display_control::workload::retire_cgroup(
                fd.as_fd(), Instant::now() + Duration::from_secs(5),
            ).unwrap();
            for entry in std::fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() { remove_owned(&entry.path()); }
            }
            std::fs::remove_dir(path).unwrap();
        }
        // Fixture teardown only; this never creates a successful mutation receipt.
        remove_owned(&self.0);
    }
}

struct Fixture {
    state: DaemonState,
    broker: crate::agentd::supervisor::BrokerContext,
    admission: Arc<Admission>,
    _services: Arc<crate::clawd::app_services::AppServiceManager>,
    request: String,
    review: String,
    record: Record,
    _environment: Vec<crate::test_env::TestEnvVarGuard>,
    _group: RootGroup,
    _backing: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        assert_eq!(unsafe { libc::geteuid() }, 0);
        for namespace in ["mnt", "net"] {
            assert_ne!(std::fs::read_link(format!("/proc/self/ns/{namespace}")).unwrap(),
                std::fs::read_link(format!("/proc/1/ns/{namespace}")).unwrap(),
                "ordinary custody fixture requires private mount/network namespaces");
        }
        let backing = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let cos = PathBuf::from(std::env::var_os("COS_ORDINARY_TEST_COS").expect("compiled cos fixture"));
        let runner = PathBuf::from(std::env::var_os("COS_ORDINARY_TEST_RUNNER").expect("compiled runner fixture"));
        let actor = std::env::current_exe().unwrap();
        mount_tmpfs(c"/run");
        mount_tmpfs(c"/var/lib");
        mount_tmpfs(c"/usr/local/bin");
        for path in [
            BASE, "/run/lock", "/var/lib/dpkg/info", "/var/lib/cos",
            &format!("{BASE}/apps"), &format!("{BASE}/trust"), &format!("{BASE}/state"),
            &format!("{BASE}/runtime"), &format!("{BASE}/data"),
        ] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::set_permissions("/run/lock", std::fs::Permissions::from_mode(0o1777)).unwrap();
        std::fs::write("/var/lib/dpkg/status", "").unwrap();
        std::fs::write("/var/lib/dpkg/lock", "").unwrap();
        copy_executable(&cos, Path::new("/usr/local/bin/cos"));
        copy_executable(&runner, Path::new("/usr/local/bin/claw-app-runner"));
        copy_executable(&actor, &Path::new(BASE).join("actor"));
        synthetic_accounts();
        let data = backing.path().join("data");
        std::fs::create_dir(&data).unwrap();
        chown(&data, OWNER, OWNER);
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700)).unwrap();
        bind_mount(&data, &Path::new(BASE).join("data"));
        let group = Path::new("/sys/fs/cgroup").join(format!("cos-ordinary-fixture-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&group).unwrap();
        let environment = vec![
            crate::test_env::TestEnvVarGuard::set("CLAWD_EXTENSION_CGROUP_ROOT", &group),
            crate::test_env::TestEnvVarGuard::set("COS_EXTENSION_EXEC_GROUP", "nogroup"),
            crate::test_env::TestEnvVarGuard::set("COS_RUNTIME_DIR", format!("{BASE}/runtime")),
            crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", format!("{BASE}/state")),
            crate::test_env::TestEnvVarGuard::set("COS_USER_DATA_DIR", format!("{BASE}/data")),
            crate::test_env::TestEnvVarGuard::set("COS_APPS_DIR", format!("{BASE}/apps")),
            crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", format!("{BASE}/state")),
        ];
        let key = sign::SigningKeyFile::generate(Some("isolated ordinary custody fixture".into())).unwrap();
        std::fs::write(Path::new(BASE).join("trust/key.json"),
            serde_json::to_vec(&key.trust_entry(&[PackageKind::App])).unwrap()).unwrap();
        crate::test_env::record_trust_state(&roots());
        install_trust();
        let installed = Path::new(BASE).join("apps").join(ID);
        sign_app(&installed, "1.0.0", &key);
        let old = crate::apps::find_verified(&Path::new(BASE).join("apps"), ID).unwrap();
        let review = crate::approvals::system_review::submit(
            OWNER, crate::approvals::system_review::ReviewKind::AppActivation,
            old.require_verified().unwrap(), "private ordinary activation".into(),
        ).unwrap();
        crate::approvals::system_review::decide(OWNER, &review.id, true, Some(old.require_verified().unwrap())).unwrap();
        crate::approvals::system_review::consume(OWNER, &review.id, old.require_verified().unwrap()).unwrap();
        let request = uuid::Uuid::new_v4().simple().to_string();
        let source_parent = Path::new("/var/lib/cos/app-store/staging").join(&request).join("package");
        sign_app(&source_parent, "2.0.0", &key);
        // Discovery derives the App ID from its directory name; retain the real staging layout.
        let source = crate::apps::installation::DirectoryInstall::prepare(
            &source_parent, &Path::new(BASE).join("apps"),
        ).unwrap().preview().unwrap();
        let package = source.require_verified().unwrap();
        let manifest = std::fs::read(source_parent.join("app.json")).unwrap();
        let descriptor = std::fs::read(source_parent.join("web-ui.json")).unwrap();
        let envelope = std::fs::read(source_parent.join(".provenance.json")).unwrap();
        let fixture_record_dir = Path::new(BASE).join("record").join(ID);
        std::fs::create_dir_all(fixture_record_dir.join("web")).unwrap();
        for path in ["main.py", "app.json", "web-ui.json", ".provenance.json", "web/index.html"] {
            std::fs::copy(source_parent.join(path), fixture_record_dir.join(path)).unwrap();
        }
        let record = record(&fixture_record_dir);
        assert_eq!(record.manifest.sha256, store::sha256(&manifest));
        assert_eq!(record.web_ui.sha256, store::sha256(&descriptor));
        assert_eq!(record.provenance.sha256, store::sha256(&envelope));
        let review = crate::approvals::system_review::submit(
            OWNER, crate::approvals::system_review::ReviewKind::AppInstall,
            package, "private reviewed Store replacement".into(),
        ).unwrap();
        crate::approvals::system_review::decide(OWNER, &review.id, true, Some(package)).unwrap();
        let state = DaemonState::new().unwrap();
        let admission = Admission::new(AdmissionLimits::default());
        let primary = Path::new(BASE).join("broker.sock");
        let _socket = std::os::unix::net::UnixListener::bind(&primary).unwrap();
        let broker = crate::agentd::supervisor::BrokerContext::new(
            state.clone(), admission.clone(), primary,
        ).unwrap();
        app_execution::initialize(&broker);
        crate::clawd::gui::Manager::start(state.clone(), admission.clone()).unwrap();
        let services = crate::clawd::app_services::AppServiceManager::new(broker.clone());
        state.install_app_service_manager(&services).unwrap();
        Self {
            state, broker, admission, _services: services, request, review: review.id, record,
            _environment: environment, _group: RootGroup(group), _backing: backing,
        }
    }

    fn version(&self) -> String {
        let value: Value = serde_json::from_slice(
            &std::fs::read(Path::new(BASE).join("apps").join(&self.record.app_id).join("app.json")).unwrap(),
        ).unwrap();
        value["version"].as_str().unwrap().into()
    }

    fn approved(&self) -> bool {
        crate::approvals::system_review::get(OWNER, &self.review).unwrap().effective_state().unwrap()
            == crate::approvals::system_review::ReviewState::Approved
    }

    async fn publish(&self) -> Result<Value, String> {
        app_store::commit_fixture(
            &self.state, OWNER, self.request.clone(), self.review.clone(), self.record.clone(),
        ).await
    }
}

#[derive(Default)]
struct Observed {
    session: String,
    pid: u32,
    socket: PathBuf,
}

struct Actor {
    child: Child,
    observed: Arc<Mutex<Observed>>,
    server: Option<tokio::task::JoinHandle<()>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    namespace: Option<PrivateMountNamespace>,
    task_data: Option<Arc<TaskAppData>>,
    host_group: Option<crate::worker::cgroup::Scope>,
    paths: HostPaths,
}

impl Actor {
    fn start(fixture: &Fixture, task: bool, grandchild: bool) -> Self {
        let owner = crate::agentd::spawn::resolve_identity(OWNER).unwrap();
        let execution_uid = if task { EXECUTION } else { OWNER };
        let execution_gid = if task { fixture.broker.isolated_execution_gid } else { OWNER };
        let execution = ExtensionIdentity { uid: execution_uid, gid: execution_gid, username: "custody-fixture-host".into() };
        let paths = HostPaths::create(&owner).unwrap();
        let listener = crate::extension_host::broker::bind_listener(
            &paths, execution_uid, execution_gid,
        ).unwrap();
        // Keep AF_UNIX paths short while resolving into the existing writable
        // task directory, not an additional Host namespace exception.
        let runtime = Path::new(BASE).join(if task { "rt-task" } else { "rt-cli" });
        std::os::unix::fs::symlink(&paths.control_dir, &runtime).unwrap();
        let mut command = std::process::Command::new(Path::new(BASE).join("actor"));
        command.args(["--exact", ACTOR, "--ignored", "--nocapture", "--test-threads=1"])
            .env_clear().env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", &paths.control_dir)
            .env("COS_DATA_DIR", paths.control_dir.join("data"))
            .env("COS_LOG_DIR", paths.control_dir.join("log"))
            .env("COS_CACHE_DIR", paths.control_dir.join("cache"))
            .env("COS_BIN", "/usr/local/bin/cos")
            .env("COS_USER_DATA_DIR", format!("{BASE}/data"))
            .env("COS_APPS_DIR", format!("{BASE}/apps"))
            .env("COS_PROC_DATA_DIR", format!("/run/cos/caps/{OWNER}"))
            .env("COS_SESSION", if task { "task-host" } else { "ordinary-parent" })
            .env("XDG_RUNTIME_DIR", runtime)
            .env("COS_ORDINARY_CUSTODY_ACTOR", "1")
            .env("COS_ORDINARY_CUSTODY_TASK", if task { "1" } else { "0" })
            .env("COS_ORDINARY_CUSTODY_OPERATION", if grandchild { "grandchild" } else { "hold" })
            .current_dir(&paths.control_dir).stdin(Stdio::piped())
            .stdout(Stdio::inherit()).stderr(Stdio::inherit());
        if task {
            command.env(crate::extension_host::protocol::BROKER_SOCKET_ENV, &paths.broker_socket);
        } else {
            command.env("CLAWD_SOCKET", &paths.broker_socket);
        }
        let task_path = c_path(&paths.dir);
        unsafe {
            command.pre_exec(move || {
                if task { setup_private_mount_namespace(&task_path)?; }
                if libc::setgroups(0, std::ptr::null()) != 0
                    || libc::setresgid(execution_gid, execution_gid, execution_gid) != 0
                    || libc::setresuid(execution_uid, execution_uid, execution_uid) != 0
                    || (task && libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0)
                    || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
                { return Err(std::io::Error::last_os_error()); }
                crate::agentd::spawn::mark_inherited_descriptors_cloexec(3);
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        let mut namespace = task.then(|| PrivateMountNamespace::capture(child.id(), &paths.dir, None).unwrap());
        let mut host_group = task.then(|| crate::worker::cgroup::create_app(
            &fixture.broker.extension_containment().unwrap(), "fixture-task-host",
            &crate::worker::Limits::operation(), None,
        ).unwrap());
        if let Some(group) = &mut host_group { group.attach(child.id()).unwrap(); }
        let data = namespace.as_mut().map(|namespace| {
            let data = Arc::new(TaskAppData::new(
                owner.clone(), execution.clone(), binding(&owner, &execution, &paths, child.id()),
                paths.clone(), namespace.fd.clone(),
            ));
            namespace.task_app_data = Some(data.clone());
            data
        });
        let caps = crate::caps::CapSet::from_caps([
            crate::caps::Cap::new(crate::caps::Verb::AGENT_INVOKE, crate::caps::Scope::name(ID)),
        ]);
        let task_session = Arc::new(session("task-parent", child.id(), caps.clone()));
        let client = match &data {
            Some(data) => crate::clawd::client_identity::ClientIdentity::from_verified_delegation(
                child.id(), OWNER, execution_uid, execution_gid,
                crate::proc::read_start_time_ticks_pub(child.id()).unwrap(),
                AuthenticatedExtensionHost {
                    purpose: crate::extension_host::protocol::HostPurpose::Task,
                    lease_id: data.binding.task_id.clone(),
                    authority_session_id: Some(task_session.session_id.clone()),
                    host_session_id: Some("task-host".into()),
                    owner_uid: OWNER, extension_uid: execution_uid,
                    capability_generation: data.binding.capability_generation.clone(),
                    host_pid: child.id(), host_start_time_ticks: data.binding.host_start_time_ticks,
                    task_session: Some(task_session.clone()), task_app_data: Some(data.clone()),
                },
            ),
            None => crate::clawd::client_identity::ClientIdentity::from_peer(peer::PeerProcess {
                pid: child.id(), uid: OWNER, gid: OWNER,
                start_time_ticks: crate::proc::read_start_time_ticks_pub(child.id()).unwrap(),
            }),
        };
        let mut parent = session(if task { "task-host" } else { "ordinary-parent" }, child.id(), caps);
        if let Some(data) = &data {
            parent.parent = Some(task_session.session_id.clone());
            parent.command[1] = data.binding.task_id.clone();
        }
        if !task {
            parent.group = Some("user-cli".into());
            parent.tier = Some(crate::caps::Role::Automator.credential_tier());
            parent.role = Some(crate::caps::Role::Automator.name().into());
            parent.client = crate::session::SessionClient::new(
                crate::session::SessionSource::LocalCli, false, true,
            );
        }
        crate::proc::register_session_for_owner(parent, OWNER).unwrap();
        if task { crate::storage::install_routed_extension_reader(OWNER, execution_uid).unwrap(); }
        let observed = Arc::new(Mutex::new(Observed::default()));
        let captured = observed.clone();
        let state = fixture.state.clone();
        let admission = fixture.admission.clone();
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            loop {
                let connection = tokio::select! {
                    _ = &mut stopped => break,
                    connection = listener.accept() => connection.unwrap().0,
                };
                let mut stream = PeerStream::new(connection).unwrap();
                let ReadOutcome::Frame(frame) = stream.read_request(crate::clawd::wire::MAX_REQUEST_BYTES).await.unwrap()
                    else { continue };
                let credentials = peer::verify(frame.credentials).unwrap();
                assert_eq!(credentials.pid, client.pid.unwrap());
                assert_eq!(credentials.uid, client.execution_uid.unwrap_or(OWNER));
                assert_eq!(credentials.gid, client.gid.unwrap());
                assert_eq!(credentials.start_time_ticks, client.start_time_ticks.unwrap());
                let request: Request = serde_json::from_slice(&frame.body).unwrap();
                let command = request.command;
                let params = request.params.clone();
                let response = crate::clawd::server::dispatch_verified_request(
                    request, &client, &state, &admission,
                ).await;
                if command == crate::clawd::routes::Command::AppSessionRegister {
                    assert!(response.error.is_none(), "real Root registration failed: {:?}", response.error);
                    if let Some(value) = response.result.as_ref() {
                        captured.lock().unwrap().session = value["session_id"].as_str().unwrap().into();
                        assert_eq!(value["root_custody"], true);
                        if task { assert!(value["task_app_data_dir"].is_string()); }
                    }
                }
                if command == crate::clawd::routes::Command::AppSessionBind {
                    let mut captured = captured.lock().unwrap();
                    captured.pid = params["pid"].as_u64().unwrap() as u32;
                    captured.socket = params["custody_socket"].as_str().unwrap().into();
                    assert!(response.error.is_none(), "real Root binding failed: {:?}", response.error);
                }
                stream.write_response(&crate::clawd::protocol::encode_response(&response).unwrap()).await.unwrap();
            }
        });
        child.stdin.as_mut().unwrap().write_all(&[1]).unwrap();
        Self { child, observed, server: Some(server), stop: Some(stop), namespace, task_data: data, host_group, paths }
    }

    async fn ready(&mut self) {
        let ready = Path::new(BASE).join("data/apps").join(ID).join("ready.json");
        let deadline = Instant::now() + Duration::from_secs(30);
        while !ready.exists() {
            assert!(self.child.try_wait().unwrap().is_none(), "OS Host exited before actual App execution");
            assert!(Instant::now() < deadline, "App never crossed its real Root bind gate");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let captured = self.observed.lock().unwrap();
        assert_eq!(std::fs::metadata(&ready).unwrap().uid(), OWNER);
        assert!(!captured.session.is_empty());
        assert!(captured.pid > 1);
        assert!(captured.socket.exists());
        let process = unsafe { libc::syscall(libc::SYS_pidfd_open, captured.pid, 0) };
        assert!(process >= 0);
        let process = unsafe { OwnedFd::from_raw_fd(process as i32) };
        assert!(!claw_display_control::identity::pidfd_exited(process.as_fd()).unwrap());
    }

    fn session(&self) -> String { self.observed.lock().unwrap().session.clone() }

    fn group(&self, fixture: &Fixture) -> PathBuf {
        let root = fixture.broker.extension_containment().unwrap();
        if self.host_group.is_some() {
            root.path().join("fixture-task-host").join(self.session())
        } else { root.path().join(self.session()) }
    }

    fn kill_host(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }

    async fn finish(mut self) {
        if self.child.try_wait().unwrap().is_none() {
            let _ = self.child.stdin.as_mut().unwrap().write_all(&[2]);
            let deadline = Instant::now() + Duration::from_secs(10);
            while self.child.try_wait().unwrap().is_none() {
                if Instant::now() >= deadline {
                    self.child.kill().unwrap();
                    self.child.wait().unwrap();
                    panic!("OS Host did not exit after checked retirement");
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        if let Some(stop) = self.stop.take() { let _ = stop.send(()); }
        self.server.take().unwrap().await.unwrap();
        if let Some(namespace) = &mut self.namespace { namespace.cleanup().unwrap(); }
        if let Some(data) = &self.task_data { assert!(data.views.lock().unwrap().is_empty()); }
        if let Some(group) = &mut self.host_group {
            group.retire_checked(Instant::now() + Duration::from_secs(5)).unwrap();
            group.remove_checked().unwrap();
            crate::storage::remove_routed_extension_reader(OWNER, EXECUTION).unwrap();
        }
        self.paths.cleanup().unwrap();
    }
}

impl Drop for Actor {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(stop) = self.stop.take() { let _ = stop.send(()); }
    }
}

async fn wait_fence_release() {
    let deadline = Instant::now() + Duration::from_secs(20);
    while crate::clawd::app_mutation::admit(ID).is_err() {
        assert!(Instant::now() < deadline, "cleanup worker abandoned its mutation fence");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn group_empty(path: &Path) -> bool {
    let fd = claw_display_control::workload::open_directory(path).unwrap();
    claw_display_control::workload::cgroup_is_empty(fd.as_fd()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Root private mount/network namespaces, cgroup v2, ptrace and COS_ORDINARY_TEST_COS/RUNNER"]
async fn ordinary_root_standalone_retires_host_before_publication() {
    let _environment = crate::test_env::lock_env();
    let fixture = Fixture::new();
    let mut actor = Actor::start(&fixture, false, false);
    actor.ready().await;
    let pid = actor.observed.lock().unwrap().pid;
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    assert!(raw >= 0);
    let process = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    let result = fixture.publish().await.unwrap();
    assert_eq!(result["version"], "2.0.0");
    assert_eq!(fixture.version(), "2.0.0");
    assert!(!fixture.approved());
    assert!(claw_display_control::identity::pidfd_exited(process.as_fd()).unwrap());
    assert!(!actor.group(&fixture).exists());
    assert!(!actor.observed.lock().unwrap().socket.parent().unwrap().exists());
    actor.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Root private mount/network namespaces, cgroup v2, ptrace and COS_ORDINARY_TEST_COS/RUNNER"]
async fn ordinary_root_standalone_cancel_retry_publishes_only_after_cleanup() {
    let _environment = crate::test_env::lock_env();
    let fixture = Fixture::new();
    let mut actor = Actor::start(&fixture, false, false);
    actor.ready().await;
    let session = actor.session();
    let mut provider = app_execution::provider(&session, OWNER).unwrap().unwrap();
    let state = fixture.state.clone();
    let request = fixture.request.clone();
    let review = fixture.review.clone();
    let record = fixture.record.clone();
    let pending = tokio::spawn(async move {
        app_store::commit_fixture(&state, OWNER, request, review, record).await
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !group_empty(&actor.group(&fixture)) {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(fixture.version(), "1.0.0");
    assert!(fixture.approved());
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    assert!(crate::clawd::app_mutation::admit(ID).is_err(), "cancelled waiter released active cleanup custody");
    provider.record_result(&Ok(json!({"completed": true})));
    drop(provider);
    wait_fence_release().await;
    assert_eq!(fixture.version(), "1.0.0", "cancelled publication resumed after cleanup");
    let result = fixture.publish().await.unwrap();
    assert_eq!(result["version"], "2.0.0");
    assert_eq!(fixture.version(), "2.0.0");
    assert!(!fixture.approved());
    assert!(!actor.group(&fixture).exists());
    assert!(!actor.observed.lock().unwrap().socket.parent().unwrap().exists());
    actor.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Root private mount/network namespaces, cgroup v2, ptrace, idmapped mounts and COS_ORDINARY_TEST_COS/RUNNER"]
async fn ordinary_root_task_host_retires_namespace_data_before_publication() {
    let _environment = crate::test_env::lock_env();
    let fixture = Fixture::new();
    let mut actor = Actor::start(&fixture, true, false);
    actor.ready().await;
    assert_eq!(actor.task_data.as_ref().unwrap().views.lock().unwrap().len(), 1);
    let result = fixture.publish().await.unwrap();
    assert_eq!(result["version"], "2.0.0");
    assert!(!actor.group(&fixture).exists());
    assert!(actor.task_data.as_ref().unwrap().views.lock().unwrap().is_empty());
    assert!(!actor.observed.lock().unwrap().socket.parent().unwrap().exists());
    actor.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Root private mount/network namespaces, cgroup v2, ptrace and COS_ORDINARY_TEST_COS/RUNNER"]
async fn ordinary_root_indeterminate_provider_blocks_store_publication() {
    let _environment = crate::test_env::lock_env();
    let fixture = Fixture::new();
    let mut actor = Actor::start(&fixture, false, false);
    actor.ready().await;
    let mut provider = app_execution::provider(&actor.session(), OWNER).unwrap().unwrap();
    provider.record_result(&Err(crate::clawd::protocol::BrokerError::indeterminate(
        "synthetic provider has no completion receipt",
    )));
    drop(provider);
    let error = fixture.publish().await.unwrap_err();
    wait_fence_release().await;
    assert!(error.contains("provider completion is unknown"), "{error}");
    assert_eq!(fixture.version(), "1.0.0");
    assert!(fixture.approved());
    assert!(group_empty(&actor.group(&fixture)));
    assert!(app_execution::provider(&actor.session(), OWNER).is_err(), "uncertain custody was dropped");
    actor.finish().await;
}

struct HeldDescendant {
    release: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct HelperProcess(Child);

impl Drop for HelperProcess {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

impl HeldDescendant {
    fn stop(pid: u32) -> Self {
        let (ready, stopped) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            assert_eq!(unsafe {
                libc::ptrace(libc::PTRACE_SEIZE, pid as i32, 0usize, libc::PTRACE_O_TRACEEXIT as usize)
            }, 0, "capture real descendant: {}", std::io::Error::last_os_error());
            assert_eq!(unsafe { libc::ptrace(libc::PTRACE_INTERRUPT, pid as i32, 0usize, 0usize) }, 0);
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let mut status = 0;
                let waited = unsafe { libc::waitpid(pid as i32, &mut status, libc::__WALL | libc::WNOHANG) };
                assert!(waited >= 0);
                if waited == pid as i32 {
                    assert!(libc::WIFSTOPPED(status));
                    break;
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            ready.send(()).unwrap();
            loop {
                if released.try_recv().is_ok() { break; }
                let mut status = 0;
                let waited = unsafe { libc::waitpid(pid as i32, &mut status, libc::__WALL | libc::WNOHANG) };
                assert!(waited >= 0);
                if waited == pid as i32 {
                    assert!(libc::WIFSTOPPED(status));
                    assert_eq!(status >> 16, libc::PTRACE_EVENT_EXIT);
                    // SIGKILL reached the child, but Root must not acknowledge an
                    // alive task still stopped before kernel cgroup exit.
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(unsafe { libc::ptrace(libc::PTRACE_DETACH, pid as i32, 0usize, 0usize) }, 0,
                "release real descendant: {}", std::io::Error::last_os_error());
        });
        stopped.recv_timeout(Duration::from_secs(10)).unwrap();
        Self { release: Some(release), thread: Some(thread) }
    }

    fn release(mut self) {
        self.release.take().unwrap().send(()).unwrap();
        self.thread.take().unwrap().join().unwrap();
    }
}

impl Drop for HeldDescendant {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() { let _ = release.send(()); }
        if let Some(thread) = self.thread.take() {
            if let Err(error) = thread.join() {
                eprintln!("fixture descendant tracer failed during teardown: {error:?}");
            }
        }
    }
}

async fn grandchild_pid(group: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let members = std::fs::read_to_string(group.join("cgroup.procs")).unwrap();
        for member in members.split_whitespace() {
            let pid = member.parse::<u32>().unwrap();
            if std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .is_ok_and(|name| name.trim() == "custody-grand")
            { return pid; }
        }
        assert!(Instant::now() < deadline, "App did not create its real process-group-escaping child");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Root private mount/network namespaces, cgroup v2, ptrace and COS_ORDINARY_TEST_COS/RUNNER"]
async fn ordinary_root_live_grandchild_blocks_publication_until_checked_retry() {
    let _environment = crate::test_env::lock_env();
    let fixture = Fixture::new();
    let mut actor = Actor::start(&fixture, false, true);
    actor.ready().await;
    let group = actor.group(&fixture);
    let pid = grandchild_pid(&group).await;
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    assert!(raw >= 0);
    let process = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    let held = HeldDescendant::stop(pid);
    let error = fixture.publish().await.unwrap_err();
    assert!(error.contains("pending") || error.contains("populated"), "{error}");
    assert_eq!(fixture.version(), "1.0.0");
    assert!(fixture.approved());
    assert!(!claw_display_control::identity::pidfd_exited(process.as_fd()).unwrap());
    assert!(!group_empty(&group));
    assert!(app_execution::provider(&actor.session(), OWNER).is_err());
    held.release();
    wait_fence_release().await;
    fixture.publish().await.unwrap();
    assert_eq!(fixture.version(), "2.0.0");
    assert!(!group.exists());
    actor.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Root private mount/network namespaces, cgroup v2, ptrace and COS_ORDINARY_TEST_COS/RUNNER"]
async fn ordinary_root_foreign_and_lost_ack_cannot_authorize_publication() {
    let _environment = crate::test_env::lock_env();
    let fixture = Fixture::new();
    let mut actor = Actor::start(&fixture, false, false);
    actor.ready().await;
    let original_pid = actor.child.id();
    let socket = actor.observed.lock().unwrap().socket.clone();
    actor.kill_host();
    std::fs::remove_file(&socket).unwrap();
    let mut command = std::process::Command::new(Path::new(BASE).join("actor"));
    command.args(["--exact", FOREIGN, "--ignored", "--nocapture", "--test-threads=1"])
        .env_clear().env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", format!("{BASE}/home/custody-fixture-owner"))
        .env("COS_ORDINARY_CUSTODY_ACTOR", "1")
        .env("COS_ORDINARY_CUSTODY_FOREIGN_SOCKET", &socket)
        .stdin(Stdio::piped()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
    unsafe {
        command.pre_exec(|| {
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setresgid(OWNER, OWNER, OWNER) != 0
                || libc::setresuid(OWNER, OWNER, OWNER) != 0
                || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
            { return Err(std::io::Error::last_os_error()); }
            Ok(())
        });
    }
    let mut foreign = HelperProcess(command.spawn().unwrap());
    assert_ne!(foreign.0.id(), original_pid);
    foreign.0.stdin.as_mut().unwrap().write_all(&[1]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // The replacement really returns a positive byte, but comes from another
    // same-UID process and is not Root's pinned OS Host connection.
    let mut spoof = std::os::unix::net::UnixStream::connect(&socket).unwrap();
    spoof.write_all(&[1]).unwrap();
    let mut positive = [0];
    spoof.read_exact(&mut positive).unwrap();
    assert_eq!(positive, [1]);
    let error = crate::worker::app_custody::connect(&socket, original_pid, OWNER)
        .await.err().expect("foreign cleanup peer was accepted");
    assert!(error.contains("authenticated launcher"), "{error}");
    let result = fixture.publish().await;
    wait_fence_release().await;
    foreign.0.kill().unwrap();
    foreign.0.wait().unwrap();
    assert!(result.is_err(), "lost genuine acknowledgement was replaced by a foreign positive reply");
    assert_eq!(fixture.version(), "1.0.0");
    assert!(fixture.approved());
    assert!(group_empty(&actor.group(&fixture)));
    assert!(app_execution::provider(&actor.session(), OWNER).is_err(), "lost-ack custody was discarded");
    actor.finish().await;
}
