use super::*;
use support::CapabilityPolicyCase;

pub(super) fn initial_policy(
    context: &ProcessContext,
    case: CapabilityPolicyCase,
) -> crate::activities::CapabilityPolicyDraft {
    let path = json!({"kind":"path","value":format!("{}/**", context.root.display())});
    let rules = match case {
        CapabilityPolicyCase::Confirm => json!([
            {"verb":"agent.invoke","mode":"require_approval","scopes":[{"kind":"name","value":APP_ID}]},
            {"verb":"fs.read","mode":"require_approval","scopes":[path.clone()]},
            {"verb":"fs.write","mode":"require_approval","scopes":[path]},
        ]),
        CapabilityPolicyCase::Deny => json!([{"verb":"fs.write","mode":"deny","scopes":[]}]),
        _ => json!([{"verb":"fs.write","mode":"normal","scopes":[path]}]),
    };
    serde_json::from_value(json!({"rules":rules})).unwrap()
}

pub(super) fn change_policy(
    service: &dyn ActivityService,
    context: &ProcessContext,
    activity: &str,
    case: CapabilityPolicyCase,
) {
    match case {
        CapabilityPolicyCase::Disable => {
            service
                .set_capability_policy_enabled(context.uid, activity, 1, false)
                .unwrap();
        }
        CapabilityPolicyCase::Revision | CapabilityPolicyCase::Introduce => {
            let revision = (case == CapabilityPolicyCase::Revision).then_some(1);
            service
                .set_capability_policy(
                    context.uid,
                    activity,
                    revision,
                    initial_policy(context, CapabilityPolicyCase::Deny),
                )
                .unwrap();
        }
        _ => panic!("case does not change a running policy"),
    }
}

pub(super) async fn confirm_while<T>(
    context: &ProcessContext,
    parent: &str,
    inspect: impl std::future::Future<Output = T>,
) -> T {
    let mut expected = vec![Cap::new(Verb::AGENT_INVOKE, Scope::name(APP_ID))];
    let mut paths = vec![context.input()];
    if context.stateful {
        paths.push(context.second_input());
    }
    for path in paths {
        for verb in [Verb::FS_READ, Verb::FS_WRITE] {
            expected.push(Cap::new(verb, Scope::path(path.to_string_lossy())));
        }
    }
    let mut confirmed = Vec::new();
    tokio::pin!(inspect);
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    loop {
        tokio::select! {
            result = &mut inspect => {
                assert_eq!(confirmed.len(), expected.len(), "each invocation need is confirmed once");
                return result;
            }
            _ = tick.tick() => {
                for pending in crate::approvals::list_pending_for_owner(Some(context.uid)) {
                    assert_eq!(pending.session, parent);
                    let cap = Cap::new(Verb::parse(&pending.verb).unwrap(), pending.scope);
                    assert!(expected.contains(&cap), "unexpected confirmation: {cap:?}");
                    assert!(!confirmed.contains(&cap), "a preflight or effect spent confirmation twice: {cap:?}");
                    crate::approvals::approve_for_owner(
                        &pending.id, crate::approvals::GrantDuration::Once,
                        Some("isolated-test-operator".into()), None, Some(context.uid),
                    ).unwrap();
                    confirmed.push(cap);
                }
            }
        }
    }
}
