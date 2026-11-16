use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

use super::{configured_user_command, DesktopEnvironment};

pub(in crate::clawd) fn protected_helper_command(
    args: &[String],
    environment: &DesktopEnvironment,
) -> Result<std::process::Command, String> {
    let path = Path::new("/usr/local/bin/clawd");
    crate::provenance::fsec::require_secure_location(path, &[0])
        .map_err(|error| error.to_string())?;
    let mut program = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| error.to_string())?;
    let metadata = program.metadata().map_err(|error| error.to_string())?;
    let mut magic = [0; 4];
    program
        .read_exact(&mut magic)
        .map_err(|error| error.to_string())?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.nlink() != 1
        || metadata.mode() & 0o022 != 0
        || metadata.mode() & 0o111 == 0
        || &magic != b"\x7fELF"
    {
        return Err("OS helper requires the protected installed clawd ELF".into());
    }
    // Pin the OS program, not an embedding libtest executable or a caller path.
    let descriptor = program.as_raw_fd();
    let mut command = configured_user_command(
        &PathBuf::from(format!("/proc/self/fd/{descriptor}")),
        args,
        environment,
    );
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(program.as_raw_fd(), libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(command)
}

pub(in crate::clawd) fn authenticated_helper_gate(fd: i32) -> Result<UnixStream, String> {
    if fd < 3 || unsafe { libc::getuid() } != unsafe { libc::geteuid() } {
        return Err("OS helper requires the authenticated owner identity".into());
    }
    let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(peer).cast(),
            &mut length,
        )
    } != 0
        || length as usize != std::mem::size_of::<libc::ucred>()
        || peer.uid != 0
        || peer.pid != unsafe { libc::getppid() }
    {
        return Err("OS helper was not started by root clawd".into());
    }
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
        return Err(format!(
            "harden OS helper: {}",
            std::io::Error::last_os_error()
        ));
    }
    let gate = unsafe { UnixStream::from_raw_fd(fd) };
    gate.set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    Ok(gate)
}
