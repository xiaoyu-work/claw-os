use super::*;

fn config(kind: &str, content: &str) -> Import {
    serde_json::from_value(json!({"kind":kind,"name":"work","content":content})).unwrap()
}

#[test]
fn vpn_import_accepts_data_but_rejects_programs_and_ungranted_files() {
    validate_import(&config("wireguard", "[Interface]\nPrivateKey = synthetic-key\nAddress = 10.1.1.2/32\n[Peer]\nPublicKey = synthetic-public\nAllowedIPs = 0.0.0.0/0\n")).unwrap();
    validate_import(&config("vpn", "client\ndev tun\nremote vpn.example.test 1194\nproto udp\nauth-user-pass\n<ca>\nsynthetic-certificate\n</ca>\n")).unwrap();
    for (kind, text) in [
        ("wireguard", "[Interface]\nPostUp = arbitrary-program\n"),
        ("wireguard", "[Interface]\nPreDown = arbitrary-program\n"),
        ("vpn", "client\nup arbitrary-program\n"),
        ("vpn", "client\nconfig ungranted-file\n"),
        ("vpn", "client\nca ungranted-file\n"),
        ("vpn", "client\nauth-user-pass ungranted-secret\n"),
        ("vpn", "client\nremote host \\\nup arbitrary-program\n"),
        ("vpn", "<connection>\nremote host\n</connection>\n"),
        ("vpn", "<ca>\nunterminated\n"),
        (
            "vpn",
            "<ca>\n</ca> # ambiguous closing tag\nup arbitrary-program\n</ca>\n",
        ),
    ] {
        assert!(validate_import(&config(kind, text)).is_err(), "{kind}");
    }
}

#[tokio::test]
async fn private_network_service_round_trips_metadata_patch_and_stale_denial() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    use std::sync::{Arc, Mutex};
    const PROFILE: &str = "/org/freedesktop/NetworkManager/Settings/one";
    struct Bus(std::process::Child);
    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let directory = tempfile::Builder::new()
        .prefix(".nm-")
        .tempdir_in(std::env::current_dir().unwrap())
        .unwrap();
    let socket = directory.path().join("bus");
    let mut child = std::process::Command::new("/usr/bin/dbus-daemon")
        .args([
            "--session",
            "--nofork",
            "--print-address",
            &format!("--address=unix:path={}", socket.display()),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut address = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    let _bus = Bus(child);

    struct Nm;
    #[zbus::interface(name = "org.freedesktop.NetworkManager")]
    impl Nm {
        #[zbus(property)]
        fn wireless_enabled(&self) -> bool {
            true
        }
        #[zbus(property)]
        fn wwan_enabled(&self) -> bool {
            false
        }
        #[zbus(property)]
        fn connectivity(&self) -> u32 {
            4
        }
        #[zbus(property)]
        fn active_connections(&self) -> Vec<OwnedObjectPath> {
            Vec::new()
        }
        fn get_devices(&self) -> Vec<OwnedObjectPath> {
            Vec::new()
        }
    }
    struct Inventory;
    #[zbus::interface(name = "org.freedesktop.NetworkManager.Settings")]
    impl Inventory {
        fn list_connections(&self) -> Vec<OwnedObjectPath> {
            vec![OwnedObjectPath::try_from(PROFILE).unwrap()]
        }
    }
    struct ProfileObject {
        raw: Arc<Mutex<Settings>>,
        writes: Arc<std::sync::atomic::AtomicUsize>,
    }
    #[zbus::interface(name = "org.freedesktop.NetworkManager.Settings.Connection")]
    impl ProfileObject {
        fn get_settings(&self) -> zbus::fdo::Result<Settings> {
            self.raw
                .lock()
                .unwrap()
                .iter()
                .map(|(group, entries)| {
                    entries
                        .iter()
                        .map(|(key, value)| {
                            value
                                .try_clone()
                                .map(|value| (key.clone(), value))
                                .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))
                        })
                        .collect::<Result<HashMap<_, _>, _>>()
                        .map(|entries| (group.clone(), entries))
                })
                .collect()
        }
        fn update(&self, settings: Settings) {
            *self.raw.lock().unwrap() = settings;
            self.writes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let initial: Profile = serde_json::from_value(
        json!({"kind":"ethernet","name":"before","autoconnect":true,"ipv4_method":"auto"}),
    )
    .unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let mut settings = Settings::new();
    patch(&mut settings, &initial, &id).unwrap();
    settings.insert(
        "unexposed".into(),
        HashMap::from([("password".into(), text("not-for-the-client"))]),
    );
    let raw = Arc::new(Mutex::new(settings));
    let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server = zbus::connection::Builder::address(address.trim())
        .unwrap()
        .name(SERVICE)
        .unwrap()
        .serve_at(BASE, Nm)
        .unwrap()
        .serve_at(SETTINGS, Inventory)
        .unwrap()
        .serve_at(
            PROFILE,
            ProfileObject {
                raw,
                writes: writes.clone(),
            },
        )
        .unwrap()
        .build()
        .await
        .unwrap();
    let connection = zbus::connection::Builder::address(address.trim())
        .unwrap()
        .build()
        .await
        .unwrap();
    let backend = Backend {
        connection,
        owner: server.unique_name().unwrap().to_owned(),
    };
    let snapshot = backend.snapshot().await.unwrap();
    assert_eq!(snapshot["profiles"][0]["name"], "before");
    assert!(!snapshot.to_string().contains("not-for-the-client"));
    let (_, raw) = backend.find(&id).await.unwrap();
    let before = project(&raw).unwrap();
    let mut edited = before.clone();
    edited.name = serde_json::from_value(json!("after")).unwrap();
    edited.ipv4_dns = vec!["192.0.2.53".parse().unwrap()];
    let saved = backend.save(edited).await.unwrap();
    assert_eq!(saved["profile"]["name"], "after");
    assert_eq!(saved["profile"]["ipv4_dns"][0], "192.0.2.53");
    assert_eq!(writes.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(backend
        .save(before)
        .await
        .unwrap_err()
        .contains("changed since"));
    assert_eq!(writes.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn profile_patch_preserves_unedited_security_and_revision_changes() {
    let profile: Profile = serde_json::from_value(json!({
        "kind":"wifi","name":"original","autoconnect":true,"ipv4_method":"auto",
        "ssid":"work","security":"psk"
    }))
    .unwrap();
    validate_profile(&profile).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let mut raw = Settings::new();
    patch(&mut raw, &profile, &id).unwrap();
    raw.get_mut("802-11-wireless-security")
        .unwrap()
        .insert("key-mgmt".into(), text("sae"));
    let before = project(&raw).unwrap();
    let mut edit = before.clone();
    edit.name = serde_json::from_value(json!("renamed")).unwrap();
    patch(&mut raw, &edit, &id).unwrap();
    assert_eq!(
        string(&raw, "802-11-wireless-security", "key-mgmt").as_deref(),
        Some("sae")
    );
    let after = project(&raw).unwrap();
    assert_ne!(
        before.revision.as_ref().map(Text::as_str),
        after.revision.as_ref().map(Text::as_str)
    );
    assert_eq!(after.name.as_str(), "renamed");
    let mut wrong = edit;
    wrong.ipv4_method = serde_json::from_value(json!("manual")).unwrap();
    assert!(validate_profile(&wrong).is_err());
}

#[test]
fn stdin_network_payload_is_closed_and_cannot_select_an_owner_or_authority() {
    let profile = json!({"kind":"ethernet","name":"work","autoconnect":true,"ipv4_method":"auto"});
    let value = cli_request(
        "fixture",
        "profile-save",
        &json!({"profile":profile}).to_string(),
    )
    .unwrap();
    assert_eq!(value["session"], "fixture");
    assert_eq!(value["action"], "profile-save");
    for extra in ["session", "action", "owner", "bus", "executable", "caps"] {
        let value = json!({"profile":profile,extra:"injected"}).to_string();
        assert!(
            cli_request("fixture", "profile-save", &value).is_err(),
            "{extra}"
        );
    }
}

#[test]
fn native_mutations_and_wifi_export_cannot_borrow_other_category_or_secret_rights() {
    use crate::caps::Cap;
    let _lock = crate::test_env::lock_env();
    let directory = tempfile::Builder::new()
        .prefix(".network-native-authority-")
        .tempdir_in(std::env::current_dir().unwrap())
        .unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", directory.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", directory.path());
    let id = uuid::Uuid::new_v4().to_string();
    let join:Request=serde_json::from_value(json!({"session":"fixture","action":"wifi-join",
            "profile":{"kind":"wifi","name":"work","ssid":"work","security":"psk","autoconnect":true,"ipv4_method":"auto"},
            "credential":"network/work"
        })).unwrap();
    let wifi = Cap::new(Verb::NET_MANAGE, Scope::name("wifi"));
    let secret = Cap::new(Verb::SECRET_READ, Scope::name("network/work"));
    for app in ["independent-client", "cosmic-settings"] {
        for caps in [
            vec![wifi.clone()],
            vec![secret.clone()],
            vec![
                Cap::new(Verb::NET_MANAGE, Scope::name("ethernet")),
                secret.clone(),
            ],
            vec![
                wifi.clone(),
                Cap::new(Verb::SECRET_READ, Scope::name("network/other")),
            ],
        ] {
            let decision = super::super::tests::decision(app, caps, true);
            assert!(authorize(&join, &decision).is_err());
            assert!(!crate::clawd::authority::obligation_met(Some(&decision)));
        }
        let decision = super::super::tests::decision(app, vec![wifi.clone(), secret.clone()], true);
        assert_eq!(
            authorize(&join, &decision).unwrap().1.spent(),
            &[wifi.clone(), secret.clone()]
        );
        let export: Request =
            serde_json::from_value(json!({"session":"fixture","action":"wifi-share","target":id}))
                .unwrap();
        let observer = Cap::new(Verb::SYS_OBSERVE, Scope::name("network"));
        let denied = super::super::tests::decision(
            app,
            vec![observer.clone(), wifi.clone(), secret.clone()],
            true,
        );
        assert!(authorize(&export, &denied).is_err());
        let exact = Cap::new(
            Verb::SECRET_READ,
            Scope::name(format!("network.profile/{id}")),
        );
        let allowed = super::super::tests::decision(app, vec![observer, exact], true);
        assert!(authorize(&export, &allowed).is_ok());
    }
}
