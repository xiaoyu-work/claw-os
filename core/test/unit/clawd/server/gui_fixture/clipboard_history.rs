use super::*;
use std::io::Write;
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
        for path in ["run/display-home/.config/copyq", "run/user/62050/bus", "run/dbus/system_bus_socket"] {
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
        let (frame, request, use_record) = loop {
            driver.live("Clipboard original popup and real controlled CopyQ history read").await;
            assert!(self.child.try_wait().unwrap().is_none(), "CopyQ provider exited");
            let audit = calendar_popup::audit_since(0);
            let requests = audit.iter().filter(|record| record["event"] == "clawd.request"
                && record["command"] == "system.clipboard.control"
                && record["params"]["session"] == session).collect::<Vec<_>>();
            assert!(requests.len() <= 1, "unexpected Clipboard history query repetition");
            if let Some(request) = requests.first() {
                assert_eq!(request["ok"], true);
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
                    break (frame, (*request).clone(), (*use_record).clone());
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
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
            "fixture_items":[FIRST,SECOND],"real_copyq":true,"provider_qpa":"offscreen",
            "raw_history_or_bus":false,"live_selection_restore_asserted":false,
            "sdk_reply_delivery_and_decode_asserted":false,"history_text_rendering_asserted":false,
            "coverage":"Original Clipboard popup, exact history-read Root call and real private CopyQ history; offscreen provider is not live selection/Restore acceptance. Exact painted text remains a separate inspection.",
        });
        let bytes = serde_json::to_vec_pretty(&report).unwrap();
        assert!(bytes.len() <= 16384);
        std::fs::write(REPORT, bytes).unwrap();
    }

    pub(super) async fn stop(mut self) {
        assert!(self.child.try_wait().unwrap().is_none(),
            "App retirement stopped the independent OS history provider");
        assert_eq!(String::from_utf8(query(&["eval", "tab('Clipboard'); print(size());"]).await)
            .unwrap().trim(), "2");
        query(&["exit"]).await;
        assert!(tokio::time::timeout(Duration::from_secs(10), self.child.wait())
            .await.expect("CopyQ provider retirement deadline").unwrap().success());
    }
}
