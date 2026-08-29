use super::*;

fn claims(worker_pid: u32) -> GrantClaims {
    GrantClaims {
        v: GRANT_VERSION,
        audience: GRANT_AUDIENCE.to_string(),
        broker_pid: 4242,
        task_id: "task-a".to_string(),
        session_id: Some("session-a".to_string()),
        owner_uid: 1000,
        client: crate::session::SessionClient::new(
            crate::session::SessionSource::BrokerTask,
            true,
            true,
        ),
        presence: Some(crate::session::SessionPresence {
            owner_uid: 1000,
            pid: 55,
            start_time_ticks: 44,
            expires_at_ms: 30_000,
        }),
        capability_generation: "caps-a".to_string(),
        extension: None,
        owner_gid: 1000,
        worker_pid,
        worker_start_time_ticks: Some(99),
        issued_at_ms: 1_000,
        expires_at_ms: 61_000,
        routes: vec!["hello".to_string(), "result".to_string()],
    }
}

fn expectation(route: &str) -> GrantExpectation {
    GrantExpectation {
        broker_pid: 4242,
        task_id: "task-a".to_string(),
        session_id: Some("session-a".to_string()),
        owner_uid: 1000,
        client: crate::session::SessionClient::new(
            crate::session::SessionSource::BrokerTask,
            true,
            true,
        ),
        presence: Some(crate::session::SessionPresence {
            owner_uid: 1000,
            pid: 55,
            start_time_ticks: 44,
            expires_at_ms: 30_000,
        }),
        capability_generation: "caps-a".to_string(),
        extension: None,
        worker_pid: 77,
        worker_start_time_ticks: Some(99),
        route: route.to_string(),
    }
}

fn extension(
    host_pid: u32,
    task: &str,
    session: &str,
) -> crate::extension_host::protocol::ExtensionBinding {
    crate::extension_host::protocol::ExtensionBinding {
        protocol: crate::extension_host::protocol::PROTOCOL_VERSION,
        task_id: task.to_string(),
        session_id: Some(session.to_string()),
        owner_uid: 1000,
        owner_gid: 1000,
        worker_pid: 77,
        worker_start_time_ticks: Some(99),
        host_pid,
        host_start_time_ticks: Some(123),
        lease_nonce: "0123456789abcdef0123456789abcdef".to_string(),
        expires_at_ms: 61_000,
        control_socket: "/run/cos/extensions/control.sock".to_string(),
        broker_socket: "/run/cos/extensions/broker.sock".to_string(),
    }
}

#[test]
fn a_grant_the_broker_issued_verifies_against_its_bindings() {
    let signer = GrantSigner::from_secret([7u8; 32]);
    let grant = signer.issue(claims(77));
    assert_eq!(signer.verify(&grant, &expectation("hello"), 2_000), Ok(()));
}

#[test]
fn a_worker_cannot_mint_or_edit_its_own_grant() {
    let broker = GrantSigner::from_secret([7u8; 32]);
    let forger = GrantSigner::from_secret([8u8; 32]);
    let forged = forger.issue(claims(77));
    assert_eq!(
        broker.verify(&forged, &expectation("hello"), 2_000),
        Err(GrantError::Signature)
    );

    // Editing any claim on a legitimately signed grant invalidates it,
    // so a worker cannot promote itself to another task or owner.
    let mut tampered = broker.issue(claims(77));
    tampered.claims.owner_uid = 0;
    assert_eq!(
        broker.verify(&tampered, &expectation("hello"), 2_000),
        Err(GrantError::Signature)
    );
}

#[test]
fn an_extension_binding_cannot_be_replayed_for_another_host_or_session() {
    let signer = GrantSigner::from_secret([7u8; 32]);
    let mut claims = claims(77);
    claims.extension = Some(extension(88, "task-a", "session-a"));
    let grant = signer.issue(claims);
    let mut expected = expectation("hello");
    expected.extension = Some(extension(88, "task-a", "session-a"));
    assert_eq!(signer.verify(&grant, &expected, 2_000), Ok(()));

    expected.extension = Some(extension(89, "task-a", "session-a"));
    assert_eq!(
        signer.verify(&grant, &expected, 2_000),
        Err(GrantError::Extension)
    );

    let mut replayed = grant.clone();
    replayed.claims.extension = Some(extension(88, "task-a", "session-b"));
    assert_eq!(
        signer.verify(&replayed, &expectation("hello"), 2_000),
        Err(GrantError::Signature)
    );
}

#[test]
fn a_grant_is_bound_to_one_task_owner_and_worker_process() {
    let signer = GrantSigner::from_secret([7u8; 32]);
    let grant = signer.issue(claims(77));

    let mut other_task = expectation("hello");
    other_task.task_id = "task-b".to_string();
    assert!(matches!(
        signer.verify(&grant, &other_task, 2_000),
        Err(GrantError::Task { .. })
    ));

    let mut other_owner = expectation("hello");
    other_owner.owner_uid = 1001;
    assert!(matches!(
        signer.verify(&grant, &other_owner, 2_000),
        Err(GrantError::Owner { .. })
    ));

    let mut other_source = expectation("hello");
    other_source.client.source = crate::session::SessionSource::ExternalMcp;
    assert_eq!(
        signer.verify(&grant, &other_source, 2_000),
        Err(GrantError::Client)
    );

    let mut other_generation = expectation("hello");
    other_generation.capability_generation = "caps-b".to_string();
    assert_eq!(
        signer.verify(&grant, &other_generation, 2_000),
        Err(GrantError::CapabilityGeneration)
    );

    let mut other_presence = expectation("hello");
    other_presence.presence.as_mut().unwrap().pid = 56;
    assert_eq!(
        signer.verify(&grant, &other_presence, 2_000),
        Err(GrantError::Presence)
    );

    let mut other_session = expectation("hello");
    other_session.session_id = Some("session-b".to_string());
    assert_eq!(
        signer.verify(&grant, &other_session, 2_000),
        Err(GrantError::Session)
    );

    let mut other_pid = expectation("hello");
    other_pid.worker_pid = 78;
    assert!(matches!(
        signer.verify(&grant, &other_pid, 2_000),
        Err(GrantError::WorkerPid { .. })
    ));

    // Same pid, recycled process: the kernel start time no longer
    // matches what the broker recorded at spawn.
    let mut recycled = expectation("hello");
    recycled.worker_start_time_ticks = Some(100);
    assert_eq!(
        signer.verify(&grant, &recycled, 2_000),
        Err(GrantError::WorkerIdentity)
    );
}

#[test]
fn a_grant_from_a_previous_daemon_instance_is_rejected() {
    let signer = GrantSigner::from_secret([7u8; 32]);
    let grant = signer.issue(claims(77));
    let mut restarted = expectation("hello");
    restarted.broker_pid = 4243;
    assert!(matches!(
        signer.verify(&grant, &restarted, 2_000),
        Err(GrantError::Broker { .. })
    ));
}

#[test]
fn a_grant_stops_working_when_its_lease_expires() {
    let signer = GrantSigner::from_secret([7u8; 32]);
    let grant = signer.issue(claims(77));
    assert!(matches!(
        signer.verify(&grant, &expectation("hello"), 61_001),
        Err(GrantError::Expired { .. })
    ));
}

#[test]
fn a_route_outside_the_allowlist_is_refused() {
    let signer = GrantSigner::from_secret([7u8; 32]);
    let grant = signer.issue(claims(77));
    // `scheduler.run` is a broker route; it is not on the worker
    // channel and no grant may carry it.
    assert_eq!(
        signer.verify(&grant, &expectation("scheduler.run"), 2_000),
        Err(GrantError::Route("scheduler.run".to_string()))
    );
}

#[test]
fn a_worker_refuses_a_grant_minted_for_another_process() {
    let signer = GrantSigner::from_secret([7u8; 32]);
    let grant = signer.issue(claims(77));
    assert!(matches!(
        grant.validate_for_self(2_000, 1000, 78, Some(99)),
        Err(GrantError::WorkerPid { .. })
    ));
    assert!(matches!(
        grant.validate_for_self(2_000, 1001, 77, Some(99)),
        Err(GrantError::Owner { .. })
    ));
    assert_eq!(
        grant.validate_for_self(2_000, 1000, 77, Some(100)),
        Err(GrantError::WorkerIdentity)
    );
    assert_eq!(grant.validate_for_self(2_000, 1000, 77, Some(99)), Ok(()));
}

#[test]
fn a_grant_for_a_different_audience_is_not_accepted_here() {
    let signer = GrantSigner::from_secret([7u8; 32]);
    let mut other = claims(77);
    other.audience = "cos.clawd.broker.v1".to_string();
    let grant = signer.issue(other);
    assert!(matches!(
        signer.verify(&grant, &expectation("hello"), 2_000),
        Err(GrantError::Audience { .. })
    ));
    assert!(matches!(
        grant.validate_for_self(2_000, 1000, 77, Some(99)),
        Err(GrantError::Audience { .. })
    ));
}

#[test]
fn a_grant_version_mismatch_fails_closed() {
    let signer = GrantSigner::from_secret([7u8; 32]);
    let mut future = claims(77);
    future.v = GRANT_VERSION + 1;
    let grant = signer.issue(future);
    assert!(matches!(
        signer.verify(&grant, &expectation("hello"), 2_000),
        Err(GrantError::Version { .. })
    ));
}

#[test]
fn the_signing_input_cannot_be_confused_across_field_boundaries() {
    // Length-prefixed framing: moving a character between adjacent
    // string fields must change the signed bytes.
    let signer = GrantSigner::from_secret([7u8; 32]);
    let mut left = claims(77);
    left.task_id = "ab".to_string();
    left.session_id = Some("c".to_string());
    let mut right = claims(77);
    right.task_id = "a".to_string();
    right.session_id = Some("bc".to_string());
    assert_ne!(signer.issue(left).mac, signer.issue(right).mac);
}

#[test]
fn each_broker_process_signs_with_its_own_secret() {
    let first = GrantSigner::generate().expect("generate");
    let second = GrantSigner::generate().expect("generate");
    let grant = first.issue(claims(77));
    assert_eq!(first.verify(&grant, &expectation("hello"), 2_000), Ok(()));
    assert_eq!(
        second.verify(&grant, &expectation("hello"), 2_000),
        Err(GrantError::Signature)
    );
}
