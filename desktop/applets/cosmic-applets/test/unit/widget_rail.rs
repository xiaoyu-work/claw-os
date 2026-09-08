use super::*;

#[test]
fn translates_all_presented_fields_without_inventing_state() {
    let event = calendar_event(calendar::CalendarEvent {
        id: "event-1".into(), title: "Review".into(), start: "2026-09-08".into(),
        end: Some("2026-09-09".into()), location: "Office".into(),
    });
    assert_eq!(event.id, "event-1");
    assert_eq!(event.title, "Review");
    assert_eq!(event.start, "2026-09-08");
    assert_eq!(event.end.as_deref(), Some("2026-09-09"));
    assert_eq!(event.location, "Office");
    let summary = system_summary(system::SystemSummary {
        cpu_percent: Some(42.0),
        memory: Some(system::Usage { used_mb: 2, total_mb: 4 }),
        storage: Some(system::Usage { used_mb: 3, total_mb: 5 }),
        network_down_bps: Some(123),
        network_up_bps: Some(456),
        fallback: true,
    });
    assert_eq!(summary.cpu_percent, Some(42.0));
    assert_eq!(summary.memory.unwrap(), Usage { used_mb: 2, total_mb: 4 });
    assert_eq!(summary.storage.unwrap(), Usage { used_mb: 3, total_mb: 5 });
    assert_eq!(summary.network_down_bps, Some(123));
    assert_eq!(summary.network_up_bps, Some(456));
    assert!(summary.fallback);
}

#[tokio::test]
async fn host_providers_check_exact_scopes_before_data_access() {
    use std::{ffi::OsString, fs, os::unix::fs::PermissionsExt, path::PathBuf};
    struct Fixture {
        root: PathBuf,
        original: Option<OsString>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            unsafe {
                match &self.original {
                    Some(value) => std::env::set_var("COS_BIN", value),
                    None => std::env::remove_var("COS_BIN"),
                }
            }
            fs::remove_dir_all(&self.root).unwrap();
        }
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../build")
        .join(format!("widget-provider-test-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let fixture = Fixture { root, original: std::env::var_os("COS_BIN") };
    let binary = fixture.root.join("cos");
    fs::write(&binary, r#"#!/bin/sh
root=${0%/*}
printf '%s\n' "$*" >> "$root/calls"
if [ "$1" = "__policy" ]; then
    if [ -f "$root/allow" ]; then
        printf '%s\n' '{"decision":"allow"}'
    else
        printf '%s\n' '{"decision":"deny","reason":"denied"}'
    fi
elif [ "$1 $2" = "agent ls" ]; then
    printf '%s\n' '{"n":1,"tasks":[{"id":"task-1","purpose":"Review","status":"running","created_at":"2026-09-08"}]}'
elif [ "$1 $2" = "sys resources" ]; then
    printf '%s\n' '{"memory":{"used_mb":200,"total_mb":1000},"disk":{"used_mb":300,"total_mb":2000}}'
else
    exit 1
fi
"#).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    unsafe { std::env::set_var("COS_BIN", &binary); }
    let sources = providers();
    assert_eq!((sources.calendar)().await, Err("denied".into()));
    assert_eq!((sources.tasks)().await, Err("denied".into()));
    assert_eq!((sources.system)().await, Err("denied".into()));
    assert_eq!(fs::read_to_string(fixture.root.join("calls")).unwrap(),
        "__policy check data.db.read --name calendar\n\
         __policy check agent.observe --name tasks\n\
         __policy check sys.observe --wild\n");

    fs::write(fixture.root.join("allow"), "").unwrap();
    let tasks = (sources.tasks)().await.unwrap();
    assert_eq!(tasks, vec![Task {
        id: "task-1".into(), purpose: "Review".into(),
        status: "running".into(), created_at: "2026-09-08".into(),
    }]);
    let summary = (sources.system)().await.unwrap();
    assert_eq!(summary.memory, Some(Usage { used_mb: 200, total_mb: 1000 }));
    assert_eq!(summary.storage, Some(Usage { used_mb: 300, total_mb: 2000 }));
    assert!(!summary.fallback);
    assert!(fs::read_to_string(fixture.root.join("calls")).unwrap().ends_with(
        "__policy check agent.observe --name tasks\nagent ls\n\
         __policy check sys.observe --wild\nsys resources\n"));
}
