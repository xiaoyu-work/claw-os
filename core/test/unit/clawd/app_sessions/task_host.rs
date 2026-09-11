use super::*;

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use crate::caps::{Cap, CapSet, Manifest, Role, Scope, Verb};
use crate::clawd::app_sessions::{
    authenticate_launcher, authorize_plan, installed_app, issue_launch_grant, operation_plan,
    register, require_launch_grant,
};
use crate::clawd::protocol::BrokerErrorKind;
use crate::provenance::{Ceiling, PackageKind, TrustTier};
use serde_json::json;

const MANIFEST: &str = r#"{
  "id": "fs", "version": "0.1.0", "name": "Files",
  "desktop": {"exec": "--gui"},
  "operations": {
    "read": {
      "label": "Read",
      "args": [
        {"name": "path", "kind": "path", "required": true},
        {"name": "format", "kind": "text", "binding": "flag", "default": "utf8"}
      ],
      "needs": [
        {"verb": "fs.read", "scope": {"kind": "from-arg", "arg": "path"}, "why": "read"}
      ]
    },
    "copy": {
      "label": "Copy",
      "args": [
        {"name": "source", "kind": "path", "required": true},
        {"name": "target", "kind": "path", "required": true}
      ],
      "needs": [
        {"verb": "fs.read", "scope": {"kind": "from-arg", "arg": "source"}, "why": "read"},
        {"verb": "fs.write", "scope": {"kind": "from-arg", "arg": "target"}, "why": "write"}
      ]
    }
  }
}"#;

struct HostProcess {
    child: Child,
    client: ClientIdentity,
}

impl HostProcess {
    fn spawn(no_new_privs: bool) -> Self {
        let root = unsafe { libc::geteuid() } == 0;
        let uid = if root {
            65534
        } else {
            unsafe { libc::geteuid() }
        };
        let gid = if root {
            65534
        } else {
            unsafe { libc::getegid() }
        };
        let mut command = Command::new("/bin/sleep");
        command
            .arg("120")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(move || {
                if root
                    && (libc::setgroups(0, std::ptr::null()) != 0
                        || libc::setresgid(gid, gid, gid) != 0
                        || libc::setresuid(uid, uid, uid) != 0)
                {
                    return Err(std::io::Error::last_os_error());
                }
                if no_new_privs && libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().expect("spawn a separate unprivileged host");
        let client = ClientIdentity {
            pid: Some(child.id()),
            uid: Some(uid),
            gid: Some(gid),
            start_time_ticks: crate::proc::read_start_time_ticks_pub(child.id()),
        };
        Self { child, client }
    }
}

impl Drop for HostProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Fixture {
    _lock: std::sync::MutexGuard<'static, ()>,
    root: PathBuf,
    previous: Vec<(&'static str, Option<OsString>)>,
    replaced_trust: bool,
}

impl Fixture {
    fn new() -> Self {
        let lock = crate::test_env::lock_env();
        let root = crate::test_env::secure_scratch_dir("task-host-authority");
        let mut fixture = Self {
            _lock: lock,
            root,
            previous: Vec::new(),
            replaced_trust: false,
        };
        for (key, child) in [
            ("COS_DATA_DIR", "data"),
            ("COS_CAPS_DATA_DIR", "caps"),
            ("COS_PROC_DATA_DIR", "procdata"),
            ("COS_PROVENANCE_RUNTIME_DIR", "runtime"),
            ("COS_APPS_DIR", "apps"),
        ] {
            fixture.previous.push((key, std::env::var_os(key)));
            std::env::set_var(key, fixture.root.join(child));
        }
        fixture
    }

    fn signed_app(&mut self) -> App {
        self.signed_app_with_manifest(MANIFEST)
    }

    fn signed_app_with_manifest(&mut self, manifest: &str) -> App {
        use crate::provenance::{sign, trust::TrustRootSpec, TrustStore};

        let declaration = Manifest::from_json(manifest).unwrap();
        let app_dir = self.root.join("apps").join(&declaration.id);
        let trust_dir = self.root.join("trust");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::create_dir(&trust_dir).unwrap();
        for dir in [&app_dir, &trust_dir] {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        for (name, contents) in [
            ("app.json", manifest),
            (
                "main.py",
                "from pathlib import Path\nPath(__file__).with_name('executed').write_text('unexpected execution')\n",
            ),
        ] {
            let path = app_dir.join(name);
            std::fs::write(&path, contents).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let key = sign::SigningKeyFile::generate(Some("task-host fixture".to_string())).unwrap();
        let key_path = trust_dir.join("publisher.json");
        std::fs::write(
            &key_path,
            serde_json::to_vec(&key.trust_entry(&[PackageKind::App])).unwrap(),
        )
        .unwrap();
        std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let roots = vec![TrustRootSpec {
            path: trust_dir,
            tier: TrustTier::User,
            allowed_uids: vec![unsafe { libc::geteuid() }],
            domain: crate::provenance::state::TrustDomain::Owner(unsafe { libc::geteuid() }),
        }];
        crate::test_env::record_trust_state(&roots);
        let trust = TrustStore::load_roots(&roots);
        assert!(!trust.is_empty(), "{:?}", trust.diagnostics());
        crate::provenance::set_trust_store_for_roots(trust, roots);
        self.replaced_trust = true;
        sign::sign_directory(
            &app_dir,
            &sign::SignRequest {
                kind: PackageKind::App,
                id: declaration.id.clone(),
                version: declaration.version,
                manifest_schema: "test".to_string(),
                manifest_path: "app.json".to_string(),
                entrypoints: vec!["main.py".to_string()],
                resources: vec![],
            },
            &key,
        )
        .unwrap();
        installed_app(&declaration.id)
            .expect("authenticate the fixture before reading its declaration")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if self.replaced_trust {
            crate::provenance::reload_trust();
        }
        for (key, previous) in self.previous.drain(..).rev() {
            match previous {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn argv(tokens: &[&str]) -> Vec<String> {
    tokens.iter().map(|token| (*token).to_string()).collect()
}

fn parent(caps: CapSet) -> SessionInfo {
    SessionInfo {
        session_id: "task-parent".to_string(),
        pid: std::process::id(),
        command: vec!["agent task".to_string()],
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: Some("agent".to_string()),
        parent: None,
        workdir: None,
        exit_code: None,
        ended_at: None,
        tier: Some(Role::AgentHost.credential_tier()),
        scope: Some("task-scope".to_string()),
        priority: Some("normal".to_string()),
        caps: Some(caps),
        transient_caps: None,
        role: Some(Role::AgentHost.name().to_string()),
        app_id: None,
        pending_bind: false,
        start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
    }
}

fn invoke_caps() -> CapSet {
    CapSet::from_caps([Cap::new(Verb::AGENT_INVOKE, Scope::name("fs"))])
}

fn delegation_for(client: &ClientIdentity, parent: &SessionInfo, params: Value) -> Delegation {
    let launcher = authenticate_task_host(client, parent).expect("trusted task host");
    task_host_delegation(
        &launcher,
        client.uid.unwrap(),
        &PathBuf::from("/home/task"),
        parent,
        &params,
    )
    .unwrap()
}

fn manifest_fixture() -> App {
    App {
        manifest: Manifest::from_json(MANIFEST).unwrap(),
        dir: PathBuf::from("/usr/lib/cos/apps/fs"),
        provenance: Err("pure argument-plan fixture, never installed".to_string()),
    }
}

fn assigned_invocation<'a>(
    app: &'a App,
    operation: &'a str,
    args: &'a [String],
) -> TaskHostInvocation<'a> {
    TaskHostInvocation {
        app_id: &app.manifest.id,
        operation,
        args,
        package_digest: app.require_verified().unwrap().content_digest(),
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn private_host_exception_preserves_public_no_new_privs_rejection() {
    let before = super::super::process_no_new_privs(std::process::id());
    let host = HostProcess::spawn(true);
    let parent = parent(invoke_caps());
    let launcher = authenticate_task_host(&host.client, &parent).unwrap();
    assert_eq!(launcher.parent.as_deref(), Some("task-parent"));
    assert_eq!(launcher.caps, parent.caps.clone().unwrap());
    assert_eq!(launcher.start_time_ticks, host.client.start_time_ticks);
    assert_eq!(launcher.scope, parent.scope);

    let uid = host.client.uid.unwrap();
    let home = host.client.require_home_dir().unwrap();
    let error = runtime()
        .block_on(authenticate_launcher(&host.client, uid, home))
        .unwrap_err();
    assert_eq!(error, "App processes cannot manage App sessions");
    let error = runtime()
        .block_on(register(
            json!({"app_id": "fs", "kind": "operation", "operation": "read", "args": []}),
            &host.client,
        ))
        .unwrap_err();
    assert_eq!(error.message, "App processes cannot manage App sessions");
    assert_eq!(
        super::super::process_no_new_privs(std::process::id()),
        before
    );
}

#[test]
fn task_host_refuses_wrong_or_missing_process_identity() {
    let mut host = HostProcess::spawn(true);
    let parent = parent(invoke_caps());
    for pid in [None, Some(0), Some(1), Some(u32::MAX)] {
        let mut forged = host.client.clone();
        forged.pid = pid;
        assert!(authenticate_task_host(&forged, &parent).is_err());
    }
    for uid in [None, Some(0), Some(host.client.uid.unwrap() + 1)] {
        let mut forged = host.client.clone();
        forged.uid = uid;
        assert!(authenticate_task_host(&forged, &parent).is_err());
    }
    for ticks in [
        None,
        Some(0),
        host.client.start_time_ticks.map(|ticks| ticks + 1),
    ] {
        let mut forged = host.client.clone();
        forged.start_time_ticks = ticks;
        assert!(authenticate_task_host(&forged, &parent).is_err());
    }
    let mut forged = host.client.clone();
    forged.gid = Some(host.client.gid.unwrap() + 1);
    assert!(authenticate_task_host(&forged, &parent).is_err());
    forged.gid = None;
    assert!(authenticate_task_host(&forged, &parent).is_err());
    host.child.kill().unwrap();
    host.child.wait().unwrap();
    assert!(authenticate_task_host(&host.client, &parent).is_err());
}

#[test]
fn task_host_refuses_a_launcher_without_no_new_privs() {
    if super::super::process_no_new_privs(std::process::id()) != Some(false) {
        eprintln!("skipped: this runner already inherited NoNewPrivs");
        return;
    }
    let host = HostProcess::spawn(false);
    assert!(authenticate_task_host(&host.client, &parent(invoke_caps())).is_err());
}

#[test]
fn task_host_refuses_unusable_or_app_parent_authority() {
    let host = HostProcess::spawn(true);
    let valid = parent(invoke_caps());
    for id in ["", " ", " padded ", "../other", "other\nsession"] {
        let mut forged = valid.clone();
        forged.session_id = id.to_string();
        assert!(authenticate_task_host(&host.client, &forged).is_err());
    }
    let mut forged = valid.clone();
    forged.session_id = "x".repeat(129);
    assert!(authenticate_task_host(&host.client, &forged).is_err());
    for caps in [None, Some(CapSet::new())] {
        let mut forged = valid.clone();
        forged.caps = caps;
        assert!(authenticate_task_host(&host.client, &forged).is_err());
    }
    let mut forged = valid.clone();
    forged.app_id = Some("fs".to_string());
    assert!(authenticate_task_host(&host.client, &forged).is_err());
    let mut forged = valid.clone();
    forged.pending_bind = true;
    assert!(authenticate_task_host(&host.client, &forged).is_err());
    for group in ["app", "mcp"] {
        let mut forged = valid.clone();
        forged.group = Some(group.to_string());
        assert!(authenticate_task_host(&host.client, &forged).is_err());
    }
    let mut ended = valid;
    ended.ended_at = Some(chrono::Utc::now().to_rfc3339());
    assert!(authenticate_task_host(&host.client, &ended).is_err());
}

#[test]
fn task_host_accepts_only_its_assigned_app_and_operation() {
    let args = argv(&["/home/task/file"]);
    let invocation = TaskHostInvocation {
        app_id: "fs",
        operation: "read",
        args: &args,
        package_digest: "sha256:fixture",
    };
    let valid = json!({
        "app_id": "fs", "kind": "operation", "operation": "read", "args": args,
    });
    invocation.validate_registration(&valid).unwrap();
    for kind in ["gui", "mcp", "native", "register_native"] {
        let mut forged = valid.clone();
        forged["kind"] = json!(kind);
        assert!(invocation.validate_registration(&forged).is_err());
    }
    for (field, value) in [("app_id", "other"), ("operation", "copy")] {
        let mut forged = valid.clone();
        forged[field] = json!(value);
        assert!(invocation.validate_registration(&forged).is_err());
    }
    for field in ["owner_uid", "session_id", "parent", "caps", "trust_tier"] {
        let mut forged = valid.clone();
        forged[field] = json!("forged");
        assert!(invocation.validate_registration(&forged).is_err());
    }
}

#[test]
fn task_host_allows_equivalent_defaults_but_refuses_argument_swaps() {
    let host = HostProcess::spawn(true);
    let context = delegation_for(&host.client, &parent(invoke_caps()), json!({}));
    let app = manifest_fixture();
    let args = argv(&["~/file"]);
    let invocation = TaskHostInvocation {
        app_id: "fs",
        operation: "read",
        args: &args,
        package_digest: "sha256:fixture",
    };
    invocation
        .validate_args(&app, &argv(&["/home/task/file", "--format=utf8"]), &context)
        .unwrap();
    for reported in [
        argv(&["/home/task/another"]),
        argv(&["/etc/passwd"]),
        argv(&["/home/task/file", "--format=other"]),
    ] {
        let error = invocation
            .validate_args(&app, &reported, &context)
            .unwrap_err();
        assert_eq!(error.kind, BrokerErrorKind::Unauthorized);
    }
    assert!(invocation.validate_args(&app, &[], &context).is_err());
    assert!(operation_plan(
        &app,
        "__schema__",
        &[],
        &context,
        &Ceiling::for_tier(TrustTier::User),
    )
    .is_err());
}

#[test]
fn task_host_parent_caps_only_narrow_the_authenticated_task_ceiling() {
    let host = HostProcess::spawn(true);
    let trusted = CapSet::from_caps([
        Cap::new(Verb::FS_READ, Scope::path("/home/task/**")),
        Cap::new(Verb::AGENT_INVOKE, Scope::name("fs")),
    ]);
    let parent = parent(trusted.clone());
    let inflated = CapSet::from_caps([
        Cap::new(Verb::FS_READ, Scope::path("/**")),
        Cap::new(Verb::FS_READ, Scope::path("/home/task/file")),
        Cap::new(Verb::SYS_PACKAGE, Scope::name("nano")),
        Cap::new(Verb::AGENT_INVOKE, Scope::name("fs")),
    ]);
    let context = delegation_for(&host.client, &parent, json!({"parent_caps": inflated}));
    assert!(trusted.covers_all(&context.ceiling));
    assert!(context
        .ceiling
        .covers(&Cap::new(Verb::FS_READ, Scope::path("/home/task/file"))));
    assert!(!context
        .ceiling
        .covers(&Cap::new(Verb::FS_READ, Scope::path("/home/task/other"))));
    assert!(!context
        .ceiling
        .covers(&Cap::new(Verb::FS_READ, Scope::path("/etc/passwd"))));
    assert!(!context
        .ceiling
        .covers(&Cap::new(Verb::SYS_PACKAGE, Scope::name("nano"))));
}

#[test]
fn task_host_relative_targets_use_only_the_trusted_parent_directory() {
    let host = HostProcess::spawn(true);
    let mut parent = parent(invoke_caps());
    parent.workdir = Some("/home/task/work".to_string());
    let context = delegation_for(&host.client, &parent, json!({}));
    assert_eq!(context.paths.cwd, Some(PathBuf::from("/home/task/work")));
    let app = manifest_fixture();
    let args = argv(&["relative-file"]);
    let invocation = TaskHostInvocation {
        app_id: "fs",
        operation: "read",
        args: &args,
        package_digest: "sha256:fixture",
    };
    invocation
        .validate_args(&app, &argv(&["/home/task/work/relative-file"]), &context)
        .unwrap();
    invocation
        .validate_args(&app, &argv(&["/home/task/relative-file"]), &context)
        .unwrap_err();

    parent.workdir = None;
    let context = delegation_for(&host.client, &parent, json!({}));
    assert_eq!(context.paths.cwd, Some(PathBuf::from("/home/task")));
    parent.workdir = Some("../elsewhere".to_string());
    let launcher = authenticate_task_host(&host.client, &parent).unwrap();
    assert!(task_host_delegation(
        &launcher,
        host.client.uid.unwrap(),
        &PathBuf::from("/home/task"),
        &parent,
        &json!({}),
    )
    .is_err());
}

#[test]
fn task_host_retries_share_parent_approvals_but_not_launch_handles() {
    let _fixture = Fixture::new();
    let first = HostProcess::spawn(true);
    let retry = HostProcess::spawn(true);
    let parent = parent(invoke_caps());
    let first_context = delegation_for(&first.client, &parent, json!({}));
    let retry_context = delegation_for(&retry.client, &parent, json!({}));
    assert_eq!(first_context.grant_session, parent.session_id);
    assert_eq!(first_context.grant_session, retry_context.grant_session);
    assert_ne!(first_context.requester, retry_context.requester);

    let app = manifest_fixture();
    let args = argv(&["/home/task/source", "/home/task/target"]);
    let ceiling = Ceiling::for_tier(TrustTier::User);
    let plan =
        |context: &Delegation| operation_plan(&app, "copy", &args, context, &ceiling).unwrap();
    let denied = authorize_plan(&first_context, plan(&first_context), &ceiling, "fs").unwrap_err();
    assert_eq!(denied.audit_class, Some("approval_required"));
    let owner = first.client.uid;
    let pending = crate::approvals::list_pending_for_owner(owner);
    assert_eq!(pending.len(), 2);
    assert!(pending
        .iter()
        .all(|request| request.session == parent.session_id));
    crate::approvals::approve_for_owner(
        &pending[0].id,
        crate::approvals::GrantDuration::Once,
        Some("uid:0".to_string()),
        None,
        owner,
    )
    .unwrap();
    authorize_plan(&retry_context, plan(&retry_context), &ceiling, "fs").unwrap_err();
    let first_cap = Cap::new(
        Verb::parse(&pending[0].verb).unwrap(),
        pending[0].scope.clone(),
    );
    assert!(
        crate::approvals::has_approved_grant_for_owner(&parent.session_id, &first_cap, owner,)
            .unwrap()
    );
    assert!(!crate::approvals::has_approved_grant_for_owner(
        &parent.session_id,
        &first_cap,
        owner.map(|uid| uid + 1),
    )
    .unwrap());
    crate::approvals::approve_for_owner(
        &pending[1].id,
        crate::approvals::GrantDuration::Once,
        Some("uid:0".to_string()),
        None,
        owner,
    )
    .unwrap();
    let caps = authorize_plan(&retry_context, plan(&retry_context), &ceiling, "fs").unwrap();
    assert!(caps.covers(&Cap::new(Verb::FS_READ, Scope::path("/home/task/source"))));
    assert!(caps.covers(&Cap::new(Verb::FS_WRITE, Scope::path("/home/task/target"))));
    assert!(
        !crate::approvals::has_approved_grant_for_owner(&parent.session_id, &first_cap, owner,)
            .unwrap()
    );

    let launcher = authenticate_task_host(&retry.client, &parent).unwrap();
    let session = format!("app-{}", uuid::Uuid::new_v4().simple());
    let handle = issue_launch_grant(
        &session,
        Some("fs"),
        owner.unwrap(),
        &launcher,
        &caps,
        Some(&ceiling),
    )
    .unwrap();
    require_launch_grant(&retry.client, &handle, &session, owner.unwrap()).unwrap();
    require_launch_grant(&first.client, &handle, &session, owner.unwrap()).unwrap_err();
    crate::clawd::authority::authority().revoke_session(&session);
}

#[test]
fn task_host_requires_verified_metadata_and_the_exact_package_digest() {
    let mut fixture = Fixture::new();
    let app = fixture.signed_app();
    let args = argv(&["/home/task/file"]);
    let digest = app.require_verified().unwrap().content_digest().to_string();
    let invocation = TaskHostInvocation {
        app_id: "fs",
        operation: "read",
        args: &args,
        package_digest: &digest,
    };
    invocation.validate_package(&app).unwrap();
    invocation
        .validate_package(&manifest_fixture())
        .unwrap_err();
    let forged = TaskHostInvocation {
        package_digest: "sha256:forged",
        ..invocation
    };
    assert_eq!(
        forged.validate_package(&app).unwrap_err().kind,
        BrokerErrorKind::Unauthorized
    );
    let other = TaskHostInvocation {
        app_id: "other",
        ..invocation
    };
    other.validate_package(&app).unwrap_err();
    let gui = TaskHostInvocation {
        operation: "--gui",
        ..invocation
    };
    gui.validate_package(&app).unwrap_err();

    let host = HostProcess::spawn(true);
    let error = runtime()
        .block_on(register_for_task_host(
            json!({"app_id": "fs", "kind": "operation", "operation": "read", "args": args}),
            &host.client,
            &parent(invoke_caps()),
            &forged,
        ))
        .unwrap_err();
    assert_eq!(error.kind, BrokerErrorKind::Unauthorized);
    assert!(error.message.contains("verified App package"));

    std::fs::write(
        app.dir.join("app.json"),
        MANIFEST.replace("fs.read", "fs.write"),
    )
    .unwrap();
    assert!(
        installed_app("fs").is_err(),
        "a changed manifest must be quarantined"
    );
}

#[test]
fn prepare_task_host_canonicalizes_without_grants_approvals_sessions_or_execution() {
    let mut fixture = Fixture::new();
    let app = fixture.signed_app();
    let host = HostProcess::spawn(true);
    let home = host.client.require_home_dir().unwrap();
    let uid = host.client.uid.unwrap();
    let work = fixture.root.join("work");
    std::fs::create_dir(&work).unwrap();
    let file = work.join("note.txt");
    std::fs::write(&file, "unchanged").unwrap();
    let canonical = file.canonicalize().unwrap().to_str().unwrap().to_string();
    let mut parent = parent(invoke_caps());
    parent.workdir = Some(work.to_str().unwrap().to_string());
    let original = argv(&["./note.txt"]);
    let invocation = assigned_invocation(&app, "read", &original);

    // Even an already-approved missing capability must not be consumed by preparation.
    let cap = Cap::new(Verb::FS_READ, Scope::path(&canonical));
    let approval = crate::approvals::submit_owned(
        cap.verb,
        cap.scope.clone(),
        parent.session_id.clone(),
        "preparation fixture".to_string(),
        None,
        Some(uid),
    )
    .unwrap();
    crate::approvals::approve_for_owner(
        &approval,
        crate::approvals::GrantDuration::Once,
        Some("uid:0".to_string()),
        None,
        Some(uid),
    )
    .unwrap();
    let grants_before = crate::clawd::authority::authority().len();
    let registry_snapshot = || {
        runtime().block_on(crate::paths::with_user_override(uid, home.clone(), async {
            serde_json::to_value(crate::proc::registry_sessions()).unwrap()
        }))
    };
    let sessions_before = registry_snapshot();

    let prepared = prepare_for_task_host(&host.client, &parent, &invocation).unwrap();
    assert_eq!(prepared, argv(&[&canonical, "--format", "utf8"]));
    assert_eq!(original, argv(&["./note.txt"]));
    let launcher = authenticate_task_host(&host.client, &parent).unwrap();
    let delegation = task_host_delegation(&launcher, uid, &home, &parent, &Value::Null).unwrap();
    invocation
        .validate_args(&app, &prepared, &delegation)
        .unwrap();
    let mut swapped = prepared;
    swapped[0] = work.join("other.txt").to_str().unwrap().to_string();
    invocation
        .validate_args(&app, &swapped, &delegation)
        .unwrap_err();

    assert_eq!(crate::clawd::authority::authority().len(), grants_before);
    assert_eq!(registry_snapshot(), sessions_before);
    assert!(crate::approvals::list_pending_for_owner(Some(uid)).is_empty());
    assert!(
        crate::approvals::has_approved_grant_for_owner(&parent.session_id, &cap, Some(uid),)
            .unwrap()
    );
    assert!(!fixture.root.join("runtime").exists());
    assert!(!fixture.root.join("procdata").exists());
    assert!(!app.dir.join("executed").exists());
    assert_eq!(std::fs::read_to_string(file).unwrap(), "unchanged");
}

#[test]
fn prepare_task_host_uses_passwd_home_for_tilde_and_missing_workdir() {
    let mut fixture = Fixture::new();
    let app = fixture.signed_app();
    let host = HostProcess::spawn(true);
    let parent = parent(invoke_caps());
    let home = host.client.require_home_dir().unwrap();
    let expected = home.join("task-host-file").to_str().unwrap().to_string();
    for original in [argv(&["~/task-host-file"]), argv(&["task-host-file"])] {
        let invocation = assigned_invocation(&app, "read", &original);
        assert_eq!(
            prepare_for_task_host(&host.client, &parent, &invocation).unwrap(),
            argv(&[&expected, "--format", "utf8"]),
        );
    }
    assert!(!fixture.root.join("caps").exists());
    assert!(!app.dir.join("executed").exists());
}

#[test]
fn prepare_task_host_materializes_derived_path_defaults_before_host_dispatch() {
    let mut fixture = Fixture::new();
    let mut manifest: Value = serde_json::from_str(MANIFEST).unwrap();
    manifest["operations"]["download"] = json!({
        "label": "Download",
        "args": [
            {"name": "url", "kind": "text", "required": true},
            {"name": "output", "kind": "path", "default_from": {
                "arg": "url", "transform": "url-path-basename", "prefix": "./",
                "fallback": "download",
            }},
        ],
        "needs": [
            {"verb": "fs.write", "scope": {"kind": "from-arg", "arg": "output"}, "why": "save"},
        ],
    });
    let app = fixture.signed_app_with_manifest(&manifest.to_string());
    let host = HostProcess::spawn(true);
    let work = fixture.root.join("downloads");
    std::fs::create_dir(&work).unwrap();
    let file = work.join("note.txt");
    std::fs::write(&file, "untouched").unwrap();
    let mut parent = parent(invoke_caps());
    parent.workdir = Some(work.to_str().unwrap().to_string());
    let original = argv(&["https://example.test/files/note.txt"]);
    let invocation = assigned_invocation(&app, "download", &original);
    let canonical = file.canonicalize().unwrap().to_str().unwrap().to_string();
    assert_eq!(
        prepare_for_task_host(&host.client, &parent, &invocation).unwrap(),
        argv(&["https://example.test/files/note.txt", &canonical]),
    );
    assert!(!fixture.root.join("caps").exists());
    assert!(!app.dir.join("executed").exists());
    assert_eq!(std::fs::read_to_string(file).unwrap(), "untouched");
}

#[test]
fn prepare_task_host_preserves_repeatable_alias_boolean_and_literal_arguments() {
    let mut fixture = Fixture::new();
    let mut manifest: Value = serde_json::from_str(MANIFEST).unwrap();
    manifest["operations"]["echo"] = json!({
        "label": "Echo",
        "args": [
            {"name": "payload", "kind": "text", "required": true, "repeatable": true},
            {"name": "enabled", "kind": "bool", "default": true},
            {"name": "silent", "kind": "bool"},
            {"name": "tag", "kind": "text", "binding": "flag", "repeatable": true, "aliases": ["-t"]},
            {"name": "count", "kind": "integer", "binding": "flag", "default": 2},
            {"name": "ratio", "kind": "number", "binding": "flag", "default": 2},
        ],
        "needs": [],
    });
    let app = fixture.signed_app_with_manifest(&manifest.to_string());
    let host = HostProcess::spawn(true);
    let parent = parent(invoke_caps());
    let original = argv(&[
        "--enabled=false",
        "-t=--literal",
        "-t",
        "other",
        "--",
        "--payload",
        "data",
    ]);
    let invocation = assigned_invocation(&app, "echo", &original);
    assert_eq!(
        prepare_for_task_host(&host.client, &parent, &invocation).unwrap(),
        argv(&[
            "--enabled=false",
            "--tag=--literal",
            "--tag",
            "other",
            "--count",
            "2",
            "--ratio",
            "2",
            "--",
            "--payload",
            "data",
        ]),
    );
    assert!(!fixture.root.join("caps").exists());
    assert!(!app.dir.join("executed").exists());
}

#[test]
fn prepare_task_host_rejects_mismatched_package_surface_and_identity_without_side_effects() {
    let mut fixture = Fixture::new();
    let app = fixture.signed_app();
    let host = HostProcess::spawn(true);
    let parent = parent(invoke_caps());
    let original = argv(&["/home/task/file"]);
    let invocation = assigned_invocation(&app, "read", &original);
    let grants_before = crate::clawd::authority::authority().len();
    let forged = TaskHostInvocation {
        package_digest: "sha256:other",
        ..invocation
    };
    assert_eq!(
        prepare_for_task_host(&host.client, &parent, &forged)
            .unwrap_err()
            .kind,
        BrokerErrorKind::Unauthorized,
    );
    for operation in ["--gui", "__schema__", "session", "unknown"] {
        let forged = TaskHostInvocation {
            operation,
            ..invocation
        };
        assert!(prepare_for_task_host(&host.client, &parent, &forged).is_err());
    }
    let forged = TaskHostInvocation {
        app_id: "other",
        ..invocation
    };
    assert!(prepare_for_task_host(&host.client, &parent, &forged).is_err());
    let mut stale = host.client.clone();
    stale.start_time_ticks = stale.start_time_ticks.map(|ticks| ticks + 1);
    assert!(prepare_for_task_host(&stale, &parent, &invocation).is_err());
    let mut app_parent = parent.clone();
    app_parent.app_id = Some("fs".to_string());
    assert!(prepare_for_task_host(&host.client, &app_parent, &invocation).is_err());
    let missing = TaskHostInvocation {
        args: &[],
        ..invocation
    };
    assert!(prepare_for_task_host(&host.client, &parent, &missing).is_err());
    assert_eq!(crate::clawd::authority::authority().len(), grants_before);
    for dir in ["caps", "runtime", "procdata"] {
        assert!(!fixture.root.join(dir).exists());
    }
    assert!(!app.dir.join("executed").exists());
}

#[test]
fn prepare_task_host_requires_explicit_runtime_selected_arguments() {
    let mut fixture = Fixture::new();
    let app = fixture.signed_app_with_manifest(
        &json!({
            "id": "calendar", "version": "0.1.0", "name": "Calendar",
            "operations": {
                "today": {
                    "label": "Today",
                    "args": [{
                        "name": "provider", "kind": "name", "binding": "flag",
                        "trusted_resolver": "calendar-provider", "choices": ["local", "google"],
                    }],
                    "needs": [],
                },
            },
        })
        .to_string(),
    );
    let host = HostProcess::spawn(true);
    let parent = parent(CapSet::from_caps([Cap::new(
        Verb::AGENT_INVOKE,
        Scope::name("calendar"),
    )]));
    let invocation = assigned_invocation(&app, "today", &[]);
    let error = prepare_for_task_host(&host.client, &parent, &invocation).unwrap_err();
    assert!(error
        .message
        .contains("runtime-selected argument `provider`"));
    let explicit = argv(&["--provider=local"]);
    let invocation = assigned_invocation(&app, "today", &explicit);
    assert_eq!(
        prepare_for_task_host(&host.client, &parent, &invocation).unwrap(),
        argv(&["--provider", "local"]),
    );
    for dir in ["caps", "runtime", "procdata"] {
        assert!(!fixture.root.join(dir).exists());
    }
    assert!(!app.dir.join("executed").exists());
}

#[test]
fn prepare_task_host_refuses_canonicalization_that_changes_conditional_needs() {
    let mut fixture = Fixture::new();
    let mut manifest: Value = serde_json::from_str(MANIFEST).unwrap();
    manifest["operations"]["observe"] = json!({
        "label": "Observe",
        "args": [{"name": "ratio", "kind": "number", "binding": "flag", "default": 2}],
        "needs": [{
            "verb": "sys.observe", "scope": {"kind": "wild"},
            "when": {"kind": "arg-not-equals", "arg": "ratio", "value": 2},
            "why": "conditional observation",
        }],
    });
    let app = fixture.signed_app_with_manifest(&manifest.to_string());
    let host = HostProcess::spawn(true);
    let invocation = assigned_invocation(&app, "observe", &[]);
    let error =
        prepare_for_task_host(&host.client, &parent(invoke_caps()), &invocation).unwrap_err();
    assert_eq!(error.kind, BrokerErrorKind::Unauthorized);
    assert!(error.message.contains("arguments do not match"));
    assert!(!fixture.root.join("caps").exists());
    assert!(!app.dir.join("executed").exists());
}

#[test]
fn prepare_task_host_refuses_defaults_that_exceed_registration_bounds() {
    let mut fixture = Fixture::new();
    let mut manifest: Value = serde_json::from_str(MANIFEST).unwrap();
    manifest["operations"]["wide"] = json!({
        "label": "Wide",
        "args": (0..65).map(|index| json!({
            "name": format!("value_{index}"), "kind": "text",
            "binding": "flag", "default": "value",
        })).collect::<Vec<_>>(),
        "needs": [],
    });
    let app = fixture.signed_app_with_manifest(&manifest.to_string());
    let host = HostProcess::spawn(true);
    let invocation = assigned_invocation(&app, "wide", &[]);
    let error =
        prepare_for_task_host(&host.client, &parent(invoke_caps()), &invocation).unwrap_err();
    assert_eq!(error.kind, BrokerErrorKind::Execution);
    assert!(error.message.contains("invalid task-host App registration"));
    assert!(!fixture.root.join("caps").exists());
    assert!(!app.dir.join("executed").exists());
}
