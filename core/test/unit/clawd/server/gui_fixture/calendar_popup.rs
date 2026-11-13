use super::*;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

const EVENT_ID: &str = "evt-native-calendar-consumer";
const EVENT_TITLE: &str = "Synthetic same-owner Calendar consumer event";
const AUDIT_LIMIT: u64 = 2 * 1024 * 1024;
const REPORT: &str = "/run/gui-fixture/native-calendar-consumer.json";

pub(super) struct Fixture {
    database: PathBuf,
    before: Vec<u8>,
    first: chrono::NaiveDate,
    end: chrono::NaiveDate,
}

impl Fixture {
    pub(super) fn prepare() -> Self {
        assert_eq!(unsafe { libc::geteuid() }, 0);
        assert_eq!(
            std::fs::read("/run/gui-fixture/isolated").unwrap(),
            b"private-v1"
        );
        assert!(
            std::fs::read_to_string("/proc/self/status").unwrap()
                .lines().any(|line| line.split_whitespace().eq(["Umask:", "0077"])),
            "Calendar must exercise the normal private clawd umask"
        );
        assert!(!Path::new("/run/cos/calendar-queries").exists());
        let context =
            crate::paths::RoutedPathContext::for_owner(OWNER, PathBuf::from("/run/display-home"));
        let data = context.clone().scope_sync(crate::paths::user_data_dir);
        assert_eq!(data, Path::new("/run/display-home/.local/share/cos"));
        let today = chrono::Utc::now().date_naive();
        let first = today - chrono::Duration::days(1);
        let end = today + chrono::Duration::days(2);
        let database;
        {
            let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(OWNER).unwrap();
            let partition = context
                .scope_sync(|| crate::worker::derive::app_partition(&data, "calendar"))
                .unwrap();
            let directory = partition.join("calendar");
            std::fs::create_dir(&directory).unwrap();
            database = directory.join("events.db");
            assert!(
                !database.exists(),
                "never overwrite an existing Calendar store"
            );
            let connection = rusqlite::Connection::open(&database).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE events (
                    id TEXT PRIMARY KEY, title TEXT NOT NULL, start_time TEXT NOT NULL,
                    end_time TEXT, description TEXT DEFAULT '', location TEXT DEFAULT '',
                    created_at TEXT NOT NULL, updated_at TEXT NOT NULL
                );",
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO events
                 (id,title,start_time,end_time,location,created_at,updated_at)
                 VALUES (?1,?2,?3,?4,'Private synthetic fixture','fixture','fixture')",
                    rusqlite::params![EVENT_ID, EVENT_TITLE, first.to_string(), end.to_string()],
                )
                .unwrap();
            connection.execute(
                "INSERT INTO events
                 (id,title,start_time,end_time,created_at,updated_at)
                 VALUES ('excluded-old-event','Not this day','2000-01-01','2000-01-02','fixture','fixture')",
                [],
            ).unwrap();
            drop(connection);
            std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let metadata = std::fs::symlink_metadata(&database).unwrap();
        assert!(metadata.is_file() && metadata.uid() == OWNER && metadata.nlink() == 1);
        let before = std::fs::read(&database).unwrap();
        assert!(before.len() < 64 * 1024);
        Self {
            database,
            before,
            first,
            end,
        }
    }

    fn query_day(&self, params: &serde_json::Value) -> Option<chrono::NaiveDate> {
        let day = chrono::NaiveDate::from_ymd_opt(
            i32::try_from(params["year"].as_i64()?).ok()?,
            u32::try_from(params["month"].as_u64()?).ok()?,
            u32::try_from(params["day"].as_u64()?).ok()?,
        )?;
        (self.first <= day && day < self.end).then_some(day)
    }

    pub(super) fn verify_after_retirement(&self) {
        assert_eq!(std::fs::read(&self.database).unwrap(), self.before);
        let metadata = std::fs::symlink_metadata(&self.database).unwrap();
        assert!(metadata.uid() == OWNER && metadata.nlink() == 1);
        let queries = Path::new("/run/cos/calendar-queries");
        assert!(
            queries.is_dir(),
            "the real database reader view was never prepared"
        );
        let metadata = std::fs::symlink_metadata(queries).unwrap();
        assert_eq!(metadata.uid(), 0);
        assert_eq!(metadata.mode() & 0o777, 0o711);
        assert!(
            queries.read_dir().unwrap().next().is_none(),
            "Calendar query view leaked"
        );
    }

    pub(super) async fn consume_widgets(
        &self, retained: &RetainedInstance, process: &NativeProcess, session: &str,
        deadline: Instant, first_frame: &serde_json::Value,
    ) -> Result<(), String> {
        let instance = claw_display_control::Epoch(retained.instance.0).label();
        let guest = PathBuf::from(format!("/proc/{}/root", process.identity.pid()));
        assert!(guest.join("usr/local/bin/cos").is_file());
        for path in [
            self.database.strip_prefix("/").unwrap(),
            Path::new("run/user/62050/bus"),
            Path::new("run/dbus/system_bus_socket"),
        ] {
            match std::fs::symlink_metadata(guest.join(path)) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                result => return Err(format!("raw Calendar database or bus was not hidden from Widget Rail: {result:?}")),
            }
        }
        let mut selector = std::fs::OpenOptions::new().write(true).create_new(true)
            .mode(0o644).custom_flags(libc::O_NOFOLLOW)
            .open("/run/gui-public/native-widget-rail-snapshot-enabled")
            .map_err(|error| error.to_string())?;
        selector.set_permissions(std::fs::Permissions::from_mode(0o644)).unwrap();
        selector.write_all(&serde_json::to_vec(&serde_json::json!({
            "schema":"claw.private-widget-rail-synthetic-shm/v1", "instance":instance,
        })).unwrap()).unwrap();
        drop(selector);
        let expected = [
            ("system.calendar.day", "day", Verb::DATA_DB_READ, "calendar"),
            ("system.observation.query", "tasks", Verb::AGENT_OBSERVE, "tasks"),
            ("system.observation.query", "resources", Verb::SYS_OBSERVE, "resources"),
        ];
        let mut floor = first_frame["buffer_commits"].as_u64().unwrap();
        let mut readers_completed = false;
        loop {
            check_native_process(retained, process)?;
            let audit = audit_since(0);
            let mut requests = Vec::new();
            let mut uses = Vec::new();
            for (route, action, verb, scope) in expected {
                let matching = audit.iter().filter(|record| record["event"] == "clawd.request"
                    && record["command"] == route && record["params"]["session"] == session
                    && (route == "system.calendar.day" || record["params"]["action"] == action))
                    .collect::<Vec<_>>();
                assert!(matching.len() <= 8, "unbounded Widget Rail startup query repetition");
                for request in &matching {
                    if request["ok"] != true || request["client"]["uid"] != OWNER
                        || !request["client"]["pid"].as_u64().is_some_and(|pid| pid > 1)
                        || (route == "system.calendar.day" && self.query_day(&request["params"]).is_none())
                    {
                        return Err(format!("Widget Rail {action} reader failed: {request}"));
                    }
                }
                let cap = crate::clawd::authority::audit::CapFacts::of(&Cap::new(verb, Scope::name(scope)));
                let grants = audit.iter().filter(|record| record["event"] == "clawd.grant.use"
                    && record["route"] == route && record["app_id"] == WIDGETS.app
                    && record["owner_uid"] == OWNER
                    && record["session"] == serde_json::to_value(crate::audit_policy::text_digest(session)).unwrap()
                    && record["decision"] == "allow"
                    && record["bound_pid"].as_u64().is_some_and(|pid| pid > 1)
                    && record["caps"] == serde_json::json!([cap]))
                    .collect::<Vec<_>>();
                if let (Some(request), Some(grant)) = (matching.last(), grants.last()) {
                    requests.push((*request).clone());
                    uses.push((*grant).clone());
                }
            }
            let frame = observe().filter(|record| buffered_widget_panel(record, &instance));
            if requests.len() == expected.len() && !readers_completed {
                if let Some(frame) = &frame {
                    floor = frame["buffer_commits"].as_u64().unwrap().max(floor);
                    readers_completed = true;
                }
            }
            if let Some(frame) = frame.filter(|frame| readers_completed
                && frame["buffer_commits"].as_u64().is_some_and(|commit| commit > floor)
                && frame["snapshot"]["status"] == "saved"
                && frame["snapshot"]["instance"] == instance
                && frame["snapshot"]["matches_current_configure"] == true
                && frame["snapshot"]["snapshot_commit"] == frame["buffer_commits"])
            {
                let report = serde_json::json!({
                    "schema":"claw.private-native-desktop-widgets-consumer/v1",
                    "instance":instance, "owner_uid":OWNER, "requests":requests, "exact_scope_uses":uses,
                    "fixture_event_id":EVENT_ID, "fixture_event_title":EVENT_TITLE,
                    "passive_panel_snapshot":frame["snapshot"], "window":frame,
                    "raw_calendar_database":false, "real_existing_reader_success":true,
                    "sdk_reply_delivery_and_decode_asserted":false, "card_text_rendering_asserted":false,
                    "coverage":"Original Widget Rail panel pixels and successful exact-owner Calendar/task/resource SDK reads; no Agent/model, raw Calendar database or owner bus. Reply decoding and exact painted card text require independent inspection.",
                });
                let path = "/run/gui-fixture/native-desktop-widgets-consumer.json";
                let bytes = serde_json::to_vec_pretty(&report).unwrap();
                assert!(bytes.len() <= 16384);
                std::fs::write(path, bytes).unwrap();
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("Widget Rail exact read completions and subsequent real panel frame deadline".into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

fn buffered_popup(record: &serde_json::Value, instance: &str) -> bool {
    buffered_popup_for(record, instance, CALENDAR.desktop_id)
}

pub(super) fn buffered_popup_for(
    record: &serde_json::Value, instance: &str, namespace: &str,
) -> bool {
    let popup = &record["popup"];
    buffered_panel(record, instance, namespace)
        && popup["role"] == "xdg_popup"
        && popup["parent_role"] == "zwlr_layer_surface_v1"
        && popup["parent_instance"] == instance
        && popup["parent_bound"] == true
        && popup["open"] == true
        && popup["closed"] == false
        && popup["configure_acked"] == true
        && popup["buffer_commits"]
            .as_u64()
            .is_some_and(|count| count > 0)
        && popup["raster"]["nonuniform_pixels"] == true
        && popup["raster"]["width"]
            .as_u64()
            .is_some_and(|size| (100..=4096).contains(&size))
        && popup["raster"]["height"]
            .as_u64()
            .is_some_and(|size| (100..=4096).contains(&size))
        && popup["geometry"][2] == popup["raster"]["width"]
        && popup["geometry"][3] == popup["raster"]["height"]
}

fn audit_file() -> std::fs::File {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(crate::paths::data_dir().join("clawd/audit.jsonl"))
        .unwrap();
    let metadata = file.metadata().unwrap();
    assert!(
        metadata.is_file()
            && metadata.uid() == 0
            && metadata.nlink() == 1
            && metadata.mode() & 0o022 == 0
            && metadata.len() <= AUDIT_LIMIT
    );
    file
}

pub(super) fn audit_since(offset: u64) -> Vec<serde_json::Value> {
    let mut file = audit_file();
    assert!(
        file.metadata().unwrap().len() >= offset,
        "audit rotated during a bounded private query"
    );
    file.seek(SeekFrom::Start(offset)).unwrap();
    let mut bytes = Vec::new();
    file.take(AUDIT_LIMIT + 1).read_to_end(&mut bytes).unwrap();
    assert!(bytes.len() as u64 <= AUDIT_LIMIT);
    // Ignore only a concurrently appended, not-yet-complete final line.
    bytes
        .split_inclusive(|byte| *byte == b'\n')
        .filter(|line| line.ends_with(b"\n"))
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
}

fn audit_offset() -> u64 {
    let mut bytes = Vec::new();
    audit_file()
        .take(AUDIT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() as u64 <= AUDIT_LIMIT);
    bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index as u64 + 1)
}

fn calendar_use(record: &serde_json::Value, session: &str) -> bool {
    let cap = crate::clawd::authority::audit::CapFacts::of(&Cap::new(
        Verb::DATA_DB_READ,
        Scope::name("calendar"),
    ));
    record["event"] == "clawd.grant.use"
        && record["route"] == "system.calendar.day"
        && record["owner_uid"] == OWNER
        && record["session"]
            == serde_json::to_value(crate::audit_policy::text_digest(session)).unwrap()
        && record["app_id"] == CALENDAR.app
        && record["decision"] == "allow"
        && record["bound_pid"].as_u64().is_some_and(|pid| pid > 1)
        && record["caps"] == serde_json::json!([cap])
}

fn calendar_request(record: &serde_json::Value, session: &str, fixture: &Fixture) -> bool {
    record["event"] == "clawd.request"
        && record["command"] == "system.calendar.day"
        && record["params"]["session"] == session
        && record["client"]["uid"] == OWNER
        && record["client"]["pid"].as_u64().is_some_and(|pid| pid > 1)
        && record["ok"] == true
        && fixture.query_day(&record["params"]).is_some()
}

pub(super) struct Driver<'a> {
    control: &'a OwnedFd,
    pam: &'a mut Child,
    retained: &'a RetainedInstance,
    process: &'a NativeProcess,
    session: &'a str,
    instance: String,
    sequence: u64,
    panel_clicks: u8,
    popup_clicks: u8,
    deadline: Instant,
}

impl<'a> Driver<'a> {
    pub(super) fn new(
        control: &'a OwnedFd, pam: &'a mut Child, retained: &'a RetainedInstance,
        process: &'a NativeProcess, session: &'a str, deadline: Instant,
    ) -> Self {
        Self {
            control, pam, retained, process, session, deadline, sequence: 0,
            panel_clicks: 0, popup_clicks: 0,
            instance: claw_display_control::Epoch(retained.instance.0).label(),
        }
    }

    pub(super) async fn live(&mut self, stage: &str) {
        if let Err(error) = check_native_process(self.retained, self.process) {
            self.fail(&format!("{stage}: {error}")).await;
        }
        if Instant::now() >= self.deadline {
            self.fail(stage).await;
        }
    }

    pub(super) async fn fail(&mut self, stage: &str) -> ! {
        renderer_failure(
            self.control,
            self.pam,
            self.retained,
            self.process,
            self.session,
            stage,
        )
        .await
    }

    pub(super) async fn click(&mut self) {
        self.panel_clicks += 1;
        assert!(self.panel_clicks <= 2, "one open and one close only");
        self.pointer("panel", "center-click".to_string()).await;
    }

    pub(super) async fn popup_click(&mut self, x: u16, y: u16) -> u64 {
        self.popup_clicks += 1;
        assert!(self.popup_clicks <= 4, "one restore, one delete and one confirmed clear only");
        self.pointer("popup", format!("{x},{y}")).await
    }

    async fn pointer(&mut self, surface: &str, value: String) -> u64 {
        self.sequence += 1;
        let directory = Path::new(OBSERVER).parent().unwrap();
        let request = serde_json::json!({
            "schema": "claw.private-keyboard/v1", "sequence": self.sequence,
            "instance": self.instance, "surface": surface,
            "expires_ms": claw_display_control::wire::monotonic_ms().unwrap() + 5_000,
            "kind": "pointer", "value": value,
        });
        let next = directory.join("keyboard-command.next");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&next)
            .unwrap();
        file.set_permissions(std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(file.metadata().unwrap().uid() == 0 && file.metadata().unwrap().nlink() == 1);
        file.write_all(&serde_json::to_vec(&request).unwrap())
            .unwrap();
        drop(file);
        std::fs::rename(next, directory.join("keyboard-command.json")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.live("native panel ordinary pointer delivery").await;
            let ack = directory.join("keyboard-ack.json");
            if let Ok(metadata) = std::fs::symlink_metadata(&ack) {
                assert!(
                    metadata.is_file()
                        && metadata.uid() == OWNER
                        && metadata.mode() & 0o077 == 0
                        && metadata.nlink() == 1
                        && metadata.len() <= 1024
                );
                let record: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(ack).unwrap()).unwrap();
                if record["sequence"] == self.sequence {
                    assert_eq!(record["instance"], self.instance);
                    assert_eq!(record["surface"], surface);
                    assert_eq!(record["pointer_events_sent"], true);
                    assert!(record.get("keyboard_events_sent").is_none());
                    return record["buffer_commits_at_delivery"].as_u64().unwrap();
                }
            }
            if Instant::now() >= deadline {
                self.fail("native panel pointer acknowledgement deadline").await;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

pub(super) async fn run(
    control: &OwnedFd,
    pam: &mut Child,
    retained: &RetainedInstance,
    process: &NativeProcess,
    session: &str,
    fixture: &Fixture,
) {
    let instance = claw_display_control::Epoch(retained.instance.0).label();
    assert!(observe().is_some_and(|record| buffered_panel(
        &record,
        &instance,
        CALENDAR.desktop_id
    ) && record["popup"]["open"] == false));
    let mut driver = Driver::new(
        control, pam, retained, process, session, Instant::now() + Duration::from_secs(25),
    );
    let guest_root = PathBuf::from(format!("/proc/{}/root", process.identity.pid()));
    match std::fs::metadata(guest_root.join("usr/local/bin/cos")) {
        Ok(metadata) if metadata.is_file() && metadata.mode() & 0o111 != 0 => {}
        result => driver
            .fail(&format!(
                "cannot verify the real fixed SDK Cos helper in this native sandbox: {result:?}"
            ))
            .await,
    }
    let hidden = guest_root.join(fixture.database.strip_prefix("/").unwrap());
    match std::fs::symlink_metadata(hidden) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        result => {
            driver
                .fail(&format!(
                    "raw Calendar DB absence was not verified: {result:?}"
                ))
                .await
        }
    }
    let offset = audit_offset();
    assert_eq!(std::fs::read(&fixture.database).unwrap(), fixture.before);
    let selector = Path::new("/run/gui-public/native-calendar-snapshot-enabled");
    let mut snapshot = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .custom_flags(libc::O_NOFOLLOW)
        .open(selector)
        .unwrap();
    snapshot
        .set_permissions(std::fs::Permissions::from_mode(0o644))
        .unwrap();
    assert!(snapshot.metadata().unwrap().uid() == 0 && snapshot.metadata().unwrap().nlink() == 1);
    snapshot
        .write_all(
            &serde_json::to_vec(&serde_json::json!({
                "schema":"claw.private-calendar-synthetic-shm/v1", "instance":driver.instance,
            }))
            .unwrap(),
        )
        .unwrap();
    drop(snapshot);
    driver.click().await;
    let (frame, request, use_record) = loop {
        driver
            .live("Calendar original popup pixels and real owner-scoped SDK day request")
            .await;
        let audit = audit_since(offset);
        let requests = audit
            .iter()
            .filter(|record| {
                record["command"] == "system.calendar.day" && record["params"]["session"] == session
            })
            .collect::<Vec<_>>();
        let uses = audit
            .iter()
            .filter(|record| calendar_use(record, session))
            .collect::<Vec<_>>();
        assert!(
            requests.len() <= 1 && uses.len() <= 1,
            "unexpected Calendar request repetition"
        );
        if let Some(request) = requests.first() {
            if !calendar_request(request, session, fixture) {
                driver.fail("actual Calendar SDK request was denied, failed or selected an unexpected day").await;
            }
            if let (Some(use_record), Some(frame)) = (
                uses.first(),
                observe().filter(|record| buffered_popup(record, &driver.instance)),
            ) {
                break (frame, (*request).clone(), (*use_record).clone());
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    driver.click().await;
    let closed_frame = loop {
        driver
            .live("Calendar original popup close while panel and native process remain live")
            .await;
        if let Some(record) = observe().filter(|record| {
            buffered_panel(record, &driver.instance, CALENDAR.desktop_id)
                && record["popup"]["parent_instance"] == driver.instance
                && record["popup"]["closed"] == true
                && record["popup"]["open"] == false
        }) {
            break record;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(std::fs::read(&fixture.database).unwrap(), fixture.before);
    let queries = Path::new("/run/cos/calendar-queries");
    if !queries.is_dir() || queries.read_dir().unwrap().next().is_some() {
        driver
            .fail("the real Calendar database reader view was not prepared and retired")
            .await;
    }
    let report = serde_json::json!({
        "schema": "claw.private-native-calendar-consumer/v1",
        "instance": driver.instance, "owner_uid": OWNER,
        "fixture_event_id": EVENT_ID, "fixture_event_title": EVENT_TITLE,
        "fixture_first_day": fixture.first.to_string(), "fixture_end_day_exclusive": fixture.end.to_string(),
        "request": request, "exact_scope_use": use_record, "popup": frame["popup"],
        "passive_agenda_snapshot": closed_frame["popup"]["snapshot"],
        "ordinary_pointer_deliveries": driver.sequence, "original_popup_closed": true,
        "real_existing_reader_success": true,
        "sdk_reply_delivery_and_decode_asserted": false,
        "response_content_inspected": false, "agenda_text_rendering_asserted": false,
        "coverage": "The real managed SDK day request completed successfully at Root against the canonical synthetic owner DB; a same-instance Layer-parented popup rendered real pixels and closed. Production audit omits response records/delivery acknowledgements; App reply decoding and exact returned/painted event text are NOT asserted.",
    });
    std::fs::write(REPORT, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    std::fs::set_permissions(REPORT, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn native_calendar_popup_readiness_requires_real_same_layer_parent_pixels() {
    let mut record = serde_json::json!({
        "schema": "claw.private-native-panel/v1", "instance": "instance",
        "role": "zwlr_layer_surface_v1", "namespace": CALENDAR.desktop_id, "layer": "Top",
        "layer_surfaces": 1, "toplevels": 0, "keyboard_interactivity": "None",
        "configure_acked": true, "configure_size": [40,40], "closed": false,
        "pending_configures": 0,
        "buffer_commits": 1, "raster": {"width":40,"height":40,"nonuniform_pixels":true},
    });
    assert!(!buffered_popup(&record, "instance"));
    record["popup"] = serde_json::json!({
        "role":"xdg_popup", "parent_role":"zwlr_layer_surface_v1", "parent_instance":"instance",
        "parent_bound":true, "open":true, "closed":false, "configure_acked":true,
        "buffer_commits":1, "geometry":[0,40,380,640],
        "raster":{"width":380,"height":640,"nonuniform_pixels":true},
    });
    assert!(buffered_popup(&record, "instance"));
    for (field, invalid) in [
        ("role", serde_json::json!("xdg_toplevel")),
        ("parent_instance", serde_json::json!("foreign")),
        ("parent_bound", serde_json::json!(false)),
        ("configure_acked", serde_json::json!(false)),
        ("buffer_commits", serde_json::json!(0)),
        ("geometry", serde_json::json!([0, 40, 381, 640])),
        (
            "raster",
            serde_json::json!({"width":380,"height":640,"nonuniform_pixels":false}),
        ),
        ("closed", serde_json::json!(true)),
    ] {
        let mut invalid_record = record.clone();
        invalid_record["popup"][field] = invalid;
        assert!(!buffered_popup(&invalid_record, "instance"), "{field}");
    }
}

#[test]
fn native_calendar_popup_audit_requires_bound_owner_exact_scope_day_and_success() {
    let session = "app-synthetic-calendar";
    let cap = crate::clawd::authority::audit::CapFacts::of(&Cap::new(
        Verb::DATA_DB_READ,
        Scope::name("calendar"),
    ));
    let record = serde_json::json!({
        "event":"clawd.grant.use", "route":"system.calendar.day", "owner_uid":OWNER,
        "session":crate::audit_policy::text_digest(session), "app_id":CALENDAR.app,
        "decision":"allow", "bound_pid":42, "caps":[cap],
    });
    assert!(calendar_use(&record, session));
    assert!(!calendar_use(&record, "foreign"));
    for (field, invalid) in [
        ("owner_uid", serde_json::json!(OWNER + 1)),
        ("app_id", serde_json::json!("calendar")),
        ("decision", serde_json::json!("deny")),
        ("caps", serde_json::json!([])),
    ] {
        let mut bad = record.clone();
        bad[field] = invalid;
        assert!(!calendar_use(&bad, session), "{field}");
    }
    let mut bad_scope = record;
    bad_scope["caps"] = serde_json::json!([crate::clawd::authority::audit::CapFacts::of(
        &Cap::new(Verb::DATA_DB_READ, Scope::name("other")),
    )]);
    assert!(!calendar_use(&bad_scope, session));
    let day = chrono::NaiveDate::from_ymd_opt(2026, 9, 9).unwrap();
    let fixture = Fixture {
        database: PathBuf::new(),
        before: Vec::new(),
        first: day,
        end: day.succ_opt().unwrap(),
    };
    let request = serde_json::json!({
        "event":"clawd.request", "command":"system.calendar.day", "params":{
            "session":session, "year":2026, "month":9, "day":9,
        }, "client":{"uid":OWNER,"pid":43}, "ok":true,
    });
    assert!(calendar_request(&request, session, &fixture));
    let mut bad = request.clone();
    bad["ok"] = false.into();
    assert!(!calendar_request(&bad, session, &fixture));
    let mut foreign = request.clone();
    foreign["client"]["uid"] = (OWNER + 1).into();
    assert!(!calendar_request(&foreign, session, &fixture));
    let mut bad = request;
    bad["params"]["day"] = 10.into();
    assert!(!calendar_request(&bad, session, &fixture));
}
