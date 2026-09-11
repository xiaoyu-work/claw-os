use std::collections::VecDeque;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const RETAINED_BYTES: usize = 65536;
const RETIREMENT_DRAIN_BYTES: usize = 1024 * 1024;

pub(super) use super::inputs::CapturedOutput as Captured;

#[derive(Clone)]
pub(super) struct Report {
    pub captured: Captured,
    pub error: Option<String>,
}

pub(super) struct Output {
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<Report>>,
    captured: Option<Result<Report, String>>,
}

impl Output {
    pub fn start(descriptor: OwnedFd) -> Result<Self, String> {
        let flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe {
                libc::fcntl(
                    descriptor.as_raw_fd(),
                    libc::F_SETFL,
                    flags | libc::O_NONBLOCK,
                )
            } < 0
        {
            return Err(format!(
                "configure GUI output: {}",
                std::io::Error::last_os_error()
            ));
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let reader = std::thread::Builder::new()
            .name("gui-output".to_string())
            .spawn(move || {
                let mut retained = VecDeque::with_capacity(RETAINED_BYTES);
                let mut truncated = false;
                let mut buffer = [0_u8; 16384];
                let mut drained = 0;
                let error = loop {
                    let mut poll = libc::pollfd {
                        fd: descriptor.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    let ready = unsafe { libc::poll(&mut poll, 1, 50) };
                    if ready < 0 {
                        let error = std::io::Error::last_os_error();
                        if error.kind() == std::io::ErrorKind::Interrupted {
                            continue;
                        }
                        break Some(format!("poll GUI output: {error}"));
                    }
                    if ready > 0 {
                        let count = unsafe {
                            libc::read(
                                descriptor.as_raw_fd(),
                                buffer.as_mut_ptr().cast(),
                                buffer.len(),
                            )
                        };
                        if count == 0 {
                            break None;
                        }
                        if count < 0 {
                            let error = std::io::Error::last_os_error();
                            if !matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                            ) {
                                break Some(format!("read GUI output: {error}"));
                            }
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && stopping.load(Ordering::Acquire)
                            {
                                break None;
                            }
                        } else {
                            let count = count as usize;
                            let excess = (retained.len() + count).saturating_sub(RETAINED_BYTES);
                            if excess > 0 {
                                retained.drain(..excess);
                                truncated = true;
                            }
                            retained.extend(&buffer[..count]);
                            if stopping.load(Ordering::Acquire) {
                                drained += count;
                                if drained >= RETIREMENT_DRAIN_BYTES {
                                    truncated = true;
                                    break None;
                                }
                            }
                        }
                    }
                    if ready == 0 && stopping.load(Ordering::Acquire) {
                        break None;
                    }
                };
                Report {
                    captured: Captured {
                        text: String::from_utf8_lossy(retained.make_contiguous()).into_owned(),
                        truncated,
                    },
                    error,
                }
            })
            .map_err(|error| format!("start GUI output reader: {error}"))?;
        Ok(Self {
            stop,
            reader: Some(reader),
            captured: None,
        })
    }

    pub fn finish(&mut self, deadline: Instant) -> Result<Report, String> {
        if let Some(captured) = &self.captured {
            return captured.clone();
        }
        self.stop.store(true, Ordering::Release);
        let reader = self
            .reader
            .as_ref()
            .ok_or("GUI output reader is unavailable")?;
        while !reader.is_finished() {
            if Instant::now() >= deadline {
                return Err("GUI output retirement is pending".to_string());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let captured = self
            .reader
            .take()
            .ok_or("GUI output reader disappeared")?
            .join()
            .map_err(|_| "GUI output reader panicked".to_string());
        self.captured = Some(captured.clone());
        captured
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        if self.reader.is_some() {
            match self.finish(Instant::now() + Duration::from_secs(1)) {
                Err(error) => tracing::error!(%error, "GUI output cleanup failed"),
                Ok(Report {
                    error: Some(error), ..
                }) => {
                    tracing::error!(%error, "GUI output capture failed");
                }
                Ok(_) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/gui/output.rs"
    ));
}
