//! Root custody for ordinary App executions, independent of registry-row lifetime.

use std::collections::HashMap;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use claw_display_control::{identity::pidfd_exited, workload::SessionGroup, ProcessIdentity};

use super::client_identity::ClientIdentity;
use crate::extension_host::spawn::ContainmentRoot;

static ROOT: OnceLock<Result<Arc<ContainmentRoot>, String>> = OnceLock::new();
static JOBS: OnceLock<Mutex<HashMap<String, Arc<Job>>>> = OnceLock::new();

fn jobs() -> &'static Mutex<HashMap<String, Arc<Job>>> {
    JOBS.get_or_init(Default::default)
}

fn initialized_root() -> Result<&'static Arc<ContainmentRoot>, String> {
    ROOT.get().ok_or("Root ordinary App custody is unavailable")?
        .as_ref().map_err(Clone::clone)
}

struct Job {
    owner: u32,
    home: PathBuf,
    app: String,
    session: String,
    process: OwnedFd,
    launcher: ProcessIdentity,
    launch_handle: String,
    retiring: AtomicBool,
    bound: AtomicBool,
    providers: AtomicUsize,
    uncertain: AtomicBool,
    resources: Mutex<Resources>,
}

struct Resources {
    group: crate::worker::cgroup::Scope,
    controller: UnixStream,
    cleanup_sent: bool,
    cleanup_confirmed: bool,
}

pub(crate) fn initialize(broker: &crate::agentd::supervisor::BrokerContext) {
    let _ = ROOT.set(broker.extension_containment());
    if let Err(error) = std::thread::Builder::new().name("ordinary-app-custody".into()).spawn(|| loop {
        std::thread::sleep(Duration::from_millis(250));
        let retained: Vec<_> = match jobs().lock() {
            Ok(jobs) => jobs.values().cloned().collect(),
            Err(_) => return,
        };
        for job in retained {
            if !job.bound.load(Ordering::Acquire) { continue; }
            if job.retiring.load(Ordering::Acquire)
                || pidfd_exited(job.process.as_fd()).unwrap_or(false)
                || job.launcher.assert_current().is_err()
                || !authorized(&job)
            {
                if let Err(error) = retire(&job, Instant::now() + Duration::from_secs(5)) {
                    tracing::warn!(app = %job.app, session = %job.session, %error, "ordinary App cleanup remains pending");
                }
            }
        }
    }) {
        // A missing sweep never makes retained custody reusable.
        tracing::error!(%error, "ordinary App custody sweeper could not start");
    }
}

fn process(pid: u32) -> Result<(ProcessIdentity, OwnedFd), String> {
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if raw < 0 {
        return Err(format!("capture ordinary App pidfd: {}", std::io::Error::last_os_error()));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    let identity = ProcessIdentity::from_pidfd(fd.as_fd()).map_err(|error| error.to_string())?;
    Ok((identity, fd))
}

pub(crate) async fn bind(
    session: &str,
    app: &str,
    child: u32,
    socket: &Path,
    handle: &str,
    client: &ClientIdentity,
) -> Result<(), String> {
    let root = initialized_root()?;
    let launcher_pid = client.pid.ok_or("App launcher PID is unavailable")?;
    let (launcher, _) = process(launcher_pid)?;
    let execution_uid = client.execution_uid.unwrap_or(client.require_uid()?);
    if launcher.uid() != execution_uid || Some(launcher.start_ticks()) != client.start_time_ticks {
        return Err("ordinary App launcher identity changed".into());
    }
    let (identity, pidfd) = process(child)?;
    if identity.uid() != execution_uid {
        return Err("ordinary App gate has the wrong execution owner".into());
    }
    let expected = Path::new("/usr/local/bin/claw-app-runner");
    crate::provenance::fsec::require_secure_location(expected, &[0])
        .map_err(|error| error.to_string())?;
    use std::os::unix::fs::MetadataExt;
    let expected = std::fs::metadata(expected).map_err(|error| error.to_string())?;
    let actual = std::fs::metadata(format!("/proc/{child}/exe")).map_err(|error| error.to_string())?;
    if (expected.dev(), expected.ino()) != (actual.dev(), actual.ino()) {
        return Err("ordinary App must bind the protected pre-sandbox custody gate".into());
    }
    let controller = crate::worker::app_custody::connect(socket, launcher_pid, execution_uid).await?;
    launcher.assert_current().map_err(|error| error.to_string())?;
    let task_parent = client.extension_host.as_ref().filter(|host|
        host.purpose == crate::extension_host::protocol::HostPurpose::Task).map(|_| &launcher);
    let group = crate::worker::cgroup::create_app(root, session, &crate::worker::Limits::operation(), task_parent)?;
    let job = Arc::new(Job {
        owner: client.require_uid()?,
        home: client.require_home_dir()?,
        app: app.into(),
        session: session.into(),
        process: pidfd,
        launcher,
        launch_handle: handle.into(),
        retiring: AtomicBool::new(false),
        bound: AtomicBool::new(false),
        providers: AtomicUsize::new(0),
        uncertain: AtomicBool::new(false),
        resources: Mutex::new(Resources { group, controller, cleanup_sent: false, cleanup_confirmed: false }),
    });
    {
        let mut jobs = jobs().lock().map_err(|_| "ordinary App custody lock poisoned")?;
        if jobs.contains_key(session) || jobs.len() >= 256 {
            return Err("ordinary App custody is duplicate or at capacity".into());
        }

        // Retain every resource before moving the still-gated child.
        jobs.insert(session.into(), job.clone());
    }
    let attached = (|| {
        // Ptrace holds the gated task across numeric cgroup.procs attachment. A pidfd
        // alone does not prevent PID recycling between identity checks and that write.
        let mut stopped = BindingStop::capture(&identity)?;
        let current = std::fs::metadata(format!("/proc/{child}/exe")).map_err(|error| error.to_string())?;
        if (expected.dev(), expected.ino()) != (current.dev(), current.ino())
            || !claw_display_control::identity::read_bounded(
                &format!("/proc/{child}/task/{child}/children"), 8192,
            ).map_err(|error| error.to_string())?.trim().is_empty()
        {
            return Err("ordinary App bind gate changed or forked before Root custody".into());
        }
        let resources = job.resources.lock().map_err(|_| "ordinary App resource lock poisoned")?;
        resources.group.attach(child)?;
        identity.assert_current().map_err(|error| error.to_string())?;
        let actual = std::fs::File::from(SessionGroup::of(&identity).map_err(|error| error.to_string())?
            .open().map_err(|error| error.to_string())?).metadata().map_err(|error| error.to_string())?;
        let expected = std::fs::File::from(resources.group.duplicate_checked()?).metadata().map_err(|error| error.to_string())?;
        if (expected.dev(), expected.ino()) != (actual.dev(), actual.ino()) {
            return Err("ordinary App did not enter its Root-held cgroup".into());
        }
        stopped.release()
    })();
    if attached.is_err() {
        job.retiring.store(true, Ordering::Release);
    }
    job.bound.store(true, Ordering::Release);
    attached
}

fn authorized(job: &Job) -> bool {
    crate::paths::RoutedPathContext::for_owner(job.owner, job.home.clone())
        .scope_sync(|| authorized_for_owner(job))
}

fn authorized_for_owner(job: &Job) -> bool {
    let presentation = super::authority::Presentation::new(
        job.owner, job.launcher.pid() as u32, Some(job.launcher.start_ticks()),
        super::authority::Audience::AppLaunch, "app_session",
    );
    let Ok(grant) = super::authority::authority().resolve(&job.launch_handle, &presentation) else {
        return false;
    };
    grant.subject.session_id.as_deref() == Some(job.session.as_str())
        && grant.subject.app_id.as_deref() == Some(job.app.as_str())
        && grant.subject.activity.as_ref().is_none_or(|binding| binding.check().is_ok())
        && crate::provenance::runtime::assert_live_instance_now(job.owner, &job.session).is_ok()
}

struct BindingStop {
    pid: libc::pid_t,
    attached: bool,
}

impl BindingStop {
    fn capture(identity: &ProcessIdentity) -> Result<Self, String> {
        let pid = identity.pid();
        if unsafe { libc::ptrace(libc::PTRACE_SEIZE, pid, 0usize, libc::PTRACE_O_TRACEEXIT as usize) } != 0 {
            return Err(format!("Root cannot hold the ordinary App bind gate: {}", std::io::Error::last_os_error()));
        }
        let stopped = Self { pid, attached: true };
        if unsafe { libc::ptrace(libc::PTRACE_INTERRUPT, pid, 0usize, 0usize) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let mut status = 0;
            let waited = unsafe { libc::waitpid(pid, &mut status, libc::__WALL | libc::WNOHANG) };
            if waited == pid {
                if !libc::WIFSTOPPED(status) || status >> 16 == libc::PTRACE_EVENT_EXIT {
                    return Err("ordinary App bind gate exited before containment".into());
                }
                identity.assert_current().map_err(|error| error.to_string())?;
                return Ok(stopped);
            }
            if waited < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                return Err(std::io::Error::last_os_error().to_string());
            }
            if Instant::now() >= deadline {
                return Err("ordinary App bind gate did not stop".into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn release(&mut self) -> Result<(), String> {
        if self.attached {
            if unsafe { libc::ptrace(libc::PTRACE_DETACH, self.pid, 0usize, 0usize) } != 0 {
                return Err(format!("release contained App bind gate: {}", std::io::Error::last_os_error()));
            }
            self.attached = false;
        }
        Ok(())
    }
}

impl Drop for BindingStop {
    fn drop(&mut self) {
        if let Err(error) = self.release() {
            tracing::error!(%error, "ordinary App bind stop remains unconfirmed");
        }
    }
}

pub(crate) struct Provider {
    job: Arc<Job>,
    completed: bool,
}

impl Provider {
    pub(crate) fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        if !self.completed {
            self.job.uncertain.store(true, Ordering::Release);
        }
        self.job.providers.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) fn provider(session: &str, owner: u32) -> Result<Option<Provider>, String> {
    let job = jobs().lock().map_err(|_| "ordinary App custody lock poisoned")?.get(session).cloned();
    let Some(job) = job else { return Ok(None) };
    if job.owner != owner || job.retiring.load(Ordering::Acquire) {
        return Err("ordinary App execution is retiring".into());
    }
    job.providers.fetch_add(1, Ordering::AcqRel);
    if job.retiring.load(Ordering::Acquire) {
        job.providers.fetch_sub(1, Ordering::AcqRel);
        return Err("ordinary App execution is retiring".into());
    }
    Ok(Some(Provider { job, completed: false }))
}

fn retire(job: &Arc<Job>, deadline: Instant) -> Result<(), String> {
    job.retiring.store(true, Ordering::Release);
    super::authority::revoke_session_for_owner(&job.session, job.owner);
    let mut resources = job.resources.lock().map_err(|_| "ordinary App resource lock poisoned")?;
    let signalled = unsafe {
        libc::syscall(libc::SYS_pidfd_send_signal, std::os::fd::AsRawFd::as_raw_fd(&job.process),
            libc::SIGKILL, std::ptr::null::<libc::siginfo_t>(), 0)
    };
    if signalled != 0 && !pidfd_exited(job.process.as_fd()).map_err(|error| error.to_string())? {
        return Err(format!("retire exact App gate: {}", std::io::Error::last_os_error()));
    }
    resources.group.retire_checked(deadline)?;
    while !pidfd_exited(job.process.as_fd()).map_err(|error| error.to_string())?
        || job.providers.load(Ordering::Acquire) != 0
    {
        if Instant::now() >= deadline {
            return Err("ordinary App processes/provider calls have not retired".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    if job.uncertain.load(Ordering::Acquire) {
        return Err("ordinary App provider completion is unknown; broker recovery is required".into());
    }
    if !resources.cleanup_confirmed {
        let Resources { controller, cleanup_sent, .. } = &mut *resources;
        crate::worker::app_custody::retire(controller, cleanup_sent, deadline)?;
        resources.cleanup_confirmed = true;
    }
    resources.group.remove_checked()?;
    crate::proc::try_deregister_session_for_owner(&job.session, job.owner)?;
    crate::provenance::runtime::deregister_checked(job.owner, &job.session)?;
    drop(resources);
    jobs().lock().map_err(|_| "ordinary App custody lock poisoned")?.remove(&job.session);
    Ok(())
}

pub(crate) fn retire_app(app: &str, deadline: Instant) -> Result<Vec<(u32, String)>, String> {
    initialized_root()?;
    let retained: Vec<_> = jobs().lock().map_err(|_| "ordinary App custody lock poisoned")?
        .values().filter(|job| job.app == app).cloned().collect();
    let mut completed = Vec::new();
    let mut errors = Vec::new();
    for job in retained {
        match retire(&job, deadline) {
            Ok(()) => completed.push((job.owner, job.session.clone())),
            Err(error) => errors.push(format!("{}: {error}", job.session)),
        }
    }
    if errors.is_empty() { Ok(completed) } else { Err(errors.join("; ")) }
}

pub(crate) async fn retire_session(owner: u32, session: &str) -> Result<bool, String> {
    let job = jobs().lock().map_err(|_| "ordinary App custody lock poisoned")?.get(session).cloned();
    let Some(job) = job else { return Ok(false) };
    if job.owner != owner {
        return Err("ordinary App execution belongs to another owner".into());
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    // Dropping or timing out the RPC cannot abandon the retained cleanup worker.
    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline),
        tokio::task::spawn_blocking(move || retire(&job, deadline)))
        .await.map_err(|_| "ordinary App retirement remains pending")?
        .map_err(|error| error.to_string())??;
    Ok(true)
}
