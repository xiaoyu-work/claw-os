use super::*;
use crate::clawd::authority::{
    Audience, AudienceSet, Binding, Decision, Issuance, Issuer, Presentation, Principal,
    Requirement, Subject, Uses,
};

fn files() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let base = std::env::var_os("TMPDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap());
    let directory = tempfile::tempdir_in(base).unwrap();
    let home = directory.path().join("owner");
    std::fs::create_dir(&home).unwrap();
    let selected = home.join("selected.ron");
    let denied = home.join("not-shared.ron");
    std::fs::write(&selected, "independently shared source").unwrap();
    std::fs::write(&denied, "must remain unread").unwrap();
    (directory, selected, denied)
}

fn resource_app(path: &str) -> App {
    app_from(&json!({
        "id":"owner-resource-window","version":"1","schema_version":2,
        "name":{"en":"Owner resource window"},
        "desktop":{"exec":"--gui","needs":[],"optional_needs":[
            {"verb":"fs.read","scope":{"kind":"owner-path","path":path},"why":{"en":"Read only independently shared resources"}}
        ]},
        "mcp":{"entry":"main.py","transport":"stdio","tools":[
            {"name":"unrelated.all","summary":{"en":"Must not supply GUI authority"},"args":[],
             "needs":[{"verb":"fs.read","scope":{"kind":"wild"},"why":{"en":"Unrelated MCP surface"}}]}
        ]}
    }).to_string())
}

fn reader(caps: CapSet) -> Decision {
    use crate::clawd::authority::authority;
    let uid = unsafe { libc::geteuid() };
    let session = format!("settings-resource-{}", uuid::Uuid::new_v4().simple());
    let (_, view) = authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, std::process::id()).unwrap(),
            binding: Binding::ProcessTree,
            subject: Subject::session(&session).with_app(Some("owner-resource-window".into())),
            audience: AudienceSet::one(Audience::SystemService),
            caps,
            lifetime: std::time::Duration::from_secs(60),
            uses: Uses::Unbounded,
            index_session: true,
        })
        .unwrap();
    Decision::for_test(
        view,
        "system.filesystem.read",
        Audience::SystemService,
        Presentation {
            uid,
            pid: std::process::id(),
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
            audience: Audience::SystemService,
            route: "system.filesystem.read",
            session_id: Some(session),
        },
        None,
        &Requirement::RouteDerived,
    )
}

fn assert_root_reads_only_selected(
    caps: CapSet,
    selected: &std::path::Path,
    denied: &std::path::Path,
) {
    assert!(!caps.covers(&Cap::new(Verb::SYS_IDENTITY, Scope::name("manage"))));
    let decision = reader(caps);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let read = |path: &std::path::Path| json!({"request":{"action":"read","path":path}});
    let result = runtime
        .block_on(crate::clawd::filesystem::access(
            read(selected),
            &decision,
            false,
        ))
        .unwrap();
    assert_eq!(result["content"], "independently shared source");
    assert!(runtime
        .block_on(crate::clawd::filesystem::access(
            read(denied),
            &decision,
            false
        ))
        .is_err());
}

#[test]
fn exact_optional_owner_path_requires_the_independent_parent_right_before_root_read() {
    let _sandbox = approval_sandbox();
    let (directory, selected, denied) = files();
    let _data =
        crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", directory.path().join("data"));
    let app = resource_app("selected.ron");
    let mut parent = delegation(CapSet::from_caps([Cap::new(
        Verb::FS_READ,
        Scope::path(selected.to_str().unwrap()),
    )]));
    parent.uid = unsafe { libc::geteuid() };
    parent.paths.home = selected.parent().unwrap().into();
    let (caps, optional) = optional_gui_caps(&app, None, "--gui", &parent).unwrap();
    assert!(optional.ungranted.is_empty());
    assert_root_reads_only_selected(caps, &selected, &denied);
    parent.ceiling = CapSet::new();
    let (caps, optional) = optional_gui_caps(&app, None, "--gui", &parent).unwrap();
    assert!(caps.is_empty());
    assert_eq!(optional.ungranted, [0]);
    assert!(crate::approvals::list_pending_for_owner(Some(parent.uid)).is_empty());
}

#[test]
fn owner_path_gui_ceiling_inherits_only_exact_parent_resource_and_marks_partial() {
    let _sandbox = approval_sandbox();
    let (directory, selected, denied) = files();
    let _data =
        crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", directory.path().join("data"));
    let app = resource_app("**");
    let selected_cap = Cap::new(Verb::FS_READ, Scope::path(selected.to_str().unwrap()));
    let mut parent = delegation(CapSet::from_caps([selected_cap.clone()]));
    parent.uid = unsafe { libc::geteuid() };
    parent.paths.home = selected.parent().unwrap().into();
    let (caps, optional) = optional_gui_caps(&app, None, "--gui", &parent).unwrap();
    assert_eq!(
        caps.iter().cloned().collect::<Vec<_>>(),
        [selected_cap],
        "the technical Home ceiling must not require or mint a raw Home read grant"
    );
    assert_eq!(
        optional.ungranted,
        [0],
        "the Home ceiling remains only partially granted"
    );
    assert_root_reads_only_selected(caps, &selected, &denied);
    assert!(crate::approvals::list_pending_for_owner(Some(parent.uid)).is_empty());
}
