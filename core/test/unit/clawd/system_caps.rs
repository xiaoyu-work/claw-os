use super::*;
use crate::caps::Verb;
use crate::session::SessionOrigin;
use crate::test_env::{lock_env, TestEnvVarGuard};

/// Owner roots a test Agent is scoped to, plus a second account's home
/// alongside it so cross-user reads are testable without touching the
/// host's real `/home`.
struct Owner {
    _temp: tempfile::TempDir,
    _data: TestEnvVarGuard,
    uid: u32,
    home: PathBuf,
    other_home: PathBuf,
}

fn owner() -> Owner {
    let temp = tempfile::tempdir().expect("tempdir");
    let data = TestEnvVarGuard::set("COS_DATA_DIR", temp.path().join("var"));
    let home = temp.path().join("home").join("owner");
    let other_home = temp.path().join("home").join("neighbour");
    std::fs::create_dir_all(&home).expect("owner home");
    std::fs::create_dir_all(&other_home).expect("neighbour home");
    Owner {
        _temp: temp,
        _data: data,
        uid: 1001,
        home,
        other_home,
    }
}

fn caps_for(owner: &Owner) -> CapSet {
    system_agent_caps(owner.uid, &owner.home)
}

// -----------------------------------------------------------------
// Filesystem: owner roots only
// -----------------------------------------------------------------

/// Regression: a task owned by an unprivileged user runs inside root
/// `clawd`, so a global path scope hands it the daemon's view of the
/// whole machine. The baseline is bounded to the owner's own roots.
#[test]
fn system_agent_caps_bound_paths_to_owner_roots() {
    let _lock = lock_env();
    let owner = owner();
    let caps = caps_for(&owner);

    let home_file = owner.home.join("notes.md");
    assert!(caps.covers(&Cap::new(
        Verb::FS_READ,
        Scope::path(home_file.to_string_lossy().into_owned())
    )));
    assert!(caps.covers(&Cap::new(
        Verb::FS_WRITE,
        Scope::path(home_file.to_string_lossy().into_owned())
    )));
    assert!(caps.covers(&Cap::new(
        Verb::FS_META,
        Scope::path(home_file.to_string_lossy().into_owned())
    )));

    // The daemon keeps per-user Agent state (memory database, notes,
    // semantic index) outside the home, so that root is granted too —
    // and only that one.
    let state = crate::paths::clawd_user_agent_state_dir(owner.uid);
    assert!(caps.covers(&Cap::new(
        Verb::FS_READ,
        Scope::path(state.join("memory.db").to_string_lossy().into_owned())
    )));
    assert!(!caps.covers(&Cap::new(
        Verb::FS_READ,
        Scope::path(
            crate::paths::clawd_user_agent_state_dir(1002)
                .join("memory.db")
                .to_string_lossy()
                .into_owned()
        )
    )));

    for denied in [
        "/etc/shadow",
        "/etc/sudoers",
        "/root/.ssh/id_ed25519",
        "/proc/1/environ",
        "/**",
        "/",
    ] {
        assert!(
            !caps.covers(&Cap::new(Verb::FS_READ, Scope::path(denied))),
            "fs.read must not cover {denied}"
        );
    }
    assert!(!caps.covers(&Cap::new(
        Verb::FS_READ,
        Scope::path(
            owner
                .other_home
                .join(".ssh/id_ed25519")
                .to_string_lossy()
                .into_owned()
        )
    )));
    assert!(!caps.covers(&Cap::new(Verb::FS_WRITE, Scope::path("/etc/passwd"))));
    assert!(!caps.covers(&Cap::new(
        Verb::FS_DELETE,
        Scope::path(owner.home.join("notes.md").to_string_lossy().into_owned())
    )));
    assert!(!caps.covers(&Cap::new(Verb::FS_EXEC, Scope::path("/bin/sh"))));
    assert!(!caps.covers(&Cap::new(
        Verb::FS_EXEC,
        Scope::path(owner.home.join("payload.sh").to_string_lossy().into_owned())
    )));
}

/// Root-owned tasks are not a privileged class: uid 0 gets the same
/// table, bounded to root's own home, and never global authority.
#[test]
fn root_owner_gets_the_same_bounded_baseline() {
    let _lock = lock_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", temp.path().join("var"));
    let root_home = temp.path().join("root");
    std::fs::create_dir_all(&root_home).expect("root home");

    let root = system_agent_caps(0, &root_home);
    let user = system_agent_caps(1001, temp.path().join("home").join("owner").as_path());
    assert_eq!(root.verbs(), user.verbs());

    assert!(!root.covers(&Cap::new(Verb::FS_READ, Scope::path("/etc/shadow"))));
    assert!(!root.covers(&Cap::new(Verb::FS_READ, Scope::path("/**"))));
    assert!(!root.covers(&Cap::new(Verb::PROC_SPAWN, Scope::Wild)));
    assert!(!root.covers(&Cap::new(Verb::SECRET_READ, Scope::name("default/TOKEN"))));
}

// -----------------------------------------------------------------
// Denied by default
// -----------------------------------------------------------------

/// Everything that reaches another machine, another process, a
/// credential, or machine-wide state has to arrive through an
/// authenticated delegation or an exact approval.
#[test]
fn system_agent_caps_withhold_egress_execution_secrets_and_system_mutation() {
    let _lock = lock_env();
    let owner = owner();
    let caps = caps_for(&owner);

    // Egress, including the local-network and cloud-metadata targets a
    // Low catalog risk would happily wave through.
    for host in [
        "example.com",
        "**",
        "169.254.169.254",
        "metadata.google.internal",
        "127.0.0.1",
        "localhost:8080",
        "192.168.1.1",
    ] {
        assert!(
            !caps.covers(&Cap::new(Verb::NET_DIAL, Scope::host(host))),
            "net.dial must not cover {host}"
        );
        assert!(
            !caps.covers(&Cap::new(Verb::NET_RESOLVE, Scope::host(host))),
            "net.resolve must not cover {host}"
        );
        assert!(
            !caps.covers(&Cap::new(Verb::NET_PROBE, Scope::host(host))),
            "net.probe must not cover {host}"
        );
        assert!(
            !caps.covers(&Cap::new(Verb::BROWSER_NAV, Scope::host(host))),
            "browser.nav must not cover {host}"
        );
        assert!(
            !caps.covers(&Cap::new(Verb::BROWSER_DOM_READ, Scope::host(host))),
            "browser.dom.read must not cover {host}"
        );
    }

    // Running a program, in every shape the kernel offers one.
    assert!(!caps.covers(&Cap::new(Verb::PROC_SPAWN, Scope::Wild)));
    assert!(!caps.covers(&Cap::new(Verb::PROC_SPAWN, Scope::self_ref("self"))));
    assert!(!caps.covers(&Cap::new(Verb::PROC_SIGNAL, Scope::Wild)));
    assert!(!caps.covers(&Cap::new(Verb::DESKTOP_LAUNCH, Scope::name("terminal"))));

    // Credentials.
    for verb in [Verb::SECRET_READ, Verb::SECRET_WRITE, Verb::SECRET_GRANT] {
        assert!(
            !caps.covers(&Cap::new(verb, Scope::name("default/OPENAI_API_KEY"))),
            "{} must not be ambient",
            verb.as_str()
        );
    }

    // System, package, service, identity, storage, mount, power,
    // firewall and backup mutation.
    for (verb, scope) in [
        (Verb::SYS_PACKAGE, Scope::name("openssh-server")),
        (Verb::SYS_SERVICE, Scope::name("sshd")),
        (Verb::SYS_IDENTITY, Scope::name("manage")),
        (Verb::SYS_CONFIG, Scope::path("/etc/ssh/sshd_config")),
        (Verb::SYS_SECURITY, Scope::name("audit")),
        (Verb::SYS_STORAGE, Scope::name("diagnose")),
        (Verb::SYS_MOUNT, Scope::path("/dev/sda1")),
        (Verb::SYS_SNAPSHOT, Scope::Wild),
        (Verb::SYS_POWER, Scope::Wild),
        (Verb::SYS_TIME, Scope::Wild),
        (Verb::SYS_KERNEL, Scope::Wild),
        (Verb::SYS_EVENTS, Scope::name("observe")),
        (Verb::SYS_CONTAINER, Scope::name("control")),
        (Verb::SYS_CRASH, Scope::name("system")),
        (Verb::NET_FIREWALL, Scope::name("manage")),
        (Verb::NET_MANAGE, Scope::name("wifi")),
        (Verb::NET_LISTEN, Scope::host("0.0.0.0:8080")),
        (Verb::NET_RAW, Scope::host("**")),
        (Verb::DATA_BACKUP, Scope::path("/")),
        (Verb::DATA_KV_DELETE, Scope::name("key")),
        (Verb::CLIPBOARD_READ, Scope::name("selection")),
        (Verb::CLIPBOARD_WRITE, Scope::name("selection")),
        (Verb::UI_ACCESSIBILITY, Scope::name("control")),
        (Verb::UI_INPUT, Scope::Wild),
        (Verb::UI_WINDOW, Scope::Wild),
        (Verb::DEVICE_AUDIO, Scope::name("output")),
        (Verb::DEVICE_MICROPHONE, Scope::name("input")),
        (Verb::DEVICE_CAMERA, Scope::name("capture")),
        (Verb::DEVICE_USB, Scope::name("control")),
        (Verb::DEVICE_SENSOR, Scope::name("ambient")),
        (Verb::DEVICE_LOCATION, Scope::Wild),
        (Verb::DESKTOP_WINDOW, Scope::name("control")),
        // Persistence that outlives the conversation.
        (Verb::TIME_CRON, Scope::Wild),
        (Verb::AGENT_SPAWN, Scope::name("helper")),
        (Verb::AGENT_DELEGATE, Scope::name("helper")),
        // Cross-user local channels: queues and locks are addressed by
        // another session's id in the shared daemon data directory.
        (Verb::IPC_PUBLISH, Scope::name("other-session")),
        (Verb::IPC_SUBSCRIBE, Scope::name("other-session")),
        (Verb::IPC_INVOKE, Scope::name("lock")),
        // Model surfaces the catalog rates High.
        (Verb::AI_CHAT_UNTRUSTED, Scope::name("gpt-4o")),
        (Verb::AI_VISION_ANALYZE, Scope::name("gpt-4o")),
        (Verb::AI_BYPASS, Scope::Wild),
        (Verb::BROWSER_EVAL, Scope::host("example.com")),
        (Verb::BROWSER_INPUT_SECRET, Scope::host("example.com")),
        (Verb::BROWSER_DOM_WRITE, Scope::host("example.com")),
    ] {
        assert!(
            !caps.covers(&Cap::new(verb, scope.clone())),
            "{}:{} must not be ambient authority",
            verb.as_str(),
            scope
        );
    }
}

// -----------------------------------------------------------------
// Kept by default
// -----------------------------------------------------------------

/// The baseline still has to carry an ordinary owner-scoped
/// conversation: the model, its own memory, its own process rows,
/// read-only observation, the owner-partitioned App stores, and the
/// verbs that address nothing at all.
#[test]
fn system_agent_caps_keep_the_owner_scoped_conversation_baseline() {
    let _lock = lock_env();
    let owner = owner();
    let caps = caps_for(&owner);

    assert!(caps.covers(&Cap::new(Verb::AI_CHAT, Scope::name("claude-sonnet-4"))));
    assert!(caps.covers(&Cap::new(Verb::AI_EMBED, Scope::name("text-embedding-3"))));
    assert!(caps.covers(&Cap::new(Verb::MEMORY_READ, Scope::self_ref("web"))));
    assert!(caps.covers(&Cap::new(Verb::MEMORY_WRITE, Scope::self_ref("calendar"))));
    assert!(caps.covers(&Cap::new(Verb::PROC_OBSERVE, Scope::Wild)));
    assert!(caps.covers(&Cap::new(Verb::AGENT_INVOKE, Scope::name("web"))));
    assert!(caps.covers(&Cap::new(Verb::AGENT_OBSERVE, Scope::name("tasks"))));
    assert!(caps.covers(&Cap::new(Verb::DATA_KV_READ, Scope::name("notes/last"))));
    assert!(caps.covers(&Cap::new(Verb::DATA_DB_READ, Scope::name("calendar"))));
    assert!(caps.covers(&Cap::new(Verb::DATA_INBOX_READ, Scope::name("inbox"))));
    assert!(caps.covers(&Cap::new(Verb::UI_NOTIFY, Scope::Wild)));
    assert!(caps.covers(&Cap::new(Verb::UI_PROMPT, Scope::Wild)));
    assert!(caps.covers(&Cap::new(Verb::TIME_DELAY, Scope::Wild)));
    assert!(caps.covers(&Cap::new(Verb::BROWSER_TABS_READ, Scope::Wild)));
}

// -----------------------------------------------------------------
// Observation
// -----------------------------------------------------------------

/// `sys.observe` is not ambient just because it is read-only. The
/// baseline names the device-status domains the owner already sees on
/// their own machine, and refuses every domain that describes another
/// principal, another account's units, or the machine's security and
/// administrative posture.
#[test]
fn sys_observe_is_limited_to_owner_facing_device_status() {
    let _lock = lock_env();
    let owner = owner();
    let caps = caps_for(&owner);

    for domain in OBSERVABLE_DEVICE_DOMAINS {
        assert!(
            caps.covers(&Cap::new(Verb::SYS_OBSERVE, Scope::name(*domain))),
            "sys.observe:{domain} is ordinary owner-facing device status"
        );
    }

    for denied in [
        // Windows and shell state of whoever holds the seat.
        "desktop",
        // The account database.
        "identities",
        // Security posture and administrative state.
        "firewall",
        "system-snapshots",
        // Systemd units, including other accounts' `user@<uid>` slices.
        "user@1002.service",
        "sshd.service",
        // A different namespace borrowing the verb.
        "openssh-server",
        // And the wildcards this baseline used to hand out.
        "**",
        "*",
    ] {
        assert!(
            !caps.covers(&Cap::new(Verb::SYS_OBSERVE, Scope::name(denied))),
            "sys.observe:{denied} must require an exact approval"
        );
    }
    assert!(!caps.covers(&Cap::new(Verb::SYS_OBSERVE, Scope::Wild)));
}

// -----------------------------------------------------------------
// Owner home derivation
// -----------------------------------------------------------------

/// Every baseline and delegation derivation resolves the owner's root
/// the same way: canonical, existing, and owned by that uid.
#[test]
fn owner_home_derivation_is_canonical_and_owned() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let uid = unsafe { libc::geteuid() };
        let Ok(home) = verified_owner_home(uid) else {
            // No usable passwd home in this sandbox; the fail-closed
            // half of the contract is covered by the test below.
            return;
        };
        assert!(home.is_absolute());
        assert_eq!(home, home.canonicalize().expect("canonical home"));
        let metadata = std::fs::metadata(&home).expect("home metadata");
        assert!(metadata.is_dir());
        assert_eq!(metadata.uid(), uid);
    }
}

/// A home the daemon cannot verify yields no authority at all, rather
/// than a raw passwd string or a fallback root.
#[test]
fn owner_home_derivation_fails_closed_for_an_unresolvable_account() {
    for uid in [4_000_000_001_u32, 4_000_000_002, 4_000_000_003] {
        assert!(
            verified_owner_home(uid).is_err(),
            "uid {uid} has no passwd entry and must not resolve to a home"
        );
    }
}

/// Canonical roots are what makes the bound hold through symlinks: a
/// path that reaches the owner's home by another name is inside, and a
/// link planted *in* the home that points out of it is not.
#[test]
fn owner_path_roots_follow_symlinks_to_real_identity() {
    let _lock = lock_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", temp.path().join("var"));
    let real_home = temp.path().join("real-home");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&real_home).expect("real home");
    std::fs::create_dir_all(&outside).expect("outside");
    std::fs::write(real_home.join("notes.md"), b"notes").expect("notes");
    std::fs::write(outside.join("secret.txt"), b"secret").expect("secret");

    let canonical = real_home.canonicalize().expect("canonical home");
    let caps = system_agent_caps(1001, &canonical);

    #[cfg(unix)]
    {
        let linked_home = temp.path().join("linked-home");
        std::os::unix::fs::symlink(&real_home, &linked_home).expect("home symlink");
        assert!(caps.covers(&Cap::new(
            Verb::FS_READ,
            Scope::path(linked_home.join("notes.md").to_string_lossy().into_owned())
        )));

        let escape = real_home.join("escape");
        std::os::unix::fs::symlink(&outside, &escape).expect("escape symlink");
        assert!(
            !caps.covers(&Cap::new(
                Verb::FS_READ,
                Scope::path(escape.join("secret.txt").to_string_lossy().into_owned())
            )),
            "a link planted in the home must not extend the bound"
        );
        assert!(!caps.covers(&Cap::new(Verb::FS_READ, Scope::path("/etc/shadow"))));
    }
}

// -----------------------------------------------------------------
// Delegation
// -----------------------------------------------------------------

/// A clawd-issued scheduler snapshot keeps the exact executor verb and
/// exactly-named credentials the owner proved at creation, and nothing
/// else the creating session happened to hold.
#[test]
fn clamp_for_origin_readmits_only_the_reviewed_delegated_set() {
    let _lock = lock_env();
    let owner = owner();
    let mut stored = CapSet::new();
    stored.insert(Cap::new(Verb::PROC_SPAWN, Scope::Wild));
    stored.insert(Cap::new(Verb::AGENT_SPAWN, Scope::Wild));
    stored.insert(Cap::new(
        Verb::SECRET_READ,
        Scope::name("default/SMTP_PASSWORD"),
    ));
    stored.insert(Cap::new(Verb::SECRET_READ, Scope::name("default/*")));
    stored.insert(Cap::new(Verb::SECRET_WRITE, Scope::name("default/TOKEN")));
    stored.insert(Cap::new(Verb::SYS_SERVICE, Scope::name("sshd")));
    stored.insert(Cap::new(Verb::FS_EXEC, Scope::path("/bin/sh")));
    stored.insert(Cap::new(Verb::AGENT_DELEGATE, Scope::name("**")));

    let cron = clamp_for_origin(
        &stored,
        SessionOrigin::CronDelegation,
        owner.uid,
        &owner.home,
    );
    assert!(cron.covers(&Cap::new(Verb::PROC_SPAWN, Scope::Wild)));
    assert!(cron.covers(&Cap::new(
        Verb::SECRET_READ,
        Scope::name("default/SMTP_PASSWORD")
    )));
    assert!(!cron.covers(&Cap::new(Verb::AGENT_SPAWN, Scope::Wild)));
    // A glob credential grants nothing, so a sibling secret stays out.
    assert!(!cron.covers(&Cap::new(Verb::SECRET_READ, Scope::name("default/OTHER"))));
    assert!(!cron.covers(&Cap::new(Verb::SECRET_WRITE, Scope::name("default/TOKEN"))));
    assert!(!cron.covers(&Cap::new(Verb::SYS_SERVICE, Scope::name("sshd"))));
    assert!(!cron.covers(&Cap::new(Verb::FS_EXEC, Scope::path("/bin/sh"))));
    assert!(!cron.covers(&Cap::new(Verb::AGENT_DELEGATE, Scope::name("helper"))));

    let ambient = clamp_for_origin(
        &stored,
        SessionOrigin::SystemAgentTask,
        owner.uid,
        &owner.home,
    );
    assert!(!ambient.covers(&Cap::new(Verb::PROC_SPAWN, Scope::Wild)));
    assert!(!ambient.covers(&Cap::new(
        Verb::SECRET_READ,
        Scope::name("default/SMTP_PASSWORD")
    )));
}

/// Re-admission is verbatim: an executor verb the snapshot holds at a
/// narrower shape than the subsystem needs is not promoted to `Wild`,
/// and a credential name that could normalize onto another secret is
/// refused.
#[test]
fn delegated_readmission_never_widens_a_scope() {
    assert!(delegated_cap_is_admissible(
        &Cap::new(Verb::PROC_SPAWN, Scope::Wild),
        Verb::PROC_SPAWN
    ));
    assert!(!delegated_cap_is_admissible(
        &Cap::new(Verb::PROC_SPAWN, Scope::self_ref("self")),
        Verb::PROC_SPAWN
    ));
    assert!(delegated_cap_is_admissible(
        &Cap::new(Verb::SECRET_READ, Scope::name("default/TOKEN")),
        Verb::AGENT_SPAWN
    ));
    for rejected in [
        "",
        "*",
        "**",
        "default/*",
        "default/../other",
        "/default",
        "default/",
    ] {
        assert!(
            !delegated_cap_is_admissible(
                &Cap::new(Verb::SECRET_READ, Scope::name(rejected)),
                Verb::AGENT_SPAWN
            ),
            "secret.read:{rejected} is not an exact credential"
        );
    }
    assert!(!delegated_cap_is_admissible(
        &Cap::new(Verb::SECRET_READ, Scope::Wild),
        Verb::AGENT_SPAWN
    ));
}

// -----------------------------------------------------------------
// Shape of the grant
// -----------------------------------------------------------------

/// A resource-addressing verb never receives `Scope::Wild`, which
/// covers every scope of every kind and is therefore indistinguishable
/// from `fs.read:/**`. Path and host grants are never global either.
#[test]
fn system_agent_caps_never_grant_untyped_or_global_resource_scopes() {
    let _lock = lock_env();
    let owner = owner();
    let caps = caps_for(&owner);

    for cap in caps.iter() {
        let meta = crate::caps::catalog::lookup(cap.verb).expect("catalog entry");
        match meta.scope_kind {
            ScopeKind::Path | ScopeKind::Host | ScopeKind::Name => {
                assert_eq!(
                    cap.scope.kind(),
                    meta.scope_kind,
                    "{} must keep its declared scope kind, got {}",
                    cap.verb.as_str(),
                    cap.scope
                );
            }
            ScopeKind::SelfRef | ScopeKind::Wild | ScopeKind::None => {}
        }
        if matches!(meta.scope_kind, ScopeKind::Path | ScopeKind::Host) {
            assert!(
                !cap.scope.is_wildcard(),
                "{} must not hold a global scope, got {}",
                cap.verb.as_str(),
                cap.scope
            );
        }
    }
}

/// Every catalog verb carries one explicit decision, and that decision
/// agrees with the scope kind the catalog declares. A verb added to the
/// catalog without a row here falls through to
/// [`Baseline::Denied`] — this test names it so the omission is
/// deliberate rather than silent.
#[test]
fn every_catalog_verb_has_an_explicit_baseline_decision() {
    for meta in crate::caps::catalog::CATALOG {
        let decisions = BASELINE
            .iter()
            .filter(|(verb, _)| *verb == meta.verb)
            .count();
        assert_eq!(
            decisions,
            1,
            "{} needs exactly one explicit baseline decision",
            meta.verb.as_str()
        );
    }
    assert_eq!(
        BASELINE.len(),
        crate::caps::catalog::CATALOG.len(),
        "the baseline table describes verbs the catalog does not"
    );

    for (verb, baseline) in BASELINE {
        let meta = crate::caps::catalog::lookup(*verb).expect("catalog entry");
        let agrees = matches!(
            (*baseline, meta.scope_kind),
            (Baseline::Denied, _)
                | (Baseline::OwnerPaths, ScopeKind::Path)
                | (Baseline::Names(_), ScopeKind::Name)
                | (Baseline::AnyName, ScopeKind::Name)
                | (Baseline::SelfScoped, ScopeKind::SelfRef)
                | (Baseline::Resourceless, ScopeKind::None)
        );
        assert!(
            agrees,
            "{} is {:?} but the catalog declares {:?}",
            verb.as_str(),
            baseline,
            meta.scope_kind
        );
        if let Baseline::Names(names) = baseline {
            assert!(!names.is_empty(), "{} lists no names", verb.as_str());
            for name in *names {
                assert!(
                    !name.contains('*'),
                    "{} must name exact resources, got {name}",
                    verb.as_str()
                );
            }
        }
    }
}

/// The derivation fails closed on both axes: a verb the table denies,
/// and a verb whose catalog scope kind stopped agreeing with its
/// decision, produce no scope rather than an untyped wildcard.
#[test]
fn baseline_scopes_fail_closed_on_denial_and_kind_mismatch() {
    let _lock = lock_env();
    let owner = owner();
    let roots = owner_path_roots(owner.uid, &owner.home);

    assert!(baseline_scopes(Verb::SECRET_READ, ScopeKind::Name, &roots).is_empty());
    assert!(baseline_scopes(Verb::FS_EXEC, ScopeKind::Path, &roots).is_empty());
    assert!(baseline_scopes(Verb::FS_READ, ScopeKind::Name, &roots).is_empty());
    assert!(baseline_scopes(Verb::SYS_OBSERVE, ScopeKind::Path, &roots).is_empty());
    assert!(baseline_scopes(Verb::AI_CHAT, ScopeKind::Path, &roots).is_empty());
    assert!(baseline_scopes(Verb::MEMORY_READ, ScopeKind::None, &roots).is_empty());
    assert!(baseline_scopes(Verb::UI_NOTIFY, ScopeKind::Name, &roots).is_empty());
}

// -----------------------------------------------------------------
// Trusted-session clamp
// -----------------------------------------------------------------

/// A stored capability set is authority the daemon wrote earlier, not
/// authority it must honour now: the clamp is an intersection, so a
/// session row carrying something broader cannot inject it into a
/// worker, and one carrying something narrower is not widened.
#[test]
fn clamp_to_owner_baseline_only_ever_narrows() {
    let _lock = lock_env();
    let owner = owner();
    let in_home = owner.home.join("notes.md").to_string_lossy().into_owned();

    let mut stored = CapSet::new();
    stored.insert(Cap::new(Verb::FS_READ, Scope::path("/**")));
    stored.insert(Cap::new(Verb::FS_READ, Scope::Wild));
    stored.insert(Cap::new(
        Verb::FS_READ,
        Scope::path(format!("{}/**", owner.home.display())),
    ));
    stored.insert(Cap::new(Verb::SECRET_READ, Scope::name("**")));
    stored.insert(Cap::new(Verb::PROC_SPAWN, Scope::Wild));
    stored.insert(Cap::new(Verb::NET_DIAL, Scope::host("**")));

    let clamped = clamp_to_owner_baseline(&stored, owner.uid, &owner.home);
    assert!(clamped.covers(&Cap::new(Verb::FS_READ, Scope::path(&in_home))));
    assert!(!clamped.covers(&Cap::new(Verb::FS_READ, Scope::path("/etc/shadow"))));
    assert!(!clamped.covers(&Cap::new(Verb::SECRET_READ, Scope::name("default/TOKEN"))));
    assert!(!clamped.covers(&Cap::new(Verb::PROC_SPAWN, Scope::Wild)));
    assert!(!clamped.covers(&Cap::new(Verb::NET_DIAL, Scope::host("example.com"))));

    // Narrower than policy stays narrower: the clamp is not a refresh.
    let mut narrow = CapSet::new();
    narrow.insert(Cap::new(Verb::FS_READ, Scope::path(&in_home)));
    let clamped = clamp_to_owner_baseline(&narrow, owner.uid, &owner.home);
    assert!(clamped.covers(&Cap::new(Verb::FS_READ, Scope::path(&in_home))));
    assert!(!clamped.covers(&Cap::new(
        Verb::FS_READ,
        Scope::path(owner.home.join("other.md").to_string_lossy().into_owned())
    )));
    assert!(!clamped.covers(&Cap::new(Verb::AI_CHAT, Scope::name("claude-sonnet-4"))));
}

// -----------------------------------------------------------------
// Unregistered launcher ceiling (unchanged policy)
// -----------------------------------------------------------------

#[test]
fn local_launcher_ceiling_is_an_unprivileged_home_bounded_policy() {
    let home = Path::new("/home/test");
    let caps = local_launcher_ceiling(home);
    assert!(caps.covers(&Cap::new(Verb::AGENT_INVOKE, Scope::name("pkg"))));
    assert!(caps.covers(&Cap::new(
        Verb::FS_READ,
        Scope::path("/home/test/notes.txt")
    )));
    assert!(!caps.covers(&Cap::new(Verb::FS_READ, Scope::path("/etc/shadow"))));
    assert!(!caps.covers(&Cap::new(Verb::FS_READ, Scope::path("/**"))));

    for meta in crate::caps::catalog::CATALOG {
        let held = caps.verbs().contains(&meta.verb);
        let expected = meta.risk <= Risk::Medium && !LOCAL_LAUNCH_DENIED_VERBS.contains(&meta.verb);
        assert_eq!(
            held,
            expected,
            "unexpected unregistered-launcher policy for {} ({:?})",
            meta.verb.as_str(),
            meta.risk
        );
    }
    for verb in LOCAL_LAUNCH_DENIED_VERBS {
        assert!(
            !caps.verbs().contains(verb),
            "{} must never reach an unregistered launcher",
            verb.as_str()
        );
    }
}
