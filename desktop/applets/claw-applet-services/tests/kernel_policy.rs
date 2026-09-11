#![cfg(all(feature = "provider", target_os = "linux"))]

use claw_os_sdk::applet::{
    CalendarDate, Client, ClientError, HistoryPermission,
    protocol::{ErrorCode, Failure},
};

fn denied(result: Result<(), ClientError>) {
    assert!(
        matches!(
            result,
            Err(ClientError::Provider(Failure {
                code: ErrorCode::PermissionDenied,
                ..
            }))
        ),
        "{result:?}"
    );
}

#[tokio::test]
#[ignore = "run through test/support/run_kernel_fixture.py with real built cos/provider binaries"]
async fn real_kernel_context() {
    assert_ne!(unsafe { libc::geteuid() }, 0);
    assert_eq!(
        unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) },
        1
    );
    let case = std::env::var("CLAW_APPLET_KERNEL_CASE").unwrap();
    let client = Client::installed();
    match case.as_str() {
        "exact-history-read" => {
            client
                .require_history(HistoryPermission::Read)
                .await
                .unwrap();
            denied(client.require_history(HistoryPermission::Write).await);
        }
        "calendar-own" => {
            let events = client
                .calendar_day(CalendarDate {
                    year: 2026,
                    month: 9,
                    day: 9,
                })
                .await
                .unwrap();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].id, "private-owner-event");
        }
        "calendar-foreign" => {
            let result = client
                .calendar_day(CalendarDate {
                    year: 2026,
                    month: 9,
                    day: 9,
                })
                .await;
            assert!(
                matches!(
                    result,
                    Err(ClientError::Provider(Failure {
                        code: ErrorCode::ProviderFailure,
                        ..
                    }))
                ),
                "{result:?}"
            );
        }
        "missing-session" | "missing-grant" | "wrong-scope" | "wrong-pid" | "untrusted-app"
        | "wrong-app" | "wrong-start" => {
            denied(client.require_history(HistoryPermission::Read).await);
        }
        _ => panic!("unknown private kernel fixture case: {case}"),
    }
    println!("verified real kernel case: {case}; euid={}", unsafe {
        libc::geteuid()
    });
}
