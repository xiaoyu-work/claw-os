use super::*;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

const CATALOG: &str = "/var/lib/swcatalog/xml/private-store.xml";
const STATUS: &str = "/var/lib/dpkg/status";
const REPORT: &str = "/run/gui-fixture/native-store-consumer.json";

pub(super) struct Fixture {
    catalog: Vec<u8>,
    status: Vec<u8>,
}

fn directory(path: &str) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn file(path: &str, data: &[u8]) {
    std::fs::write(path, data).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
}

impl Fixture {
    pub(super) fn prepare(package_owned: bool) -> Self {
        assert_eq!(unsafe { libc::geteuid() }, 0);
        assert_eq!(std::fs::read("/run/gui-fixture/isolated").unwrap(), b"private-v1");
        for prefix in ["/usr/share", "/var/lib", "/var/cache"] {
            for name in ["swcatalog", "app-info"] {
                let path = format!("{prefix}/{name}");
                std::fs::create_dir_all(&path).unwrap();
                let target = std::ffi::CString::new(path).unwrap();
                assert_eq!(unsafe {
                    libc::mount(c"tmpfs".as_ptr(), target.as_ptr(), c"tmpfs".as_ptr(),
                        libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                        c"mode=0755".as_ptr().cast())
                }, 0, "private standard catalog source mount");
            }
        }
        for path in [
            "/var/lib/swcatalog/xml", "/var/lib/dpkg", "/var/lib/dpkg/info",
            "/var/lib/dpkg/updates", "/var/lib/dpkg/triggers",
            "/var/lib/apt/lists/partial", "/var/cache/apt/archives/partial",
            "/etc/apt/apt.conf.d", "/etc/apt/auth.conf.d", "/etc/apt/sources.list.d",
        ] {
            directory(path);
        }
        let status = if package_owned {
            std::fs::read(STATUS).unwrap()
        } else {
            let status = b"Package: claw-native-store-fixture\nStatus: install ok installed\n\
Priority: optional\nSection: misc\nArchitecture: all\nVersion: 1.0.0\n\
Maintainer: Private Acceptance <noreply@fixture.invalid>\n\
Description: Private controlled Store inventory\n\n".to_vec();
            file(STATUS, &status);
            status
        };
        let packages = if package_owned {
            ["claw-app-acceptance-bundle", "claw-package-acceptance-side-effect"]
        } else {
            ["claw-native-store-fixture", "claw-native-store-available"]
        };
        let components = packages
            .iter().enumerate().map(|(index, package)| format!(
                "<component type=\"desktop-application\"><id>org.claw.fixture.Store{index}</id>\
                 <pkgname>{package}</pkgname><name>Controlled Store fixture {index}</name>\
                 <name xml:lang=\"fr\">Catalogue prive {index}</name>\
                 <summary>Public synthetic controlled catalog</summary>\
                 <icon type=\"stock\">package-x-generic</icon></component>"
            )).collect::<String>();
        let catalog = format!(
            "<components version=\"1.0\" origin=\"private-store\">{components}</components>"
        ).into_bytes();
        file(CATALOG, &catalog);
        Self { catalog, status }
    }

    pub(super) async fn consume(
        &self,
        retained: &RetainedInstance,
        native: &NativeProcess,
        session: &str,
        deadline: Instant,
        window: &serde_json::Value,
    ) -> Result<(), String> {
        let cap = crate::clawd::authority::audit::CapFacts::of(&Cap::new(
            Verb::SYS_OBSERVE, Scope::name("packages"),
        ));
        let (requests, uses) = loop {
            check_native_process(retained, native)?;
            let audit = calendar_popup::audit_since(0);
            let requests = audit.iter().filter(|record| {
                record["event"] == "clawd.request" && record["command"] == "system.package.query"
                    && record["params"]["session"] == session
            }).cloned().collect::<Vec<_>>();
            if requests.iter().any(|request| request["ok"] != true
                || request["client"]["uid"] != OWNER) {
                return Err("actual Store SDK package observation was refused".into());
            }
            if audit.iter().any(|record| {
                record["event"] == "clawd.request" && record["params"]["session"] == session
                    && matches!(record["command"].as_str(), Some("system.package.control" | "system.package.install" | "system.package.restore"))
            }) {
                return Err("actual Store startup attempted a package mutation".into());
            }
            let uses = audit.into_iter().filter(|record| {
                record["event"] == "clawd.grant.use" && record["route"] == "system.package.query"
                    && record["owner_uid"] == OWNER && record["app_id"] == STORE.app
                    && record["session"] == serde_json::to_value(crate::audit_policy::text_digest(session)).unwrap()
                    && record["decision"] == "allow"
                    && record["bound_pid"].as_u64().is_some_and(|pid| pid > 1)
                    && record["caps"] == serde_json::json!([cap])
            }).collect::<Vec<_>>();
            if ["catalog", "installed", "updates"].iter().all(|action| {
                requests.iter().any(|request| request["params"]["action"] == *action)
            }) && uses.len() >= requests.len() {
                break (requests, uses);
            }
            if Instant::now() >= deadline {
                return Err(format!("actual Store catalog/inventory/update read deadline: {} requests, {} uses", requests.len(), uses.len()));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let root = PathBuf::from(format!("/proc/{}/root", native.identity.pid()));
        for path in [
            "var/lib/dpkg/status", "var/lib/apt/lists", "var/lib/swcatalog",
            "run/dbus/system_bus_socket", "run/user/62050/bus",
        ] {
            match std::fs::symlink_metadata(root.join(path)) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                _ => return Err(format!("actual Store received raw system state or bus: {path}")),
            }
        }
        self.verify_after_retirement();
        let report = serde_json::json!({
            "schema": "claw.private-native-store-consumer/v1",
            "instance": claw_display_control::Epoch(retained.instance.0).label(),
            "owner_uid": OWNER, "requests": requests, "exact_scope_uses": uses,
            "window": window, "raw_system_state_or_bus": false,
            "critical_startup_authority": false,
            "sdk_reply_delivery_and_decode_asserted": false,
            "entry_text_rendering_asserted": false,
            "coverage": "Actual signed native Store reached configured nonuniform pixels and real owner-bound Root catalog/inventory/update reads without raw state, bus or startup Critical authority. Exact painted entries and payload UI self-handoff remain separate gates.",
        });
        std::fs::write(REPORT, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        std::fs::set_permissions(REPORT, std::fs::Permissions::from_mode(0o600)).unwrap();
        Ok(())
    }

    pub(super) fn verify_after_retirement(&self) {
        assert_eq!(std::fs::read(CATALOG).unwrap(), self.catalog);
        assert_eq!(std::fs::read(STATUS).unwrap(), self.status);
        for path in ["/var/cache/swcatalog", "/var/cache/app-info"] {
            assert!(std::fs::read_dir(path).unwrap().next().is_none());
        }
    }

    pub(super) async fn input_frame(
        &self,
        retained: &RetainedInstance,
        native: &NativeProcess,
        before: &serde_json::Value,
        input: (u64, &str, &str),
    ) -> Result<serde_json::Value, String> {
        let (sequence, kind, value) = input;
        let instance = claw_display_control::Epoch(retained.instance.0).label();
        if sequence == 1 {
            let selector = serde_json::json!({
                "schema": "claw.private-store-synthetic-shm/v1", "instance": instance,
            });
            let mut selected = std::fs::OpenOptions::new().write(true).create_new(true)
                .mode(0o644).custom_flags(libc::O_NOFOLLOW)
                .open("/run/gui-public/native-store-snapshot-enabled").map_err(|error| error.to_string())?;
            selected.set_permissions(std::fs::Permissions::from_mode(0o644)).map_err(|error| error.to_string())?;
            selected.write_all(&serde_json::to_vec(&selector).unwrap()).map_err(|error| error.to_string())?;
            drop(selected);
        }
        let directory = Path::new(OBSERVER).parent().unwrap();
        let next = directory.join("keyboard-command.next");
        let request = serde_json::json!({
            "schema": "claw.private-keyboard/v1", "sequence": sequence, "instance": instance,
            "surface": "primary", "expires_ms": claw_display_control::wire::monotonic_ms().unwrap() + 5_000,
            "kind": kind, "value": value,
        });
        let mut command = std::fs::OpenOptions::new().write(true).create_new(true)
            .mode(0o644).custom_flags(libc::O_NOFOLLOW)
            .open(&next).map_err(|error| error.to_string())?;
        command.set_permissions(std::fs::Permissions::from_mode(0o644)).map_err(|error| error.to_string())?;
        command.write_all(&serde_json::to_vec(&request).unwrap()).map_err(|error| error.to_string())?;
        drop(command);
        std::fs::rename(next, directory.join("keyboard-command.json")).map_err(|error| error.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(25);
        let baseline = before["buffer_commits"].as_u64().unwrap();
        let (captured, delivery_commit) = loop {
            check_native_process(retained, native)?;
            let ack = (|| {
                let file = std::fs::OpenOptions::new().read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(directory.join("keyboard-ack.json")).ok()?;
                let metadata = file.metadata().ok()?;
                if !metadata.is_file() || metadata.uid() != OWNER || metadata.nlink() != 1
                    || metadata.mode() & 0o077 != 0 || metadata.len() > 1024 {
                    return None;
                }
                let mut bytes = Vec::new();
                file.take(1025).read_to_end(&mut bytes).ok()?;
                (bytes.len() <= 1024).then(|| serde_json::from_slice::<serde_json::Value>(&bytes).ok()).flatten()
            })();
            if let Some(ack) = ack.filter(|ack| ack["schema"] == "claw.private-keyboard/v1"
                && ack["instance"] == instance && ack["sequence"] == sequence
                && ack["surface"] == "primary"
                && ack[if kind == "pointer" { "pointer_events_sent" } else { "keyboard_events_sent" }] == true) {
                let delivery_commit = ack["buffer_commits_at_delivery"].as_u64()
                    .ok_or("ordinary input acknowledgement omitted its actual delivery frame")?;
                if let Some(record) = observe().filter(|record| {
                    buffered_window(record, &instance) && record["app_id"] == STORE.desktop_id
                        && record["buffer_commits"].as_u64().is_some_and(|count| {
                            count > baseline && count > delivery_commit
                        })
                        && record["snapshot"]["status"] == "saved"
                        && record["snapshot"]["snapshot_commit"] == record["buffer_commits"]
                        && record["snapshot"]["latest_buffer_commit"] == record["buffer_commits"]
                        && record["snapshot"]["matches_current_configure"] == true
                }) {
                    break (record, delivery_commit);
                }
            }
            if Instant::now() >= deadline {
                return Err(format!("actual Store ordinary input {sequence}/passive frame observation deadline"));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let mut report: serde_json::Value = serde_json::from_slice(&std::fs::read(REPORT).unwrap()).unwrap();
        if kind == "text" {
            report["ordinary_search"] = value.into();
            report["ordinary_keyboard_events_sent"] = true.into();
            report["window_after_search"] = captured.clone();
        } else {
            report["ordinary_selection"] = serde_json::json!({
                "kind": kind, "value": value, "sequence": sequence, "events_sent": true,
                "window": captured,
            });
        }
        report["buffer_commits_at_input_delivery"] = delivery_commit.into();
        report["passive_window_snapshot"] = captured["snapshot"].clone();
        report["coverage"] = "Actual signed native Store reached real Root catalog/inventory/update reads and a new authenticated configured frame after ordinary search keys. Passive own-instance bytes are evidence for inspection, not an automated exact-entry text or reply-decode assertion; payload UI self-handoff remains separate.".into();
        std::fs::write(REPORT, serde_json::to_vec_pretty(&report).unwrap()).map_err(|error| error.to_string())?;
        Ok(captured)
    }
}
