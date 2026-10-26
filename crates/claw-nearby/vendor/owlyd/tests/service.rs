//! owlyd's D-Bus API against a fake radio, on a private bus: leases, and
//! everything that ends one; and recovery after owlyd itself is killed.
//!
//! Each test starts its own `dbus-daemon` and `owlyd --fake`.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use owlyd::api::{self, LinkInfo, RadioProxy};

struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    bus: Child,
    address: String,
    owlyd: Option<Child>,
}

impl Fx {
    async fn new() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("owly-fake")).unwrap();
        std::fs::write(root.join("owly-fake/peers"), "86:08:04:8a:2b:f4\n").unwrap();
        let socket = root.join("bus");
        let address = format!("unix:path={}", socket.display());
        let bus = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--nopidfile"])
            .arg(format!("--address={address}"))
            .stdout(Stdio::null())
            .spawn()
            .expect("dbus-daemon");
        wait_for("the bus", || socket.exists());
        let mut fx = Fx {
            _dir: dir,
            root,
            bus,
            address,
            owlyd: None,
        };
        fx.start().await;
        fx
    }

    /// Starts owlyd and waits for it to take its name.
    async fn start(&mut self) {
        let child = Command::new(env!("CARGO_BIN_EXE_owlyd"))
            .arg("--fake")
            .arg(&self.root)
            .args(["--bus", &self.address])
            .env("RUST_LOG", "owlyd=debug")
            .spawn()
            .unwrap();
        self.owlyd = Some(child);
        let conn = self.connect().await;
        let dbus = zbus::fdo::DBusProxy::new(&conn).await.unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        while !dbus
            .name_has_owner(api::NAME.try_into().unwrap())
            .await
            .unwrap()
        {
            assert!(Instant::now() < end, "owlyd didn't take its name");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn connect(&self) -> zbus::Connection {
        api::connect(Some(&self.address)).await.unwrap()
    }

    async fn radio(&self) -> (zbus::Connection, RadioProxy<'static>) {
        let conn = self.connect().await;
        let radio = api::radio(&conn).await.unwrap();
        (conn, radio)
    }

    fn awdl0(&self) -> bool {
        self.root.join("sys/class/net/awdl0").exists()
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.root.join("owly-fake/calls"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    fn fake(&self, name: &str, content: &str) {
        std::fs::write(self.root.join("owly-fake").join(name), content).unwrap();
    }

    fn kill_owlyd(&mut self) {
        if let Some(mut c) = self.owlyd.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.kill_owlyd();
        let _ = self.bus.kill();
        let _ = self.bus.wait();
    }
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

async fn state_becomes(radio: &RadioProxy<'_>, want: &str) {
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        if radio.current_state().await.unwrap() == want {
            return;
        }
        assert!(Instant::now() < end, "State never became {want}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn error_name(e: &zbus::Error) -> String {
    api::api_error(e).map_or_else(|| e.to_string(), |(n, _)| n.to_string())
}

#[tokio::test(flavor = "multi_thread")]
async fn probe_answers_anyone() {
    let fx = Fx::new().await;
    let (_c, radio) = fx.radio().await;
    let json: serde_json::Value = serde_json::from_str(&radio.probe().await.unwrap()).unwrap();
    assert_eq!(json["driver"], "fake");
    assert_eq!(json["mode"], "shared");
    assert_eq!(json["hardware"], true);
    assert_eq!(json["missing"], serde_json::json!([]));
    assert_eq!(fx.calls(), Vec::<String>::new(), "a probe changes nothing");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lease_from_acquire_to_release() {
    let fx = Fx::new().await;
    let (_c, radio) = fx.radio().await;
    assert_eq!(radio.current_state().await.unwrap(), "idle");
    let mut states = radio.receive_radio_state().await.unwrap();

    let (lease, link) = radio.acquire(0).await.unwrap();
    assert_ne!(lease, 0);
    let link = LinkInfo::from_dict(&link).unwrap();
    assert_eq!(link.interface, "awdl0");
    assert_eq!(link.mac, "02:00:00:00:00:01");
    assert_eq!(link.address, "fe80::ff:fe00:1");
    assert_eq!(link.announce.unwrap().count, 3);
    assert!(fx.awdl0());
    assert_eq!(radio.current_state().await.unwrap(), "up");
    assert_eq!(radio.lease().await.unwrap(), lease);
    let peers: Vec<_> = radio
        .peers()
        .await
        .unwrap()
        .iter()
        .filter_map(api::peer_line)
        .collect();
    assert_eq!(peers, ["86:08:04:8a:2b:f4 rssi=?"]);
    // For clients other than the lease's owner, such as `owly receive`.
    assert_eq!(
        LinkInfo::from_dict(&radio.link().await.unwrap()),
        Some(link)
    );

    let mut seen = Vec::new();
    for _ in 0..2 {
        let s = states.next().await.unwrap();
        let a = s.args().unwrap();
        seen.push((a.lease, a.state.clone(), a.reason.clone()));
    }
    assert_eq!(
        seen,
        [
            (lease, "starting".into(), "acquire".into()),
            (lease, "up".into(), "acquired".into())
        ]
    );

    radio.release(lease).await.unwrap();
    // Release returns once the radio is down.
    assert!(!fx.awdl0());
    assert_eq!(radio.current_state().await.unwrap(), "idle");
    assert_eq!(radio.lease().await.unwrap(), 0);
    assert_eq!(fx.calls(), ["up", "down"]);
    assert!(radio.link().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn one_lease_at_a_time_and_only_its_owner_acts_on_it() {
    let fx = Fx::new().await;
    let (_a, first) = fx.radio().await;
    let (_b, second) = fx.radio().await;
    let (lease, _) = first.acquire(0).await.unwrap();

    let e = second.acquire(0).await.unwrap_err();
    assert_eq!(error_name(&e), "Busy");
    let e = second.release(lease).await.unwrap_err();
    assert_eq!(error_name(&e), "StaleLease");
    let e = first.release(lease + 1).await.unwrap_err();
    assert_eq!(error_name(&e), "StaleLease");
    assert!(fx.awdl0());

    first.release(lease).await.unwrap();
    // A later lease has another number; the old one is stale.
    let (next, _) = second.acquire(0).await.unwrap();
    assert_ne!(next, lease);
    let e = first.release(lease).await.unwrap_err();
    assert_eq!(error_name(&e), "StaleLease");
    second.release(next).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lease_ends_when_its_owner_leaves_the_bus() {
    let fx = Fx::new().await;
    let (watcher_conn, watcher) = fx.radio().await;
    {
        let (conn, radio) = fx.radio().await;
        radio.acquire(0).await.unwrap();
        drop(radio);
        drop(conn);
    }
    state_becomes(&watcher, "idle").await;
    assert!(!fx.awdl0());
    assert_eq!(fx.calls(), ["up", "down"]);
    drop(watcher_conn);
}

/// The fake trusts every caller as root, as owlyd does root for a detached
/// lease.
#[tokio::test(flavor = "multi_thread")]
async fn a_detached_lease_outlives_its_owner_until_released() {
    let fx = Fx::new().await;
    let lease = {
        let (conn, radio) = fx.radio().await;
        let (lease, _) = radio.acquire_detached(0).await.unwrap();
        drop(radio);
        drop(conn);
        lease
    };
    let (_c, radio) = fx.radio().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(radio.current_state().await.unwrap(), "up");
    assert_eq!(radio.lease().await.unwrap(), lease);
    assert!(fx.awdl0());

    radio.release(lease).await.unwrap();
    assert!(!fx.awdl0());
    assert_eq!(
        radio.last_end().await.unwrap(),
        (lease, "released".to_string())
    );
    assert_eq!(fx.calls(), ["up", "down"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_owned_lease_is_released_only_by_its_owner() {
    let fx = Fx::new().await;
    let (_a, owner) = fx.radio().await;
    let (_b, other) = fx.radio().await;
    let (lease, _) = owner.acquire(0).await.unwrap();
    // Not even by root, which may release a detached one.
    let e = other.release(lease).await.unwrap_err();
    assert_eq!(error_name(&e), "StaleLease");
    owner.release(lease).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lease_ends_when_its_time_runs_out() {
    let fx = Fx::new().await;
    let (_c, radio) = fx.radio().await;
    let mut states = radio.receive_radio_state().await.unwrap();
    let (lease, _) = radio.acquire(1).await.unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let s = states.next().await.unwrap();
            let a = s.args().unwrap();
            if a.state == "idle" {
                return (a.lease, a.reason.clone());
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(ended, (lease, "expired".to_string()));
    assert!(!fx.awdl0());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_link_ends_the_lease_with_its_reason() {
    let fx = Fx::new().await;
    let (_c, radio) = fx.radio().await;
    let mut states = radio.receive_radio_state().await.unwrap();
    let (lease, _) = radio.acquire(0).await.unwrap();
    fx.fake("lose", "");
    let reason = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let s = states.next().await.unwrap();
            let a = s.args().unwrap();
            if a.state == "idle" && a.lease == lease {
                return a.reason.clone();
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(reason, "lost: the fake radio was lost");
    assert!(!fx.awdl0());
    assert_eq!(radio.last_end().await.unwrap(), (lease, reason));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_start_says_why() {
    let fx = Fx::new().await;
    let (_c, radio) = fx.radio().await;
    fx.fake("up.fail", "3 Wi-Fi isn't connected.");
    let e = radio.acquire(0).await.unwrap_err();
    let (name, message) = api::api_error(&e).unwrap();
    assert_eq!(
        (name, message.as_str()),
        ("NotNow", "Wi-Fi isn't connected.")
    );
    assert_eq!(radio.current_state().await.unwrap(), "idle");
    fx.fake("up.fail", "4 No radio here.");
    let e = radio.acquire(0).await.unwrap_err();
    assert_eq!(error_name(&e), "NoRadio");
}

/// SIGKILL owlyd mid-session: the radio stays as it was until owlyd starts
/// again, and it undoes it before taking its name.
#[tokio::test(flavor = "multi_thread")]
async fn owlyd_killed_mid_session_restores_the_radio_on_start() {
    let mut fx = Fx::new().await;
    {
        let (_c, radio) = fx.radio().await;
        radio.acquire(0).await.unwrap();
        fx.kill_owlyd();
    }
    assert!(fx.awdl0(), "nothing undid it yet");
    fx.start().await;
    assert!(!fx.awdl0());
    assert_eq!(fx.calls(), ["up", "recover"]);
    // And it leases again.
    let (_c, radio) = fx.radio().await;
    let (lease, _) = radio.acquire(0).await.unwrap();
    radio.release(lease).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn sigterm_takes_the_radio_down() {
    let mut fx = Fx::new().await;
    let (_c, radio) = fx.radio().await;
    radio.acquire(0).await.unwrap();
    let pid = fx.owlyd.as_ref().unwrap().id();
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    let status = fx.owlyd.as_mut().unwrap().wait().unwrap();
    assert!(status.success(), "{status}");
    assert!(!fx.awdl0());
    assert_eq!(fx.calls(), ["up", "down"]);
}

struct FailedRetirement {
    inner: owly_radio::fake::Fake,
    fail_startup: bool,
}

impl owly_radio::radio::Backend for FailedRetirement {
    async fn up(&mut self) -> Result<owly_radio::radio::Link, owly_radio::radio::Error> {
        owly_radio::radio::Backend::up(&mut self.inner).await
    }

    async fn down(&mut self) -> Result<(), owly_radio::radio::Error> {
        Err(owly_radio::radio::Error::no_radio("fixture network restore failed"))
    }

    async fn recover(&mut self) -> Result<(), owly_radio::radio::Error> {
        if self.fail_startup {
            Err(owly_radio::radio::Error::no_radio("fixture startup recovery failed"))
        } else {
            owly_radio::radio::Backend::recover(&mut self.inner).await
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn release_never_acknowledges_failed_network_retirement() {
    let mut fx = Fx::new().await;
    fx.kill_owlyd();
    let (shutdown, finished) = tokio::sync::oneshot::channel();
    let backend = FailedRetirement {
        inner: owly_radio::fake::Fake::new(fx.root.clone()),
        fail_startup: false,
    };
    let address = fx.address.clone();
    let serving = tokio::spawn(owlyd::service::serve(
        backend,
        owly_radio::fake::probe,
        owlyd::service::Options { bus: Some(address), polkit: false },
        async { let _ = finished.await; },
    ));
    let connection = fx.connect().await;
    let dbus = zbus::fdo::DBusProxy::new(&connection).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !dbus.name_has_owner(api::NAME.try_into().unwrap()).await.unwrap() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.unwrap();
    let (_connection, radio) = fx.radio().await;
    let (id, _) = radio.acquire(0).await.unwrap();
    let error = radio.release(id).await.unwrap_err();
    assert_eq!(error_name(&error), "NoRadio");
    assert_eq!(radio.current_state().await.unwrap(), "recovery-failed");
    assert_ne!(radio.last_end().await.unwrap(), (id, "released".into()));
    shutdown.send(()).unwrap();
    serving.await.unwrap().unwrap();
}

#[tokio::test]
async fn failed_startup_recovery_never_advertises_an_idle_service() {
    let dir = tempfile::tempdir().unwrap();
    let result = owlyd::service::serve(
        FailedRetirement {
            inner: owly_radio::fake::Fake::new(dir.path()),
            fail_startup: true,
        },
        owly_radio::fake::probe,
        owlyd::service::Options {
            bus: Some(format!("unix:path={}", dir.path().join("nonexistent-private-bus").display())),
            polkit: false,
        },
        std::future::pending(),
    ).await;
    assert!(result.unwrap_err().contains("startup recovery failed"));
}
