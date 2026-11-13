use super::*;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

const FIRST: &str = "Public synthetic clipboard history one";
const SECOND: &str = "Public synthetic clipboard history two";
const REPORT: &str = "/run/gui-fixture/native-clipboard-consumer.json";

pub(super) struct Fixture {
    child: tokio::process::Child,
}

fn copyq() -> ProcessCommand {
    let mut command = ProcessCommand::new("/usr/bin/copyq");
    command.env_clear().env("PATH", "/usr/bin:/bin").env("LC_ALL", "C.UTF-8")
        .env("HOME", "/run/display-home")
        .env("XDG_CONFIG_HOME", "/run/display-home/.config")
        .env("XDG_RUNTIME_DIR", "/run/user/62050")
        .env("QT_QPA_PLATFORM", "offscreen").current_dir("/");
    let parent = unsafe { libc::getpid() };
    unsafe {
        command.pre_exec(move || {
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setgid(OWNER) != 0 || libc::setuid(OWNER) != 0
                || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
                || libc::getppid() != parent
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
}

async fn query(arguments: &[&str]) -> Vec<u8> {
    let mut command = tokio::process::Command::from(copyq());
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        command.args(arguments).kill_on_drop(true).output(),
    ).await.expect("private CopyQ fixture command deadline").unwrap();
    assert!(result.stdout.len() <= 65536 && result.stderr.len() <= 65536);
    assert!(result.status.success(), "private CopyQ command failed: {:?}", result);
    result.stdout
}

fn open_snapshot(window: &serde_json::Value) -> Option<Vec<u8>> {
    let file = std::fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open("/run/user/62050/native-product-observer/native-clipboard-history.shm").unwrap();
    let info = file.metadata().unwrap();
    let limit = 4 * 1024 * 1024 + 4112;
    assert!(info.is_file() && info.uid() == OWNER && info.nlink() == 1
        && info.mode() & 0o077 == 0 && info.len() <= limit);
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).unwrap();
    assert!((16..=limit as usize).contains(&bytes.len()) && &bytes[..12] == b"CLAWCLPSHM1\0");
    let size = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    assert!(size <= 4096 && 16 + size <= bytes.len());
    let frame: serde_json::Value = serde_json::from_slice(&bytes[16..16 + size]).unwrap();
    let index = &window["popup"]["snapshot"];
    if frame["instance"] != window["instance"]
        || frame["buffer_commit"] != index["snapshot_commit"]
        || frame["configure_serial"] != index["configure_serial"]
        || !observe().is_some_and(|latest| latest["instance"] == window["instance"]
            && latest["popup"]["open"] == true && latest["popup"]["snapshot"] == *index)
    { return None; }
    Some(bytes)
}

impl Fixture {
    pub(super) async fn prepare() -> Self {
        assert_eq!(std::fs::read("/run/gui-fixture/isolated").unwrap(), b"private-v1");
        {
            let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(OWNER).unwrap();
            std::fs::create_dir_all("/run/display-home/.config/copyq").unwrap();
        }
        let output = std::fs::File::create("/run/gui-fixture/copyq-provider.log").unwrap();
        let mut command = tokio::process::Command::from(copyq());
        let mut child = command.stdin(Stdio::null()).stdout(Stdio::from(output.try_clone().unwrap()))
            .stderr(Stdio::from(output)).kill_on_drop(true).spawn().unwrap();
        let socket = Path::new("/run/display-home/.config/copyq/.copyq_s");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if std::fs::symlink_metadata(socket).is_ok() { break; }
            assert!(child.try_wait().unwrap().is_none() && Instant::now() < deadline,
                "real private CopyQ provider did not start");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let script = format!(
            "config('clipboard_tab','Clipboard'); tab('Clipboard'); add('{FIRST}'); add('{SECOND}'); print(size());"
        );
        assert_eq!(String::from_utf8(query(&["eval", &script]).await).unwrap().trim(), "2");
        Self { child }
    }

    pub(super) async fn consume(
        &mut self, control: &OwnedFd, pam: &mut Child, retained: &RetainedInstance,
        process: &NativeProcess, session: &str,
    ) {
        let instance = claw_display_control::Epoch(retained.instance.0).label();
        let guest = PathBuf::from(format!("/proc/{}/root", process.identity.pid()));
        for path in ["run/display-home/.config/copyq", "run/user/62050/bus",
            "run/dbus/system_bus_socket", "run/user/62050/native-product-observer"] {
            assert!(matches!(std::fs::symlink_metadata(guest.join(path)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound));
        }
        let mut selector = std::fs::OpenOptions::new().write(true).create_new(true)
            .mode(0o644).custom_flags(libc::O_NOFOLLOW)
            .open("/run/gui-public/native-clipboard-snapshot-enabled").unwrap();
        selector.set_permissions(std::fs::Permissions::from_mode(0o644)).unwrap();
        selector.write_all(&serde_json::to_vec(&serde_json::json!({
            "schema":"claw.private-clipboard-synthetic-shm/v1","instance":instance,
        })).unwrap()).unwrap();
        drop(selector);
        let mut driver = calendar_popup::Driver::new(
            control, pam, retained, process, session, Instant::now() + Duration::from_secs(25),
        );
        driver.click().await;
        let (frame, request, use_record, pixels) = loop {
            driver.live("Clipboard original popup and real controlled CopyQ history read").await;
            assert!(self.child.try_wait().unwrap().is_none(), "CopyQ provider exited");
            let audit = calendar_popup::audit_since(0);
            let requests = audit.iter().filter(|record| record["event"] == "clawd.request"
                && record["command"] == "system.clipboard.control"
                && record["params"]["session"] == session).collect::<Vec<_>>();
            assert!(requests.len() <= 1, "unexpected Clipboard history query repetition");
            if let Some(request) = requests.first() {
                if request["ok"] != true {
                    driver.fail(&format!("Clipboard real history request failed: {request}")).await;
                }
                assert_eq!(request["client"]["uid"], OWNER);
                assert_eq!(request["params"]["action"], "history-list");
                let cap = crate::clawd::authority::audit::CapFacts::of(
                    &Cap::new(Verb::CLIPBOARD_READ, Scope::name("history")),
                );
                let uses = audit.iter().filter(|record| record["event"] == "clawd.grant.use"
                    && record["route"] == "system.clipboard.control" && record["app_id"] == CLIPBOARD.app
                    && record["owner_uid"] == OWNER && record["decision"] == "allow"
                    && record["session"] == serde_json::to_value(crate::audit_policy::text_digest(session)).unwrap()
                    && record["caps"] == serde_json::json!([cap])).collect::<Vec<_>>();
                assert!(uses.len() <= 1);
                if let (Some(use_record), Some(frame)) = (
                    uses.first(),
                    observe().filter(|frame| calendar_popup::buffered_popup_for(frame, &instance, CLIPBOARD.desktop_id)
                        && frame["popup"]["snapshot"]["status"] == "saved"
                        && frame["popup"]["snapshot"]["snapshot_commit"] == frame["popup"]["buffer_commits"]),
                ) {
                    if let Some(pixels) = open_snapshot(&frame) {
                        break (frame, (*request).clone(), (*use_record).clone(), pixels);
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let mut checkpoint = std::fs::OpenOptions::new().write(true).create_new(true)
            .mode(0o600).custom_flags(libc::O_NOFOLLOW)
            .open("/run/gui-fixture/native-clipboard-history-open.shm").unwrap();
        checkpoint.write_all(&pixels).unwrap();
        drop(checkpoint);
        let mutations = self.mutations(&mut driver, session, &instance, &frame).await;
        driver.click().await;
        let closed = loop {
            driver.live("Clipboard original popup close").await;
            if let Some(frame) = observe().filter(|frame| buffered_panel(frame, &instance, CLIPBOARD.desktop_id)
                && frame["popup"]["closed"] == true && frame["popup"]["open"] == false)
            { break frame; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let report = serde_json::json!({
            "schema":"claw.private-native-clipboard-consumer/v1","instance":instance,"owner_uid":OWNER,
            "request":request,"exact_scope_use":use_record,"popup":frame["popup"],
            "passive_history_snapshot":closed["popup"]["snapshot"],
            "open_snapshot_copied_before_close":true,
            "history_mutations":mutations,
            "fixture_items":[FIRST,SECOND],"real_copyq":true,"provider_qpa":"offscreen",
            "raw_history_or_bus":false,"live_selection_restore_asserted":false,
            "sdk_reply_delivery_and_decode_asserted":false,"history_text_rendering_asserted":false,
            "coverage":"Original Clipboard popup, real history read, ordinary delete and confirmed clear under exact independent rights; offscreen provider is not live selection/Restore acceptance. Exact painted text remains a separate inspection.",
        });
        let bytes = serde_json::to_vec_pretty(&report).unwrap();
        assert!(bytes.len() <= 16384);
        std::fs::write(REPORT, bytes).unwrap();
    }

    async fn queries(
        &mut self, driver: &mut calendar_popup::Driver<'_>, session: &str,
        instance: &str, count: usize, floor: u64, height: Option<i64>,
    ) -> serde_json::Value {
        let actions = ["history-list", "history-remove", "history-list", "history-clear", "history-list"];
        loop {
            driver.live("Clipboard original delete/confirmed-clear and broker refresh").await;
            assert!(self.child.try_wait().unwrap().is_none(), "CopyQ provider exited");
            let audit = calendar_popup::audit_since(0);
            let requests = audit.iter().filter(|record| record["event"] == "clawd.request"
                && record["command"] == "system.clipboard.control"
                && record["params"]["session"] == session).collect::<Vec<_>>();
            if requests.len() > count {
                driver.fail("unexpected Clipboard mutation query repetition").await;
            }
            for (record, action) in requests.iter().zip(actions) {
                if record["ok"] != true || record["params"]["action"] != action
                    || record["client"]["uid"] != OWNER
                { driver.fail(&format!("Clipboard mutation request failed: {record}")).await; }
            }
            let session_digest = serde_json::to_value(crate::audit_policy::text_digest(session)).unwrap();
            let uses = audit.iter().filter(|record| record["event"] == "clawd.grant.use"
                && record["route"] == "system.clipboard.control" && record["app_id"] == CLIPBOARD.app
                && record["session"] == session_digest).collect::<Vec<_>>();
            assert!(uses.len() <= count);
            for (record, action) in uses.iter().zip(actions) {
                let mut caps = Vec::new();
                if action != "history-clear" {
                    caps.push(Cap::new(Verb::CLIPBOARD_READ, Scope::name("history")));
                }
                if matches!(action, "history-remove" | "history-clear") {
                    caps.push(Cap::new(Verb::CLIPBOARD_WRITE, Scope::name("history")));
                }
                let facts = caps.iter().map(crate::clawd::authority::audit::CapFacts::of).collect::<Vec<_>>();
                assert_eq!(record["caps"], serde_json::to_value(facts).unwrap());
                assert_eq!(record["owner_uid"], OWNER);
                assert_eq!(record["decision"], "allow");
            }
            if requests.len() == count && uses.len() == count {
                if let Some(window) = observe().filter(|window| {
                    let popup = &window["popup"];
                    calendar_popup::buffered_popup_for(window, instance, CLIPBOARD.desktop_id)
                        && popup["pending_configures"] == 0
                        && popup["buffer_commits"].as_u64().is_some_and(|value| value > floor)
                        && popup["geometry"][3].as_i64().is_some_and(|value|
                            height.map_or(value < 300, |expected| value == expected))
                }) {
                    return serde_json::json!({"window":window,"requests":requests,"exact_scope_uses":uses});
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn mutations(
        &mut self, driver: &mut calendar_popup::Driver<'_>, session: &str,
        instance: &str, opened: &serde_json::Value,
    ) -> serde_json::Value {
        let height = opened["popup"]["geometry"][3].as_i64().unwrap();
        let floor = driver.popup_click(318, 155).await;
        self.queries(driver, session, instance, 3, floor, Some(height)).await;
        let remaining: serde_json::Value = serde_json::from_slice(&query(&["eval",
            "tab('Clipboard'); print(JSON.stringify({size:size(),text:str(read(0))}));"]).await).unwrap();
        assert_eq!(remaining, serde_json::json!({"size":1,"text":FIRST}));
        let floor = driver.popup_click(80, u16::try_from(height - 32).unwrap()).await;
        let confirmation = loop {
            driver.live("Clipboard original clear confirmation").await;
            if let Some(window) = observe().filter(|window| {
                let popup = &window["popup"];
                calendar_popup::buffered_popup_for(window, instance, CLIPBOARD.desktop_id)
                    && popup["pending_configures"] == 0
                    && popup["buffer_commits"].as_u64().is_some_and(|value| value > floor)
                    && popup["geometry"][3].as_i64().is_some_and(|value| value < 300)
            }) { break window; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert_eq!(calendar_popup::audit_since(0).iter().filter(|record|
            record["event"] == "clawd.request" && record["command"] == "system.clipboard.control"
                && record["params"]["session"] == session).count(), 3,
            "opening confirmation must not clear history");
        let popup = &confirmation["popup"]["geometry"];
        let floor = driver.popup_click(u16::try_from(popup[2].as_i64().unwrap() - 60).unwrap(),
            u16::try_from(popup[3].as_i64().unwrap() - 48).unwrap()).await;
        let result = self.queries(driver, session, instance, 5, floor, None).await;
        assert_eq!(String::from_utf8(query(&["eval", "tab('Clipboard'); print(size());"]).await)
            .unwrap().trim(), "0");
        serde_json::json!({"requests":result["requests"],"exact_scope_uses":result["exact_scope_uses"],
            "clear_confirmation":confirmation["popup"],"provider_counts":[2,1,0],
            "remaining_after_delete":remaining,"ordinary_popup_clicks":3})
    }

    pub(super) async fn stop(mut self) {
        assert!(self.child.try_wait().unwrap().is_none(),
            "App retirement stopped the independent OS history provider");
        assert_eq!(String::from_utf8(query(&["eval", "tab('Clipboard'); print(size());"]).await)
            .unwrap().trim(), "0");
        query(&["exit"]).await;
        assert!(tokio::time::timeout(Duration::from_secs(10), self.child.wait())
            .await.expect("CopyQ provider retirement deadline").unwrap().success());
    }
}
