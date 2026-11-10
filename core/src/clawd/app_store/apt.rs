//! Official package delivery uses APT; snapshots are not an installation fallback.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::approvals::system_review::{self as reviews, DeliveryPackage, DeliveryReview};
use crate::apps::{debian, store};
use crate::provenance::runtime::PackageRef;

use super::{apps_root, root, secure_directory, MutationFence, Prepared};
use crate::clawd::client_identity::ClientIdentity;
use crate::clawd::protocol::BrokerError;

const HOOK: &str = "/usr/lib/cos/bin/claw-app-apt-hook";
const TIMEOUT: Duration = Duration::from_secs(15 * 60);
static TRANSACTION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Plan {
    pub digest: String,
    status_sha256: String,
    packages: Vec<Package>,
    pub reviews: Vec<AppReview>,
    pub affected_apps: Vec<String>,
    pub hook_consumed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Package {
    file: String,
    identity: DeliveryPackage,
    size: u64,
    sha256: String,
    inventory: Option<store::AptPackage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AppReview {
    pub app_id: String,
    pub review_id: String,
    source: PathBuf,
    package: PackageRef,
}

#[derive(Clone)]
struct HookContext {
    pid: u32,
    start: u64,
}

fn hooks() -> &'static Mutex<BTreeMap<String, HookContext>> {
    static HOOKS: OnceLock<Mutex<BTreeMap<String, HookContext>>> = OnceLock::new();
    HOOKS.get_or_init(Default::default)
}

fn directory(id: &str) -> Result<PathBuf, String> {
    super::request_path(id)?;
    Ok(root().join("staging").join(id))
}

fn verify_source(review: &AppReview) -> Result<crate::provenance::VerifiedPackage, String> {
    let package = crate::provenance::verify::verify_package(
        &review.source,
        &crate::provenance::VerifyOptions::new(crate::provenance::PackageKind::App),
        &crate::provenance::trust_store(),
    )
    .map_err(|error| error.to_string())?;
    if PackageRef::of(&package) != review.package {
        return Err("Prepared App bytes or publisher changed after delivery review".into());
    }
    Ok(package)
}

async fn run(
    program: &'static str,
    args: Vec<String>,
    commit: Option<String>,
    input: Option<Vec<u8>>,
) -> Result<crate::worker::exec::WorkerOutput, String> {
    tokio::task::spawn_blocking(move || {
        let mut registry = if commit.is_some() {
            Some(
                hooks()
                    .lock()
                    .map_err(|_| "APT hook registry is unavailable")?,
            )
        } else {
            None
        };
        let mut command = std::process::Command::new(program);
        command
            .args(&args)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .env("HOME", "/root")
            .env("DEBIAN_FRONTEND", "noninteractive")
            .env("APT_LISTCHANGES_FRONTEND", "none")
            .env("LC_ALL", "C.UTF-8")
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        if let Some(id) = &commit {
            command.env("CLAW_APP_STORE_REQUEST", id);
            command.env(
                crate::clawd::config::SOCKET_ENV,
                crate::clawd::config::socket_path(),
            );
        }
        let parent = std::process::id() as libc::pid_t;
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0
                    || libc::getppid() != parent
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().map_err(|error| error.to_string())?;
        let pid = child.id();
        if let Some(id) = &commit {
            let Some(start) = crate::proc::read_start_time_ticks_pub(pid) else {
                let _ = child.kill();
                let _ = child.wait();
                return Err("APT child kernel identity is unavailable".into());
            };
            registry
                .as_mut()
                .unwrap()
                .insert(id.clone(), HookContext { pid, start });
        }
        drop(registry);
        let writer = input.map(|bytes| {
            let mut stdin = child.stdin.take().expect("prepared package stdin");
            std::thread::spawn(move || stdin.write_all(&bytes))
        });
        let stdout = child.stdout.take().ok_or("APT stdout is unavailable")?;
        let stderr = child.stderr.take().ok_or("APT stderr is unavailable")?;
        let output =
            std::thread::spawn(move || crate::worker::exec::read_bounded(stdout, 4 * 1024 * 1024));
        let errors =
            std::thread::spawn(move || crate::worker::exec::read_bounded(stderr, 4 * 1024 * 1024));
        let waited = crate::worker::exec::wait_bounded(&mut child, Some(TIMEOUT), || unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        });
        if waited.is_err() {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(id) = &commit {
            hooks()
                .lock()
                .map_err(|_| "APT hook registry is unavailable")?
                .remove(id);
        }
        let (stdout, stdout_truncated) = output.join().map_err(|_| "APT output reader failed")?;
        let (stderr, stderr_truncated) = errors.join().map_err(|_| "APT error reader failed")?;
        if let Some(writer) = writer {
            writer
                .join()
                .map_err(|_| "APT request writer failed")?
                .map_err(|error| error.to_string())?;
        }
        let (status, timed_out) = waited?;
        if timed_out || stdout_truncated || stderr_truncated || !status.success() {
            return Err(format!(
                "OS package command did not complete successfully (exit {:?})",
                status.code()
            ));
        }
        Ok(crate::worker::exec::WorkerOutput {
            status,
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
            timed_out,
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

fn arguments(cache: &Path, selected: &store::AptPackage, install: bool) -> Vec<String> {
    let mut args = vec![
        "-o".into(),
        format!("Dir::Cache::archives={}", cache.display()),
        "-o".into(),
        "APT::Sandbox::User=root".into(),
        "-o".into(),
        "APT::Get::AllowUnauthenticated=false".into(),
        "-o".into(),
        "Acquire::AllowInsecureRepositories=false".into(),
        "-o".into(),
        "Acquire::AllowDowngradeToInsecureRepositories=false".into(),
        "install".into(),
        "--yes".into(),
        "--no-remove".into(),
        "--no-install-recommends".into(),
        "--reinstall".into(),
        if install {
            "--no-download"
        } else {
            "--download-only"
        }
        .into(),
    ];
    if install {
        args.splice(
            0..0,
            ["-o".into(), format!("DPkg::Pre-Install-Pkgs::=exec {HOOK}")],
        );
    }
    args.push("--".into());
    args.push(if selected.architecture == "all" {
        format!("{}={}", selected.package, selected.version)
    } else {
        format!(
            "{}:{}={}",
            selected.package, selected.architecture, selected.version
        )
    });
    args
}

async fn identity(path: &Path) -> Result<DeliveryPackage, String> {
    let output = run(
        "/usr/bin/dpkg-deb",
        vec![
            "--field".into(),
            path.to_string_lossy().into_owned(),
            "Package".into(),
            "Version".into(),
            "Architecture".into(),
        ],
        None,
        None,
    )
    .await?;
    let text = std::str::from_utf8(&output.stdout).map_err(|_| "Invalid Debian identity text")?;
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(&format!("{name}: ")))
            .map(str::to_string)
            .ok_or_else(|| format!("Debian package has no {name} field"))
    };
    let identity = DeliveryPackage {
        package: field("Package")?,
        version: field("Version")?,
        architecture: field("Architecture")?,
    };
    crate::clawd::packages::validate_package_name(&identity.package)?;
    crate::update::debver::Version::parse(&identity.version)?;
    if !matches!(identity.architecture.as_str(), "all" | "amd64" | "arm64") {
        return Err("Unsupported Debian package architecture".into());
    }
    Ok(identity)
}

fn status_digest() -> Result<String, String> {
    debian::file_binding(Path::new("/var/lib/dpkg/status")).map(|(_, digest)| digest)
}

fn review_space(root: &Path) -> Result<u64, String> {
    use std::os::unix::ffi::OsStrExt;
    let path =
        std::ffi::CString::new(root.as_os_str().as_bytes()).map_err(|error| error.to_string())?;
    let mut information = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), information.as_mut_ptr()) } != 0 {
        return Err("OS package staging space is unavailable".into());
    }
    let information = unsafe { information.assume_init() };
    let available = information.f_bavail.saturating_mul(information.f_frsize);
    let budget = (available.saturating_sub(64 * 1024 * 1024) / 2).min(2 * 1024 * 1024 * 1024);
    if budget == 0 {
        return Err("Insufficient space for authenticated review and package installation".into());
    }
    Ok(budget)
}

fn download_budget(text: &str) -> Result<(), String> {
    let mut total = 0_u64;
    let mut count = 0;
    for line in text.lines().filter(|line| line.starts_with('\'')) {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 4 || !fields[0].ends_with('\'') {
            return Err("APT download-size plan has an unsupported shape".into());
        }
        let size = fields[2]
            .parse::<u64>()
            .map_err(|_| "APT omitted a concrete archive size")?;
        total = total
            .checked_add(size)
            .ok_or("APT download-size overflow")?;
        count += 1;
    }
    if count == 0 || count > 512 || total == 0 || total > 1024 * 1024 * 1024 {
        return Err("APT dependencies exceed the verified transaction download limit".into());
    }
    Ok(())
}

async fn authenticated_sources(packages: &[Package]) -> Result<Vec<bool>, String> {
    let input = packages
        .iter()
        .map(|package| {
            json!({
                "package": package.identity.package,
                "version": package.identity.version,
                "architecture": package.identity.architecture,
                "sha256": package.sha256,
            })
        })
        .collect::<Vec<_>>();
    let output = run(
        "/usr/bin/python3",
        vec!["-I".into(), "/usr/lib/cos/app-apt-metadata.py".into()],
        None,
        Some(serde_json::to_vec(&input).map_err(|error| error.to_string())?),
    )
    .await?;
    let metadata: Vec<Value> = serde_json::from_slice(&output.stdout)
        .map_err(|_| "Authenticated APT metadata has an invalid shape")?;
    if metadata.len() != input.len() {
        return Err("Authenticated APT metadata omits a prepared package".into());
    }
    input
        .iter()
        .zip(metadata)
        .map(|(expected, actual)| {
            if ["package", "version", "architecture", "sha256"]
                .iter()
                .any(|field| actual.get(field) != expected.get(field))
                || !actual.get("app_source").is_some_and(Value::is_boolean)
            {
                return Err(
                    "Authenticated APT metadata differs from the concrete package plan".into(),
                );
            }
            actual
                .get("os_source")
                .and_then(Value::as_bool)
                .ok_or_else(|| "APT metadata omits its authenticated source class".into())
        })
        .collect()
}

pub(super) async fn prepare(
    owner: u32,
    id: &str,
    record: &store::Record,
    catalog: &store::Catalog,
) -> Result<Plan, String> {
    let _transaction = TRANSACTION
        .try_lock()
        .map_err(|_| "Another App package transaction is active")?;
    let selected = catalog.apt_package(record)?;
    let root = directory(id)?;
    secure_directory(&root, 0o755)?;
    let cache = root.join("archives");
    secure_directory(&cache, 0o700)?;
    secure_directory(&cache.join("partial"), 0o700)?;
    let audit = run("/usr/bin/dpkg", vec!["--audit".into()], None, None).await?;
    if !audit.stdout.is_empty() {
        return Err("Repair the incomplete OS package transaction before installing Apps".into());
    }
    let before = status_digest()?;
    let mut planning = arguments(&cache, selected, false);
    planning.insert(0, "--print-uris".into());
    let planned = run("/usr/bin/apt-get", planning, None, None).await?;
    download_budget(&planned.stdout_string())?;
    run(
        "/usr/bin/apt-get",
        arguments(&cache, selected, false),
        None,
        None,
    )
    .await?;
    if status_digest()? != before {
        return Err("Installed packages changed during preparation; prepare the App again".into());
    }
    let mut files = std::fs::read_dir(&cache)
        .map_err(|error| error.to_string())?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    files.retain(|path| path.extension().is_some_and(|extension| extension == "deb"));
    files.sort();
    if files.is_empty() || files.len() > 512 {
        return Err("APT did not prepare a bounded concrete package transaction".into());
    }
    let mut packages = Vec::new();
    let mut apps = Vec::new();
    let mut all_apps = false;
    let mut identities = BTreeSet::new();
    let mut found = false;
    let inspected = root.join("inspect");
    secure_directory(&inspected, 0o755)?;
    let mut expansion_budget = review_space(&inspected)?;
    for path in &files {
        let identity = identity(path).await?;
        let (size, sha256) = debian::file_binding(path)?;
        packages.push(Package {
            file: path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("Invalid APT archive name")?
                .into(),
            identity,
            size,
            sha256,
            inventory: None,
        });
    }
    let trusted_os = authenticated_sources(&packages).await?;
    if packages
        .iter()
        .try_fold(0_u64, |total, package| total.checked_add(package.size))
        .is_none_or(|total| total > 1024 * 1024 * 1024)
    {
        return Err("APT changed or exceeded its bounded archive plan".into());
    }
    for (index, path) in files.iter().enumerate() {
        let identity = packages[index].identity.clone();
        if !identities.insert((identity.package.clone(), identity.architecture.clone())) {
            return Err("APT prepared duplicate package identities".into());
        }
        if identity.package.starts_with("claw-os-") {
            return Err(
                "Update the OS Agent/services separately before installing this App".into(),
            );
        }
        let official = catalog.packages.iter().find(|candidate| {
            candidate.package == identity.package
                && candidate.version == identity.version
                && candidate.architecture == identity.architecture
        });
        if official.is_none()
            && catalog
                .packages
                .iter()
                .any(|candidate| candidate.package == identity.package)
        {
            return Err(
                "An App dependency is absent from the authenticated package inventory".into(),
            );
        }
        if official.is_none() && !trusted_os[index] {
            return Err(
                "An App-source dependency is absent from the authenticated Store inventory".into(),
            );
        }
        if let Some(official) = official {
            packages[index].inventory = Some(official.clone());
            let destination = inspected.join(index.to_string());
            let path = path.clone();
            let expected = official.clone();
            let extract = destination.clone();
            let budget = expansion_budget;
            let expanded = tokio::task::spawn_blocking(move || {
                debian::inspect(&path, &expected, &extract, budget)
            })
            .await
            .map_err(|error| error.to_string())??;
            expansion_budget = expansion_budget
                .checked_sub(expanded)
                .ok_or("App payloads exceed the shared review expansion budget")?;
            let discovery = crate::apps::discover_all(&destination.join("usr/lib/cos/apps"));
            if !discovery.quarantined.is_empty()
                || discovery.verified.keys().cloned().collect::<Vec<_>>() != official.apps
            {
                return Err(
                    "Debian App identities or signatures differ from the authenticated inventory"
                        .into(),
                );
            }
            debian::validate_exports(&destination, &discovery.verified)?;
            for (_, app) in discovery.verified {
                let package = app.require_verified()?;
                if package.audit_facts().get("trust").and_then(Value::as_str) != Some("publisher") {
                    return Err("Official Debian Apps require ordinary publisher provenance".into());
                }
                if app.manifest.id == record.app_id {
                    record.verify_package(package)?;
                    if official.sha256 != selected.sha256 {
                        return Err("Selected App belongs to a different Debian package".into());
                    }
                    found = true;
                }
                apps.push((
                    app.manifest.id.clone(),
                    app.dir.clone(),
                    PackageRef::of(package),
                ));
            }
            all_apps |= official.apps.is_empty();
        } else {
            all_apps = true;
        }
    }
    if !found {
        return Err("APT did not download the authenticated selected App package".into());
    }
    let digest = store::sha256(
        &serde_json::to_vec(&(&before, &packages)).map_err(|error| error.to_string())?,
    );
    let delivery = DeliveryReview {
        digest: digest.clone(),
        packages: packages
            .iter()
            .map(|package| package.identity.clone())
            .collect(),
    };
    let mut reviews = Vec::new();
    for (app_id, source, package_ref) in apps {
        let package = crate::provenance::verify::verify_package(
            &source,
            &crate::provenance::VerifyOptions::new(crate::provenance::PackageKind::App),
            &crate::provenance::trust_store(),
        )
        .map_err(|error| error.to_string())?;
        let review = reviews::submit_delivery(
            owner,
            &package,
            format!("Official APT Store install {id}"),
            delivery.clone(),
        )?;
        reviews.push(AppReview {
            app_id,
            review_id: review.id,
            source,
            package: package_ref,
        });
    }
    let mut affected: BTreeSet<String> =
        reviews.iter().map(|review| review.app_id.clone()).collect();
    if all_apps {
        affected.extend(crate::apps::discover_all(&apps_root()).verified.into_keys());
        affected.extend(
            crate::apps::discover_all(&apps_root())
                .quarantined
                .into_keys(),
        );
    }
    Ok(Plan {
        digest,
        status_sha256: before,
        packages,
        reviews,
        affected_apps: affected.into_iter().collect(),
        hook_consumed: false,
    })
}

pub(super) async fn publish(
    state: &crate::clawd::state::DaemonState,
    pending: &Prepared,
    catalog: &store::Catalog,
) -> Result<Value, String> {
    let _transaction = TRANSACTION
        .try_lock()
        .map_err(|_| "Another App package transaction is active")?;
    let plan = pending
        .apt
        .as_ref()
        .ok_or("Prepare a current authenticated APT installation first")?;
    let selected = catalog.apt_package(&pending.record)?;
    for package in &plan.packages {
        if package
            .inventory
            .as_ref()
            .is_some_and(|expected| !catalog.packages.contains(expected))
        {
            return Err(
                "An App package changed or disappeared from the authenticated catalog".into(),
            );
        }
    }
    let mut fences: Vec<MutationFence> = Vec::new();
    for app in &plan.affected_apps {
        let fence = std::sync::Arc::new(crate::clawd::app_mutation::fence(app).await?);
        super::retire_installed(state, app, fence.clone()).await?;
        fences.push(fence);
    }
    if status_digest()? != plan.status_sha256 {
        return Err(
            "Installed package state changed after review; prepare a new delivery plan".into(),
        );
    }
    let result = run(
        "/usr/bin/apt-get",
        arguments(
            &directory(&pending.request_id)?.join("archives"),
            selected,
            true,
        ),
        Some(pending.request_id.clone()),
        None,
    )
    .await;
    let current = super::prepared(&pending.request_id, pending.owner_uid)?;
    let consumed = current.apt.as_ref().is_some_and(|plan| plan.hook_consumed);
    if result.is_err() || !consumed {
        return Err(format!(
            "APT installation is not confirmed; checked recovery is required: {}",
            result
                .err()
                .unwrap_or_else(|| "the protected pre-install hook did not run".into())
        ));
    }
    let app = verify_installed(&current).await?;
    drop(fences);
    Ok(app)
}

pub(super) async fn verify_installed(pending: &Prepared) -> Result<Value, String> {
    let plan = pending
        .apt
        .as_ref()
        .ok_or("Missing protected APT installation plan")?;
    if !plan.hook_consumed {
        return Err("No protected APT dispatch receipt exists for this request".into());
    }
    for package in &plan.packages {
        let selected = if package.identity.architecture == "all" {
            package.identity.package.clone()
        } else {
            format!(
                "{}:{}",
                package.identity.package, package.identity.architecture
            )
        };
        let output = run(
            "/usr/bin/dpkg-query",
            vec![
                "--show".into(),
                "--showformat=${Status}\\t${Version}\\t${Architecture}".into(),
                "--".into(),
                selected,
            ],
            None,
            None,
        )
        .await?;
        if output.stdout_string()
            != format!(
                "install ok installed\t{}\t{}",
                package.identity.version, package.identity.architecture
            )
        {
            return Err("APT did not confirm the exact reviewed installed package state".into());
        }
    }
    for review in &plan.reviews {
        let app = crate::clawd::app_sessions::installed_app(&review.app_id)?;
        if PackageRef::of(app.require_verified()?) != review.package {
            return Err("Installed App bytes differ from the protected delivery review".into());
        }
    }
    let app = crate::clawd::app_sessions::installed_app(&pending.record.app_id)?;
    pending.record.verify_package(app.require_verified()?)?;
    store::web_metadata(app.require_verified()?)
}

pub async fn hook(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    if client.require_uid().map_err(BrokerError::authorization)? != 0 {
        return Err(BrokerError::authorization(
            "APT hooks require the protected OS helper",
        ));
    }
    let id = crate::clawd::permissions::required_string(&params, "request_id")?;
    let files: Vec<String> =
        serde_json::from_value(params.get("files").cloned().unwrap_or(Value::Null))
            .map_err(|_| BrokerError::authorization("Invalid APT archive list"))?;
    if files.is_empty() || files.len() > 512 {
        return Err(BrokerError::authorization(
            "APT archive list exceeds its bound",
        ));
    }
    let context = hooks()
        .lock()
        .map_err(|_| BrokerError::unavailable("APT hook registry is unavailable"))?
        .get(&id)
        .cloned()
        .ok_or_else(|| BrokerError::authorization("No owned APT transaction matches this hook"))?;
    let pid = client
        .pid
        .ok_or_else(|| BrokerError::authorization("APT helper pid is unavailable"))?;
    let status = std::fs::read_to_string(format!("/proc/{pid}/status"))
        .map_err(|error| BrokerError::authorization(error.to_string()))?;
    let parent = status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:"))
        .and_then(|value| value.trim().parse::<u32>().ok());
    let executable = std::fs::read_link(format!("/proc/{pid}/exe"))
        .map_err(|error| BrokerError::authorization(error.to_string()))?;
    if parent != Some(context.pid)
        || crate::proc::read_start_time_ticks_pub(context.pid) != Some(context.start)
        || executable != Path::new(HOOK)
    {
        return Err(BrokerError::authorization(
            "APT helper is not the tracked protected transaction child",
        ));
    }
    let mut pending: Prepared =
        super::read_json(&super::request_path(&id).map_err(BrokerError::authorization)?)
            .map_err(BrokerError::authorization)?;
    let plan = pending
        .apt
        .as_ref()
        .ok_or_else(|| BrokerError::authorization("Missing APT plan"))?;
    if pending.phase != "committing"
        || plan.hook_consumed
        || status_digest().map_err(BrokerError::authorization)? != plan.status_sha256
    {
        return Err(BrokerError::authorization(
            "APT state or protected delivery phase changed",
        ));
    }
    let trusted_os = authenticated_sources(&plan.packages)
        .await
        .map_err(BrokerError::authorization)?;
    if plan
        .packages
        .iter()
        .zip(trusted_os)
        .any(|(package, trusted)| package.inventory.is_none() && !trusted)
    {
        return Err(BrokerError::authorization(
            "APT dependency no longer has authenticated OS package ownership",
        ));
    }
    let archives = directory(&id)
        .map_err(BrokerError::authorization)?
        .join("archives");
    let actual: BTreeSet<_> = files
        .iter()
        .map(|file| PathBuf::from(file).canonicalize())
        .collect::<Result<_, _>>()
        .map_err(|error| BrokerError::authorization(error.to_string()))?;
    let expected: BTreeSet<_> = plan
        .packages
        .iter()
        .map(|package| archives.join(&package.file))
        .collect();
    if actual.len() != files.len() || actual != expected {
        return Err(BrokerError::authorization(
            "APT changed the reviewed package transaction",
        ));
    }
    for package in &plan.packages {
        if debian::file_binding(&archives.join(&package.file))
            .map_err(BrokerError::authorization)?
            != (package.size, package.sha256.clone())
        {
            return Err(BrokerError::authorization(
                "APT archive changed after owner review",
            ));
        }
    }
    for review in &plan.reviews {
        let package = verify_source(review).map_err(BrokerError::authorization)?;
        reviews::consume_delivery(pending.owner_uid, &review.review_id, &package, &plan.digest)
            .map_err(BrokerError::authorization)?;
    }
    pending.apt.as_mut().unwrap().hook_consumed = true;
    pending.phase = "installing".into();
    super::write_json(
        &super::request_path(&id).map_err(BrokerError::indeterminate)?,
        &pending,
    )
    .map_err(BrokerError::indeterminate)?;
    Ok(json!({"proceed": true}))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/app_store/apt.rs"
    ));
}
