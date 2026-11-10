use super::*;
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

struct Bus {
    child: std::process::Child,
    _directory: tempfile::TempDir,
    address: String,
}

impl Bus {
    fn new() -> Self {
        let directory = tempfile::Builder::new()
            .prefix("fm-bus-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        let address = format!("unix:path={}/bus", directory.path().display());
        let child = std::process::Command::new("dbus-daemon")
            .args([
                "--session",
                "--nofork",
                "--nopidfile",
                "--nosyslog",
                "--print-address=1",
            ])
            .arg(format!("--address={address}"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("private dbus-daemon is required");
        let mut bus = Self {
            child,
            _directory: directory,
            address,
        };
        let mut line = String::new();
        BufReader::new(bus.child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert!(
            line.starts_with(&bus.address),
            "private bus did not become ready"
        );
        bus
    }

    async fn connection(&self) -> Connection {
        Builder::address(self.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap()
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct FakeCos {
    directory: tempfile::TempDir,
    executable: PathBuf,
}

impl FakeCos {
    fn new(block: bool) -> Self {
        let directory = tempfile::Builder::new()
            .prefix("fm-cos-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        fs::create_dir(directory.path().join("records")).unwrap();
        if !block {
            fs::write(directory.path().join("release"), b"").unwrap();
        }
        let executable = directory.path().join("fake-cos");
        let root = serde_json::to_string(directory.path().to_str().unwrap()).unwrap();
        fs::write(
            &executable,
            format!(
                r#"#!/usr/bin/python3
import json, os, sys, time
from pathlib import Path
root = Path({root})
record = {{
    "argv": sys.argv[1:],
    "authority": {{k: v for k, v in os.environ.items() if k.startswith(("COS_", "CLAW"))}},
    "loader": os.environ.get("LD_PRELOAD"),
    "path": os.environ.get("PATH"),
    "cwd": os.getcwd(),
    "loginuid": Path("/proc/self/loginuid").read_text(),
    "sessionid": Path("/proc/self/sessionid").read_text(),
    "pid": os.getpid(),
}}
target = root / "records" / (str(os.getpid()) + ".json")
staging = target.with_suffix(".pending")
staging.write_text(json.dumps(record))
staging.rename(target)
deadline = time.monotonic() + 10
while not (root / "release").exists() and time.monotonic() < deadline:
    time.sleep(0.01)
sys.exit(9)
"#
            ),
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            directory,
            executable,
        }
    }

    fn launcher(&self) -> Launcher {
        Launcher::fixture(
            self.executable.clone(),
            "alternate-files".into(),
            [
                ("COS_SESSION", "old-model-grant"),
                ("COS_APP_ID", "untrusted-parent"),
                ("COS_MCP_SERVER", "1"),
                ("COS_APP_GUI", "1"),
                ("COS_CAPS", "admin"),
                ("COS_PARENT_SESSION", "old-parent"),
                ("COS_APPS_DIR", "/not-installed"),
                ("CLAWD_SOCKET_PATH", "/not-the-broker"),
                ("LD_PRELOAD", "/not-a-library"),
                ("LANG", "C.UTF-8"),
            ]
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect(),
        )
    }

    fn records(&self) -> Vec<serde_json::Value> {
        fs::read_dir(self.directory.path().join("records"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .map(|path| serde_json::from_slice(&fs::read(path).unwrap()).unwrap())
            .collect()
    }

    async fn wait_records(&self, count: usize) -> Vec<serde_json::Value> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let records = self.records();
                if records.len() >= count {
                    return records;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fake cos did not execute")
    }
}

struct Running {
    connection: Connection,
    stop: watch::Sender<bool>,
    dispatcher: tokio::task::JoinHandle<()>,
    slots: Arc<Semaphore>,
}

impl Running {
    async fn new(bus: &Bus, owner_uid: u32, launcher: Launcher) -> Self {
        let (interface, pending) = FileManager::new(owner_uid);
        let slots = interface.slots.clone();
        let connection = register(Builder::address(bus.address.as_str()).unwrap(), interface)
            .await
            .unwrap();
        let (stop, stopping) = watch::channel(false);
        let dispatcher = tokio::spawn(dispatch(pending, launcher, stopping));
        Self {
            connection,
            stop,
            dispatcher,
            slots,
        }
    }

    async fn idle(&self) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.slots.available_permits() != MAX_PENDING {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Root GUI launcher was not waited");
    }

    async fn stop(self) {
        self.stop.send_replace(true);
        tokio::time::timeout(Duration::from_secs(5), self.dispatcher)
            .await
            .expect("owned launcher cleanup timed out")
            .unwrap();
        assert_eq!(self.slots.available_permits(), MAX_PENDING);
        self.connection.close().await.unwrap();
    }
}

async fn call(
    connection: &Connection,
    method: &str,
    uris: &[String],
    startup_id: &str,
) -> zbus::Result<zbus::Message> {
    tokio::time::timeout(
        Duration::from_secs(5),
        connection.call_method(
            Some(SERVICE),
            OBJECT_PATH,
            Some(SERVICE),
            method,
            &(uris, startup_id),
        ),
    )
    .await
    .expect("private FileManager1 call timed out")
}

fn assert_error(result: zbus::Result<zbus::Message>, expected: &str) {
    match result {
        Err(zbus::Error::MethodError(name, _, _)) => {
            assert_eq!(name.as_str(), expected);
        }
        _ => panic!("expected a structured desktop intent refusal"),
    }
}

#[test]
fn configured_target_is_only_a_canonical_routing_preference() {
    assert_eq!(configured_app(std::iter::empty()).unwrap(), "cosmic-files");
    assert_eq!(
        configured_app(["--app", "another_gui-2"].map(OsString::from).into_iter()).unwrap(),
        "another_gui-2"
    );
    for arguments in [
        vec!["--app"],
        vec!["--app", "--gui"],
        vec!["--app", "/usr/bin/files"],
        vec!["--app", "other.gui"],
        vec!["--app", "other;command"],
        vec!["--app", "Root"],
        vec!["--app", "install"],
        vec!["--app", "stdio"],
        vec!["--app", "valid", "--session", "borrowed"],
        vec!["--cos", "/not-installed"],
    ] {
        assert!(configured_app(arguments.into_iter().map(OsString::from)).is_err());
    }
}

#[test]
fn uri_bounds_include_the_encoded_gui_request() {
    assert!(validate_intent(&[], "").is_ok());
    assert!(validate_intent(&["file:///nul\0value".into()], "").is_err());
    assert!(validate_intent(&vec!["file:///a".into(); MAX_URIS], "").is_ok());
    let longest = format!("file:///{}", "a".repeat(MAX_URI_BYTES - "file:///".len()));
    assert!(validate_intent(std::slice::from_ref(&longest), "").is_ok());
    assert!(validate_intent(&[format!("{longest}a")], "").is_err());
    assert!(validate_intent(&vec!["file:///a".into(); MAX_URIS + 1], "").is_err());
    let escaped = format!("file:///{}", "\"".repeat(3000));
    let uris = vec![escaped; 12];
    assert!(uris.iter().map(String::len).sum::<usize>() < MAX_ENCODED_BYTES);
    assert!(validate_intent(&uris, "").is_err());
}

#[tokio::test]
async fn private_bus_authenticates_the_sender_and_preserves_literal_gui_arguments() {
    let bus = Bus::new();
    let fake = FakeCos::new(false);
    let uid = unsafe { libc::getuid() };
    let running = Running::new(&bus, uid, fake.launcher()).await;
    let client = bus.connection().await;
    let marker = fake.directory.path().join("shell-ran");
    let uris = vec![
        format!("file:///literal name/'\";$(touch {})", marker.display()),
        "https://example.invalid/a?quote=\"&other=;literal".into(),
    ];
    for method in ["ShowFolders", "ShowItems", "ShowItemProperties"] {
        call(&client, method, &uris, "presentation-only")
            .await
            .unwrap();
    }
    let records = fake.wait_records(3).await;
    assert_eq!(records.len(), 3);
    for record in records {
        let expected = ["app", "alternate-files", "--gui"]
            .into_iter()
            .map(String::from)
            .chain(uris.iter().cloned())
            .collect::<Vec<_>>();
        assert_eq!(record["argv"], serde_json::json!(expected));
        assert_eq!(record["authority"], serde_json::json!({}));
        assert!(record["loader"].is_null());
        assert_eq!(record["path"], "/usr/local/bin:/usr/bin:/bin");
        assert_eq!(record["cwd"], "/");
        for field in ["loginuid", "sessionid"] {
            assert_eq!(
                record[field],
                fs::read_to_string(Path::new("/proc/self").join(field)).unwrap()
            );
        }
    }
    assert!(!marker.exists(), "GUI URI was interpreted as shell input");
    running.idle().await;
    assert_eq!(fake.records().len(), 3, "failed launcher was retried");
    running.stop().await;

    let wrong_owner = Running::new(&bus, uid.wrapping_add(1), fake.launcher()).await;
    for uris in [&uris[..], &["--not-a-url".into()][..]] {
        assert_error(
            call(&client, "ShowItems", uris, "").await,
            "org.freedesktop.DBus.Error.AccessDenied",
        );
    }
    wrong_owner.stop().await;
    assert_eq!(fake.records().len(), 3, "wrong-owner request executed");
    client.close().await.unwrap();
}

#[tokio::test]
async fn private_bus_rejects_bad_uris_and_limits_without_execution() {
    let bus = Bus::new();
    let fake = FakeCos::new(false);
    let running = Running::new(&bus, unsafe { libc::getuid() }, fake.launcher()).await;
    let client = bus.connection().await;
    for uris in [
        vec!["--gui:other".into()],
        vec!["/not-a-url".into()],
        vec!["file:///line\nbreak".into()],
        vec!["file:///control\u{0085}".into()],
        vec![format!("file:///{}", "a".repeat(MAX_URI_BYTES))],
        vec!["file:///a".into(); MAX_URIS + 1],
        vec![format!("file:///{}", "a".repeat(4000)); 17],
        vec![format!("file:///{}", "\"".repeat(3000)); 12],
    ] {
        assert_error(
            call(&client, "ShowFolders", &uris, "").await,
            "org.freedesktop.DBus.Error.InvalidArgs",
        );
    }
    for startup in ["stale\nsession".into(), "x".repeat(MAX_URI_BYTES + 1)] {
        assert_error(
            call(&client, "ShowItems", &[], &startup).await,
            "org.freedesktop.DBus.Error.InvalidArgs",
        );
    }
    running.stop().await;
    assert!(fake.records().is_empty());
    client.close().await.unwrap();
}

#[tokio::test]
async fn private_bus_bounds_pending_children_and_shutdown_waits_without_retry() {
    let bus = Bus::new();
    let fake = FakeCos::new(true);
    let running = Running::new(&bus, unsafe { libc::getuid() }, fake.launcher()).await;
    let client = bus.connection().await;
    for _ in 0..MAX_PENDING {
        call(&client, "ShowFolders", &[], "").await.unwrap();
    }
    let records = fake.wait_records(MAX_PENDING).await;
    assert_error(
        call(&client, "ShowItems", &[], "").await,
        "org.freedesktop.DBus.Error.LimitsExceeded",
    );
    assert_eq!(fake.records().len(), MAX_PENDING);
    running.stop().await;
    for record in records {
        let pid = record["pid"].as_u64().unwrap() as libc::pid_t;
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
            -1,
            "launcher was not reaped"
        );
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
    assert_eq!(fake.records().len(), MAX_PENDING, "intent was retried");
    client.close().await.unwrap();
}
