use super::*;
use crate::clawd::authority::{MAX_CHILDREN, MAX_LINEAGE_DEPTH};

use crate::caps::{Cap, Scope, Verb};

/// Every test binds to the running test process, which is the only
/// process a unit test can prove anything about.
fn self_principal() -> Principal {
    Principal::of_process(current_uid(), std::process::id())
        .expect("the test process can name itself")
}

#[cfg(unix)]
fn current_uid() -> u32 {
    unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

fn presentation(audience: Audience) -> Presentation {
    Presentation {
        uid: current_uid(),
        pid: std::process::id(),
        start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
        audience,
        route: "test.route",
        session_id: None,
    }
}

fn caps(items: &[Cap]) -> CapSet {
    CapSet::from_caps(items.iter().cloned())
}

fn read_cap() -> Cap {
    Cap::new(Verb::SYS_OBSERVE, Scope::name("power"))
}

fn write_cap() -> Cap {
    Cap::new(Verb::SYS_POWER, Scope::Wild)
}

fn issuance(session: &str, audience: &[Audience]) -> Issuance {
    Issuance {
        issuer: Issuer::AppSessionAuthority,
        principal: self_principal(),
        binding: Binding::ProcessTree,
        subject: Subject::session(session).with_app(Some("power-manager".to_string())),
        audience: AudienceSet::of(audience),
        caps: caps(&[read_cap(), write_cap()]),
        lifetime: Duration::from_secs(60),
        uses: Uses::Unbounded,
        index_session: true,
    }
}

fn store() -> Authority {
    Authority::new()
}

#[test]
fn a_session_id_alone_resolves_nothing() {
    let store = store();
    let (_handle, _view) = store
        .issue(issuance("app-1", &[Audience::SystemService]))
        .unwrap();

    // The right process resolves it.
    store
        .resolve_session("app-1", &presentation(Audience::SystemService))
        .expect("the bound process resolves its own session");

    // A same-uid sibling — a different pid the kernel named on its own
    // message — does not, even knowing the session id.
    let mut sibling = presentation(Audience::SystemService);
    sibling.pid = 1;
    assert_eq!(
        store.resolve_session("app-1", &sibling).unwrap_err(),
        AuthorityError::PrincipalMismatch
    );
}

#[test]
fn a_guessed_handle_is_indistinguishable_from_a_miss() {
    let store = store();
    let (handle, _) = store
        .issue(issuance("app-1", &[Audience::SystemService]))
        .unwrap();
    let real = handle.into_wire();

    assert_eq!(
        store
            .resolve(&"f".repeat(64), &presentation(Audience::SystemService))
            .unwrap_err(),
        AuthorityError::UnknownGrant
    );
    // Truncating or extending a real handle is also just a miss.
    assert_eq!(
        store
            .resolve(&real[..32], &presentation(Audience::SystemService))
            .unwrap_err(),
        AuthorityError::UnknownGrant
    );
}

#[test]
fn a_handle_stolen_by_another_uid_is_inert() {
    let store = store();
    let (handle, _) = store
        .issue(issuance("app-1", &[Audience::SystemService]))
        .unwrap();
    let stolen = handle.into_wire();

    let mut thief = presentation(Audience::SystemService);
    thief.uid = current_uid().wrapping_add(1);
    assert_eq!(
        store.resolve(&stolen, &thief).unwrap_err(),
        AuthorityError::PrincipalMismatch
    );
}

#[test]
fn a_process_bound_handle_refuses_a_recycled_pid() {
    let store = store();
    let mut request = issuance("app-1", &[Audience::SystemService]);
    request.binding = Binding::Process;
    let (handle, _) = store.issue(request).unwrap();
    let handle = handle.into_wire();

    // Same pid, different start time: the pid was recycled by another
    // process, or the original re-execed into something else.
    let mut recycled = presentation(Audience::SystemService);
    recycled.start_time_ticks = Some(recycled.start_time_ticks.unwrap_or_default() + 1);
    assert_eq!(
        store.resolve(&handle, &recycled).unwrap_err(),
        AuthorityError::PrincipalMismatch
    );
}

#[test]
fn a_grant_is_refused_on_another_audience() {
    let store = store();
    let (handle, _) = store
        .issue(issuance("app-1", &[Audience::SystemService]))
        .unwrap();
    let handle = handle.into_wire();

    assert!(matches!(
        store
            .resolve(&handle, &presentation(Audience::Credential))
            .unwrap_err(),
        AuthorityError::Audience { .. }
    ));
}

#[test]
fn a_grant_is_refused_for_another_session() {
    let store = store();
    let (handle, _) = store
        .issue(issuance("app-1", &[Audience::SystemService]))
        .unwrap();
    let handle = handle.into_wire();

    let mut other = presentation(Audience::SystemService);
    other.session_id = Some("app-2".to_string());
    assert_eq!(
        store.resolve(&handle, &other).unwrap_err(),
        AuthorityError::Subject
    );
}

#[test]
fn an_expired_grant_is_refused() {
    let store = store();
    let mut request = issuance("app-1", &[Audience::SystemService]);
    request.lifetime = Duration::from_millis(1);
    let (handle, _) = store.issue(request).unwrap();
    let handle = handle.into_wire();

    std::thread::sleep(Duration::from_millis(5));
    assert_eq!(
        store
            .resolve(&handle, &presentation(Audience::SystemService))
            .unwrap_err(),
        AuthorityError::Expired
    );
}

#[test]
fn a_revoked_grant_is_refused() {
    let store = store();
    let (handle, view) = store
        .issue(issuance("app-1", &[Audience::SystemService]))
        .unwrap();
    let handle = handle.into_wire();

    assert_eq!(store.revoke(view.id), 1);
    assert_eq!(
        store
            .resolve(&handle, &presentation(Audience::SystemService))
            .unwrap_err(),
        AuthorityError::UnknownGrant
    );
}

#[test]
fn a_one_shot_grant_cannot_be_double_spent() {
    let store = store();
    let mut request = issuance("app-1", &[Audience::SystemService]);
    request.uses = Uses::Budget(1);
    let (_handle, view) = store.issue(request).unwrap();

    let presentation = presentation(Audience::SystemService);
    let spent = store
        .consume(view.id, &[read_cap()], &presentation)
        .expect("the first spend succeeds");
    assert_eq!(spent.uses_remaining, Some(0));

    assert_eq!(
        store
            .consume(view.id, &[read_cap()], &presentation)
            .unwrap_err(),
        AuthorityError::UnknownGrant,
        "an exhausted grant is retired in the transaction that spent it"
    );
}

#[test]
fn concurrent_spends_of_a_one_shot_grant_produce_exactly_one_winner() {
    let store = std::sync::Arc::new(store());
    let mut request = issuance("app-1", &[Audience::SystemService]);
    request.uses = Uses::Budget(1);
    let (_handle, view) = store.issue(request).unwrap();

    let winners = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut threads = Vec::new();
    for _ in 0..8 {
        let store = std::sync::Arc::clone(&store);
        let winners = std::sync::Arc::clone(&winners);
        threads.push(std::thread::spawn(move || {
            if store
                .consume(
                    view.id,
                    &[read_cap()],
                    &presentation(Audience::SystemService),
                )
                .is_ok()
            {
                winners.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }));
    }
    for thread in threads {
        thread.join().expect("spend thread");
    }
    assert_eq!(winners.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn a_multi_capability_spend_is_all_or_none() {
    let store = store();
    let mut request = issuance("app-1", &[Audience::SystemService]);
    request.uses = Uses::Budget(2);
    let (_handle, view) = store.issue(request).unwrap();
    let presentation = presentation(Audience::SystemService);

    let missing = Cap::new(Verb::SECRET_READ, Scope::name("openai/prod"));
    assert!(matches!(
        store
            .consume(view.id, &[read_cap(), missing], &presentation)
            .unwrap_err(),
        AuthorityError::Capability { .. }
    ));

    // Nothing was spent, so the full budget is still there.
    let spent = store
        .consume(view.id, &[read_cap(), write_cap()], &presentation)
        .expect("a covered set spends once");
    assert_eq!(spent.uses_remaining, Some(1));
}

#[test]
fn a_capability_outside_the_grant_is_refused() {
    let store = store();
    let (_handle, view) = store
        .issue(issuance("app-1", &[Audience::SystemService]))
        .unwrap();
    assert!(matches!(
        store
            .consume(
                view.id,
                &[Cap::new(Verb::FS_READ, Scope::path("/etc/shadow"))],
                &presentation(Audience::SystemService),
            )
            .unwrap_err(),
        AuthorityError::Capability { .. }
    ));
}

// ---------------------------------------------------------------------------
// Attenuation
// ---------------------------------------------------------------------------

fn attenuation(caps_for_child: CapSet, audience: &[Audience]) -> Attenuation {
    attenuation_for(caps_for_child, audience, Duration::from_secs(30))
}

fn attenuation_for(
    caps_for_child: CapSet,
    audience: &[Audience],
    lifetime: Duration,
) -> Attenuation {
    Attenuation {
        issuer: Issuer::AppSessionAuthority,
        principal: self_principal(),
        binding: Binding::ProcessTree,
        subject: Subject::session("app-1"),
        audience: AudienceSet::of(audience),
        caps: caps_for_child,
        lifetime,
        uses: Uses::Budget(1),
        index_session: false,
    }
}

#[test]
fn a_child_may_only_narrow() {
    let store = store();
    let (handle, _) = store
        .issue(issuance(
            "app-1",
            &[Audience::SystemService, Audience::Credential],
        ))
        .unwrap();
    let handle = handle.into_wire();

    let (_child, view) = store
        .attenuate(
            &handle,
            attenuation(caps(&[read_cap()]), &[Audience::SystemService]),
        )
        .expect("a narrower child is admissible");
    assert_eq!(view.depth, 1);
    assert_eq!(view.uses_remaining, Some(1));
}

#[test]
fn a_child_cannot_widen_capabilities() {
    let store = store();
    let (handle, _) = store
        .issue(issuance("app-1", &[Audience::SystemService]))
        .unwrap();
    let handle = handle.into_wire();

    let error = store
        .attenuate(
            &handle,
            attenuation(
                caps(&[Cap::new(Verb::FS_READ, Scope::path("/**"))]),
                &[Audience::SystemService],
            ),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        AuthorityError::Attenuation(AttenuationError::CapabilityWiden { .. })
    ));
}

#[test]
fn a_child_cannot_introduce_wild_on_a_resource_verb() {
    let store = store();
    let mut request = issuance("app-1", &[Audience::SystemService]);
    // Parent holds an unbounded name scope, but not `Wild`.
    request.caps = caps(&[Cap::new(Verb::SECRET_READ, Scope::name("**"))]);
    let (handle, _) = store.issue(request).unwrap();
    let handle = handle.into_wire();

    let error = store
        .attenuate(
            &handle,
            attenuation(
                caps(&[Cap::new(Verb::SECRET_READ, Scope::Wild)]),
                &[Audience::SystemService],
            ),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        AuthorityError::Attenuation(AttenuationError::WildIntroduced { .. })
    ));
}

#[test]
fn a_child_may_keep_wild_where_it_is_the_canonical_scope() {
    let store = store();
    let mut request = issuance("app-1", &[Audience::SystemService]);
    request.caps = caps(&[Cap::unscoped(Verb::UI_NOTIFY)]);
    let (handle, _) = store.issue(request).unwrap();
    let handle = handle.into_wire();

    store
        .attenuate(
            &handle,
            attenuation(
                caps(&[Cap::unscoped(Verb::UI_NOTIFY)]),
                &[Audience::SystemService],
            ),
        )
        .expect("a resourceless verb has no narrower representable scope");
}

#[test]
fn a_child_cannot_broaden_its_audience() {
    let store = store();
    let (handle, _) = store
        .issue(issuance("app-1", &[Audience::SystemService]))
        .unwrap();
    let handle = handle.into_wire();

    let error = store
        .attenuate(
            &handle,
            attenuation(caps(&[read_cap()]), &[Audience::Credential]),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        AuthorityError::Attenuation(AttenuationError::AudienceWiden)
    ));
}

#[test]
fn a_child_cannot_outlive_its_parent() {
    let store = store();
    let mut request = issuance("app-1", &[Audience::SystemService]);
    request.lifetime = Duration::from_secs(5);
    let (handle, _) = store.issue(request).unwrap();
    let handle = handle.into_wire();

    let mut child = attenuation(caps(&[read_cap()]), &[Audience::SystemService]);
    child.lifetime = Duration::from_secs(3600);
    assert!(matches!(
        store.attenuate(&handle, child).unwrap_err(),
        AuthorityError::Attenuation(AttenuationError::LifetimeExtended)
    ));
}

#[test]
fn a_child_cannot_increase_the_use_budget() {
    let store = store();
    let mut request = issuance("app-1", &[Audience::SystemService]);
    request.uses = Uses::Budget(1);
    let (handle, _) = store.issue(request).unwrap();
    let handle = handle.into_wire();

    let mut child = attenuation(caps(&[read_cap()]), &[Audience::SystemService]);
    child.uses = Uses::Budget(9);
    assert!(matches!(
        store.attenuate(&handle, child).unwrap_err(),
        AuthorityError::Attenuation(AttenuationError::UseBudgetIncreased)
    ));

    let mut unbounded_child = attenuation(caps(&[read_cap()]), &[Audience::SystemService]);
    unbounded_child.uses = Uses::Unbounded;
    assert!(matches!(
        store.attenuate(&handle, unbounded_child).unwrap_err(),
        AuthorityError::Attenuation(AttenuationError::UseBudgetIncreased)
    ));
}

#[test]
fn a_child_cannot_change_owner() {
    let store = store();
    let (handle, _) = store
        .issue(issuance("app-1", &[Audience::SystemService]))
        .unwrap();
    let handle = handle.into_wire();

    let mut child = attenuation(caps(&[read_cap()]), &[Audience::SystemService]);
    child.principal.uid = current_uid().wrapping_add(1);
    assert!(matches!(
        store.attenuate(&handle, child).unwrap_err(),
        AuthorityError::Attenuation(AttenuationError::OwnerChanged)
    ));
}

#[test]
fn lineage_depth_is_bounded() {
    let store = store();
    let mut root_request = issuance("app-1", &[Audience::SystemService]);
    root_request.lifetime = Duration::from_secs(3600);
    let (root, _) = store.issue(root_request).unwrap();
    let mut current = root.into_wire();

    // Each level asks for strictly less time than its parent, which is
    // what monotonic attenuation requires.
    for depth in 1..=MAX_LINEAGE_DEPTH {
        let lifetime = Duration::from_secs(3600 - u64::from(depth) * 60);
        let mut child = attenuation_for(caps(&[read_cap()]), &[Audience::SystemService], lifetime);
        child.uses = Uses::Unbounded;
        let (handle, view) = store
            .attenuate(&current, child)
            .unwrap_or_else(|error| panic!("depth {depth} should be admissible: {error}"));
        assert_eq!(view.depth, depth);
        current = handle.into_wire();
    }

    let mut too_deep = attenuation_for(
        caps(&[read_cap()]),
        &[Audience::SystemService],
        Duration::from_secs(60),
    );
    too_deep.uses = Uses::Unbounded;
    assert!(matches!(
        store.attenuate(&current, too_deep).unwrap_err(),
        AuthorityError::Attenuation(AttenuationError::DepthExceeded)
    ));
}

#[test]
fn child_count_is_bounded() {
    let store = store();
    let (root, _) = store
        .issue(issuance("app-1", &[Audience::SystemService]))
        .unwrap();
    let root = root.into_wire();

    for _ in 0..MAX_CHILDREN {
        store
            .attenuate(
                &root,
                attenuation(caps(&[read_cap()]), &[Audience::SystemService]),
            )
            .expect("a child under the ceiling is admissible");
    }
    assert!(matches!(
        store
            .attenuate(
                &root,
                attenuation(caps(&[read_cap()]), &[Audience::SystemService]),
            )
            .unwrap_err(),
        AuthorityError::Attenuation(AttenuationError::TooManyChildren)
    ));
}

#[test]
fn revoking_a_parent_invalidates_every_descendant() {
    let store = store();
    let mut root_request = issuance("app-1", &[Audience::SystemService]);
    root_request.lifetime = Duration::from_secs(3600);
    let (root, root_view) = store.issue(root_request).unwrap();
    let root = root.into_wire();

    let mut child = attenuation_for(
        caps(&[read_cap()]),
        &[Audience::SystemService],
        Duration::from_secs(1800),
    );
    child.uses = Uses::Unbounded;
    let (child_handle, child_view) = store.attenuate(&root, child).unwrap();
    let child_handle = child_handle.into_wire();

    let mut grandchild = attenuation_for(
        caps(&[read_cap()]),
        &[Audience::SystemService],
        Duration::from_secs(600),
    );
    grandchild.uses = Uses::Unbounded;
    let (grandchild_handle, _) = store.attenuate(&child_handle, grandchild).unwrap();
    let grandchild_handle = grandchild_handle.into_wire();

    assert_eq!(store.revoke(root_view.id), 3);
    for handle in [&root, &child_handle, &grandchild_handle] {
        assert_eq!(
            store
                .resolve(handle, &presentation(Audience::SystemService))
                .unwrap_err(),
            AuthorityError::UnknownGrant
        );
    }
    assert_eq!(
        store
            .consume(
                child_view.id,
                &[read_cap()],
                &presentation(Audience::SystemService)
            )
            .unwrap_err(),
        AuthorityError::UnknownGrant
    );
    assert_eq!(store.len(), 0);
}

#[test]
fn exhausting_a_parent_leaves_its_already_clamped_child_alone() {
    let store = store();
    let mut request = issuance("app-1", &[Audience::SystemService]);
    request.uses = Uses::Budget(1);
    let (root, root_view) = store.issue(request).unwrap();
    let root = root.into_wire();

    let (child_handle, _) = store
        .attenuate(
            &root,
            attenuation(caps(&[read_cap()]), &[Audience::SystemService]),
        )
        .unwrap();
    let child_handle = child_handle.into_wire();

    store
        .consume(
            root_view.id,
            &[read_cap()],
            &presentation(Audience::SystemService),
        )
        .expect("the parent spends its last use");

    // The parent is gone; the child, already clamped to it, is not.
    assert_eq!(
        store
            .resolve(&root, &presentation(Audience::SystemService))
            .unwrap_err(),
        AuthorityError::UnknownGrant
    );
    store
        .resolve(&child_handle, &presentation(Audience::SystemService))
        .expect("the child keeps its own bounded authority");
}

#[test]
fn one_session_index_cannot_be_reclaimed_while_it_is_live() {
    let store = store();
    store
        .issue(issuance("app-1", &[Audience::SystemService]))
        .expect("first claim");
    assert_eq!(
        store
            .issue(issuance("app-1", &[Audience::SystemService]))
            .unwrap_err(),
        AuthorityError::Quota("session-index"),
        "a second registration cannot re-point a live session id"
    );
}

#[test]
fn revoking_a_session_retires_its_lineage() {
    let store = store();
    let (root, _) = store
        .issue(issuance("app-1", &[Audience::SystemService]))
        .unwrap();
    let root = root.into_wire();
    store
        .attenuate(
            &root,
            attenuation(caps(&[read_cap()]), &[Audience::SystemService]),
        )
        .unwrap();

    assert_eq!(store.revoke_session("app-1"), 2);
    assert_eq!(store.len(), 0);
}

#[test]
fn revoking_a_session_retires_unindexed_approval_grants() {
    let store = store();
    let mut request = issuance("agent-session", &[Audience::AgentWorker]);
    request.issuer = Issuer::Approval;
    request.binding = Binding::Process;
    request.subject = Subject::session("agent-session").with_task(Some("task-a".to_string()));
    request.uses = Uses::Budget(1);
    request.index_session = false;
    let (_handle, view) = store.issue_with_generation(request, 7).unwrap();

    assert_eq!(view.generation, 7);
    assert_eq!(store.revoke_session("agent-session"), 1);
    assert_eq!(
        store
            .consume(
                view.id,
                &[read_cap()],
                &Presentation {
                    session_id: Some("agent-session".to_string()),
                    ..presentation(Audience::AgentWorker)
                },
            )
            .unwrap_err(),
        AuthorityError::UnknownGrant
    );
}

#[test]
fn approval_revocation_does_not_retire_the_sessions_base_grant() {
    let store = store();
    let (_base_handle, _) = store
        .issue(issuance("agent-session", &[Audience::SystemService]))
        .unwrap();
    let mut approval = issuance("agent-session", &[Audience::AgentWorker]);
    approval.issuer = Issuer::Approval;
    approval.binding = Binding::Process;
    approval.subject =
        Subject::session("agent-session").with_task(Some("task-a".to_string()));
    approval.uses = Uses::Budget(1);
    approval.index_session = false;
    let (_approval_handle, approval_view) =
        store.issue_with_generation(approval, 3).unwrap();

    assert_eq!(store.revoke_approvals_for_session("agent-session"), 1);
    store
        .resolve_session(
            "agent-session",
            &Presentation {
                session_id: Some("agent-session".to_string()),
                ..presentation(Audience::SystemService)
            },
        )
        .expect("base session authority stays live");
    assert_eq!(
        store
            .consume(
                approval_view.id,
                &[read_cap()],
                &Presentation {
                    session_id: Some("agent-session".to_string()),
                    ..presentation(Audience::AgentWorker)
                },
            )
            .unwrap_err(),
        AuthorityError::UnknownGrant
    );
}

#[test]
fn revoking_an_owner_retires_everything_it_holds() {
    let store = store();
    store
        .issue(issuance("app-1", &[Audience::SystemService]))
        .unwrap();
    let mut second = issuance("app-2", &[Audience::SystemService]);
    second.subject = Subject::session("app-2");
    store.issue(second).unwrap();

    assert_eq!(store.revoke_owner(current_uid()), 2);
    assert_eq!(store.len(), 0);
}

#[test]
fn per_process_grant_count_is_bounded() {
    let store = store();
    for index in 0..MAX_GRANTS_PER_PROCESS {
        let mut request = issuance(&format!("app-{index}"), &[Audience::SystemService]);
        request.subject = Subject::session(format!("app-{index}"));
        store.issue(request).expect("under the ceiling");
    }
    let mut overflow = issuance("app-overflow", &[Audience::SystemService]);
    overflow.subject = Subject::session("app-overflow");
    assert!(matches!(
        store.issue(overflow).unwrap_err(),
        AuthorityError::Quota(_)
    ));
}

#[test]
fn a_grant_cannot_be_issued_without_a_process_identity() {
    let store = store();
    let mut request = issuance("app-1", &[Audience::SystemService]);
    request.principal.start_time_ticks = None;
    assert_eq!(
        store.issue(request).unwrap_err(),
        AuthorityError::UnverifiablePrincipal
    );
}

#[test]
fn a_grant_bound_to_a_dead_process_is_refused() {
    let store = store();
    let mut request = issuance("app-1", &[Audience::SystemService]);
    // A pid that cannot be alive with this start time.
    request.principal.pid = u32::MAX - 1;
    request.principal.start_time_ticks = Some(1);
    let (handle, _) = store.issue(request).unwrap();
    let handle = handle.into_wire();

    assert_eq!(
        store
            .resolve(&handle, &presentation(Audience::SystemService))
            .unwrap_err(),
        AuthorityError::PrincipalMismatch,
        "a grant whose process is gone authorises nothing"
    );
    // ... and the row does not linger.
    assert_eq!(store.sweep_now(), 1);
    assert_eq!(store.len(), 0);
}

#[test]
fn the_sweep_drops_expired_grants() {
    let store = store();
    let mut request = issuance("app-1", &[Audience::SystemService]);
    request.lifetime = Duration::from_millis(1);
    store.issue(request).unwrap();
    assert_eq!(store.len(), 1);

    std::thread::sleep(Duration::from_millis(5));
    assert_eq!(store.sweep_now(), 1);
    assert_eq!(store.len(), 0);
}

#[test]
fn a_fresh_daemon_holds_nothing() {
    // The store is in memory only. A restart is modelled here by a new
    // instance: a handle minted by the previous one resolves to
    // nothing, which is what "ephemeral grants fail closed across a
    // daemon restart" means.
    let before = store();
    let (handle, _) = before
        .issue(issuance("app-1", &[Audience::SystemService]))
        .unwrap();
    let handle = handle.into_wire();

    let after = store();
    assert_eq!(after.len(), 0);
    assert_eq!(
        after
            .resolve(&handle, &presentation(Audience::SystemService))
            .unwrap_err(),
        AuthorityError::UnknownGrant
    );
    assert_eq!(
        after
            .resolve_session("app-1", &presentation(Audience::SystemService))
            .unwrap_err(),
        AuthorityError::UnknownGrant
    );
}
