// Process-wide test utilities. cfg(test)-only.
//
// Several modules' tests mutate global env vars (`COS_DATA_DIR`,
// `COS_SESSION`, etc.). cargo runs all tests in the same binary on
// a thread pool, so each test module owning its *own* `Mutex<()>`
// is not enough — two modules can race. Anything that touches
// env vars in tests must take this single shared lock.

use std::sync::{Mutex, MutexGuard};
use std::{ffi::OsString, path::Path};

use crate::caps::{Cap, Role, Scope, Verb};
use crate::proc::SessionInfo;

static ENV_LOCK: Mutex<()> = Mutex::new(());

pub(crate) fn lock_env() -> MutexGuard<'static, ()> {
    // Recover from a poisoned mutex so a single panicked test doesn't
    // cascade into N "PoisonError" failures that obscure the real cause.
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Point App-session tests at the current build's real runner with only
/// debug symbols removed. Cargo's unstripped test binary can exceed the
/// production runtime snapshot limit; stripping a cached copy preserves
/// the executable bytes under test without weakening that limit.
pub(crate) fn use_stripped_app_runner() -> TestEnvVarGuard {
    static RUNNER: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    let runner = RUNNER.get_or_init(|| {
        let source = crate::bridge::app_runner_path()
            .canonicalize()
            .expect("resolve the current claw-app-runner");
        let dir = secure_scratch_dir("app-runner");
        let target = dir.join("claw-app-runner");
        std::fs::copy(&source, &target).expect("copy claw-app-runner for tests");
        let status = std::process::Command::new("strip")
            .arg(&target)
            .status()
            .expect("run strip for the test App runner");
        assert!(status.success(), "strip the test App runner");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
                .expect("make the test App runner executable");
        }
        target
    });
    TestEnvVarGuard::set("CLAW_APP_RUNNER_BIN", runner)
}

// ---------------------------------------------------------------------------
// Extension-provenance fixtures
//
// Every extension kind is authenticated before use, so a test that
// installs an App / Skill / MCP package has to produce a signed one.
// These helpers keep that to a single call: one process-wide test
// publisher key, signed into a per-process trust root that is owned by
// the test user and mode 0700. Core tests run with `--test-threads=1`,
// so a single shared store is safe.
// ---------------------------------------------------------------------------

/// The process-wide test publisher key.
/// A scratch directory with *secure ancestry*.
///
/// Production trust roots require every ancestor up to `/` to be
/// non-symlink, correctly owned and free of group/world write bits.
/// `/tmp` is world-writable, so a trust root under it is refused — and
/// must stay refused, because that is the property being tested
/// elsewhere. Test trust roots therefore live under the owner's home,
/// which satisfies the real rule instead of weakening it.
pub(crate) fn secure_scratch_dir(label: &str) -> std::path::PathBuf {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let base = home.join(".cache").join("cos-test-scratch");
    let dir = base.join(format!("{label}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).expect("create secure scratch dir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ =
            std::fs::set_permissions(home.join(".cache"), std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    dir
}

pub(crate) fn test_signing_key() -> &'static crate::provenance::sign::SigningKeyFile {
    static KEY: std::sync::OnceLock<crate::provenance::sign::SigningKeyFile> =
        std::sync::OnceLock::new();
    KEY.get_or_init(|| {
        crate::provenance::sign::SigningKeyFile::generate(Some("cos test key".to_string()))
            .expect("generate test signing key")
    })
}

/// Install the test publisher key as the process trust store. Idempotent.
/// Record the durable generation for a set of test trust roots.
///
/// Production refuses a domain that has trust files but no recorded
/// state — otherwise deleting one file would be the way to reinstate a
/// revoked key — so a fixture that writes trust files has to record it
/// too, exactly as every `cos provenance trust` command does.
pub(crate) fn record_trust_state(roots: &[crate::provenance::trust::TrustRootSpec]) {
    use std::collections::BTreeMap;

    let mut domains: BTreeMap<
        String,
        (
            crate::provenance::state::TrustDomain,
            std::path::PathBuf,
            Vec<std::path::PathBuf>,
        ),
    > = BTreeMap::new();
    for root in roots {
        let entry = domains
            .entry(root.domain.as_key())
            .or_insert_with(|| (root.domain, root.state_dir(), Vec::new()));
        entry.2.push(root.path.clone());
    }
    for (_, (domain, dir, paths)) in domains {
        let _ = std::fs::create_dir_all(&dir);
        crate::provenance::state::bump(&dir, domain, &paths)
            .expect("record the test trust generation");
    }
}

/// The shared test trust root, created once per process.
pub(crate) fn install_test_trust_root() -> std::path::PathBuf {
    static ROOT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let dir = secure_scratch_dir("trust");
        let key = test_signing_key();
        let mut body = key.trust_entry(&[
            crate::provenance::PackageKind::App,
            crate::provenance::PackageKind::Skill,
            crate::provenance::PackageKind::Mcp,
        ]);
        if let Some(object) = body.as_object_mut() {
            object
                .entry("revoked_packages")
                .or_insert_with(|| serde_json::json!([]));
        }
        let path = dir.join("test.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&body).unwrap())
            .expect("write test trust entry");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        dir
    })
    .clone()
}

fn test_trust_roots(root: &std::path::Path) -> Vec<crate::provenance::trust::TrustRootSpec> {
    vec![crate::provenance::trust::TrustRootSpec {
        path: root.to_path_buf(),
        tier: crate::provenance::TrustTier::User,
        allowed_uids: vec![crate::provenance::fsec::effective_uid()],
        domain: crate::provenance::state::TrustDomain::Owner(
            crate::provenance::fsec::effective_uid(),
        ),
    }]
}

pub(crate) fn install_test_trust() {
    let root = install_test_trust_root();
    #[cfg(unix)]
    {
        let roots = test_trust_roots(&root);
        // Record the domain's durable generation in the directory the
        // loader looks in — the *parent* of the root, as
        // `TrustRootSpec::state_dir` defines it. Without it the domain
        // fails closed on trust files with no recorded generation,
        // which is exactly what it should do.
        record_trust_state(&roots);
        let store = crate::provenance::TrustStore::load_roots(&roots);
        assert!(
            !store.is_empty(),
            "test trust root did not load: {:?}",
            store.diagnostics()
        );
        crate::provenance::set_trust_store_for_roots(store, roots);
    }
    #[cfg(not(unix))]
    {
        let _ = root;
    }
}

/// Sign `dir` as a package of `kind`/`id` with the test publisher key
/// and make sure the key is trusted for this process.
///
/// Every regular file in the tree is declared as an entrypoint and a
/// resource so tests can exercise launch and disclosure paths without
/// listing files by hand.
pub(crate) fn sign_test_package(
    dir: &std::path::Path,
    kind: crate::provenance::PackageKind,
    id: &str,
) {
    install_test_trust();
    normalise_modes(dir);
    let files = collect_relative_files(dir, dir);
    let manifest_path = kind.manifest_file().to_string();
    let request = crate::provenance::sign::SignRequest {
        kind,
        id: id.to_string(),
        version: "0.0.0-test".to_string(),
        manifest_schema: "test".to_string(),
        manifest_path,
        entrypoints: files.clone(),
        resources: files,
    };
    crate::provenance::sign::sign_directory(dir, &request, test_signing_key())
        .unwrap_or_else(|e| panic!("sign test package {}: {e}", dir.display()));
}

/// Sign `dir` declaring only `entrypoints`.
///
/// The blanket helper declares every file, which is convenient but
/// makes "is this file a declared entrypoint?" untestable. This one
/// lets a test ship a signed file that was deliberately *not* declared.
pub(crate) fn sign_test_package_with_entrypoints(
    dir: &std::path::Path,
    kind: crate::provenance::PackageKind,
    id: &str,
    entrypoints: &[&str],
) {
    install_test_trust();
    normalise_modes(dir);
    let files = collect_relative_files(dir, dir);
    let request = crate::provenance::sign::SignRequest {
        kind,
        id: id.to_string(),
        version: "0.0.0-test".to_string(),
        manifest_schema: "test".to_string(),
        manifest_path: kind.manifest_file().to_string(),
        entrypoints: entrypoints.iter().map(|e| (*e).to_string()).collect(),
        resources: files,
    };
    crate::provenance::sign::sign_directory(dir, &request, test_signing_key())
        .unwrap_or_else(|e| panic!("sign test package {}: {e}", dir.display()));
}

/// Revoke one artifact digest in the process trust store.
///
/// Writes it into the same root `install_test_trust` created and
/// re-records the domain's generation, exactly as
/// `cos provenance trust revoke` does.
#[allow(dead_code)]
pub(crate) fn revoke_test_package(content_digest: &str) {
    #[cfg(unix)]
    {
        let root = install_test_trust_root();
        let path = root.join("test.json");
        let mut body: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read test trust"))
                .expect("parse test trust");
        body["revoked_packages"] = serde_json::json!([content_digest]);
        std::fs::write(&path, serde_json::to_vec_pretty(&body).unwrap()).expect("write test trust");
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        let roots = test_trust_roots(&root);
        record_trust_state(&roots);
        let store = crate::provenance::TrustStore::load_roots(&roots);
        crate::provenance::set_trust_store_for_roots(store, roots);
    }
    #[cfg(not(unix))]
    {
        let _ = content_digest;
    }
}

/// Clear every revocation the process trust store carries.
///
/// [`revoke_test_package`] writes into the one shared root, and the
/// root is created once per process — so a test that revokes an
/// artifact would otherwise leave it revoked for every test that runs
/// after it.
#[allow(dead_code)]
pub(crate) fn clear_test_revocations() {
    #[cfg(unix)]
    {
        let root = install_test_trust_root();
        let path = root.join("test.json");
        let mut body: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read test trust"))
                .expect("parse test trust");
        body["revoked_packages"] = serde_json::json!([]);
        std::fs::write(&path, serde_json::to_vec_pretty(&body).unwrap()).expect("write test trust");
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        let roots = test_trust_roots(&root);
        record_trust_state(&roots);
        let store = crate::provenance::TrustStore::load_roots(&roots);
        crate::provenance::set_trust_store_for_roots(store, roots);
        crate::provenance::verify::invalidate_cache();
    }
}

/// Sign `dir` as an App package and bind it for launch.
///
/// The launch path takes one verified snapshot, so a test that runs an
/// App has to produce a real signed package like any caller would.
pub(crate) fn app_launch(dir: &std::path::Path, id: &str) -> crate::bridge::AppLaunch {
    sign_test_package(dir, crate::provenance::PackageKind::App, id);
    let trust = crate::provenance::trust_store();
    let options =
        crate::provenance::VerifyOptions::new(crate::provenance::PackageKind::App).expect_id(id);
    let package = crate::provenance::verify::verify_package(dir, &options, &trust)
        .unwrap_or_else(|e| panic!("verify test app {id}: {e}"));
    crate::bridge::AppLaunch::new(std::sync::Arc::new(package))
        .unwrap_or_else(|e| panic!("bind test app {id}: {e}"))
}

/// Try the same, returning the error instead of panicking.
pub(crate) fn try_app_launch(
    dir: &std::path::Path,
    id: &str,
) -> Result<crate::bridge::AppLaunch, String> {
    let trust = crate::provenance::trust_store();
    let options =
        crate::provenance::VerifyOptions::new(crate::provenance::PackageKind::App).expect_id(id);
    let package = crate::provenance::verify::verify_package(dir, &options, &trust)
        .map_err(|e| e.to_string())?;
    crate::bridge::AppLaunch::new(std::sync::Arc::new(package))
}

fn collect_relative_files(root: &std::path::Path, dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            out.extend(collect_relative_files(root, &path));
        } else if meta.is_file() {
            if let Ok(rel) = path.strip_prefix(root) {
                let rel = rel.to_string_lossy().replace('\\', "/");
                if rel != crate::provenance::envelope::ENVELOPE_FILE {
                    out.push(rel);
                }
            }
        }
    }
    out.sort();
    out
}

/// Strip group/world write bits so a fixture written with a permissive
/// umask still signs and verifies.
fn normalise_modes(dir: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let _ = std::fs::metadata(dir).map(|m| {
            std::fs::set_permissions(
                dir,
                std::fs::Permissions::from_mode(m.permissions().mode() & !0o022),
            )
        });
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            let _ = std::fs::set_permissions(
                &path,
                std::fs::Permissions::from_mode(meta.permissions().mode() & !0o022),
            );
            if meta.is_dir() {
                normalise_modes(&path);
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

/// RAII guard that sets `COS_PERMS_MODE=permissive` while held and
/// restores the previous value (including "unset") on drop. Use this
/// in tool/runtime tests that do not bootstrap a real session but
/// still call into capability-gated code paths (`ai.chat`,
/// `sys.kernel`, …). The cap layer treats permissive mode as
/// "allow-all + audit"; that is exactly what these tests want.
pub(crate) struct PermissiveModeGuard {
    prev: Option<std::ffi::OsString>,
}

impl PermissiveModeGuard {
    pub(crate) fn new() -> Self {
        let prev = std::env::var_os("COS_PERMS_MODE");
        std::env::set_var("COS_PERMS_MODE", "permissive");
        Self { prev }
    }
}

impl Drop for PermissiveModeGuard {
    fn drop(&mut self) {
        match self.prev.take() {
            Some(v) => std::env::set_var("COS_PERMS_MODE", v),
            None => std::env::remove_var("COS_PERMS_MODE"),
        }
    }
}

pub(crate) struct TestSessionGuard {
    session_id: String,
    previous_session: Option<OsString>,
    previous_proc_dir: Option<OsString>,
}

pub(crate) struct TestEnvVarGuard {
    name: &'static str,
    previous: Option<OsString>,
}

impl TestEnvVarGuard {
    pub(crate) fn set(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(name);
        std::env::set_var(name, value);
        Self { name, previous }
    }

    pub(crate) fn remove(name: &'static str) -> Self {
        let previous = std::env::var_os(name);
        std::env::remove_var(name);
        Self { name, previous }
    }
}

impl Drop for TestEnvVarGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

impl TestSessionGuard {
    pub(crate) fn admin(proc_dir: &Path) -> Self {
        Self::admin_with_caps(proc_dir, std::iter::empty())
    }

    pub(crate) fn admin_with_caps(
        proc_dir: &Path,
        extra_caps: impl IntoIterator<Item = Cap>,
    ) -> Self {
        let previous_session = std::env::var_os("COS_SESSION");
        let previous_proc_dir = std::env::var_os("COS_PROC_DATA_DIR");
        std::env::set_var("COS_PROC_DATA_DIR", proc_dir);

        let session_id = format!("test-parent-{}", uuid::Uuid::new_v4().simple());
        let role = Role::Admin;
        let mut caps =
            role.caps_with_scopes(Some(Scope::Wild), Some(Scope::Wild), Some(Scope::Wild));
        caps.insert(Cap::new(Verb::SYS_KERNEL, Scope::Wild));
        caps.extend(extra_caps);
        crate::proc::register_session(SessionInfo {
            session_id: session_id.clone(),
            pid: std::process::id(),
            command: vec!["cargo test".to_string()],
            started_at: chrono::Utc::now().to_rfc3339(),
            stdout_path: String::new(),
            stderr_path: String::new(),
            group: Some("test".to_string()),
            parent: None,
            workdir: std::env::current_dir()
                .ok()
                .map(|path| path.to_string_lossy().into_owned()),
            exit_code: None,
            ended_at: None,
            tier: Some(role.credential_tier()),
            scope: Some("test".to_string()),
            priority: None,
            caps: Some(caps),
            transient_caps: None,
            role: Some(role.name().to_string()),
            app_id: None,
            pending_bind: false,
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
            client: crate::session::SessionClient::default(),
        })
        .expect("register test parent session");
        std::env::set_var("COS_SESSION", &session_id);

        Self {
            session_id,
            previous_session,
            previous_proc_dir,
        }
    }
}

impl Drop for TestSessionGuard {
    fn drop(&mut self) {
        crate::proc::deregister_session(&self.session_id);
        match self.previous_session.take() {
            Some(value) => std::env::set_var("COS_SESSION", value),
            None => std::env::remove_var("COS_SESSION"),
        }
        match self.previous_proc_dir.take() {
            Some(value) => std::env::set_var("COS_PROC_DATA_DIR", value),
            None => std::env::remove_var("COS_PROC_DATA_DIR"),
        }
    }
}
