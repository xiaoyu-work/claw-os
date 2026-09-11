//! The OS login stack's Root activation handoff. No user-facing launch interface.

#![cfg(target_os = "linux")]

use std::ffi::{c_char, c_int, c_void, CStr};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use claw_display_control::identity::pidfd_exited;
use claw_display_control::wire::{
    LoginReply, LoginRequest, PamCommand, PamReply, DISPLAY_HOST, LOGIN_SOCKET,
};
use claw_display_control::workload::{SessionGroup, Workload};
use claw_display_control::{Connection, Epoch, Error, KernelLogin, ProcessIdentity};

const PAM_SUCCESS: c_int = 0;
const PAM_SESSION_ERR: c_int = 14;
const PAM_NO_MODULE_DATA: c_int = 18;
const PAM_SERVICE: c_int = 1;
const START_TIMEOUT: Duration = Duration::from_secs(20);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
const DATA_KEY: &CStr = c"claw-display-login-v1";

#[repr(C)]
pub struct PamHandle {
    _private: [u8; 0],
}

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_get_item(handle: *const PamHandle, item: c_int, value: *mut *const c_void) -> c_int;
    fn pam_getenv(handle: *mut PamHandle, name: *const c_char) -> *const c_char;
    fn pam_get_user(
        handle: *mut PamHandle,
        user: *mut *const c_char,
        prompt: *const c_char,
    ) -> c_int;
    fn pam_get_data(
        handle: *const PamHandle,
        name: *const c_char,
        data: *mut *const c_void,
    ) -> c_int;
    fn pam_set_data(
        handle: *mut PamHandle,
        name: *const c_char,
        data: *mut c_void,
        cleanup: Option<unsafe extern "C" fn(*mut PamHandle, *mut c_void, c_int)>,
    ) -> c_int;
    fn pam_syslog(handle: *const PamHandle, priority: c_int, format: *const c_char, ...);
}

struct Login {
    controller: ProcessIdentity,
    child: Child,
    child_identity: ProcessIdentity,
    child_pidfd: OwnedFd,
    channel: Connection,
    epoch: Epoch,
    anchor: SessionGroup,
    workload: Option<Workload>,
    retired: bool,
}

impl Login {
    fn start(
        controller: ProcessIdentity,
        anchor: SessionGroup,
        locale: claw_display_control::locale::LocaleEnvironment,
    ) -> Result<Self, Error> {
        claw_display_control::install::protected_executable(DISPLAY_HOST)?;
        let broker = Self::wait_for_broker(Instant::now() + START_TIMEOUT)?;
        let broker_identity = broker.peer()?;
        if broker_identity.uid() != 0 {
            return Err(Error::Identity);
        }
        let (channel, child_channel) = Connection::pair()?;
        let (authority, child_authority) = Connection::pair()?;
        let child_channel = reserve(child_channel.as_fd())?;
        let child_authority = reserve(child_authority.as_fd())?;
        let first = child_channel.as_raw_fd();
        let second = child_authority.as_raw_fd();
        let parent_pid = controller.pid();
        let mut command = Command::new(DISPLAY_HOST);
        command.arg("--supervisor").env_clear().current_dir("/");
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        unsafe {
            command.pre_exec(move || {
                if libc::getppid() != parent_pid || libc::getuid() != 0 || libc::geteuid() != 0 {
                    return Err(std::io::Error::from_raw_os_error(libc::EPERM));
                }
                if libc::dup2(first, 3) < 0 || libc::dup2(second, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::syscall(
                    libc::SYS_close_range,
                    5_u32,
                    u32::MAX,
                    libc::CLOSE_RANGE_CLOEXEC,
                ) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::setsid() < 0 || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        let preparation = (|| {
            let child_identity = ProcessIdentity::child(&child)?;
            let child_pidfd = child_identity.pidfd()?;
            let deadline = Instant::now() + START_TIMEOUT;
            broker.send(
                &LoginRequest::Register {},
                &[authority.as_fd(), child_pidfd.as_fd()],
                deadline,
            )?;
            let mut reply = broker.receive::<LoginReply>(&broker_identity, deadline)?;
            let LoginReply::Registered { epoch } = reply.message;
            let broker_pidfd = reply.descriptors.pop().ok_or(Error::Identity)?;
            if ProcessIdentity::from_pidfd(broker_pidfd.as_fd())? != broker_identity {
                return Err(Error::Identity);
            }
            channel.send(
                &PamCommand::Start { epoch, locale },
                &[broker_pidfd.as_fd()],
                deadline,
            )?;
            Ok((child_identity, child_pidfd, epoch))
        })();
        let (child_identity, child_pidfd, epoch) = match preparation {
            Ok(value) => value,
            Err(error) => {
                if let Err(cleanup_error) = kill_child(&mut child, Instant::now() + STOP_TIMEOUT) {
                    eprintln!("display preparation failed: {error}; child cleanup failed: {cleanup_error}");
                    return Err(cleanup_error);
                }
                return Err(error);
            }
        };
        let mut login = Self {
            controller,
            child,
            child_identity,
            child_pidfd,
            channel,
            epoch,
            anchor,
            workload: None,
            retired: false,
        };
        let deadline = Instant::now() + START_TIMEOUT;
        let mut prepared = login
            .channel
            .receive::<PamReply>(&login.child_identity, deadline)?;
        if !matches!(prepared.message, PamReply::Prepared { epoch } if epoch == login.epoch) {
            return Err(Error::Protocol("unexpected display preparation response"));
        }
        login.workload = Some(Workload::receive(
            prepared.descriptors.pop().ok_or(Error::Identity)?,
            &login.anchor,
            login.epoch,
        )?);
        login
            .channel
            .send(&PamCommand::Prepared { epoch: login.epoch }, &[], deadline)?;
        let ready = login
            .channel
            .receive::<PamReply>(&login.child_identity, deadline)?;
        if !matches!(ready.message, PamReply::Ready { epoch } if epoch == login.epoch) {
            return Err(Error::Protocol("compositor startup did not complete"));
        }
        Ok(login)
    }

    fn wait_for_broker(deadline: Instant) -> Result<Connection, Error> {
        loop {
            match Connection::connect(std::path::Path::new(LOGIN_SOCKET)) {
                Err(Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    if Instant::now() >= deadline {
                        return Err(Error::Timeout);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                result => return result,
            }
        }
    }

    fn close(&mut self) -> Result<(), Error> {
        if self.retired {
            return Ok(());
        }
        self.controller.assert_current()?;
        if ProcessIdentity::current()? != self.controller {
            return Err(Error::Identity);
        }
        let deadline = Instant::now() + STOP_TIMEOUT;
        self.channel
            .send(&PamCommand::Close { epoch: self.epoch }, &[], deadline)?;
        let reply = self
            .channel
            .receive::<PamReply>(&self.child_identity, deadline)?;
        if !matches!(reply.message, PamReply::Retired { epoch } if epoch == self.epoch) {
            return Err(Error::Protocol("display retirement was not acknowledged"));
        }
        self.workload
            .as_ref()
            .ok_or(Error::Identity)?
            .retire(deadline)?;
        self.channel
            .send(&PamCommand::Finished { epoch: self.epoch }, &[], deadline)?;
        wait_child(&mut self.child, self.child_pidfd.as_fd(), deadline)?;
        self.retired = true;
        Ok(())
    }
}

impl Drop for Login {
    fn drop(&mut self) {
        if self.retired
            || unsafe { libc::getpid() } != self.controller.pid()
            || unsafe { libc::geteuid() } != 0
        {
            return;
        }
        let deadline = Instant::now() + STOP_TIMEOUT;
        if let Some(workload) = &self.workload {
            if let Err(error) = workload.retire(deadline) {
                eprintln!("claw display emergency retirement failed: {error}");
            }
        }
        if let Err(error) = self.child.kill() {
            if error.kind() != std::io::ErrorKind::InvalidInput {
                eprintln!("claw display supervisor termination failed: {error}");
            }
        }
        if let Err(error) = wait_child(&mut self.child, self.child_pidfd.as_fd(), deadline) {
            eprintln!("claw display supervisor retirement failed: {error}");
        }
    }
}

fn wait_child(
    child: &mut Child,
    pidfd: std::os::fd::BorrowedFd<'_>,
    deadline: Instant,
) -> Result<(), Error> {
    while !pidfd_exited(pidfd)? {
        if Instant::now() >= deadline {
            return Err(Error::Timeout);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Protocol(
            "display supervisor exited without a successful retirement",
        ))
    }
}

fn kill_child(child: &mut Child, deadline: Instant) -> Result<(), Error> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    child.kill()?;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::Timeout);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn reserve(fd: std::os::fd::BorrowedFd<'_>) -> Result<OwnedFd, Error> {
    let raw = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 64) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

unsafe fn locale_environment(
    handle: *mut PamHandle,
) -> Result<claw_display_control::locale::LocaleEnvironment, Error> {
    let mut values = std::collections::BTreeMap::new();
    for key in claw_display_control::locale::KEYS {
        let name = std::ffi::CString::new(*key).map_err(|_| Error::Identity)?;
        let value = unsafe { pam_getenv(handle, name.as_ptr()) };
        if value.is_null() {
            continue;
        }
        if unsafe { libc::strnlen(value, claw_display_control::locale::MAX_VALUE + 1) }
            > claw_display_control::locale::MAX_VALUE
        {
            return Err(Error::Protocol("PAM locale value exceeds its bound"));
        }
        let value = unsafe { CStr::from_ptr(value) }
            .to_str()
            .map_err(|_| Error::Protocol("PAM locale is not UTF-8"))?;
        values.insert((*key).to_string(), value.to_string());
    }
    values.try_into()
}

unsafe fn phase(argc: c_int, argv: *const *const c_char) -> Result<bool, Error> {
    if argc != 1 || argv.is_null() || unsafe { (*argv).is_null() } {
        return Err(Error::Protocol(
            "PAM display module requires one fixed phase",
        ));
    }
    match unsafe { CStr::from_ptr(*argv) }.to_bytes() {
        b"open-after" => Ok(true),
        b"close-before" => Ok(false),
        _ => Err(Error::Protocol("invalid PAM display phase")),
    }
}

unsafe fn authenticated(handle: *mut PamHandle) -> Result<(ProcessIdentity, SessionGroup), Error> {
    if handle.is_null() || unsafe { libc::getuid() } != 0 || unsafe { libc::geteuid() } != 0 {
        return Err(Error::Identity);
    }
    let mut service = std::ptr::null();
    if unsafe { pam_get_item(handle, PAM_SERVICE, &mut service) } != PAM_SUCCESS
        || service.is_null()
        || unsafe { CStr::from_ptr(service.cast()) }.to_bytes() != b"cosmic-greeter"
    {
        return Err(Error::Protocol("unexpected login PAM service"));
    }
    let controller = ProcessIdentity::current()?;
    let login = KernelLogin::of(&controller)?;
    let mut user = std::ptr::null();
    if unsafe { pam_get_user(handle, &mut user, std::ptr::null()) } != PAM_SUCCESS || user.is_null()
    {
        return Err(Error::UnauthenticatedLogin);
    }
    let mut record: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found = std::ptr::null_mut();
    let mut buffer = vec![0; 64 * 1024];
    let result = unsafe {
        libc::getpwnam_r(
            user,
            &mut record,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        )
    };
    if result != 0 || found.is_null() || record.pw_uid != login.owner_uid {
        return Err(Error::UnauthenticatedLogin);
    }
    let anchor = SessionGroup::of(&controller)?;
    Ok((controller, anchor))
}

unsafe fn existing(handle: *mut PamHandle) -> Result<Option<*mut Login>, Error> {
    if handle.is_null() {
        return Err(Error::Identity);
    }
    let mut data = std::ptr::null();
    match unsafe { pam_get_data(handle, DATA_KEY.as_ptr(), &mut data) } {
        PAM_SUCCESS if !data.is_null() => Ok(Some(data.cast_mut().cast())),
        PAM_NO_MODULE_DATA => Ok(None),
        _ => Err(Error::Protocol("cannot read PAM display lifecycle")),
    }
}

unsafe extern "C" fn cleanup(handle: *mut PamHandle, data: *mut c_void, _status: c_int) {
    if std::panic::catch_unwind(|| {
        if !data.is_null() {
            drop(unsafe { Box::from_raw(data.cast::<Login>()) });
        }
    })
    .is_err()
    {
        diagnostic(handle, c"claw display lifecycle cleanup panicked");
    }
}

fn diagnostic(handle: *mut PamHandle, message: &CStr) {
    unsafe {
        if handle.is_null() {
            libc::syslog(libc::LOG_ERR, c"%s".as_ptr(), message.as_ptr());
        } else {
            pam_syslog(handle, libc::LOG_ERR, c"%s".as_ptr(), message.as_ptr());
        }
    }
}

fn boundary(handle: *mut PamHandle, run: impl FnOnce() -> Result<(), Error>) -> c_int {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
        Ok(Ok(())) => PAM_SUCCESS,
        Ok(Err(error)) => {
            match std::ffi::CString::new(format!("claw display activation: {error}")) {
                Ok(message) => diagnostic(handle, &message),
                Err(_) => diagnostic(
                    handle,
                    c"claw display activation failed; invalid diagnostic text",
                ),
            }
            PAM_SESSION_ERR
        }
        Err(_) => {
            diagnostic(handle, c"claw display activation panicked");
            PAM_SESSION_ERR
        }
    }
}

/// # Safety
/// Called only by libpam with its live handle and argument array.
#[no_mangle]
pub unsafe extern "C" fn pam_sm_open_session(
    handle: *mut PamHandle,
    _flags: c_int,
    argc: c_int,
    argv: *const *const c_char,
) -> c_int {
    boundary(handle, || {
        if !unsafe { phase(argc, argv)? } {
            return Ok(());
        }
        let (controller, anchor) = unsafe { authenticated(handle)? };
        if unsafe { existing(handle)? }.is_some() {
            return Err(Error::Protocol("duplicate display activation"));
        }
        let locale = unsafe { locale_environment(handle)? };
        let login = Box::new(Login::start(controller, anchor, locale)?);
        let raw = Box::into_raw(login);
        if unsafe { pam_set_data(handle, DATA_KEY.as_ptr(), raw.cast(), Some(cleanup)) }
            != PAM_SUCCESS
        {
            drop(unsafe { Box::from_raw(raw) });
            return Err(Error::Protocol("cannot retain PAM display lifecycle"));
        }
        Ok(())
    })
}

/// # Safety
/// Called only by libpam with its live handle and argument array.
#[no_mangle]
pub unsafe extern "C" fn pam_sm_close_session(
    handle: *mut PamHandle,
    _flags: c_int,
    argc: c_int,
    argv: *const *const c_char,
) -> c_int {
    boundary(handle, || {
        if unsafe { phase(argc, argv)? } {
            return Ok(());
        }
        if let Some(login) = unsafe { existing(handle)? } {
            unsafe { &mut *login }.close()?;
        }
        Ok(())
    })
}
