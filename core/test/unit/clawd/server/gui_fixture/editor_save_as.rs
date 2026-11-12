use super::*;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

const AUTHORED: &str = "editor ui save as scoped bytes\nsecond line\n";

fn authority_snapshot(session: &str, parent: &str) -> (CapSet, CapSet) {
    crate::paths::RoutedPathContext::for_owner(OWNER, PathBuf::from("/run/display-home"))
        .scope_sync(|| {
            (
                crate::proc::session_info_by_id(session)
                    .unwrap()
                    .caps
                    .unwrap(),
                crate::proc::session_info_by_id(parent)
                    .unwrap()
                    .caps
                    .unwrap(),
            )
        })
}

struct Driver<'a> {
    control: &'a OwnedFd,
    pam: &'a mut Child,
    retained: &'a RetainedInstance,
    process: &'a NativeProcess,
    session: &'a str,
    window: &'a str,
    sequence: u64,
}

impl Driver<'_> {
    async fn fail(&mut self, stage: &str) -> ! {
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

    async fn key(&mut self, surface: &str, kind: &str, value: &str) {
        self.sequence += 1;
        let directory = Path::new(OBSERVER).parent().unwrap();
        let next = directory.join("keyboard-command.next");
        let request = serde_json::json!({
            "schema": "claw.private-keyboard/v1", "sequence": self.sequence,
            "instance": self.window, "surface": surface,
            "expires_ms": claw_display_control::wire::monotonic_ms().unwrap() + 5_000,
            "kind": kind, "value": value,
        });
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&next)
            .unwrap();
        file.set_permissions(std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert_eq!(file.metadata().unwrap().uid(), 0);
        file.write_all(&serde_json::to_vec(&request).unwrap())
            .unwrap();
        drop(file);
        std::fs::rename(next, directory.join("keyboard-command.json")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            native_process(self.retained, self.process, "Editor UI keyboard delivery");
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
                    serde_json::from_slice(&std::fs::read(&ack).unwrap()).unwrap();
                if record["sequence"] == self.sequence {
                    assert_eq!(record["instance"], self.window);
                    assert_eq!(record["surface"], surface);
                    assert_eq!(record["keyboard_events_sent"], true);
                    break;
                }
            }
            if Instant::now() >= deadline {
                self.fail("Editor UI keyboard delivery deadline").await;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn frame(
        &mut self,
        stage: &str,
        mut expected: impl FnMut(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            native_process(self.retained, self.process, stage);
            if let Some(record) = observe().filter(|record| expected(record)) {
                return record;
            }
            if Instant::now() >= deadline {
                self.fail(stage).await;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn review(&mut self, access: &str, path: &Path) -> crate::approvals::Request {
        let deadline = Instant::now() + Duration::from_secs(25);
        let prefix = format!("gui-{access}-{}-", self.window);
        loop {
            native_process(
                self.retained,
                self.process,
                "Editor UI protected write review",
            );
            if let Some(request) = crate::approvals::list_pending_for_owner(Some(OWNER))
                .into_iter()
                .find(|request| request.verb == "fs.write" && request.session.starts_with(&prefix))
            {
                let purpose = if access == "create" {
                    format!("Create ONLY absent regular file {} once,", path.display())
                } else {
                    format!("Write ONLY selected existing file {},", path.display())
                };
                assert!(
                    request.reason.starts_with(&purpose),
                    "UI selected an unexpected target"
                );
                assert!(request.reason.contains(&format!("window {}", self.window)));
                assert_eq!(request.owner_uid, Some(OWNER));
                assert_eq!(request.scope, Scope::path("/run/display-home/**"));
                assert!(request.operation_digest.is_some());
                return request;
            }
            if Instant::now() >= deadline {
                self.fail("Editor UI protected write review deadline").await;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

pub(super) struct Saved {
    path: PathBuf,
    overwrite: String,
    journal: crate::session::SessionId,
}

impl Saved {
    pub(super) fn verify_after_retirement(&self) {
        assert_eq!(std::fs::read(&self.path).unwrap(), AUTHORED.as_bytes());
        assert!(matches!(
            crate::approvals::status_for_owner(&self.overwrite, Some(OWNER)),
            crate::approvals::RequestStatus::Denied
        ));
        let context =
            crate::paths::RoutedPathContext::for_owner(OWNER, PathBuf::from("/run/display-home"));
        assert_eq!(
            context
                .scope_sync(|| crate::session::get_meta(&self.journal))
                .unwrap()
                .status,
            crate::session::Status::Done
        );
    }
}

pub(super) async fn run(
    control: &OwnedFd,
    pam: &mut Child,
    retained: &RetainedInstance,
    process: &NativeProcess,
    session: &str,
    parent: &str,
) -> Saved {
    let window = claw_display_control::Epoch(retained.instance.0).label();
    let leaf = format!("native-editor-ui-{window}.txt");
    let path = Path::new("/run/display-home").join(&leaf);
    assert!(!path.exists());
    let original = authority_snapshot(session, parent);
    let before = observe().unwrap()["buffer_commits"].as_u64().unwrap();
    let mut driver = Driver {
        control,
        pam,
        retained,
        process,
        session,
        window: &window,
        sequence: 0,
    };

    // Ctrl+T is Editor's NewFile; Ctrl+S on this untitled buffer opens its original Save As dialog.
    driver.key("primary", "key", "ctrl-t").await;
    let untitled = driver
        .frame("Editor new-document frame", |record| {
            buffered_window(record, &window)
                && !loaded(record)
                && record["buffer_commits"].as_u64().unwrap() > before
        })
        .await;
    driver.key("primary", "text", AUTHORED).await;
    driver
        .frame("Editor authored-buffer frame", |record| {
            buffered_window(record, &window)
                && record["buffer_commits"].as_u64().unwrap()
                    > untitled["buffer_commits"].as_u64().unwrap()
                && record["raster"]["pixel_checksum"] != untitled["raster"]["pixel_checksum"]
        })
        .await;
    driver.key("primary", "key", "ctrl-s").await;
    let chooser = driver
        .frame("real Editor Save As chooser frame", |record| {
            let dialog = &record["auxiliary"];
            dialog["open"] == true
                && dialog["configure_acked"] == true
                && dialog["buffer_commits"]
                    .as_u64()
                    .is_some_and(|count| count > 0)
                && dialog["raster"]["width"]
                    .as_u64()
                    .is_some_and(|size| size >= 100)
                && dialog["raster"]["height"]
                    .as_u64()
                    .is_some_and(|size| size >= 100)
                && dialog["raster"]["nonuniform_pixels"] == true
        })
        .await;

    // The pinned chooser's TabRescan focuses filename_id, whose Enter handler is Message::Save(false).
    driver.key("auxiliary", "key", "focus").await;
    driver
        .frame("chooser keyboard-focus frame", |record| {
            let dialog = &record["auxiliary"];
            dialog["open"] == true
                && dialog["raster"]["nonuniform_pixels"] == true
                && dialog["buffer_commits"].as_u64().unwrap()
                    > chooser["auxiliary"]["buffer_commits"].as_u64().unwrap()
        })
        .await;
    driver.key("auxiliary", "key", "ctrl-a").await;
    driver.key("auxiliary", "text", &leaf).await;
    driver.key("auxiliary", "key", "enter").await;
    let create = driver.review("create", &path).await;
    assert!(create
        .reason
        .contains("without replacing an existing target"));
    assert!(create.reason.contains("parent native node 1"));
    assert!(
        !path.exists(),
        "UI creation happened before independent protected consent"
    );
    let before_approval = observe().unwrap()["buffer_commits"].as_u64().unwrap();
    let approved = approve_native_resource(&create.id).await;
    assert_eq!(approved.id, create.id);

    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        native_process(retained, process, "durable actual Editor Save As");
        if std::fs::read(&path).is_ok_and(|bytes| bytes == AUTHORED.as_bytes()) {
            break;
        }
        if Instant::now() >= deadline {
            driver.fail("actual Editor Save As bytes").await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(matches!(
        crate::approvals::status_for_owner(&create.id, Some(OWNER)),
        crate::approvals::RequestStatus::Consumed
    ));
    driver
        .frame("Editor chooser closed after native Save As", |record| {
            buffered_window(record, &window)
                && record["auxiliary"]["open"] == false
                && record["buffer_commits"].as_u64().unwrap() > before_approval
        })
        .await;
    assert_eq!(authority_snapshot(session, parent), original);

    let context =
        crate::paths::RoutedPathContext::for_owner(OWNER, PathBuf::from("/run/display-home"));
    let journal = crate::clawd::gui::selected_mutation_session(OWNER, session).unwrap();
    let mutations = context
        .clone()
        .scope_sync(|| crate::session::iter_mutations(&journal))
        .unwrap();
    assert_eq!(mutations.len(), 1);
    assert!(matches!(&mutations[0].mutation,
        crate::session::Mutation::FsWrite { path: written, prev_blob: None } if Path::new(written) == path));
    let projection = context
        .clone()
        .scope_sync(|| {
            crate::session::journal::projection::build(
                &crate::session::journal::Partition::Session(journal.clone()),
                OWNER,
            )
        })
        .unwrap();
    assert!(projection.health.is_verified());
    assert_eq!(projection.mutations.len(), 1);
    assert_eq!(projection.mutations[0].status, "committed");
    assert_eq!(
        projection.mutations[0].session_mutation,
        Some(mutations[0].seq)
    );
    assert!(context
        .clone()
        .scope_sync(|| crate::session::get_caps(&journal))
        .unwrap()
        .is_empty());

    // A real subsequent UI save must ask separately; the one-shot creation cannot overwrite.
    driver.key("primary", "key", "ctrl-a").await;
    driver
        .key("primary", "text", "unapproved overwrite\n")
        .await;
    driver.key("primary", "key", "ctrl-s").await;
    let overwrite = driver.review("write", &path).await;
    assert_ne!(overwrite.id, create.id);
    assert_ne!(overwrite.operation_digest, create.operation_digest);
    assert_eq!(std::fs::read(&path).unwrap(), AUTHORED.as_bytes());
    assert_eq!(
        context
            .scope_sync(|| crate::session::iter_mutations(&journal))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(authority_snapshot(session, parent), original);
    println!("private actual Editor UI Save As: exact durable bytes, canonical Missing inverse, independent creation and separately pending overwrite passed");
    Saved {
        path,
        overwrite: overwrite.id,
        journal,
    }
}
