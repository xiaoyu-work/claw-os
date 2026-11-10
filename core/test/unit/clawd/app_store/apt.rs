// Actual APT/dpkg acceptance after synthetic catalog selection and publisher trust setup.
// The signed repository uses private loopback HTTP, not production HTTPS/catalog keys.
// This does not exercise live accounts, App execution, active Host retirement, UI consent
// or publication readiness. Root supplies decisions to the existing protected review store;
// neither presentation flags nor digests approve. Run each main case in separate Root
// mount/network namespaces through `scripts/ci/test-boundaries.sh app-apt`.

use super::*;
use crate::clawd::app_store as controller;
use crate::clawd::protocol::BrokerErrorKind;
use crate::clawd::state::DaemonState;
use crate::clawd::transport::{frame::PeerStream, peer, ReadOutcome};
use crate::clawd::wire::Request;
use crate::provenance::trust::TrustRootSpec;
use crate::provenance::{sign, PackageKind, TrustStore, TrustTier};
use std::io::Read;
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

const BASE: &str = "/run/cos-app-apt-fixture";
const OWNER: u32 = 61994;
const FOREIGN_UID: u32 = 61995;
const APPS: [&str; 2] = ["apt-fixture-alpha", "apt-fixture-beta"];
const BUNDLE: &str = "claw-app-acceptance-bundle";
const SUPPORT: &str = "claw-app-acceptance-support";
const SUPPORT_PATH: &str = "/usr/lib/cos/python/fixture_support.py";
const SUPPORT_BYTES: &[u8] = b"VALUE = 'separately dpkg-owned fixture support'\n";
const PROBE: &str = "clawd::app_store::apt::tests::private_apt_foreign_uid_probe";
const PLUMBING: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/test/unit/clawd/app_store/repository.py"
);

fn checked(command: &mut std::process::Command) -> Vec<u8> {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{command:?}: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn plumbing(action: &str) {
    checked(
        std::process::Command::new("/usr/bin/python3")
            .args(["-I", "-B", PLUMBING, action])
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin"),
    );
}

fn copy_executable(source: &Path, destination: &Path) {
    std::fs::copy(source, destination).unwrap_or_else(|error| {
        panic!(
            "copy actual binary {} ({} bytes): {error}",
            source.display(),
            source.metadata().unwrap().len()
        )
    });
    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(destination.metadata().unwrap().uid(), 0);
}

fn parent(pid: u32) -> u32 {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("PPid:"))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn sign_app(directory: &Path, id: &str, key: &sign::SigningKeyFile) {
    std::fs::create_dir_all(directory.join("web")).unwrap();
    std::fs::write(
        directory.join("main.py"),
        "def run(command, args):\n    return {'fixture': True}\n",
    )
    .unwrap();
    std::fs::write(
        directory.join("app.json"),
        json!({
            "schema_version": 2, "id": id, "version": "1.0.0", "runtime": "python",
            "name": {"en": id}, "summary": {"en": "Private APT acceptance App"},
            "operations": {"probe": {
                "label": {"en": "Probe"}, "needs": [], "args": []
            }}
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        directory.join("web-ui.json"),
        json!({
            "schema": "claw.web-app/v1", "app_id": id,
            "source_kind": "product", "source_name": "apt-fixture",
            "title": id, "description": "Private APT acceptance App",
            "entry": "web/index.html"
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        directory.join("web/index.html"),
        "<!doctype html><title>Private APT fixture</title>",
    )
    .unwrap();
    sign::sign_directory(
        directory,
        &sign::SignRequest {
            kind: PackageKind::App,
            id: id.into(),
            version: "1.0.0".into(),
            manifest_schema: "cos.app/v2".into(),
            manifest_path: "app.json".into(),
            entrypoints: vec!["main.py".into()],
            resources: vec!["web-ui.json".into(), "web/index.html".into()],
        },
        key,
    )
    .unwrap();
}

fn record(directory: &Path, package_sha256: &str) -> store::Record {
    let package = crate::provenance::verify::verify_package(
        directory,
        &crate::provenance::VerifyOptions::new(PackageKind::App),
        &crate::provenance::trust_store(),
    )
    .unwrap();
    let manifest = std::fs::read(directory.join("app.json")).unwrap();
    let descriptor = std::fs::read(directory.join("web-ui.json")).unwrap();
    let envelope = std::fs::read(directory.join(".provenance.json")).unwrap();
    let mut snapshot = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::default(),
    ));
    snapshot.append_dir_all("package", directory).unwrap();
    let snapshot = snapshot.into_inner().unwrap().finish().unwrap();
    let checksum = store::sha256(&snapshot);
    let record = store::Record {
        app_id: package.id().into(),
        app_version: "1.0.0".into(),
        architecture: "all".into(),
        artifact: store::Artifact {
            file: format!("claw-app-snapshot-{checksum}.tar.gz"),
            size: snapshot.len() as u64,
            sha256: checksum,
            media_type: "application/gzip".into(),
        },
        manifest: store::ManifestBinding {
            path: "app.json".into(),
            size: manifest.len() as u64,
            sha256: store::sha256(&manifest),
        },
        provenance: store::ProvenanceBinding {
            path: ".provenance.json".into(),
            sha256: store::sha256(&envelope),
            content_digest: package.content_digest().into(),
            publisher_key_id: package.audit_facts()["publisher_key_id"]
                .as_str()
                .unwrap()
                .into(),
        },
        release_version: "1.0.0".into(),
        release_tag: "app-apt-fixture-v1.0.0".into(),
        display: store::Display {
            name: package.id().into(),
            description: "Private APT acceptance App".into(),
            source_kind: "product".into(),
            source_name: "apt-fixture".into(),
        },
        web_ui: store::WebBinding {
            path: "web-ui.json".into(),
            sha256: store::sha256(&descriptor),
            entry: "web/index.html".into(),
        },
        delivery: store::AptDelivery {
            kind: "apt".into(),
            package_sha256: package_sha256.into(),
        },
    };
    record.verify_package(&package).unwrap();
    record
}

struct RootGroup(PathBuf);

impl Drop for RootGroup {
    fn drop(&mut self) {
        fn remove_owned(path: &Path) {
            let fd = claw_display_control::workload::open_directory(path).unwrap();
            claw_display_control::workload::check_cgroup(fd.as_fd()).unwrap();
            claw_display_control::workload::retire_cgroup(
                fd.as_fd(),
                std::time::Instant::now() + Duration::from_secs(5),
            )
            .unwrap();
            for entry in std::fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    remove_owned(&entry.path());
                }
            }
            std::fs::remove_dir(path).unwrap();
        }
        remove_owned(&self.0);
    }
}

struct LocalRepository(std::process::Child);

impl LocalRepository {
    fn start() -> Self {
        let log = std::fs::File::create(Path::new(BASE).join("http.log")).unwrap();
        let mut command = std::process::Command::new("/usr/bin/python3");
        command
            .args(["-I", "-B", PLUMBING, "serve"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log);
        let parent = std::process::id() as libc::pid_t;
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0
                    || libc::getppid() != parent
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        let mut repository = Self(child);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let address = "127.0.0.1:38081".parse().unwrap();
        loop {
            assert!(
                repository.0.try_wait().unwrap().is_none(),
                "private HTTP repository exited"
            );
            if let Ok(mut stream) =
                std::net::TcpStream::connect_timeout(&address, Duration::from_millis(100))
            {
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream
                    .write_all(b"GET /dists/stable/InRelease HTTP/1.0\r\nHost: localhost\r\n\r\n")
                    .unwrap();
                let mut response = Vec::new();
                stream.take(16384).read_to_end(&mut response).unwrap();
                assert!(response.starts_with(b"HTTP/1.0 200"), "{response:?}");
                assert!(String::from_utf8_lossy(&response).contains("BEGIN PGP SIGNED MESSAGE"));
                return repository;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "private HTTP repository not ready"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for LocalRepository {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
        }
        self.0.wait().unwrap();
    }
}

#[derive(Clone, Copy, Debug)]
enum Substitution {
    Archive,
    Plan,
    Status,
}

fn substituted_status() -> &'static [u8] {
    b"Package: private-status-substitution\nStatus: install ok installed\nArchitecture: all\nVersion: 1.0.0\nDescription: Fixture concurrent status generation\n\n"
}

fn substitute(id: &str, substitution: Substitution) {
    let mut pending = controller::prepared(id, OWNER).unwrap();
    match substitution {
        Substitution::Archive => {
            let file = &pending.apt.as_ref().unwrap().packages[0].file;
            let path = directory(id).unwrap().join("archives").join(file);
            let mut bytes = std::fs::read(&path).unwrap();
            bytes[0] ^= 1;
            std::fs::write(path, bytes).unwrap();
        }
        Substitution::Plan => {
            pending.apt.as_mut().unwrap().digest = store::sha256(b"substituted delivery plan");
            controller::write_json(&controller::request_path(id).unwrap(), &pending).unwrap();
        }
        Substitution::Status => {
            std::fs::write("/var/lib/dpkg/status", substituted_status()).unwrap();
        }
    }
}

struct HeldHook {
    pid: u32,
    files: Vec<String>,
}

enum HookAction {
    Substitute(Substitution),
    HoldAndLoseReply {
        arrived: tokio::sync::oneshot::Sender<HeldHook>,
        release: tokio::sync::oneshot::Receiver<()>,
    },
}

#[derive(Debug)]
struct Observation {
    uid: u32,
    pid: u32,
    parent: u32,
    executable: PathBuf,
    ok: bool,
    code: Option<String>,
    message: Option<String>,
}

struct Broker {
    action: Arc<Mutex<Option<HookAction>>>,
    observed: Arc<Mutex<Vec<Observation>>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Broker {
    fn start(
        state: DaemonState,
        admission: Arc<crate::clawd::transport::limits::Admission>,
    ) -> Self {
        let listener = tokio::net::UnixListener::bind("/run/cos/clawd.sock").unwrap();
        peer::enable_credential_passing(listener.as_raw_fd()).unwrap();
        std::fs::set_permissions(
            "/run/cos/clawd.sock",
            std::fs::Permissions::from_mode(0o666),
        )
        .unwrap();
        let action: Arc<Mutex<Option<HookAction>>> = Arc::new(Mutex::new(None));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let next = action.clone();
        let captured = observed.clone();
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let accepted = tokio::select! {
                    _ = &mut stopped => break,
                    completed = connections.join_next(), if !connections.is_empty() => {
                        completed.unwrap().unwrap();
                        continue;
                    }
                    accepted = listener.accept() => accepted.unwrap().0,
                };
                let state = state.clone();
                let admission = admission.clone();
                let next = next.clone();
                let captured = captured.clone();
                connections.spawn(async move {
                    let mut stream = PeerStream::new(accepted).unwrap();
                    let ReadOutcome::Frame(frame) = stream
                        .read_request(crate::clawd::wire::MAX_REQUEST_BYTES)
                        .await
                        .unwrap()
                    else {
                        panic!("expected a real hook/probe request");
                    };
                    let credentials = peer::verify(frame.credentials).unwrap();
                    let uid = credentials.uid;
                    let pid = credentials.pid;
                    let parent = parent(pid);
                    let executable = std::fs::read_link(format!("/proc/{pid}/exe")).unwrap();
                    let client = ClientIdentity::from_peer(credentials);
                    let request: Request = serde_json::from_slice(&frame.body).unwrap();
                    assert_eq!(
                        request.command,
                        crate::clawd::routes::Command::AppStoreAptHook
                    );
                    let action = next.lock().unwrap().take();
                    let mut lose_reply = false;
                    match action {
                        Some(HookAction::Substitute(substitution)) => {
                            substitute(
                                request.params["request_id"].as_str().unwrap(),
                                substitution,
                            );
                        }
                        Some(HookAction::HoldAndLoseReply { arrived, release }) => {
                            arrived
                                .send(HeldHook {
                                    pid,
                                    files: serde_json::from_value(request.params["files"].clone())
                                        .unwrap(),
                                })
                                .unwrap_or_else(|_| panic!("fixture stopped before hook arrival"));
                            release.await.unwrap();
                            lose_reply = true;
                        }
                        None => {}
                    }
                    let response = crate::clawd::server::dispatch_verified_request(
                        request, &client, &state, &admission,
                    )
                    .await;
                    captured.lock().unwrap().push(Observation {
                        uid,
                        pid,
                        parent,
                        executable,
                        ok: response.ok,
                        code: response.error.as_ref().map(|error| error.code.clone()),
                        message: response.error.as_ref().map(|error| error.message.clone()),
                    });
                    if !lose_reply {
                        stream
                            .write_response(
                                &crate::clawd::protocol::encode_response(&response).unwrap(),
                            )
                            .await
                            .unwrap();
                    }
                });
            }
            while let Some(completed) = connections.join_next().await {
                completed.unwrap();
            }
        });
        Self {
            action,
            observed,
            stop: Some(stop),
            task: Some(task),
        }
    }

    fn count(&self) -> usize {
        self.observed.lock().unwrap().len()
    }

    async fn shutdown(&mut self) {
        self.stop.take().unwrap().send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(10), self.task.take().unwrap())
            .await
            .unwrap()
            .unwrap();
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

struct Fixture {
    state: DaemonState,
    catalog: store::Catalog,
    server: Broker,
    _repository: LocalRepository,
    _services: Arc<crate::clawd::app_services::AppServiceManager>,
    _group: RootGroup,
    _environment: Vec<crate::test_env::TestEnvVarGuard>,
}

impl Fixture {
    fn new() -> Self {
        assert_eq!(unsafe { libc::getuid() }, 0);
        assert_eq!(unsafe { libc::geteuid() }, 0);
        for uid in [OWNER, FOREIGN_UID] {
            assert!(
                unsafe { libc::getpwuid(uid) }.is_null(),
                "never borrow a real account"
            );
        }
        let helper = PathBuf::from(
            std::env::var_os("COS_APP_APT_TEST_HOOK")
                .expect("compiled claw-app-apt-hook is required"),
        );
        plumbing("isolate");
        copy_executable(&helper, Path::new(HOOK));
        copy_executable(
            &std::env::current_exe().unwrap(),
            &Path::new(BASE).join("probe"),
        );
        std::fs::write(
            "/usr/lib/cos/app-apt-metadata.py",
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../tools/app_apt_metadata.py"
            )),
        )
        .unwrap();
        std::fs::create_dir_all("/run/cos").unwrap();
        std::fs::create_dir_all("/run/lock").unwrap();
        std::fs::set_permissions("/run/lock", std::fs::Permissions::from_mode(0o1777)).unwrap();
        let group = Path::new("/sys/fs/cgroup").join(format!(
            "cos-app-apt-fixture-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir(&group).unwrap();
        let group = RootGroup(group);
        let environment = vec![
            crate::test_env::TestEnvVarGuard::set("CLAWD_EXTENSION_CGROUP_ROOT", &group.0),
            crate::test_env::TestEnvVarGuard::set("COS_EXTENSION_EXEC_GROUP", "nogroup"),
            crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", format!("{BASE}/state")),
            crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", format!("{BASE}/state")),
            crate::test_env::TestEnvVarGuard::set("COS_RUNTIME_DIR", format!("{BASE}/runtime")),
            crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", "/var/log/cos"),
            crate::test_env::TestEnvVarGuard::set("COS_APPS_DIR", "/usr/lib/cos/apps"),
            crate::test_env::TestEnvVarGuard::set("CLAWD_SOCKET", "/run/cos/clawd.sock"),
        ];
        let roots = vec![TrustRootSpec {
            path: Path::new(BASE).join("trust"),
            tier: TrustTier::System,
            allowed_uids: vec![0],
            domain: crate::provenance::state::TrustDomain::System,
        }];
        for id in APPS {
            let key = sign::SigningKeyFile::generate(Some(format!("isolated {id}"))).unwrap();
            std::fs::write(
                roots[0].path.join(format!("{id}.json")),
                serde_json::to_vec(&key.trust_entry(&[PackageKind::App])).unwrap(),
            )
            .unwrap();
            sign_app(
                &Path::new(BASE)
                    .join("payloads/bundle/usr/lib/cos/apps")
                    .join(id),
                id,
                &key,
            );
        }
        crate::test_env::record_trust_state(&roots);
        let trust = TrustStore::load_roots(&roots);
        assert!(trust.diagnostics().is_empty(), "{:?}", trust.diagnostics());
        crate::provenance::set_trust_store_for_roots(trust, roots);
        let support = Path::new(BASE).join("payloads/support/usr/lib/cos/python");
        std::fs::create_dir_all(&support).unwrap();
        std::fs::write(support.join("fixture_support.py"), SUPPORT_BYTES).unwrap();
        plumbing("repository");
        let repository = LocalRepository::start();
        plumbing("update");
        let inventory = [BUNDLE, SUPPORT]
            .into_iter()
            .map(|name| {
                let path = Path::new(BASE)
                    .join("repo/pool")
                    .join(format!("{name}_1.0.0_all.deb"));
                let (size, sha256) = debian::file_binding(&path).unwrap();
                store::AptPackage {
                    package: name.into(),
                    version: "1.0.0".into(),
                    architecture: "all".into(),
                    size,
                    sha256,
                    apps: if name == BUNDLE {
                        APPS.into_iter().map(str::to_string).collect()
                    } else {
                        vec![]
                    },
                }
            })
            .collect::<Vec<_>>();
        let now = chrono::Utc::now();
        let catalog = store::Catalog {
            schema: "claw.app-store/v2".into(),
            repository: store::REPOSITORY.into(),
            runtime_abi: 1,
            generation: "1".into(),
            issued_at: now,
            expires_at: now + chrono::Duration::hours(1),
            apps: APPS
                .into_iter()
                .map(|id| {
                    record(
                        &Path::new(BASE)
                            .join("payloads/bundle/usr/lib/cos/apps")
                            .join(id),
                        &inventory[0].sha256,
                    )
                })
                .collect(),
            packages: inventory,
        };
        let catalog = store::Catalog::decode(&serde_json::to_vec(&catalog).unwrap(), now).unwrap();
        controller::initialize().unwrap();
        let state = DaemonState::try_new().unwrap();
        let admission = crate::clawd::transport::limits::Admission::new(Default::default());
        let broker = crate::agentd::supervisor::BrokerContext::new(
            state.clone(),
            admission.clone(),
            Path::new("/run/cos/clawd.sock").into(),
        )
        .unwrap();
        broker.extension_containment().unwrap();
        crate::clawd::app_execution::initialize(&broker);
        crate::clawd::gui::Manager::start(state.clone(), admission.clone()).unwrap();
        let services = crate::clawd::app_services::AppServiceManager::new(broker);
        state.install_app_service_manager(&services).unwrap();
        let server = Broker::start(state.clone(), admission);
        Self {
            state,
            catalog,
            server,
            _repository: repository,
            _services: services,
            _group: group,
            _environment: environment,
        }
    }

    async fn prepare(&self) -> String {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let record = self.catalog.apps[0].clone();
        let plan = super::prepare(OWNER, &id, &record, &self.catalog)
            .await
            .unwrap();
        assert_eq!(plan.reviews.len(), 2);
        assert_eq!(
            plan.packages
                .iter()
                .map(|package| package.identity.package.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([BUNDLE, SUPPORT])
        );
        assert!(plan
            .packages
            .iter()
            .all(|package| package.inventory.is_some()));
        let pending = Prepared {
            owner_uid: OWNER,
            request_id: id.clone(),
            review_id: plan.reviews[0].review_id.clone(),
            record,
            expires_at: chrono::Utc::now() + chrono::Duration::minutes(30),
            phase: "ready".into(),
            apt: Some(plan),
        };
        controller::write_json(&controller::request_path(&id).unwrap(), &pending).unwrap();
        id
    }

    fn pending(&self, id: &str) -> Prepared {
        controller::prepared(id, OWNER).unwrap()
    }

    fn approve(&self, id: &str, count: usize) {
        for review in self.pending(id).apt.unwrap().reviews.iter().take(count) {
            let package = verify_source(review).unwrap();
            let approved = reviews::decide(OWNER, &review.review_id, true, Some(&package)).unwrap();
            assert_eq!(
                approved.effective_state().unwrap(),
                reviews::ReviewState::Approved
            );
        }
    }

    fn review_states(&self, id: &str) -> Vec<reviews::ReviewState> {
        self.pending(id)
            .apt
            .unwrap()
            .reviews
            .iter()
            .map(|review| {
                reviews::get(OWNER, &review.review_id)
                    .unwrap()
                    .effective_state()
                    .unwrap()
            })
            .collect()
    }

    async fn commit(&self, id: &str) -> Result<Value, BrokerError> {
        controller::commit_apt_fixture(&self.state, OWNER, id.into(), self.catalog.clone()).await
    }

    fn assert_installed(&self) {
        let discovery = crate::apps::discover_all(Path::new("/usr/lib/cos/apps"));
        assert!(discovery.quarantined.is_empty());
        assert_eq!(
            discovery
                .verified
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            APPS
        );
        for record in &self.catalog.apps {
            let app = discovery.verified.get(&record.app_id).unwrap();
            record
                .verify_package(app.require_verified().unwrap())
                .unwrap();
            for file in [
                "app.json",
                ".provenance.json",
                "main.py",
                "web-ui.json",
                "web/index.html",
            ] {
                assert_eq!(
                    std::fs::read(app.dir.join(file)).unwrap(),
                    std::fs::read(
                        Path::new(BASE)
                            .join("payloads/bundle/usr/lib/cos/apps")
                            .join(&record.app_id)
                            .join(file)
                    )
                    .unwrap()
                );
            }
            let manifest = format!("/usr/lib/cos/apps/{}/app.json", record.app_id);
            assert_eq!(
                checked(std::process::Command::new("/usr/bin/dpkg-query").args(["-S", &manifest])),
                format!("{BUNDLE}: {manifest}\n").as_bytes()
            );
        }
        assert_eq!(std::fs::read(SUPPORT_PATH).unwrap(), SUPPORT_BYTES);
        assert_eq!(
            checked(std::process::Command::new("/usr/bin/dpkg-query").args(["-S", SUPPORT_PATH])),
            format!("{SUPPORT}: {SUPPORT_PATH}\n").as_bytes()
        );
        let bundle_files =
            checked(std::process::Command::new("/usr/bin/dpkg-query").args(["-L", BUNDLE]));
        assert!(!String::from_utf8(bundle_files)
            .unwrap()
            .lines()
            .any(|line| line == SUPPORT_PATH));
        for package in [BUNDLE, SUPPORT] {
            assert_eq!(
                checked(std::process::Command::new("/usr/bin/dpkg-query").args([
                    "--show",
                    "--showformat=${Status}\\t${Version}\\t${Architecture}",
                    "--",
                    package,
                ])),
                b"install ok installed\t1.0.0\tall"
            );
        }
        assert!(checked(std::process::Command::new("/usr/bin/dpkg").arg("--audit")).is_empty());
    }

    fn assert_no_installation(&self) {
        assert!(crate::apps::discover_all(Path::new("/usr/lib/cos/apps"))
            .verified
            .is_empty());
        assert!(!Path::new(SUPPORT_PATH).exists());
        for package in [BUNDLE, SUPPORT] {
            assert!(!Path::new("/var/lib/dpkg/info")
                .join(format!("{package}.list"))
                .exists());
        }
    }

    async fn finish(mut self) {
        self.server.shutdown().await;
    }
}

fn dpkg_state() -> BTreeMap<PathBuf, Vec<u8>> {
    fn read(path: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                read(&entry.path(), result);
            } else {
                result.insert(entry.path(), std::fs::read(entry.path()).unwrap());
            }
        }
    }
    let mut result = BTreeMap::new();
    read(Path::new("/var/lib/dpkg"), &mut result);
    if Path::new("/var/log/dpkg.log").exists() {
        result.insert(
            "/var/log/dpkg.log".into(),
            std::fs::read("/var/log/dpkg.log").unwrap(),
        );
    }
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires actual Root mount/network namespaces, cgroups, python3-apt and COS_APP_APT_TEST_HOOK"]
async fn private_apt_bundle_requires_both_reviews_and_owns_shared_dependency() {
    let _lock = crate::test_env::lock_env();
    let fixture = Fixture::new();
    let id = fixture.prepare().await;
    let before = dpkg_state();
    fixture.approve(&id, 1);
    let refusal = fixture.commit(&id).await.unwrap_err();
    assert_eq!(refusal.kind, BrokerErrorKind::Unauthorized);
    assert!(refusal.message.contains("Every App"));
    assert_eq!(
        fixture.server.count(),
        0,
        "one review must not even dispatch APT"
    );
    assert_eq!(dpkg_state(), before);
    fixture.assert_no_installation();
    let pending = fixture.pending(&id);
    let second = &pending.apt.as_ref().unwrap().reviews[1];
    let package = verify_source(second).unwrap();
    reviews::decide(OWNER, &second.review_id, true, Some(&package)).unwrap();
    assert_eq!(fixture.commit(&id).await.unwrap()["status"], "installed");
    fixture.assert_installed();
    assert_eq!(
        fixture.review_states(&id),
        vec![reviews::ReviewState::Consumed; 2]
    );
    let observed = fixture.server.observed.lock().unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].uid, 0);
    assert_eq!(observed[0].executable, Path::new(HOOK));
    assert!(observed[0].ok, "{observed:?}");
    drop(observed);
    let installed = dpkg_state();
    assert_eq!(fixture.commit(&id).await.unwrap()["recovered"], true);
    assert_eq!(dpkg_state(), installed);
    assert_eq!(
        fixture.server.count(),
        1,
        "checked recovery must not dispatch APT"
    );
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires actual Root mount/network namespaces, cgroups, python3-apt and COS_APP_APT_TEST_HOOK"]
async fn private_apt_refuses_substituted_bytes_plan_and_status_before_dpkg() {
    let _lock = crate::test_env::lock_env();
    let fixture = Fixture::new();
    for (substitution, expected) in [
        (Substitution::Archive, "archive changed"),
        (Substitution::Plan, "delivery plan differs"),
        (Substitution::Status, "APT state"),
    ] {
        let id = fixture.prepare().await;
        fixture.approve(&id, 2);
        let mut before = dpkg_state();
        *fixture.server.action.lock().unwrap() = Some(HookAction::Substitute(substitution));
        assert!(fixture.commit(&id).await.is_err(), "{substitution:?}");
        if matches!(substitution, Substitution::Status) {
            before.insert("/var/lib/dpkg/status".into(), substituted_status().to_vec());
        }
        assert_eq!(
            dpkg_state(),
            before,
            "{substitution:?}: dpkg must not have run"
        );
        fixture.assert_no_installation();
        assert!(!fixture.pending(&id).apt.unwrap().hook_consumed);
        assert_eq!(
            fixture.review_states(&id),
            vec![reviews::ReviewState::Approved; 2]
        );
        let observed = fixture.server.observed.lock().unwrap();
        let refusal = observed.last().unwrap();
        assert!(!refusal.ok, "{observed:?}");
        assert_eq!(refusal.code.as_deref(), Some("not_authorized"));
        assert!(
            refusal.message.as_deref().unwrap().contains(expected),
            "{observed:?}"
        );
    }
    fixture.finish().await;
}

#[test]
#[ignore = "private dropped-UID actor of private_apt_hook_identity_and_lost_reply_never_replay"]
fn private_apt_foreign_uid_probe() {
    assert_eq!(std::env::var("COS_APP_APT_UID_PROBE").unwrap(), "1");
    assert_eq!(unsafe { libc::getuid() }, FOREIGN_UID);
    assert_eq!(unsafe { libc::geteuid() }, FOREIGN_UID);
    let response = crate::clawd::client::request_blocking(
        crate::clawd::config::socket_path(),
        Request::build(
            crate::clawd::routes::Command::AppStoreAptHook,
            json!({
                "request_id": std::env::var("COS_APP_APT_PROBE_REQUEST").unwrap(),
                "files": serde_json::from_str::<Value>(
                    &std::env::var("COS_APP_APT_PROBE_FILES").unwrap()
                ).unwrap()
            }),
        ),
    )
    .unwrap();
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().code, "not_authorized");
}

async fn child_output(
    command: std::process::Command,
    input: Option<Vec<u8>>,
) -> std::process::Output {
    let mut command = tokio::process::Command::from(command);
    command.kill_on_drop(true);
    let mut child = command.spawn().unwrap();
    if let Some(bytes) = input {
        child.stdin.take().unwrap().write_all(&bytes).await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires actual Root mount/network namespaces, cgroups, python3-apt and COS_APP_APT_TEST_HOOK"]
async fn private_apt_hook_identity_and_lost_reply_never_replay() {
    let _lock = crate::test_env::lock_env();
    let fixture = Fixture::new();
    let id = fixture.prepare().await;
    fixture.approve(&id, 2);
    let before = dpkg_state();
    let (arrived, waiting) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    *fixture.server.action.lock().unwrap() = Some(HookAction::HoldAndLoseReply {
        arrived,
        release: released,
    });
    let state = fixture.state.clone();
    let catalog = fixture.catalog.clone();
    let request = id.clone();
    let commit = tokio::spawn(async move {
        controller::commit_apt_fixture(&state, OWNER, request, catalog).await
    });
    let held = tokio::time::timeout(Duration::from_secs(30), waiting)
        .await
        .unwrap()
        .unwrap();
    let context = hooks().lock().unwrap().get(&id).unwrap().clone();
    assert_eq!(parent(held.pid), context.pid);
    assert_eq!(
        std::fs::read_link(format!("/proc/{}/exe", context.pid)).unwrap(),
        Path::new("/usr/bin/apt-get")
    );
    assert_eq!(
        crate::proc::read_start_time_ticks_pub(context.pid),
        Some(context.start)
    );
    assert!(
        crate::clawd::app_mutation::admit("apt-fixture-unrelated").is_err(),
        "the actual APT frontend lock must block unrelated new App admissions"
    );
    let mut probe = std::process::Command::new(Path::new(BASE).join("probe"));
    probe
        .args([
            "--exact",
            PROBE,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", BASE)
        .env("CLAWD_SOCKET", "/run/cos/clawd.sock")
        .env("COS_APP_APT_UID_PROBE", "1")
        .env("COS_APP_APT_PROBE_REQUEST", &id)
        .env(
            "COS_APP_APT_PROBE_FILES",
            serde_json::to_string(&held.files).unwrap(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        probe.pre_exec(|| {
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setresgid(FOREIGN_UID, FOREIGN_UID, FOREIGN_UID) != 0
                || libc::setresuid(FOREIGN_UID, FOREIGN_UID, FOREIGN_UID) != 0
                || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            crate::agentd::spawn::mark_inherited_descriptors_cloexec(3);
            Ok(())
        });
    }
    let output = child_output(probe, None).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut foreign_parent = std::process::Command::new(HOOK);
    foreign_parent
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", BASE)
        .env("CLAWD_SOCKET", "/run/cos/clawd.sock")
        .env("CLAW_APP_STORE_REQUEST", &id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = child_output(
        foreign_parent,
        Some(format!("{}\n", held.files.join("\n")).into_bytes()),
    )
    .await;
    assert!(
        !output.status.success(),
        "a Root helper with the wrong parent must fail"
    );
    assert_eq!(dpkg_state(), before);
    fixture.assert_no_installation();
    assert!(!fixture.pending(&id).apt.unwrap().hook_consumed);
    {
        let observed = fixture.server.observed.lock().unwrap();
        assert_eq!(observed.len(), 2, "{observed:?}");
        assert_eq!(observed[0].uid, FOREIGN_UID);
        assert_eq!(observed[0].code.as_deref(), Some("not_authorized"));
        assert_eq!(observed[1].uid, 0);
        assert_eq!(observed[1].executable, Path::new(HOOK));
        assert_ne!(observed[1].parent, context.pid);
        assert!(observed[1]
            .message
            .as_deref()
            .unwrap()
            .contains("transaction child"));
    }
    release.send(()).unwrap();
    let refusal = tokio::time::timeout(Duration::from_secs(30), commit)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(refusal.kind, BrokerErrorKind::Indeterminate);
    let pending = fixture.pending(&id);
    assert_eq!(pending.phase, "indeterminate");
    assert!(pending.apt.unwrap().hook_consumed);
    assert_eq!(
        fixture.review_states(&id),
        vec![reviews::ReviewState::Consumed; 2]
    );
    assert_eq!(
        dpkg_state(),
        before,
        "lost hook acknowledgement must stop dpkg"
    );
    fixture.assert_no_installation();
    for app in APPS {
        assert!(crate::clawd::app_mutation::admit(app).is_err());
    }
    let observed = fixture.server.count();
    assert_eq!(
        fixture.commit(&id).await.unwrap_err().kind,
        BrokerErrorKind::Indeterminate
    );
    assert_eq!(
        fixture.server.count(),
        observed,
        "recovery cannot replay consumed reviews"
    );
    assert_eq!(dpkg_state(), before);
    let observed = fixture.server.observed.lock().unwrap();
    let accepted = observed.last().unwrap();
    assert_eq!(accepted.pid, held.pid);
    assert!(accepted.ok, "{observed:?}");
    drop(observed);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires actual Root mount/network namespaces, cgroups, python3-apt and COS_APP_APT_TEST_HOOK"]
async fn private_apt_completed_dispatch_failure_recovers_read_only() {
    let _lock = crate::test_env::lock_env();
    let fixture = Fixture::new();
    let id = fixture.prepare().await;
    fixture.approve(&id, 2);
    // This is private OS APT configuration, not an App maintainer script or an approval hook.
    std::fs::write(
        "/etc/apt/apt.conf.d/99fixture-post-dpkg",
        "DPkg::Post-Invoke { \"/usr/bin/false\"; };\n",
    )
    .unwrap();
    assert_eq!(
        fixture.commit(&id).await.unwrap_err().kind,
        BrokerErrorKind::Indeterminate
    );
    let pending = fixture.pending(&id);
    assert_eq!(pending.phase, "indeterminate");
    assert!(pending.apt.unwrap().hook_consumed);
    assert_eq!(
        fixture.review_states(&id),
        vec![reviews::ReviewState::Consumed; 2]
    );
    fixture.assert_installed();
    for app in APPS {
        assert!(crate::clawd::app_mutation::admit(app).is_err());
    }
    let installed = dpkg_state();
    let dispatched = fixture.server.count();
    assert_eq!(dispatched, 1);
    let recovered = fixture.commit(&id).await.unwrap();
    assert_eq!(recovered["status"], "installed");
    assert_eq!(recovered["recovered"], true);
    assert_eq!(dpkg_state(), installed, "exact recovery is read-only");
    assert_eq!(fixture.server.count(), dispatched);
    assert_eq!(fixture.pending(&id).phase, "installed");
    for app in APPS {
        assert!(crate::clawd::app_mutation::admit(app).is_ok());
    }
    fixture.finish().await;
}
