use super::*;
use protocol::{ErrorCode, MAX_RESPONSE_BYTES};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(body: &str) -> Self {
        let executable = std::env::current_exe().unwrap();
        // Keep fixtures in Cargo output, never in a verified SDK source payload.
        let root = executable
            .parent()
            .expect("test executable directory")
            .join("applet-sdk-fixtures")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let root = root.canonicalize().unwrap();
        let source = format!(
            "#!/bin/sh\nset -eu\n[ \"$#\" = 1 ] && [ \"$1\" = --stdio-v1 ]\n\
             root=${{0%/*}}\nprintf '%s\\n' \"$$\" >> \"$root/pids\"\n{body}\n",
        );
        fs::write(root.join("provider"), source).unwrap();
        fs::set_permissions(root.join("provider"), fs::Permissions::from_mode(0o700)).unwrap();
        Self { root }
    }

    fn client(&self) -> Client {
        Client::with_binary(self.root.join("provider")).unwrap()
    }

    fn pid(&self) -> Option<u32> {
        fs::read_to_string(self.root.join("pids"))
            .ok()?
            .lines()
            .next_back()?
            .parse()
            .ok()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

async fn wait_for_pid(fixture: &Fixture) -> u32 {
    timeout(Duration::from_secs(3), async {
        loop {
            if let Some(pid) = fixture.pid() {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

async fn wait_for_exit(pid: u32) {
    timeout(Duration::from_secs(3), async {
        while PathBuf::from(format!("/proc/{pid}")).exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the private provider must be killed and reaped");
}

#[tokio::test]
async fn keeps_one_provider_and_never_turns_denial_into_success() {
    let fixture = Fixture::new(
        r#"
number=0
while IFS= read -r request; do
    number=$((number + 1))
    printf '%s\n' "$request" >> "$root/requests"
    if [ "$number" = 2 ]; then
        printf '{"version":1,"id":2,"outcome":{"status":"error","error":{"code":"permission-denied","message":"history denied"}}}\n'
    else
        printf '{"version":1,"id":%s,"outcome":{"status":"ok","data":{"kind":"history-allowed"}}}\n' "$number"
    fi
done
"#,
    );
    let client = fixture.client();
    client
        .require_history(HistoryPermission::Read)
        .await
        .unwrap();
    let error = client
        .require_history(HistoryPermission::Write)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ClientError::Provider(Failure {
            code: ErrorCode::PermissionDenied,
            ..
        })
    ));
    assert_eq!(error.to_string(), "history denied");
    client
        .clone()
        .require_history(HistoryPermission::Read)
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(fixture.root.join("pids"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let requests: Vec<Request> = fs::read_to_string(fixture.root.join("requests"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        requests
            .iter()
            .map(|request| request.id)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert!(requests.iter().all(|request| request.version == 1));
    assert_eq!(
        requests[1].operation,
        Operation::HistoryCheck {
            permission: HistoryPermission::Write
        }
    );
    let pid = fixture.pid().unwrap();
    drop(client);
    wait_for_exit(pid).await;
}

#[tokio::test]
async fn validates_before_spawn_and_reports_missing_provider() {
    let fixture = Fixture::new("exit 99");
    let error = fixture
        .client()
        .calendar_day(CalendarDate {
            year: 2026,
            month: 2,
            day: 30,
        })
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ClientError::Provider(Failure {
            code: ErrorCode::InvalidRequest,
            ..
        })
    ));
    assert!(!fixture.root.join("pids").exists());
    assert!(Client::with_binary("provider").is_err());
    let client = Client::with_binary(fixture.root.join("missing")).unwrap();
    assert!(matches!(
        client.tasks().await,
        Err(ClientError::Unavailable(_))
    ));
}

#[tokio::test]
async fn rejects_malformed_wrong_version_id_kind_and_oversized_output() {
    for reply in [
        "{",
        r#"{"version":2,"id":1,"outcome":{"status":"ok","data":{"kind":"tasks","tasks":[]}}}"#,
        r#"{"version":1,"id":2,"outcome":{"status":"ok","data":{"kind":"tasks","tasks":[]}}}"#,
        r#"{"version":1,"id":1,"outcome":{"status":"ok","data":{"kind":"history-allowed"}}}"#,
        r#"{"version":1,"id":1,"outcome":{"status":"ok","data":{"kind":"tasks","tasks":[]},"owner":10}}"#,
    ] {
        let fixture = Fixture::new("IFS= read -r request\ncat \"$root/reply\"");
        fs::write(fixture.root.join("reply"), format!("{reply}\n")).unwrap();
        assert!(matches!(
            fixture.client().tasks().await,
            Err(ClientError::Protocol(_))
        ));
    }
    let fixture = Fixture::new("IFS= read -r request\ncat \"$root/reply\"");
    fs::write(
        fixture.root.join("reply"),
        vec![b'x'; MAX_RESPONSE_BYTES + 1],
    )
    .unwrap();
    assert!(matches!(
        fixture.client().tasks().await,
        Err(ClientError::Io(_))
    ));
    let fixture = Fixture::new("IFS= read -r request\nexit 7");
    assert!(matches!(
        fixture.client().tasks().await,
        Err(ClientError::Protocol(_))
    ));
}

#[tokio::test]
async fn timeout_and_cancellation_kill_the_private_provider() {
    let fixture = Fixture::new("exec /bin/sleep 20");
    let client = fixture.client();
    assert!(matches!(
        client
            .request_with_timeout(Operation::Tasks {}, Duration::from_millis(100))
            .await,
        Err(ClientError::Timeout),
    ));
    wait_for_exit(fixture.pid().unwrap()).await;

    let fixture = Fixture::new("exec /bin/sleep 20");
    let client = fixture.client();
    let task = tokio::spawn(async move { client.tasks().await });
    let pid = wait_for_pid(&fixture).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    wait_for_exit(pid).await;
}

#[tokio::test]
async fn expired_and_cancelled_waiters_do_not_stop_another_clones_active_call() {
    let fixture = Fixture::new(
        r#"
number=0
while IFS= read -r request; do
    number=$((number + 1))
    printf '%s\n' "$request" >> "$root/requests"
    while [ ! -f "$root/release" ]; do /bin/sleep 0.01; done
    printf '{"version":1,"id":%s,"outcome":{"status":"ok","data":{"kind":"tasks","tasks":[]}}}\n' "$number"
done
"#,
    );
    let client = fixture.client();
    let owner = client.clone();
    let active = tokio::spawn(async move { owner.tasks().await });
    let pid = wait_for_pid(&fixture).await;
    assert!(matches!(
        client
            .request_with_timeout(Operation::Tasks {}, Duration::from_millis(20))
            .await,
        Err(ClientError::Timeout),
    ));
    let waiter = client.clone();
    let queued = tokio::spawn(async move { waiter.tasks().await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    queued.abort();
    assert!(queued.await.unwrap_err().is_cancelled());
    assert!(PathBuf::from(format!("/proc/{pid}")).exists());
    fs::write(fixture.root.join("release"), "").unwrap();
    assert!(active.await.unwrap().unwrap().is_empty());
    assert!(client.tasks().await.unwrap().is_empty());
    let requests: Vec<Request> = fs::read_to_string(fixture.root.join("requests"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        requests
            .iter()
            .map(|request| request.id)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    drop(client);
    wait_for_exit(pid).await;
}

#[tokio::test]
async fn transport_failure_respawns_without_delivering_a_stale_reply() {
    let fixture = Fixture::new(
        r#"
IFS= read -r request
if [ ! -f "$root/poisoned" ]; then
    : > "$root/poisoned"
    printf '%s\n' '{"version":1,"id":99,"outcome":{"status":"ok","data":{"kind":"tasks","tasks":[]}}}'
    printf '%s\n' '{"version":1,"id":1,"outcome":{"status":"ok","data":{"kind":"tasks","tasks":[{"id":"stale","purpose":"","status":"","created_at":""}]}}}'
    exec /bin/sleep 20
fi
printf '%s\n' '{"version":1,"id":1,"outcome":{"status":"ok","data":{"kind":"tasks","tasks":[]}}}'
while IFS= read -r request; do :; done
"#,
    );
    let client = fixture.client();
    assert!(matches!(
        client.tasks().await,
        Err(ClientError::Protocol(_))
    ));
    let old = fixture.pid().unwrap();
    wait_for_exit(old).await;
    assert!(client.tasks().await.unwrap().is_empty());
    let fresh = fixture.pid().unwrap();
    assert_ne!(old, fresh);
    drop(client);
    wait_for_exit(fresh).await;
}
