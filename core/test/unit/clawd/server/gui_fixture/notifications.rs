use super::*;
use crate::notifications::{
    DeliveryChannel, DeliveryState, Notification, NotificationDraft, NotificationPreferences,
    NotificationPresentation, NotificationService, NotificationState, Severity,
    SqliteNotificationService,
};
use std::io::Read;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

const TITLE: &str = "Actual native Notifications popup";
const BODY: &str = "<literal notification text> & no markup or authority";
const SENDER: &str = "Private synthetic producer";

type Lines = tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>;

struct Probe {
    child: tokio::process::Child,
    input: tokio::process::ChildStdin,
    output: Lines,
    helper: OwnedFd,
}

impl Probe {
    async fn start() -> Self {
        let mut command = ProcessCommand::new("/usr/lib/cos/bin/claw-notification-panel-fixture");
        command
            .env_clear()
            .env("HOME", "/run/display-home")
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("CLAWD_SOCKET", "/run/cos/clawd.sock")
            .env("CLAW_NOTIFICATION_PRIVATE_FIXTURE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        unsafe {
            command.pre_exec(|| {
                if libc::setgroups(0, std::ptr::null()) != 0
                    || libc::setgid(OWNER) != 0
                    || libc::setuid(OWNER) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut command = tokio::process::Command::from(command);
        command.kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        let input = child.stdin.take().unwrap();
        let mut output = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
        let ready = next(&mut output).await;
        assert_eq!(ready["ready"], true);
        let pid = ready["helper_pid"].as_i64().unwrap() as i32;
        let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0_u32) };
        assert!(descriptor >= 0);
        let helper = unsafe { OwnedFd::from_raw_fd(descriptor as i32) };
        let identity = ProcessIdentity::from_pidfd(helper.as_fd()).unwrap();
        assert_eq!(identity.uid(), OWNER);
        let installed = std::fs::metadata("/usr/lib/cos/bin/claw-notification-panel").unwrap();
        let actual = std::fs::metadata(format!("/proc/{pid}/exe")).unwrap();
        assert_eq!(
            (actual.dev(), actual.ino()),
            (installed.dev(), installed.ino())
        );
        Self {
            child,
            input,
            output,
            helper,
        }
    }

    async fn request(&mut self, request: serde_json::Value) -> serde_json::Value {
        self.input
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        loop {
            let value = next(&mut self.output).await;
            if value.get("response").is_some() {
                return value["response"].clone();
            }
        }
    }

    async fn card(&mut self, title: &str) -> claw_notification_presentation::Card {
        loop {
            let value = next(&mut self.output).await;
            if value["card"]["title"] == title {
                let card: claw_notification_presentation::Card =
                    serde_json::from_value(value["card"].clone()).unwrap();
                card.validate().unwrap();
                return card;
            }
        }
    }

    async fn stop(mut self) {
        drop(self.input);
        assert!(
            tokio::time::timeout(Duration::from_secs(5), self.child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
        assert!(claw_display_control::identity::pidfd_exited(self.helper.as_fd()).unwrap());
    }
}

async fn next(lines: &mut Lines) -> serde_json::Value {
    let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .expect("actual notification descriptor deadline")
        .unwrap()
        .expect("actual notification descriptor exited");
    serde_json::from_str(&line).unwrap()
}

pub(super) fn buffered(record: &serde_json::Value, instance: &str) -> bool {
    buffered_panel_size(
        record,
        instance,
        "notifications",
        300..=300,
        80..=1920,
        "None",
    )
}

pub(super) struct Fixture {
    service: SqliteNotificationService,
    preferences: NotificationPreferences,
    first: Notification,
    other_owner: Notification,
    second: Option<Notification>,
    probe: Option<Probe>,
    case: usize,
}

impl Fixture {
    pub(super) fn prepare(case: usize) -> Self {
        assert_eq!(
            std::fs::read("/run/gui-fixture/isolated").unwrap(),
            b"private-v1"
        );
        assert!(matches!(case, 42 | 43));
        let service = crate::notifications::open_default().unwrap();
        let mut preferences = service.preferences(OWNER).unwrap();
        preferences.desktop_enabled = true;
        preferences.web_enabled = false;
        preferences.ntfy_enabled = false;
        preferences.dnd_start_minute_utc = None;
        preferences.dnd_end_minute_utc = None;
        service.set_preferences(OWNER, preferences.clone()).unwrap();
        let first = publish(&service, OWNER, TITLE, "first");
        let other_owner = publish(
            &service,
            OWNER + 1,
            "Other owner's private notification",
            "first",
        );
        assert!(first
            .deliveries
            .iter()
            .any(|delivery| delivery.channel == DeliveryChannel::Desktop
                && delivery.state == DeliveryState::Queued));
        Self {
            service,
            preferences,
            first,
            other_owner,
            second: None,
            probe: None,
            case,
        }
    }

    pub(super) async fn consume(
        &mut self,
        control: &OwnedFd,
        pam: &mut Child,
        retained: &RetainedInstance,
        process: &NativeProcess,
        session: &str,
        first_frame: &serde_json::Value,
    ) {
        let instance = claw_display_control::Epoch(retained.instance.0).label();
        assert!(buffered(first_frame, &instance));
        let root = PathBuf::from(format!("/proc/{}/root", process.identity.pid()));
        for path in ["run/user/62050/bus", "run/dbus/system_bus_socket"] {
            assert!(
                !root.join(path).exists(),
                "raw bus or durable notification database reached the App"
            );
        }
        let database = crate::paths::notifications_db_path();
        assert!(database.is_file());
        assert!(
            !root.join(database.strip_prefix("/").unwrap()).exists(),
            "raw durable notification database reached the App"
        );
        let mut environment = Vec::new();
        std::fs::File::open(format!("/proc/{}/environ", process.identity.pid()))
            .unwrap()
            .take(32768)
            .read_to_end(&mut environment)
            .unwrap();
        for forbidden in [
            "DBUS_SESSION_BUS_ADDRESS=",
            "DAEMON_NOTIFICATIONS_FD=",
            "PANEL_NOTIFICATIONS_FD=",
        ] {
            assert!(!environment
                .split(|byte| *byte == 0)
                .any(|entry| entry.starts_with(forbidden.as_bytes())));
        }
        delivered(&self.service, &self.first).await;
        let mut probe = Probe::start().await;
        let card = probe.card(TITLE).await;
        assert_eq!(card.source_label, SENDER);
        assert_eq!(
            card.body
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>(),
            BODY
        );
        assert_eq!(card.activation.as_deref(), Some("default"));
        let muted = probe
            .request(serde_json::json!({"action":"muted","muted":true}))
            .await;
        assert_eq!(muted["preferences"]["muted"], true);
        assert_eq!(
            self.service.preferences(OWNER).unwrap(),
            self.preferences,
            "App popup mute changed the Root DND/delivery policy"
        );
        let unmuted = probe
            .request(serde_json::json!({"action":"muted","muted":false}))
            .await;
        assert_eq!(unmuted["preferences"]["muted"], false);
        let private_mute =
            Path::new("/run/display-home/.local/share/cos/apps/cosmic-notifications")
                .join(".config/cosmic/com.clawos.Notifications/v1/do_not_disturb");
        assert!(
            private_mute.is_file(),
            "native preference acknowledgement did not persist the original private product key"
        );
        assert_eq!(
            std::fs::symlink_metadata(&private_mute).unwrap().uid(),
            OWNER
        );
        assert!(!Path::new(
            "/run/display-home/.config/cosmic/com.clawos.Notifications/v1/do_not_disturb"
        )
        .exists());
        let first_record = self.service.get(OWNER, &self.first.id).unwrap();
        assert_eq!(first_record.state, NotificationState::Unread);
        assert!(first_record.acknowledged_at_ms.is_none());
        assert_eq!(
            self.service
                .get(OWNER + 1, &self.other_owner.id)
                .unwrap()
                .state,
            NotificationState::Unread
        );
        assert!(!self
            .service
            .get(OWNER + 1, &self.other_owner.id)
            .unwrap()
            .deliveries
            .iter()
            .any(|delivery| delivery.channel == DeliveryChannel::Desktop
                && delivery.state == DeliveryState::Delivered));
        let public = probe.request(serde_json::json!({"action":"claim"})).await;
        assert_eq!(
            public["ok"], false,
            "a second public desktop consumer stole the managed lease"
        );
        assert_eq!(public["error"]["code"], "execution_failed");
        if self.case == 42 {
            assert_eq!(
                probe
                    .request(serde_json::json!({"action":"dismiss","id":card.id}))
                    .await["dismissed"],
                card.id
            );
            wait_state(&self.service, &self.first.id, NotificationState::Dismissed).await;
            let closed = Instant::now() + Duration::from_secs(5);
            loop {
                native_process(retained, process, "original first popup checked close");
                if observe().is_some_and(|record| {
                    record["instance"] == instance
                        && record["closed"] == true
                        && record["layer_surfaces"] == 0
                }) {
                    break;
                }
                assert!(
                    Instant::now() < closed,
                    "native popup dismiss did not destroy the actual Layer Shell surface"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let second = publish(
                &self.service,
                OWNER,
                "Native popup pointer acknowledgement",
                "second",
            );
            let second_card = probe.card(&second.title).await;
            assert_ne!(second_card.id, card.id);
            delivered(&self.service, &second).await;
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                native_process(retained, process, "real native second notification popup");
                if observe().is_some_and(|record| buffered(&record, &instance)) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "second native notification did not paint actual pixels"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let mut pointer =
                calendar_popup::Driver::new(control, pam, retained, process, session, deadline);
            pointer.toast_click().await;
            wait_state(&self.service, &second.id, NotificationState::Acknowledged).await;
            self.second = Some(second);
        }
        let report = serde_json::json!({
            "schema":"claw.private-native-notifications-consumer/v1","case":self.case,"instance":instance,
            "owner_uid":OWNER,"first_notification":self.first.id,"other_owner_notification":self.other_owner.id,
            "card_handle":card.id,"actual_popup_frame":first_frame,
            "real_signed_host":true,"real_root_desktop_lease":true,"real_private_panel_descriptor":true,
            "private_product_mute_persisted":true,"root_dnd_unchanged":true,
            "panel_dismiss_and_original_popup_pointer_ack":self.case == 42,
            "delivery_is_not_acknowledgement":true,"raw_owner_bus":false,"raw_notification_database":false,
            "agent_task_or_model_required":false,
        });
        std::fs::write(
            "/run/gui-fixture/native-notifications-consumer.json",
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
        self.probe = Some(probe);
    }

    pub(super) async fn verify_after_retirement(mut self) {
        assert_eq!(self.service.preferences(OWNER).unwrap(), self.preferences);
        let first = self.service.get(OWNER, &self.first.id).unwrap();
        assert_eq!(
            first.state,
            if self.case == 42 {
                NotificationState::Dismissed
            } else {
                NotificationState::Unread
            }
        );
        if self.case == 43 {
            assert!(first.acknowledged_at_ms.is_none());
        }
        if let Some(second) = &self.second {
            assert_eq!(
                self.service.get(OWNER, &second.id).unwrap().state,
                NotificationState::Acknowledged
            );
        }
        if let Some(mut probe) = self.probe.take() {
            let unavailable = probe
                .request(serde_json::json!({"action":"availability"}))
                .await;
            assert_eq!(unavailable["ok"], false);
            assert_eq!(unavailable["error"]["code"], "unavailable");
            let released = probe.request(serde_json::json!({"action":"claim"})).await;
            assert_eq!(
                released["ok"], true,
                "presenter custody survived checked process/resource retirement"
            );
            probe.stop().await;
        }
    }
}

fn publish(service: &dyn NotificationService, owner: u32, title: &str, key: &str) -> Notification {
    let mut draft = NotificationDraft::new(
        "fixture:native-notifications",
        "native.acceptance",
        Severity::Info,
        title,
        BODY,
    )
    .dedupe(key);
    draft.presentation = Some(NotificationPresentation {
        app_name: SENDER.into(),
        icon: String::new(),
        expire_ms: 0,
        transient: false,
    });
    service.publish(owner, draft).unwrap()
}

async fn delivered(service: &dyn NotificationService, record: &Notification) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let record = service.get(record.owner_uid, &record.id).unwrap();
        assert_eq!(
            record.state,
            NotificationState::Unread,
            "delivery acknowledged the notification"
        );
        if record.deliveries.iter().any(|delivery| {
            delivery.channel == DeliveryChannel::Desktop
                && delivery.state == DeliveryState::Delivered
        }) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "real native renderer never accepted its Root delivery lease: {record:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_state(service: &dyn NotificationService, id: &str, expected: NotificationState) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let record = service.get(OWNER, id).unwrap();
        if record.state == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "native notification state did not reach {expected:?}: {record:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
