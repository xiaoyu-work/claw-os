// SPDX-License-Identifier: GPL-3.0-only

//! Internal Root-launched reader. Its working directory contains only the
//! broker's pinned, read-only Calendar database directory; no caller path is accepted.

use jiff::{civil::Date, tz::TimeZone};
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd},
    os::unix::net::UnixStream,
    path::Path,
};

pub fn run(args: &[String]) -> Result<serde_json::Value, String> {
    if args.len() != 5 || args[0] != "--query-v1" {
        return Err(
            "usage: claw-calendar-reader --query-v1 <year> <month> <day> <broker-fd>".into(),
        );
    }
    let year = args[1]
        .parse::<i16>()
        .map_err(|_| "invalid Calendar year")?;
    let month = args[2]
        .parse::<i8>()
        .map_err(|_| "invalid Calendar month")?;
    let day = args[3].parse::<i8>().map_err(|_| "invalid Calendar day")?;
    let day = Date::new(year, month, day).map_err(|error| error.to_string())?;
    let fd = args[4]
        .parse::<i32>()
        .map_err(|_| "invalid Calendar broker descriptor")?;
    if fd < 3
        || unsafe { libc::geteuid() } == 0
        || unsafe { libc::getuid() } != unsafe { libc::geteuid() }
    {
        return Err(
            "Calendar reader requires an unprivileged owner and a Root broker descriptor".into(),
        );
    }
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(credentials).cast(),
            &mut length,
        )
    } != 0
        || length as usize != std::mem::size_of::<libc::ucred>()
        || credentials.uid != 0
        || credentials.pid != unsafe { libc::getppid() }
    {
        return Err("Calendar reader was not started by its Root broker".into());
    }
    let gate = unsafe { UnixStream::from_raw_fd(fd) };
    if unsafe { libc::fcntl(gate.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } != 0
        || unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0
    {
        return Err(format!(
            "protect Calendar reader: {}",
            io::Error::last_os_error()
        ));
    }
    let events = super::load_day_from_db(Path::new("events.db"), day, TimeZone::system())?;
    Ok(serde_json::json!({"events": events}))
}
