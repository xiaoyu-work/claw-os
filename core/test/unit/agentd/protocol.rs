use super::*;

use crate::agentd::grant::{GrantClaims, GrantSigner, GRANT_AUDIENCE, GRANT_VERSION};

fn signed_grant() -> crate::agentd::grant::SignedGrant {
    GrantSigner::from_secret([3u8; 32]).issue(GrantClaims {
        v: GRANT_VERSION,
        audience: GRANT_AUDIENCE.to_string(),
        broker_pid: 1,
        task_id: "task-a".to_string(),
        session_id: None,
        owner_uid: 1000,
        client: crate::session::SessionClient::default(),
        presence: None,
        capability_generation: "empty".to_string(),
        prepare_nonce: "0123456789abcdef0123456789abcdef".to_string(),
        commit_nonce: "fedcba9876543210fedcba9876543210".to_string(),
        extension: None,
        owner_gid: 1000,
        worker_pid: 2,
        worker_start_time_ticks: None,
        issued_at_ms: 0,
        expires_at_ms: 1,
        routes: worker_routes(),
    })
}

#[test]
fn the_worker_channel_exposes_only_job_lifecycle_routes() {
    // The broker socket's own routes must not be reachable from a
    // worker grant: no admin, App-session, scheduler or
    // permission-decision surface exists on this channel.
    for route in crate::clawd::routes::ROUTES.iter().map(|route| route.name) {
        assert!(
            !WORKER_ROUTES.contains(&route),
            "broker route `{route}` leaked onto the worker channel"
        );
    }
    assert_eq!(
        WORKER_ROUTES,
        &[
            ROUTE_PREPARED,
            ROUTE_HELLO,
            ROUTE_STREAM,
            ROUTE_PROGRESS,
            ROUTE_AUDIT,
            ROUTE_HEARTBEAT,
            ROUTE_RESULT,
            ROUTE_APPROVAL
        ]
    );
}

#[test]
fn permission_mediation_is_the_only_consent_surface_and_carries_no_identity() {
    // The worker channel has a consent route, but it is not a broker
    // proxy: there is no decide route, and an ask cannot name a session,
    // an owner, a task, a capability or a decision.
    assert!(!WORKER_ROUTES.contains(&"permission.decide"));
    assert!(!WORKER_ROUTES.contains(&"permission.request"));
    assert!(!WORKER_ROUTES.contains(&"app_session.register"));
    assert!(!WORKER_ROUTES.contains(&"scheduler.run"));

    let ask = ApprovalAsk::Request {
        verb: "fs.read".to_string(),
        scope: crate::caps::Scope::path("/home/user/notes.txt"),
        operation_digest: None,
    };
    let encoded = serde_json::to_string(&ask).expect("encode");
    for forbidden in ["session", "owner", "uid", "caps", "role", "duration"] {
        assert!(
            !encoded.contains(forbidden),
            "an ask must not carry `{forbidden}`: {encoded}"
        );
    }
    assert_eq!(ask.verb(), "fs.read");
}

#[test]
fn approval_exchange_nonce_is_unpredictable_and_binds_the_exact_ask() {
    let ask = ApprovalAsk::Consume {
        verb: "proc.spawn".to_string(),
        scope: crate::caps::Scope::self_ref("children"),
        operation_digest: Some(crate::crypto::sha256_hex(b"native invocation")),
    };
    let first = ApprovalExchange::new(ask.clone());
    let second = ApprovalExchange::new(ask.clone());

    assert!(first.is_valid());
    assert!(second.is_valid());
    assert_ne!(first.nonce, second.nonce);
    assert_eq!(first.ask, ask);
    let encoded = serde_json::to_string(&first).unwrap();
    let decoded: ApprovalExchange = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, first);
}

#[test]
fn every_worker_frame_names_its_route_and_task() {
    let frames = [
        WorkerFrame::Hello(Box::new(WorkerHello {
            protocol: PROTOCOL_VERSION,
            security_epoch: crate::update::SECURITY_EPOCH,
            grant: signed_grant(),
            pid: 2,
            start_time_ticks: None,
            uid: 1000,
            euid: 1000,
            gid: 1000,
            egid: 1000,
            supplementary_groups: Vec::new(),
            no_new_privs: true,
            dumpable: false,
        })),
        WorkerFrame::Heartbeat {
            task_id: "task-a".to_string(),
        },
        WorkerFrame::Progress {
            task_id: "task-a".to_string(),
            progress: ProgressRecord::ToolStart {
                id: "call-1".to_string(),
                name: "cos_fs".to_string(),
            },
        },
        WorkerFrame::Result {
            task_id: "task-a".to_string(),
            outcome: Box::new(WorkerOutcome::Cancelled),
        },
    ];
    for frame in frames {
        assert!(WORKER_ROUTES.contains(&frame.route()));
        assert_eq!(frame.task_id(), Some("task-a"));
    }
}

#[test]
fn progress_is_persisted_in_the_existing_stream_shape() {
    let value = ProgressRecord::ToolStart {
        id: "call-1".to_string(),
        name: "cos_fs".to_string(),
    }
    .to_stream_value();
    assert_eq!(value["kind"], "tool_start");
    assert_eq!(value["id"], "call-1");
    assert_eq!(value["name"], "cos_fs");

    let value = ProgressRecord::ToolResult {
        id: "call-1".to_string(),
        name: "cos_fs".to_string(),
        ok: true,
        latency_ms: 12,
    }
    .to_stream_value();
    assert_eq!(value["kind"], "tool_result");
    assert_eq!(value["ok"], true);
    assert_eq!(value["latency_ms"], 12);
}

#[test]
fn a_worker_cannot_inflate_the_identifiers_it_persists() {
    let value = ProgressRecord::ToolStart {
        id: "x".repeat(4096),
        name: "y".repeat(4096),
    }
    .to_stream_value();
    assert_eq!(value["id"].as_str().unwrap().chars().count(), 128);
    assert_eq!(value["name"].as_str().unwrap().chars().count(), 128);
}

#[tokio::test]
async fn frames_round_trip_through_the_reader() {
    let frame = WorkerFrame::Heartbeat {
        task_id: "task-a".to_string(),
    };
    let encoded = encode(&frame).expect("encode");
    assert!(encoded.ends_with('\n'));
    let mut reader = FrameReader::new(tokio::io::BufReader::new(encoded.as_bytes()));
    let decoded: WorkerFrame = reader.next_frame().await.expect("read").expect("frame");
    assert_eq!(decoded.task_id(), Some("task-a"));
    assert!(reader
        .next_frame::<WorkerFrame>()
        .await
        .expect("eof")
        .is_none());
}

#[tokio::test]
async fn an_oversized_frame_is_refused_rather_than_buffered() {
    let mut payload = Vec::with_capacity(MAX_FRAME_BYTES + 16);
    payload.extend(std::iter::repeat(b'x').take(MAX_FRAME_BYTES + 8));
    payload.push(b'\n');
    let mut reader = FrameReader::new(tokio::io::BufReader::new(payload.as_slice()));
    let error = reader
        .next_frame::<WorkerFrame>()
        .await
        .expect_err("oversized frame must be refused");
    assert!(error.contains("exceeded"), "{error}");
}

#[test]
fn a_reported_outcome_maps_onto_the_queue_outcome() {
    let finish: crate::agent::service::FinishOutcome = WorkerOutcome::Ok(Box::new(CompletedRun {
        response: "hi".to_string(),
        turns_used: 2,
        provider: "openai".to_string(),
        model: "gpt".to_string(),
        evidence: None,
        fallback: None,
    }))
    .into();
    assert!(matches!(
        finish,
        crate::agent::service::FinishOutcome::Ok { turns_used: 2, .. }
    ));
    let finish: crate::agent::service::FinishOutcome = WorkerOutcome::Cancelled.into();
    assert!(matches!(
        finish,
        crate::agent::service::FinishOutcome::Cancelled
    ));

    let finish: crate::agent::service::FinishOutcome = WorkerOutcome::WaitingApproval {
        request_ids: vec!["approval-a".to_string()],
    }
    .into();
    assert!(matches!(
        finish,
        crate::agent::service::FinishOutcome::WaitingApproval { request_ids }
            if request_ids == vec!["approval-a"]
    ));
}
