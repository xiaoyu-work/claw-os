use super::*;
use crate::caps::CapSet;
use crate::clawd::authority::{
    self, Audience, AudienceSet, Binding, Issuance, Issuer, Presentation, Principal, Requirement,
    Subject, Uses,
};
use crate::session::journal::{self, harness::Harness, Partition};
use std::os::unix::fs::PermissionsExt;

struct Fixture {
    harness: Harness,
    path: PathBuf,
    session: String,
}

impl Fixture {
    fn new(content: Option<&[u8]>) -> Self {
        let harness = Harness::new();
        let _lease = harness.lease();
        authority::authority().clear_for_test();
        let parent = harness.data_dir().join("files");
        std::fs::create_dir(&parent).unwrap();
        let path = parent.join("document.txt");
        if let Some(content) = content {
            std::fs::write(&path, content).unwrap();
        }
        Self {
            harness,
            path,
            session: crate::session::SessionId::generate().into_string(),
        }
    }

    fn decision(&self, caps: Vec<Cap>) -> Decision {
        authority::authority().clear_for_test();
        let principal =
            Principal::of_process(self.harness.owner_uid(), std::process::id()).unwrap();
        let (_, view) = authority::authority()
            .issue(Issuance {
                issuer: Issuer::AppSessionAuthority,
                principal,
                binding: Binding::ProcessTree,
                subject: Subject::session(&self.session)
                    .with_app(Some("generic-editor".to_string())),
                audience: AudienceSet::one(Audience::SystemService),
                caps: CapSet::from_caps(caps),
                lifetime: std::time::Duration::from_secs(300),
                uses: Uses::Unbounded,
                index_session: true,
            })
            .unwrap();
        Decision::for_test(
            view,
            Command::SystemFileReplace.as_str(),
            Audience::SystemService,
            Presentation {
                uid: self.harness.owner_uid(),
                pid: std::process::id(),
                start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
                audience: Audience::SystemService,
                route: Command::SystemFileReplace.as_str(),
                session_id: Some(self.session.clone()),
            },
            None,
            &Requirement::RouteDerived,
        )
    }

    fn allowed(&self) -> Decision {
        self.decision(requirements(&self.path).to_vec())
    }

    fn expected(&self) -> FileState {
        Snapshot::read(&mut File::open(&self.path).unwrap())
            .unwrap()
            .state
    }

    fn params(&self, expected: Option<&FileState>, content: &[u8]) -> Value {
        json!({
            "session": self.session,
            "path": self.path,
            "expected": expected,
            "content_base64": base64::engine::general_purpose::STANDARD.encode(content),
        })
    }

    fn assert_no_stages(&self) {
        for entry in std::fs::read_dir(self.path.parent().unwrap()).unwrap() {
            assert!(!entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".cos-replace-"));
        }
    }
}

#[tokio::test]
async fn file_replace_requires_both_exact_caps_even_for_a_root_peer() {
    let fixture = Fixture::new(Some(b"before"));
    let expected = fixture.expected();
    for caps in [
        vec![],
        vec![requirements(&fixture.path)[0].clone()],
        vec![requirements(&fixture.path)[1].clone()],
        requirements(&fixture.path.with_file_name("other")).to_vec(),
    ] {
        let decision = fixture.decision(caps);
        let error = replace(fixture.params(Some(&expected), b"after"), &decision)
            .await
            .unwrap_err();
        assert_eq!(error.kind, BrokerErrorKind::Unauthorized);
        assert!(!authority::obligation_met(Some(&decision)));
        assert_eq!(std::fs::read(&fixture.path).unwrap(), b"before");
        fixture.assert_no_stages();
    }
    let decision = fixture.allowed();
    let proof = decision.require_all(&requirements(&fixture.path)).unwrap();
    assert_eq!(proof.spent(), requirements(&fixture.path));
    assert!(proof
        .spent()
        .iter()
        .all(|cap| cap.scope == Scope::path(fixture.path.to_string_lossy())));
    let mounts = crate::worker::derive::granted_path_mounts(decision.caps()).unwrap();
    assert_eq!(mounts.len(), 1);
    assert_eq!(mounts[0].source, fixture.path);
    assert_ne!(mounts[0].source, fixture.path.parent().unwrap());
}

#[tokio::test]
async fn file_replace_relay_uses_the_same_live_session_decision() {
    for writable in [false, true] {
        let fixture = Fixture::new(Some(b"before"));
        let expected = fixture.expected();
        let caps = if writable {
            requirements(&fixture.path).to_vec()
        } else {
            vec![requirements(&fixture.path)[0].clone()]
        };
        let _session_decision = fixture.decision(caps);
        let (relay, _) = authority::authority()
            .issue(Issuance {
                issuer: Issuer::AppSessionAuthority,
                principal: Principal::of_process(fixture.harness.owner_uid(), std::process::id())
                    .unwrap(),
                binding: Binding::Process,
                subject: Subject::session(&fixture.session),
                audience: AudienceSet::one(Audience::AppRelay),
                caps: CapSet::from_caps([]),
                lifetime: std::time::Duration::from_secs(300),
                uses: Uses::Unbounded,
                index_session: false,
            })
            .unwrap();
        let peer = ClientIdentity {
            uid: Some(fixture.harness.owner_uid()),
            gid: Some(unsafe { libc::getegid() }),
            pid: Some(std::process::id()),
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
        };
        let route = Command::SystemFileReplace.route();
        let params = fixture.params(Some(&expected), b"after");
        let decision = authority::authorize_relayed(
            &relay.into_wire(),
            &fixture.session,
            route.name,
            &route.authority,
            &params,
            &peer,
        )
        .await
        .unwrap()
        .unwrap();
        let result = replace(params, &decision).await;
        if writable {
            assert_eq!(result.unwrap()["changed"], true);
            assert_eq!(std::fs::read(&fixture.path).unwrap(), b"after");
        } else {
            assert_eq!(result.unwrap_err().kind, BrokerErrorKind::Unauthorized);
            assert_eq!(std::fs::read(&fixture.path).unwrap(), b"before");
        }
        assert_eq!(authority::obligation_met(Some(&decision)), writable);
    }
}

#[tokio::test]
async fn file_replace_serializes_competing_proposals_against_the_same_baseline() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    let fixture = Fixture::new(Some(b"before"));
    let expected = fixture.expected();
    let decision = fixture.allowed();
    let lock = REPLACEMENTS.lock().await;
    let mut first = Box::pin(replace(
        fixture.params(Some(&expected), b"first proposal"),
        &decision,
    ));
    let mut second = Box::pin(replace(
        fixture.params(Some(&expected), b"second proposal"),
        &decision,
    ));
    {
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(first.as_mut().poll(&mut context), Poll::Pending));
        assert!(matches!(second.as_mut().poll(&mut context), Poll::Pending));
    }
    assert!(!authority::obligation_met(Some(&decision)));
    assert_eq!(std::fs::read(&fixture.path).unwrap(), b"before");
    fixture.assert_no_stages();
    drop(lock);

    let (first, second) = tokio::join!(first, second);
    assert_eq!(first.unwrap()["changed"], true);
    let rejected = second.unwrap_err();
    assert_eq!(rejected.audit_class, Some("file_replace_conflict"));
    assert_eq!(std::fs::read(&fixture.path).unwrap(), b"first proposal");
    fixture.assert_no_stages();
}

#[tokio::test]
async fn file_replace_root_relay_keeps_both_unknown_brackets_unresolved() {
    struct RuntimeDirectory(Option<std::ffi::OsString>);
    impl Drop for RuntimeDirectory {
        fn drop(&mut self) {
            match self.0.take() {
                Some(previous) => std::env::set_var("COS_PROVENANCE_RUNTIME_DIR", previous),
                None => std::env::remove_var("COS_PROVENANCE_RUNTIME_DIR"),
            }
        }
    }

    for existing in [false, true] {
        let fixture = Fixture::new(existing.then_some(b"before".as_slice()));
        let expected = existing.then(|| fixture.expected());
        let _session_decision = fixture.allowed();
        let _runtime_directory = RuntimeDirectory(std::env::var_os("COS_PROVENANCE_RUNTIME_DIR"));
        std::env::set_var(
            "COS_PROVENANCE_RUNTIME_DIR",
            fixture.harness.data_dir().join("running-instances"),
        );
        crate::provenance::runtime::register_operator_mcp(
            fixture.harness.owner_uid(),
            &fixture.session,
        );
        crate::provenance::runtime::assert_live_instance_now(
            fixture.harness.owner_uid(),
            &fixture.session,
        )
        .unwrap();

        let (relay, _) = authority::authority()
            .issue(Issuance {
                issuer: Issuer::AppSessionAuthority,
                principal: Principal::of_process(fixture.harness.owner_uid(), std::process::id())
                    .unwrap(),
                binding: Binding::Process,
                subject: Subject::session(&fixture.session),
                audience: AudienceSet::one(Audience::AppRelay),
                caps: CapSet::from_caps([]),
                lifetime: std::time::Duration::from_secs(300),
                uses: Uses::Unbounded,
                index_session: false,
            })
            .unwrap();
        let peer = ClientIdentity {
            uid: Some(fixture.harness.owner_uid()),
            gid: Some(unsafe { libc::getegid() }),
            pid: Some(std::process::id()),
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
        };
        let route = Command::AppSessionRelay.route();
        route.authorize(&peer).unwrap();
        let params = (route.decode)(json!({
            "session_id": fixture.session,
            "handle": relay.into_wire(),
            "command": Command::SystemFileReplace.as_str(),
            "params": fixture.params(expected.as_ref(), b"after"),
        }))
        .unwrap();
        let decision = authority::authorize(route.name, &route.authority, &params, &peer)
            .await
            .unwrap()
            .unwrap();
        let state = crate::clawd::state::DaemonState::new().unwrap();
        let id = RequestId::parse("root-relay-replacement").unwrap();
        let guard = crate::clawd::journal::begin(route, &id, Some(&decision), &peer)
            .unwrap()
            .unwrap();

        faults::arm_parent_sync();
        let error = (route.handler)(crate::clawd::routes::RouteCall {
            state: &state,
            client: &peer,
            params,
            authority: Some(&decision),
        })
        .await
        .unwrap_err();
        assert_eq!(error.kind, BrokerErrorKind::Indeterminate);
        assert!(authority::obligation_met(Some(&decision)));
        let response = route.errors.response(id.clone(), error);
        assert_eq!(response.error.as_ref().unwrap().code, "indeterminate");
        let response = crate::clawd::journal::finish(guard, &id, &response).unwrap();
        assert_eq!(response.error.as_ref().unwrap().code, "indeterminate");
        assert_eq!(std::fs::read(&fixture.path).unwrap(), b"after");

        let partition = Partition::Session(fixture.session.parse().unwrap());
        let view = journal::projection::build(&partition, fixture.harness.owner_uid()).unwrap();
        assert_eq!(view.mutations.len(), 2);
        for mutation in &view.mutations {
            assert_eq!(mutation.status, "indeterminate", "{}", mutation.route);
        }
        for _ in 0..2 {
            assert_eq!(
                crate::clawd::journal::begin(route, &id, Some(&decision), &peer).unwrap_err(),
                crate::clawd::wire::Fault::DuplicateRequest,
            );
            assert_eq!(
                journal::unresolved_mutations(&partition, fixture.harness.owner_uid())
                    .unwrap()
                    .len(),
                2,
            );
            fixture.harness.cold_restart();
            journal::startup_recovery(journal::RecoverySource::DaemonStart).unwrap();
        }
        fixture.assert_no_stages();
    }
}

#[tokio::test]
async fn file_replace_rejects_each_hash_or_stat_mismatch_without_effect() {
    let fixture = Fixture::new(Some(b"before"));
    let expected = fixture.expected();
    let decision = fixture.allowed();
    for field in [
        "sha256",
        "size",
        "device",
        "inode",
        "mode",
        "modified_ns",
        "changed_ns",
    ] {
        let mut params = fixture.params(Some(&expected), b"after");
        params["expected"][field] = if field == "sha256" {
            json!(sha256(b"not before"))
        } else {
            json!(params["expected"][field].as_i64().unwrap() + 1)
        };
        let error = replace(params, &decision).await.unwrap_err();
        assert_eq!(
            error.audit_class,
            Some("file_replace_conflict"),
            "{field}: {error}"
        );
        assert_eq!(std::fs::read(&fixture.path).unwrap(), b"before");
        fixture.assert_no_stages();
    }
}

#[tokio::test]
async fn file_replace_absent_and_existing_preconditions_are_not_interchangeable() {
    let fixture = Fixture::new(Some(b"before"));
    let expected = fixture.expected();
    let decision = fixture.allowed();
    assert!(replace(fixture.params(None, b"after"), &decision)
        .await
        .is_err());
    std::fs::remove_file(&fixture.path).unwrap();
    let error = replace(fixture.params(Some(&expected), b"after"), &decision)
        .await
        .unwrap_err();
    assert_eq!(error.audit_class, Some("file_replace_conflict"));
    assert!(!fixture.path.exists());
    fixture.assert_no_stages();
}

#[tokio::test]
async fn file_replace_refuses_symlinks_hardlinks_special_files_and_special_modes() {
    let fixture = Fixture::new(Some(b"before"));
    let expected = fixture.expected();
    let decision = fixture.allowed();
    let other = fixture.path.with_file_name("referent");
    std::fs::rename(&fixture.path, &other).unwrap();
    std::os::unix::fs::symlink(&other, &fixture.path).unwrap();
    let error = replace(fixture.params(Some(&expected), b"after"), &decision)
        .await
        .unwrap_err();
    assert_eq!(error.audit_class, Some("file_replace_unsupported"));
    assert_eq!(std::fs::read(&other).unwrap(), b"before");
    std::fs::remove_file(&fixture.path).unwrap();
    std::fs::hard_link(&other, &fixture.path).unwrap();
    assert_eq!(
        replace(fixture.params(Some(&expected), b"after"), &decision)
            .await
            .unwrap_err()
            .audit_class,
        Some("file_replace_unsupported")
    );
    std::fs::remove_file(&fixture.path).unwrap();
    std::fs::create_dir(&fixture.path).unwrap();
    assert!(
        replace(fixture.params(Some(&expected), b"after"), &decision)
            .await
            .is_err()
    );
    std::fs::remove_dir(&fixture.path).unwrap();
    let name = CString::new(fixture.path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert_eq!(
        replace(fixture.params(Some(&expected), b"after"), &decision)
            .await
            .unwrap_err()
            .audit_class,
        Some("file_replace_unsupported")
    );
    std::fs::remove_file(&fixture.path).unwrap();
    std::fs::rename(&other, &fixture.path).unwrap();
    for bit in [0o4000, 0o2000, 0o1000] {
        std::fs::set_permissions(&fixture.path, std::fs::Permissions::from_mode(0o600 | bit))
            .unwrap();
        assert_eq!(
            replace(fixture.params(Some(&expected), b"after"), &decision)
                .await
                .unwrap_err()
                .audit_class,
            Some("file_replace_unsupported")
        );
    }
    fixture.assert_no_stages();
}

#[tokio::test]
async fn file_replace_refuses_extended_attributes_instead_of_stripping_them() {
    let fixture = Fixture::new(Some(b"before"));
    let expected = fixture.expected();
    let file = File::open(&fixture.path).unwrap();
    let name = CString::new("user.cos-file-replace-test").unwrap();
    let set = unsafe {
        libc::fsetxattr(
            file.as_raw_fd(),
            name.as_ptr(),
            b"value".as_ptr().cast(),
            5,
            0,
        )
    };
    if set != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EOPNOTSUPP) {
        eprintln!(
            "SKIP xattr regression: this filesystem does not support user extended attributes"
        );
        return;
    }
    assert_eq!(set, 0, "{}", std::io::Error::last_os_error());
    let decision = fixture.allowed();
    let error = replace(fixture.params(Some(&expected), b"after"), &decision)
        .await
        .unwrap_err();
    assert_eq!(error.audit_class, Some("file_replace_unsupported"));
    assert_eq!(std::fs::read(&fixture.path).unwrap(), b"before");
    assert!(unsafe { libc::flistxattr(file.as_raw_fd(), std::ptr::null_mut(), 0) } > 0);
}

#[tokio::test]
async fn file_replace_unchanged_is_a_noop_and_changed_preserves_host_metadata() {
    let fixture = Fixture::new(Some(b"before"));
    let file = File::open(&fixture.path).unwrap();
    if fixture.harness.owner_uid() == 0 {
        assert_eq!(unsafe { libc::fchown(file.as_raw_fd(), 65534, 65534) }, 0);
    }
    assert_eq!(unsafe { libc::fchmod(file.as_raw_fd(), 0o640) }, 0);
    let original = file.metadata().unwrap();
    let expected = fixture.expected();
    let decision = fixture.allowed();
    let unchanged = replace(fixture.params(Some(&expected), b"before"), &decision)
        .await
        .unwrap();
    assert_eq!(unchanged["changed"], false);
    assert!(same_stat(
        &original,
        &std::fs::metadata(&fixture.path).unwrap()
    ));
    let changed = replace(fixture.params(Some(&expected), b"after"), &decision)
        .await
        .unwrap();
    assert_eq!(
        changed,
        json!({"path": fixture.path, "bytes": 5, "sha256": sha256(b"after"), "changed": true})
    );
    let after = std::fs::metadata(&fixture.path).unwrap();
    assert_eq!(
        (after.uid(), after.gid(), after.mode()),
        (original.uid(), original.gid(), original.mode())
    );
    assert_ne!(after.ino(), original.ino());
    assert_eq!(std::fs::read(&fixture.path).unwrap(), b"after");
    fixture.assert_no_stages();
}

#[tokio::test]
async fn file_replace_new_file_is_owner_private_and_accepts_the_full_64k_bound() {
    let fixture = Fixture::new(None);
    let decision = fixture.allowed();
    let content = vec![0xff; MAX_FILE_BYTES];
    let result = replace(fixture.params(None, &content), &decision)
        .await
        .unwrap();
    assert_eq!(result["changed"], true);
    assert_eq!(std::fs::read(&fixture.path).unwrap(), content);
    let metadata = std::fs::metadata(&fixture.path).unwrap();
    let owner = Owner::resolve(decision.owner_uid()).unwrap();
    assert_eq!(
        (metadata.uid(), metadata.gid(), metadata.mode() & 0o7777),
        (owner.uid, owner.gid, 0o600)
    );
    fixture.assert_no_stages();
}

#[tokio::test]
async fn file_replace_root_provider_creates_files_for_the_authenticated_nonroot_owner() {
    use std::os::unix::process::CommandExt;

    if unsafe { libc::geteuid() } != 0 {
        eprintln!("SKIP cross-owner creation regression: run this test as root");
        return;
    }
    let owner = match Owner::resolve(1000) {
        Ok(owner) => owner,
        Err(_) => {
            eprintln!("SKIP cross-owner creation regression: uid 1000 needs a verified home");
            return;
        }
    };
    struct Peer(std::process::Child);
    impl Drop for Peer {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let fixture = Fixture::new(None);
    let mut command = std::process::Command::new("/usr/bin/sleep");
    command.arg("60");
    let (uid, gid) = (owner.uid, owner.gid);
    unsafe {
        command.pre_exec(move || {
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setgid(gid) != 0
                || libc::setuid(uid) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let peer = Peer(command.spawn().unwrap());
    let pid = peer.0.id();
    let (_, view) = authority::authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(owner.uid, pid).unwrap(),
            binding: Binding::Process,
            subject: Subject::session(&fixture.session),
            audience: AudienceSet::one(Audience::SystemService),
            caps: CapSet::from_caps(requirements(&fixture.path)),
            lifetime: std::time::Duration::from_secs(60),
            uses: Uses::Unbounded,
            index_session: true,
        })
        .unwrap();
    let decision = Decision::for_test(
        view,
        Command::SystemFileReplace.as_str(),
        Audience::SystemService,
        Presentation {
            uid: owner.uid,
            pid,
            start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
            audience: Audience::SystemService,
            route: Command::SystemFileReplace.as_str(),
            session_id: Some(fixture.session.clone()),
        },
        None,
        &Requirement::RouteDerived,
    );
    replace(fixture.params(None, b"owner content"), &decision)
        .await
        .unwrap();
    let metadata = std::fs::metadata(&fixture.path).unwrap();
    assert_eq!(
        (metadata.uid(), metadata.gid(), metadata.mode() & 0o777),
        (owner.uid, owner.gid, 0o600)
    );
    assert_ne!(metadata.uid(), unsafe { libc::geteuid() });
}

#[tokio::test]
async fn file_replace_content_and_paths_are_validated_before_authority_or_effects() {
    let fixture = Fixture::new(None);
    let decision = fixture.allowed();
    for content in [vec![0; MAX_FILE_BYTES + 1], vec![0; MAX_FILE_BYTES + 2000]] {
        assert!(replace(fixture.params(None, &content), &decision)
            .await
            .is_err());
        assert!(!authority::obligation_met(Some(&decision)));
        assert!(!fixture.path.exists());
    }
    let mut params = fixture.params(None, b"");
    params["content_base64"] = json!("not base64!");
    assert!(replace(params, &decision).await.is_err());
    for path in [
        "/",
        "relative/file",
        "/srv/../secret",
        "/srv/./file",
        "/srv/*.txt",
        "/srv/[ab]",
        "/srv/x>y",
        "/srv/x\n",
        "/srv//file",
        "/srv/file/",
    ] {
        let mut params = fixture.params(None, b"after");
        params["path"] = json!(path);
        assert!(replace(params, &decision).await.is_err(), "{path}");
    }
    fixture.assert_no_stages();
}

#[tokio::test]
async fn file_replace_rechecks_content_and_absence_after_staging() {
    for existing in [true, false] {
        let fixture = Fixture::new(existing.then_some(b"before".as_slice()));
        let expected = existing.then(|| fixture.expected());
        let decision = fixture.allowed();
        let path = fixture.path.clone();
        faults::on_after_stage(move || std::fs::write(path, b"intervening writer").unwrap());
        let error = replace(fixture.params(expected.as_ref(), b"after"), &decision)
            .await
            .unwrap_err();
        assert_eq!(error.audit_class, Some("file_replace_conflict"));
        assert_eq!(std::fs::read(&fixture.path).unwrap(), b"intervening writer");
        fixture.assert_no_stages();
    }
}

#[tokio::test]
async fn file_replace_refuses_an_existing_file_removed_after_staging() {
    let fixture = Fixture::new(Some(b"before"));
    let expected = fixture.expected();
    let decision = fixture.allowed();
    let path = fixture.path.clone();
    faults::on_after_stage(move || std::fs::remove_file(path).unwrap());
    assert!(
        replace(fixture.params(Some(&expected), b"after"), &decision)
            .await
            .is_err()
    );
    assert!(!fixture.path.exists());
    fixture.assert_no_stages();
}

#[tokio::test]
async fn file_replace_no_replace_commit_never_overwrites_an_intervening_create() {
    let fixture = Fixture::new(None);
    let decision = fixture.allowed();
    let path = fixture.path.clone();
    faults::on_before_commit(move || std::fs::write(path, b"winner").unwrap());
    let error = replace(fixture.params(None, b"after"), &decision)
        .await
        .unwrap_err();
    assert_eq!(error.kind, BrokerErrorKind::Execution);
    assert_eq!(error.audit_class, Some("file_replace_conflict"));
    assert_eq!(std::fs::read(&fixture.path).unwrap(), b"winner");
    fixture.assert_no_stages();
}

#[tokio::test]
async fn file_replace_final_name_race_does_not_follow_a_substituted_symlink() {
    let fixture = Fixture::new(Some(b"before"));
    let expected = fixture.expected();
    let decision = fixture.allowed();
    let referent = fixture.path.with_file_name("not-granted");
    std::fs::write(&referent, b"private referent").unwrap();
    let path = fixture.path.clone();
    let other = referent.clone();
    faults::on_before_commit(move || {
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(other, path).unwrap();
    });
    // This is the documented final-check/commit race, not a CAS promise.
    // Rename acts on the directory entry; it never follows the new link.
    assert!(
        replace(fixture.params(Some(&expected), b"after"), &decision)
            .await
            .is_ok()
    );
    assert_eq!(std::fs::read(&fixture.path).unwrap(), b"after");
    assert_eq!(std::fs::read(&referent).unwrap(), b"private referent");
}

#[tokio::test]
async fn file_replace_pins_the_parent_and_refuses_ancestor_redirection() {
    let fixture = Fixture::new(Some(b"before"));
    let expected = fixture.expected();
    let decision = fixture.allowed();
    let parent = fixture.path.parent().unwrap().to_path_buf();
    let old = parent.with_file_name("pinned");
    let other = parent.with_file_name("other");
    std::fs::create_dir(&other).unwrap();
    std::fs::write(other.join("document.txt"), b"other").unwrap();
    let saved = old.clone();
    let redirected = other.clone();
    faults::on_after_stage(move || {
        std::fs::rename(&parent, &saved).unwrap();
        std::os::unix::fs::symlink(&redirected, &parent).unwrap();
    });
    assert!(
        replace(fixture.params(Some(&expected), b"after"), &decision)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(old.join("document.txt")).unwrap(), b"before");
    assert_eq!(std::fs::read(other.join("document.txt")).unwrap(), b"other");
    assert_eq!(
        std::fs::read_dir(&old).unwrap().count(),
        1,
        "cleanup uses the pinned directory"
    );
}

#[tokio::test]
async fn file_replace_cleanup_never_unlinks_a_substituted_stage() {
    let fixture = Fixture::new(Some(b"before"));
    let expected = fixture.expected();
    let decision = fixture.allowed();
    let parent = fixture.path.parent().unwrap().to_path_buf();
    faults::on_after_stage(move || {
        let entry = std::fs::read_dir(&parent)
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".cos-replace-")
            })
            .unwrap();
        std::fs::rename(entry.path(), parent.join("moved-stage")).unwrap();
        std::fs::write(entry.path(), b"foreign entry").unwrap();
    });
    assert!(
        replace(fixture.params(Some(&expected), b"after"), &decision)
            .await
            .is_err()
    );
    let foreign = std::fs::read_dir(fixture.path.parent().unwrap())
        .unwrap()
        .map(Result::unwrap)
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".cos-replace-")
        })
        .unwrap();
    assert_eq!(std::fs::read(foreign.path()).unwrap(), b"foreign entry");
    assert_eq!(std::fs::read(&fixture.path).unwrap(), b"before");
}

#[tokio::test]
async fn file_replace_post_commit_sync_failure_stays_unresolved_and_refuses_replay() {
    for existing in [true, false] {
        let fixture = Fixture::new(existing.then_some(b"before".as_slice()));
        let expected = existing.then(|| fixture.expected());
        let decision = fixture.allowed();
        let params = fixture.params(expected.as_ref(), b"after");
        faults::arm_parent_sync();
        let error = replace(params.clone(), &decision).await.unwrap_err();
        assert_eq!(error.kind, BrokerErrorKind::Indeterminate);
        assert_eq!(std::fs::read(&fixture.path).unwrap(), b"after");
        let inode = std::fs::metadata(&fixture.path).unwrap().ino();
        let partition = Partition::Session(fixture.session.parse().unwrap());
        let open = journal::unresolved_mutations(&partition, fixture.harness.owner_uid()).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].route, "system.file.replace");
        assert!(open[0].flagged);
        for _ in 0..2 {
            let retry = replace(params.clone(), &decision).await.unwrap_err();
            assert_eq!(retry.audit_class, Some("duplicate_request"));
            assert_eq!(std::fs::metadata(&fixture.path).unwrap().ino(), inode);
            fixture.harness.cold_restart();
            journal::startup_recovery(journal::RecoverySource::DaemonStart).unwrap();
        }
        fixture.assert_no_stages();
    }
}

#[tokio::test]
async fn file_replace_a_failed_journal_start_never_creates_a_stage_or_target() {
    let fixture = Fixture::new(None);
    let decision = fixture.allowed();
    // Take the authority spend first so its audit append cannot consume the
    // injected start fault; pin/apply below follows the same bracket seam.
    let owner = Owner::resolve(decision.owner_uid()).unwrap();
    let target = Target::pin(fixture.path.to_str().unwrap(), &owner.home).unwrap();
    let _proof = decision.require_all(&requirements(&target.path)).unwrap();
    journal::faults::arm(journal::faults::Fault::AppendWrite);
    let start = crate::clawd::journal::begin(
        Command::SystemFileReplace.route(),
        &RequestId::generate(),
        Some(&decision),
        &ClientIdentity::unknown(),
    );
    journal::faults::disarm();
    assert!(start.is_err());
    assert!(!fixture.path.exists());
    fixture.assert_no_stages();
}

#[test]
fn file_replace_owner_protection_uses_the_submitting_home_and_canonical_aliases() {
    let fixture = Fixture::new(None);
    let home = fixture.harness.data_dir().join("submitting-owner");
    let private = fixture.harness.data_dir().join("private-alias");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&private).unwrap();
    std::os::unix::fs::symlink(&private, home.join(".ssh")).unwrap();
    assert!(Target::pin(home.join(".ssh/key").to_str().unwrap(), &home).is_err());
    assert!(Target::pin(private.join("key").to_str().unwrap(), &home).is_err());
    for path in [
        "/proc/self/mem",
        "/etc/shadow",
        "/etc/ssh/sshd_config",
        "/var/lib/cos/authority",
        "/root/credential",
    ] {
        assert_eq!(
            Target::pin(path, &home).err().unwrap().kind,
            BrokerErrorKind::Unauthorized
        );
    }
    assert!(Target::pin(fixture.path.to_str().unwrap(), &home).is_ok());
}

#[test]
fn file_replace_fault_hooks_are_compile_time_only() {
    let source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/clawd/file_changes.rs"
    ))
    .replace("\r\n", "\n");
    assert!(source.contains("#[cfg(test)]\n    mod faults"));
    assert_eq!(source.matches("faults::").count(), 3);
    for call in [
        "faults::after_stage()",
        "faults::before_commit()",
        "if faults::fail_parent_sync()",
    ] {
        let position = source.find(call).unwrap();
        assert!(source[position.saturating_sub(35)..position].contains("#[cfg(test)]"));
    }

    assert!(!source.contains("std::env::"));
}

#[test]
fn file_replace_stat_timestamps_cover_the_full_signed_wire_range() {
    assert_eq!(timestamp(-1, 999_999_999).unwrap(), -1);
    assert_eq!(timestamp(-9_223_372_037, 145_224_192).unwrap(), i64::MIN);
    assert_eq!(timestamp(9_223_372_036, 854_775_807).unwrap(), i64::MAX);
    assert!(timestamp(-9_223_372_037, 145_224_191).is_err());
    assert!(timestamp(9_223_372_036, 854_775_808).is_err());
}

#[test]
fn file_replace_canonical_parent_must_also_be_a_literal_utf8_scope() {
    use std::os::unix::ffi::OsStringExt;

    let fixture = Fixture::new(None);
    let owner = Owner::resolve(fixture.harness.owner_uid()).unwrap();
    for name in [
        std::ffi::OsString::from("ambiguous*directory"),
        std::ffi::OsString::from_vec(b"non-utf8-\xff".to_vec()),
    ] {
        let directory = fixture.harness.data_dir().join(name);
        std::fs::create_dir(&directory).unwrap();
        let alias = fixture.harness.data_dir().join("literal-alias");
        std::os::unix::fs::symlink(&directory, &alias).unwrap();
        let error = Target::pin(alias.join("document").to_str().unwrap(), &owner.home)
            .err()
            .unwrap();
        assert_eq!(error.audit_class, Some("file_replace_invalid"));
        std::fs::remove_file(alias).unwrap();
    }
}

mod process {
    use super::*;

    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/file_changes/process.rs"
    ));
}
