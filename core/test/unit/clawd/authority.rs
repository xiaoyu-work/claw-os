use super::*;

use crate::caps::{Cap, CapSet, Scope, Verb};
use crate::clawd::routes::{Access, Command, Kind, ROUTES};

#[cfg(unix)]
fn current_uid() -> u32 {
    unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

fn client() -> crate::clawd::client_identity::ClientIdentity {
    let pid = std::process::id();
    crate::clawd::client_identity::ClientIdentity {
        pid: Some(pid),
        uid: Some(current_uid()),
        gid: Some(0),
        execution_uid: None,
        start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
        attended_local: false,
        extension_host: None,
    }
}

// ---------------------------------------------------------------------------
// The registry is the only authorization surface
// ---------------------------------------------------------------------------

#[test]
fn every_route_declares_an_authorization_descriptor() {
    // The `routes!` macro takes `authority` positionally, so a row that
    // omits it does not compile. This asserts the declarations are
    // coherent rather than merely present.
    for route in ROUTES {
        let descriptor = &route.authority;
        match descriptor.subject {
            SubjectSource::Peer => {
                let requirement = (descriptor.requirement)(&serde_json::json!({}))
                    .expect("a peer-scoped resolver reads nothing from the body");
                assert_eq!(
                    requirement,
                    Requirement::None,
                    "{} is peer-scoped, so it has no grant to spend a capability against",
                    route.name
                );
            }
            SubjectSource::Session | SubjectSource::PeerSession => {
                let requirement = (descriptor.requirement)(&serde_json::json!({}))
                    .expect("a subject resolver is total over its validated body");
                assert!(
                    matches!(
                        requirement,
                        Requirement::RouteDerived | Requirement::Exact(_)
                    ),
                    "{} names a session but requires no capability",
                    route.name
                );
            }
            SubjectSource::Handle => {
                // A handle-addressed route *is* the authority check:
                // the grant it resolves was minted for exactly this
                // lifecycle operation, so there is no separate
                // capability to spend.
                let requirement = (descriptor.requirement)(&serde_json::json!({}))
                    .expect("a subject resolver is total over its validated body");
                assert_eq!(
                    requirement,
                    Requirement::None,
                    "{} resolves a grant minted for this operation",
                    route.name
                );
            }
        }
    }
}

#[test]
fn route_audiences_match_their_families() {
    for route in ROUTES {
        let expected = match route.name {
            name if name.starts_with("daemon.") || name.starts_with("journal.") => Audience::Daemon,
            name if name.starts_with("task.") => Audience::Task,
            name if name.starts_with("memory.")
                || name.starts_with("context.")
                || name == "agent.usage"
                || name == "system.operations" =>
            {
                Audience::Context
            }
            name if name.starts_with("notification.") => Audience::Notification,
            name if name.starts_with("permission.") => Audience::Permission,
            name if name.starts_with("transaction.") => Audience::Transaction,
            name if name == "app_service.call"
                || name == "app_service.cli_call"
                || name.starts_with("app_session.")
                || name.starts_with("mcp_session.") =>
            {
                // The relay is the one exception: it is addressed by a
                // launcher-held grant that authorizes *presenting* an
                // App session, not launching one, so it has its own
                // audience and reaches nothing directly.
                if name == "app_session.relay" {
                    Audience::AppRelay
                } else {
                    Audience::AppLaunch
                }
            }
            name if name.starts_with("scheduler.") => Audience::Scheduler,
            name if name.starts_with("credential.") => Audience::Credential,
            name if name.starts_with("system.") => Audience::SystemService,
            other => panic!("route {other} belongs to no declared audience family"),
        };
        assert_eq!(
            route.authority.audience, expected,
            "{} declares the wrong audience",
            route.name
        );
    }
}

#[test]
fn privileged_provider_routes_resolve_a_session_grant() {
    // Every privileged system provider is reached through a grant tied
    // to a session, never through the peer's ambient identity. The two
    // rollback routes use the peer's own registered session because the
    // rollback client holds no standing grant; everything else runs
    // under the App session grant issued at bind.
    for route in ROUTES {
        if !route.name.starts_with("system.") || route.name == "system.operations" {
            continue;
        }
        let expected = if route.name.ends_with(".restore") {
            SubjectSource::PeerSession
        } else {
            SubjectSource::Session
        };
        assert_eq!(
            route.authority.subject, expected,
            "{} must resolve a session grant",
            route.name
        );
        assert!(
            std::ptr::fn_addr_eq(
                route.authority.requirement,
                route_derived as RequirementResolver
            ),
            "{} must derive and spend its own exact capability",
            route.name
        );
    }
}

#[test]
fn app_session_lifecycle_routes_are_handle_addressed() {
    for name in [
        "app_session.bind",
        "app_session.set_transient",
        "app_session.deregister",
    ] {
        let route = Command::parse(name).expect("route exists").route();
        assert_eq!(route.authority.subject, SubjectSource::Handle);
        assert_eq!(route.authority.audience, Audience::AppLaunch);
    }
}

#[test]
fn the_root_only_access_class_is_unchanged() {
    // `permission.revoke` is the one addition: retiring somebody's
    // standing authority is an administrative act, and `owner_uid`
    // names whose, so a non-root peer must not reach it in either
    // direction. `journal.mutation.resolve` is the second: it states
    // what happened to a privileged mutation the machine could not
    // resolve, and that statement is what lifts the replay refusal.
    let root: Vec<&str> = ROUTES
        .iter()
        .filter(|route| route.access == Access::Root)
        .map(|route| route.name)
        .collect();
    assert_eq!(
        root,
        vec![
            "context.update",
            "journal.mutation.resolve",
            "permission.revoke"
        ]
    );
}

#[test]
fn every_mutation_declares_a_bounded_budget() {
    for route in ROUTES {
        if route.kind == Kind::Mutation {
            assert!(route.budget.max_in_flight > 0, "{}", route.name);
        }
    }
}

// ---------------------------------------------------------------------------
// Middleware behaviour
// ---------------------------------------------------------------------------

fn descriptor(subject: SubjectSource, audience: Audience) -> RouteAuthority {
    RouteAuthority {
        audience,
        subject,
        requirement: route_derived,
        approval: Approval::Eligible,
        transient: TransientCaps::Excluded,
    }
}

#[tokio::test]
async fn a_peer_scoped_route_resolves_no_grant() {
    static DESCRIPTOR: RouteAuthority = RouteAuthority {
        audience: Audience::Daemon,
        subject: SubjectSource::Peer,
        requirement: no_requirement,
        approval: Approval::Ineligible,
        transient: TransientCaps::Excluded,
    };
    let decision = authorize(
        "daemon.health",
        &DESCRIPTOR,
        &serde_json::json!({}),
        &client(),
    )
    .await
    .expect("a peer-scoped route is admitted on its access class alone");
    assert!(decision.is_none());
    assert!(obligation_met(decision.as_ref()));
}

#[tokio::test]
async fn a_session_route_without_a_session_field_is_refused() {
    static DESCRIPTOR: RouteAuthority = RouteAuthority {
        audience: Audience::SystemService,
        subject: SubjectSource::Session,
        requirement: route_derived,
        approval: Approval::Eligible,
        transient: TransientCaps::Excluded,
    };
    let fault = authorize(
        "system.power.control",
        &DESCRIPTOR,
        &serde_json::json!({"action": "status"}),
        &client(),
    )
    .await
    .expect_err("a route addressed by session must name one");
    assert_eq!(fault, Fault::NotAuthorized);
}

#[tokio::test]
async fn an_unknown_session_is_refused_exactly_like_somebody_elses() {
    static DESCRIPTOR: RouteAuthority = RouteAuthority {
        audience: Audience::SystemService,
        subject: SubjectSource::Session,
        requirement: route_derived,
        approval: Approval::Eligible,
        transient: TransientCaps::Excluded,
    };
    let missing = authorize(
        "system.power.control",
        &DESCRIPTOR,
        &serde_json::json!({"session": "app-does-not-exist"}),
        &client(),
    )
    .await
    .expect_err("nothing answers to an unknown session");
    assert_eq!(missing, Fault::NotAuthorized);
}

#[tokio::test]
async fn a_request_field_cannot_name_its_own_owner() {
    static DESCRIPTOR: RouteAuthority = RouteAuthority {
        audience: Audience::SystemService,
        subject: SubjectSource::Session,
        requirement: route_derived,
        approval: Approval::Eligible,
        transient: TransientCaps::Excluded,
    };
    // `owner_uid`, `uid`, `caps` and `role` are not fields any resolver
    // reads: the decision comes from the grant and the kernel-verified
    // peer. Sending them changes nothing.
    let injected = serde_json::json!({
        "session": "app-does-not-exist",
        "owner_uid": 0,
        "uid": 0,
        "role": "root",
        "caps": [{"verb": "sys.power", "scope": "*"}],
    });
    assert_eq!(
        authorize("system.power.control", &DESCRIPTOR, &injected, &client())
            .await
            .expect_err("injection does not create authority"),
        Fault::NotAuthorized
    );
}

#[test]
fn a_descriptor_that_derives_its_own_capability_owes_a_spend() {
    let store = authority();
    store.clear_for_test();
    let principal =
        Principal::of_process(current_uid(), std::process::id()).expect("name this process");
    let (_handle, view) = store
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal,
            binding: Binding::ProcessTree,
            subject: Subject::session("app-obligation").with_app(Some("power-manager".into())),
            audience: AudienceSet::one(Audience::SystemService),
            caps: CapSet::from_caps([Cap::new(Verb::SYS_OBSERVE, Scope::name("power"))]),
            lifetime: std::time::Duration::from_secs(60),
            uses: Uses::Unbounded,
            index_session: true,
        })
        .expect("issue a session grant");

    let presentation = Presentation {
        uid: current_uid(),
        pid: std::process::id(),
        start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
        audience: Audience::SystemService,
        route: "system.power.control",
        session_id: Some("app-obligation".to_string()),
    };
    let decision = Decision::for_test(
        view,
        "system.power.control",
        Audience::SystemService,
        presentation,
        None,
        &Requirement::RouteDerived,
    );

    assert!(
        !obligation_met(Some(&decision)),
        "a route that never checked has authorized nothing"
    );
    let _proof = decision
        .require(Cap::new(Verb::SYS_OBSERVE, Scope::name("power")))
        .expect("the held capability is allowed");
    assert!(obligation_met(Some(&decision)));

    decision
        .require(Cap::new(Verb::SYS_POWER, Scope::Wild))
        .expect_err("a capability outside the grant is refused");

    store.clear_for_test();
}

#[test]
fn a_decision_pins_the_app_identity_the_grant_carries() {
    let store = authority();
    store.clear_for_test();
    let principal =
        Principal::of_process(current_uid(), std::process::id()).expect("name this process");
    let (_handle, view) = store
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal,
            binding: Binding::ProcessTree,
            subject: Subject::session("app-identity").with_app(Some("power-manager".into())),
            audience: AudienceSet::one(Audience::SystemService),
            caps: CapSet::from_caps([Cap::new(Verb::SYS_POWER, Scope::Wild)]),
            lifetime: std::time::Duration::from_secs(60),
            uses: Uses::Unbounded,
            index_session: true,
        })
        .expect("issue a session grant");
    let presentation = Presentation {
        uid: current_uid(),
        pid: std::process::id(),
        start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
        audience: Audience::SystemService,
        route: "system.power.control",
        session_id: Some("app-identity".to_string()),
    };
    let decision = Decision::for_test(
        view,
        "system.power.control",
        Audience::SystemService,
        presentation,
        None,
        &Requirement::RouteDerived,
    );

    decision
        .require_app("power-manager")
        .expect("the grant names this App");
    decision
        .require_app("user-manager")
        .expect_err("another App's provider is refused");
    store.clear_for_test();
}

#[tokio::test]
async fn a_peer_session_route_refuses_a_session_the_peer_does_not_own() {
    static DESCRIPTOR: RouteAuthority = RouteAuthority {
        audience: Audience::SystemService,
        subject: SubjectSource::PeerSession,
        requirement: route_derived,
        approval: Approval::Eligible,
        transient: TransientCaps::Excluded,
    };
    // No such row: the seam authenticates the registry row before it
    // mints anything, so an invented id gets nothing.
    assert_eq!(
        authorize(
            "system.package.restore",
            &DESCRIPTOR,
            &serde_json::json!({"session": "not-a-registered-session"}),
            &client(),
        )
        .await
        .expect_err("an unregistered session is refused"),
        Fault::NotAuthorized
    );
}

#[tokio::test]
async fn a_peer_session_route_still_needs_a_session_field() {
    static DESCRIPTOR: RouteAuthority = RouteAuthority {
        audience: Audience::Credential,
        subject: SubjectSource::PeerSession,
        requirement: route_derived,
        approval: Approval::Eligible,
        transient: TransientCaps::Excluded,
    };
    assert_eq!(
        authorize(
            "credential.oauth-refresh",
            &DESCRIPTOR,
            &serde_json::json!({"namespace": "default", "credential": "TOKEN"}),
            &client(),
        )
        .await
        .expect_err("a session-addressed route must name one"),
        Fault::NotAuthorized
    );
}

// ---------------------------------------------------------------------------
// Authorization backstop
// ---------------------------------------------------------------------------

/// Build a live decision over a grant carrying `caps`, with the given
/// obligation.
fn decision_over(session: &str, caps: CapSet, requirement: &Requirement) -> Decision {
    let store = authority();
    let principal =
        Principal::of_process(current_uid(), std::process::id()).expect("name this process");
    let (_handle, view) = store
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal,
            binding: Binding::ProcessTree,
            subject: Subject::session(session).with_app(Some("power-manager".into())),
            audience: AudienceSet::one(Audience::SystemService),
            caps,
            lifetime: std::time::Duration::from_secs(60),
            uses: Uses::Unbounded,
            index_session: true,
        })
        .expect("issue a session grant");
    Decision::for_test(
        view,
        "system.power.control",
        Audience::SystemService,
        Presentation {
            uid: current_uid(),
            pid: std::process::id(),
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
            audience: Audience::SystemService,
            route: "system.power.control",
            session_id: Some(session.to_string()),
        },
        None,
        requirement,
    )
}

#[test]
fn an_empty_capability_check_is_refused_and_authorizes_nothing() {
    let store = authority();
    store.clear_for_test();
    let decision = decision_over(
        "app-empty",
        CapSet::from_caps([Cap::new(Verb::SYS_OBSERVE, Scope::name("power"))]),
        &Requirement::RouteDerived,
    );

    let error = decision
        .require_all(&[])
        .expect_err("authorized for nothing is not an authorization");
    assert!(error.contains("at least one capability"), "{error}");
    assert!(
        !obligation_met(Some(&decision)),
        "an empty check must not satisfy the route's obligation"
    );
    store.clear_for_test();
}

#[test]
fn an_ignored_denial_does_not_satisfy_the_obligation() {
    let store = authority();
    store.clear_for_test();
    let decision = decision_over(
        "app-ignored",
        CapSet::from_caps([Cap::new(Verb::SYS_OBSERVE, Scope::name("power"))]),
        &Requirement::RouteDerived,
    );

    // Exactly what a provider that dropped the `Result` would do.
    let refused = decision.require(Cap::new(Verb::SYS_POWER, Scope::Wild));
    assert!(refused.is_err());
    drop(refused);

    assert!(
        !obligation_met(Some(&decision)),
        "a refusal the provider ignored leaves the route owing a check, \
         so the broker withholds the answer"
    );

    // A successful spend, and only that, discharges it.
    let proof = decision
        .require(Cap::new(Verb::SYS_OBSERVE, Scope::name("power")))
        .expect("a held capability is allowed");
    assert_eq!(proof.spent().len(), 1);
    assert!(obligation_met(Some(&decision)));
    store.clear_for_test();
}

#[test]
fn a_proof_names_the_grant_and_the_capabilities_it_spent() {
    let store = authority();
    store.clear_for_test();
    let observe = Cap::new(Verb::SYS_OBSERVE, Scope::name("power"));
    let decision = decision_over(
        "app-proof",
        CapSet::from_caps([observe.clone()]),
        &Requirement::RouteDerived,
    );

    let proof = decision.require(observe.clone()).expect("allowed");
    assert_eq!(proof.spent(), &[observe]);
    assert_eq!(proof.grant_ref(), decision.grant_ref());
    store.clear_for_test();
}

// ---------------------------------------------------------------------------
// Transient capability policy
// ---------------------------------------------------------------------------

#[test]
fn the_credential_broker_does_not_borrow_a_tool_calls_capabilities() {
    // `credentials::authorize_session` read `session.caps` and
    // deliberately not `transient_caps`, so an MCP tool call granted a
    // secret for one invocation could not be turned into a token
    // refresh for a different one. Routing both through one authority
    // must not change that.
    let route = Command::parse("credential.oauth-refresh")
        .expect("route exists")
        .route();
    assert_eq!(route.authority.subject, SubjectSource::PeerSession);
    assert_eq!(
        route.authority.transient,
        TransientCaps::Excluded,
        "a credential refresh must not see a tool call's capabilities"
    );
}

#[test]
fn the_rollback_routes_keep_the_transient_set_they_always_had() {
    // `packages` and `systemd` both merged `transient_caps` before the
    // authority existed; narrowing them here would break a legitimate
    // rollback rather than close a hole.
    for name in ["system.package.restore", "system.service.restore"] {
        let route = Command::parse(name).expect("route exists").route();
        assert_eq!(route.authority.subject, SubjectSource::PeerSession);
        assert_eq!(
            route.authority.transient,
            TransientCaps::Included,
            "{name} must keep the set its provider checked before"
        );
    }
}

#[test]
fn every_peer_session_route_declares_its_transient_policy_deliberately() {
    // The middleware builds the capability set itself for this subject,
    // so the flag is the whole decision about whether a one-call grant
    // counts. Anything not on the rollback list must exclude it.
    for route in ROUTES {
        if route.authority.subject != SubjectSource::PeerSession {
            continue;
        }
        let expected = if route.name.ends_with(".restore") {
            TransientCaps::Included
        } else {
            TransientCaps::Excluded
        };
        assert_eq!(
            route.authority.transient, expected,
            "{} declares the wrong transient policy",
            route.name
        );
    }
}

#[test]
fn a_request_scoped_grant_carries_exactly_one_use() {
    // The handle is dropped without being indexed, so the grant is
    // reachable only through the decision this request holds, and that
    // decision authorizes one capability set.
    assert_eq!(PEER_SESSION_GRANT_USES, 1);
}

// ---------------------------------------------------------------------------
// Revocation surface
// ---------------------------------------------------------------------------

#[test]
fn the_revocation_route_is_root_only_and_typed() {
    let route = Command::parse("permission.revoke")
        .expect("the revocation route exists")
        .route();
    assert_eq!(route.access, Access::Root);
    assert_eq!(route.kind, Kind::Mutation);
    assert_eq!(route.authority.audience, Audience::Permission);
    assert!(
        !route.audit_fields.is_empty(),
        "a revocation must reach the audit trail with its scope classified"
    );
}

#[test]
fn a_request_scoped_grant_cannot_be_spent_twice() {
    // A `PeerSession` route mints its grant, drops the handle without
    // indexing it, and spends it once. The budget is what makes the
    // second spend impossible rather than merely unlikely: nothing else
    // can reach the grant, but the route itself must not be able to
    // reuse it either — an `apt` restore that looped would otherwise
    // authorize every iteration from one authentication.
    let store = authority();
    store.clear_for_test();
    let observe = Cap::new(Verb::SYS_OBSERVE, Scope::name("power"));
    let principal =
        Principal::of_process(current_uid(), std::process::id()).expect("name this process");
    let (_handle, view) = store
        .issue(Issuance {
            issuer: Issuer::TrustedSession,
            principal,
            binding: Binding::ProcessTree,
            subject: Subject::session("app-request-scoped"),
            audience: AudienceSet::one(Audience::SystemService),
            caps: CapSet::from_caps([observe.clone()]),
            lifetime: std::time::Duration::from_secs(120),
            uses: Uses::Budget(PEER_SESSION_GRANT_USES),
            index_session: false,
        })
        .expect("mint a request-scoped grant");
    assert_eq!(view.uses_remaining, Some(1));

    let decision = Decision::for_test(
        view,
        "system.package.restore",
        Audience::SystemService,
        Presentation {
            uid: current_uid(),
            pid: std::process::id(),
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
            audience: Audience::SystemService,
            route: "system.package.restore",
            session_id: Some("app-request-scoped".to_string()),
        },
        None,
        &Requirement::RouteDerived,
    );

    let _proof = decision.require(observe.clone()).expect("the first spend");
    let second = decision
        .require(observe)
        .expect_err("a request-scoped grant authorizes exactly one request");
    assert!(
        second.contains("no live capability grant"),
        "an exhausted grant is retired, not merely refused: {second}"
    );
    store.clear_for_test();
}

#[test]
fn app_session_grants_are_unbounded_only_where_they_stay_bound() {
    // The launch and session grants are deliberately `Unbounded` in
    // uses: an App makes many provider calls over its life, and
    // counting them would be arbitrary. What bounds them instead is
    // stated here so the exception cannot quietly widen: both expire,
    // both are pinned to a live process, and both are revoked when the
    // session ends.
    let store = authority();
    store.clear_for_test();
    let principal =
        Principal::of_process(current_uid(), std::process::id()).expect("name this process");
    let (handle, view) = store
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal,
            binding: Binding::Process,
            subject: Subject::session("app-unbounded").with_app(Some("fs".into())),
            audience: AudienceSet::one(Audience::AppLaunch),
            caps: CapSet::from_caps([Cap::new(Verb::AGENT_INVOKE, Scope::name("fs"))]),
            lifetime: std::time::Duration::from_secs(3600),
            uses: Uses::Unbounded,
            index_session: true,
        })
        .expect("launch grant");
    let handle = handle.into_wire();

    assert_eq!(view.uses_remaining, None, "unbounded in uses");
    assert!(view.expires_in.as_secs() > 0, "but bounded in time");
    assert_eq!(
        view.bound_pid,
        std::process::id(),
        "and pinned to a process"
    );

    // Revocation is the third bound, and it is immediate.
    assert!(store.revoke_session("app-unbounded") >= 1);
    let presentation = Presentation {
        uid: current_uid(),
        pid: std::process::id(),
        start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
        audience: Audience::AppLaunch,
        route: "app_session.deregister",
        session_id: None,
    };
    assert_eq!(
        store.resolve(&handle, &presentation).unwrap_err(),
        AuthorityError::UnknownGrant
    );
    store.clear_for_test();
}

// ---------------------------------------------------------------------------
// Session context is not authority
// ---------------------------------------------------------------------------

#[test]
fn conversation_context_cannot_select_a_grants_subject_or_owner() {
    // A session's conversation state — its frozen system prompt, the
    // due reminders and App data injected into one request, its
    // compressed history — lives in the owner's memory database and is
    // addressed by the same session id the authority indexes. That
    // shared identifier is the whole reason to state the boundary: the
    // grant's subject, owner, parent and audience come from the
    // daemon's own issuance, and no field of any request reaches them.
    let store = authority();
    store.clear_for_test();
    let principal =
        Principal::of_process(current_uid(), std::process::id()).expect("name this process");
    let (_handle, view) = store
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal,
            binding: Binding::ProcessTree,
            subject: Subject::session("app-context").with_app(Some("fs".into())),
            audience: AudienceSet::one(Audience::SystemService),
            caps: CapSet::from_caps([Cap::new(Verb::SYS_OBSERVE, Scope::name("power"))]),
            lifetime: std::time::Duration::from_secs(60),
            uses: Uses::Unbounded,
            index_session: true,
        })
        .expect("issue a session grant");

    // Every identity the decision exposes came from the issuance.
    assert_eq!(view.subject.session_id.as_deref(), Some("app-context"));
    assert_eq!(view.subject.app_id.as_deref(), Some("fs"));
    assert_eq!(view.owner_uid, current_uid());
    assert_eq!(view.parent, None);
    assert!(view.audience.contains(Audience::SystemService));
    assert!(!view.audience.contains(Audience::AppLaunch));

    // A request carrying context metadata that names a different
    // subject, owner, parent or audience changes none of it: the
    // resolver reads only `session`, and the grant answers for itself.
    let injected = serde_json::json!({
        "session": "app-context",
        "app_id": "user-manager",
        "owner_uid": 0,
        "parent": "app-somebody-else",
        "audience": "app-launch",
        "prompt_hash": "deadbeef",
        "context": {"session_id": "app-somebody-else", "caps": ["sys.power"]},
    });
    let requirement =
        (session_descriptor().requirement)(&injected).expect("the resolver reads a typed body");
    assert_eq!(
        requirement,
        Requirement::RouteDerived,
        "no request field contributes a capability"
    );

    let presentation = Presentation {
        uid: current_uid(),
        pid: std::process::id(),
        start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
        audience: Audience::SystemService,
        route: "system.power.control",
        session_id: Some("app-context".to_string()),
    };
    let resolved = store
        .resolve_session("app-context", &presentation)
        .expect("the grant resolves for its own subject");
    assert_eq!(resolved.subject, view.subject, "subject is unchanged");
    assert_eq!(resolved.owner_uid, view.owner_uid, "owner is unchanged");
    assert_eq!(resolved.parent, view.parent, "lineage is unchanged");
    assert_eq!(resolved.audience, view.audience, "audience is unchanged");

    store.clear_for_test();
}

fn session_descriptor() -> &'static RouteAuthority {
    &Command::parse("system.power.control")
        .expect("route exists")
        .route()
        .authority
}

#[test]
fn finishing_a_session_leaves_no_grant_behind_for_its_context() {
    // Conversation state can be replaced or compressed without the
    // session ending; authority cannot. When the session does end, the
    // grant goes with it, so a later request naming the same id — which
    // is exactly what a resumed conversation does — finds nothing.
    let store = authority();
    store.clear_for_test();
    let principal =
        Principal::of_process(current_uid(), std::process::id()).expect("name this process");
    let (handle, _) = store
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal,
            binding: Binding::ProcessTree,
            subject: Subject::session("app-finished").with_app(Some("fs".into())),
            audience: AudienceSet::one(Audience::SystemService),
            caps: CapSet::from_caps([Cap::new(Verb::SYS_OBSERVE, Scope::name("power"))]),
            lifetime: std::time::Duration::from_secs(60),
            uses: Uses::Unbounded,
            index_session: true,
        })
        .expect("issue a session grant");
    let handle = handle.into_wire();

    assert_eq!(store.revoke_session("app-finished"), 1);

    let presentation = Presentation {
        uid: current_uid(),
        pid: std::process::id(),
        start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
        audience: Audience::SystemService,
        route: "system.power.control",
        session_id: Some("app-finished".to_string()),
    };
    assert_eq!(
        store
            .resolve_session("app-finished", &presentation)
            .unwrap_err(),
        AuthorityError::UnknownGrant,
        "a resumed conversation does not resume authority"
    );
    assert_eq!(
        store.resolve(&handle, &presentation).unwrap_err(),
        AuthorityError::UnknownGrant
    );
    // The index is free again, so a genuine re-registration is possible
    // and starts from a fresh grant rather than inheriting the old one.
    store
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(current_uid(), std::process::id()).unwrap(),
            binding: Binding::ProcessTree,
            subject: Subject::session("app-finished").with_app(Some("fs".into())),
            audience: AudienceSet::one(Audience::SystemService),
            caps: CapSet::new(),
            lifetime: std::time::Duration::from_secs(60),
            uses: Uses::Unbounded,
            index_session: true,
        })
        .expect("the session index is released with the grant");
    store.clear_for_test();
}
