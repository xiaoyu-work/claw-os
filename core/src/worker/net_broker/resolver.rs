//! Keep system NSS lookup in an owned process: an in-process getaddrinfo
//! cannot be interrupted when its egress endpoint retires.

use std::io::{self, Read};
use std::net::{IpAddr, SocketAddr};
use std::os::fd::{AsRawFd, RawFd};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::Endpoint;

const OUTPUT_BYTES: usize = 64 * 1024;
const STDERR_BYTES: usize = 4 * 1024;
const MAX_ADDRESSES: usize = 64;

pub(super) fn resolve(
    endpoint: &Endpoint,
    timeout: Duration,
    stopping: impl Fn() -> bool,
) -> io::Result<Vec<SocketAddr>> {
    if timeout.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "zero DNS timeout",
        ));
    }
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "DNS timeout overflow"))?;
    if stopping() {
        return Err(cancelled());
    }
    if let Ok(ip) = endpoint.host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, endpoint.port)]);
    }
    // Include both address families even when the host has no configured
    // route for one; the public-address gate must see the complete answer.
    let mut command = Command::new("/usr/bin/getent");
    command
        .args([
            "--no-addrconfig",
            "--no-idn",
            "--",
            "ahosts",
            &endpoint.host,
        ])
        .env("LC_ALL", "C");
    let mut process = Lookup::spawn(&mut command)?;
    let output = process.collect(deadline, &stopping)?;
    if stopping() {
        return Err(cancelled());
    }
    parse_addresses(&output, endpoint.port)
}

struct Lookup {
    child: Child,
    reaped: bool,
}

impl Lookup {
    fn spawn(command: &mut Command) -> io::Result<Self> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            let parent = unsafe { libc::getpid() };
            unsafe {
                command.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                        || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
                    {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::getppid() != parent {
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "egress resolver parent exited",
                        ));
                    }
                    Ok(())
                });
            }
        }
        let lookup = Self {
            child: command.spawn()?,
            reaped: false,
        };
        nonblocking(
            lookup
                .child
                .stdout
                .as_ref()
                .ok_or_else(|| io::Error::other("DNS stdout unavailable"))?
                .as_raw_fd(),
        )?;
        nonblocking(
            lookup
                .child
                .stderr
                .as_ref()
                .ok_or_else(|| io::Error::other("DNS stderr unavailable"))?
                .as_raw_fd(),
        )?;
        Ok(lookup)
    }

    fn collect(&mut self, deadline: Instant, stopping: &impl Fn() -> bool) -> io::Result<Vec<u8>> {
        match self.read_output(deadline, stopping) {
            Ok(output) => Ok(output),
            Err(error) => match self.terminate() {
                Ok(()) => Err(error),
                Err(cleanup) => Err(io::Error::other(format!(
                    "{error}; DNS resolver cleanup failed: {cleanup}"
                ))),
            },
        }
    }

    fn read_output(
        &mut self,
        deadline: Instant,
        stopping: &impl Fn() -> bool,
    ) -> io::Result<Vec<u8>> {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdout_eof = false;
        let mut stderr_eof = false;
        let mut status = None;
        loop {
            if stopping() {
                return Err(cancelled());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "egress DNS lookup timed out",
                ));
            }
            let output = self
                .child
                .stdout
                .as_mut()
                .ok_or_else(|| io::Error::other("DNS stdout unavailable"))?;
            let errors = self
                .child
                .stderr
                .as_mut()
                .ok_or_else(|| io::Error::other("DNS stderr unavailable"))?;
            if !stdout_eof {
                stdout_eof = drain(output, &mut stdout, OUTPUT_BYTES)?;
            }
            if !stderr_eof {
                stderr_eof = drain(errors, &mut stderr, STDERR_BYTES)?;
            }
            let mut pipes = [
                libc::pollfd {
                    fd: if stdout_eof { -1 } else { output.as_raw_fd() },
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: if stderr_eof { -1 } else { errors.as_raw_fd() },
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            if status.is_none() {
                status = self.child.try_wait()?;
                self.reaped = status.is_some();
            }
            if let Some(status) = status.filter(|_| stdout_eof && stderr_eof) {
                if !status.success() {
                    return Err(io::Error::other(format!(
                        "system NSS resolver exited with {status}: {}",
                        String::from_utf8_lossy(&stderr).trim()
                    )));
                }
                return Ok(stdout);
            }
            let wait = deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .clamp(1, 50) as libc::c_int;
            let ready =
                unsafe { libc::poll(pipes.as_mut_ptr(), pipes.len() as libc::nfds_t, wait) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
            if pipes.iter().any(|pipe| pipe.revents & libc::POLLNVAL != 0) {
                return Err(io::Error::from_raw_os_error(libc::EBADF));
            }
        }
    }

    fn terminate(&mut self) -> io::Result<()> {
        if self.reaped {
            return Ok(());
        }
        let stopped = self.child.kill();
        let waited = self.child.wait();
        self.reaped = waited.is_ok();
        match (stopped, waited) {
            (Ok(()), Ok(_)) => Ok(()),
            (Err(error), Ok(_)) | (Ok(()), Err(error)) => Err(error),
            (Err(stop), Err(wait)) => Err(io::Error::other(format!(
                "stop resolver: {stop}; reap resolver: {wait}"
            ))),
        }
    }
}

impl Drop for Lookup {
    fn drop(&mut self) {
        if let Err(error) = self.terminate() {
            tracing::error!(%error, "egress DNS resolver cleanup failed");
        }
    }
}

fn nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn drain(reader: &mut impl Read, bytes: &mut Vec<u8>, limit: usize) -> io::Result<bool> {
    let mut buffer = [0_u8; 4096];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(read) => {
                if read > limit.saturating_sub(bytes.len()) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "system NSS resolver output exceeds its byte limit",
                    ));
                }
                bytes.extend_from_slice(&buffer[..read]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

fn cancelled() -> io::Error {
    io::Error::new(
        io::ErrorKind::Interrupted,
        "egress authority retired during DNS lookup",
    )
}

fn parse_addresses(bytes: &[u8], port: u16) -> io::Result<Vec<SocketAddr>> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid system NSS address output",
        )
    };
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    if !text.ends_with('\n') {
        return Err(invalid());
    }
    let mut addresses = Vec::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let ip = fields
            .next()
            .ok_or_else(invalid)?
            .parse::<IpAddr>()
            .map_err(|_| invalid())?;
        let transport = fields.next().ok_or_else(invalid)?;
        if !matches!(transport, "STREAM" | "DGRAM" | "RAW") || fields.count() > 1 {
            return Err(invalid());
        }
        let address = SocketAddr::new(ip, port);
        if transport == "STREAM" && !addresses.contains(&address) {
            if addresses.len() == MAX_ADDRESSES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "system NSS resolver returned more than 64 TCP addresses",
                ));
            }
            addresses.push(address);
        }
    }
    if addresses.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "system NSS returned no TCP addresses",
        ));
    }
    Ok(addresses)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/worker/net_broker/resolver.rs"
    ));
}
