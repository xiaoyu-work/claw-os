//! OS-only GUI transport bootstrap. Its inherited socket never reaches App code.

pub(crate) mod bootstrap;
pub(crate) mod kernel;

pub fn initialize_runner() -> Result<(), String> {
    use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
    use std::time::{Duration, Instant};

    let channel = unsafe { OwnedFd::from_raw_fd(libc::STDIN_FILENO) };
    if unsafe { libc::syscall(libc::SYS_close_range, 3_u32, u32::MAX, libc::CLOSE_RANGE_CLOEXEC) } != 0 {
        return Err(format!("close inherited GUI descriptors on exec: {}", std::io::Error::last_os_error()));
    }
    bootstrap::validate_channel(channel.as_fd()).map_err(|error| error.to_string())?;
    let parent = bootstrap::peer(channel.as_fd()).map_err(|error| error.to_string())?;
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
        return Err(format!(
            "disable GUI runner dumpability: {}",
            std::io::Error::last_os_error()
        ));
    }
    let listener = super::seccomp::gui::install()
        .map_err(|error| format!("install mandatory GUI connect guard: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    bootstrap::send(
        channel.as_fd(),
        bootstrap::READY,
        listener.as_fd(),
        deadline,
    )
    .map_err(|error| format!("handoff GUI syscall listener: {error}"))?;
    drop(listener);
    let input = bootstrap::receive(channel.as_fd(), bootstrap::INPUT, deadline)
        .map_err(|error| format!("receive GUI execution input: {error}"))?;
    if (
        input.credentials.pid,
        input.credentials.uid,
        input.credentials.gid,
    ) != (parent.pid, parent.uid, parent.gid)
    {
        return Err(
            "GUI bootstrap reply did not come from its retained parent endpoint".to_string(),
        );
    }
    let mut status: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(input.descriptor.as_raw_fd(), &mut status) } != 0
        || status.st_mode & libc::S_IFMT != libc::S_IFIFO
        || unsafe { libc::fcntl(input.descriptor.as_raw_fd(), libc::F_GETFL) } & libc::O_ACCMODE
            != libc::O_RDONLY
    {
        return Err("GUI execution input is not a read-only pipe".to_string());
    }
    drop(channel);
    if unsafe { libc::dup2(input.descriptor.as_raw_fd(), libc::STDIN_FILENO) } != libc::STDIN_FILENO
    {
        return Err(format!(
            "replace private GUI bootstrap with stdin: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}
