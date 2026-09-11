use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use tokio::{
    io::{AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
    time::{timeout, timeout_at, Instant},
};

use super::protocol::{
    self, CalendarDate, CalendarEvent, Data, Failure, HistoryPermission, Operation, Outcome,
    Request, Response, SystemSummary, Task,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("The Claw OS applet service is not installed: {0}")]
    Unavailable(std::io::Error),
    #[error("The Claw OS applet service timed out.")]
    Timeout,
    #[error("The Claw OS applet service returned invalid data: {0}")]
    Protocol(String),
    #[error("The Claw OS applet service connection failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("{operation} Provider cleanup failed: {cleanup}")]
    Cleanup {
        operation: String,
        cleanup: std::io::Error,
    },
    #[error("{0}")]
    Provider(#[from] Failure),
}

/// Clones share one private provider process. Keep a client alive for repeated
/// telemetry requests so the OS, not the UI, retains CPU/network sample deltas.
#[derive(Clone)]
pub struct Client {
    binary: PathBuf,
    connection: Arc<Mutex<Option<Connection>>>,
}

struct Connection {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next_id: u64,
}

impl Default for Client {
    fn default() -> Self {
        Self::installed()
    }
}

impl Client {
    /// Select the fixed packaged OS provider, never PATH or an environment override.
    pub fn installed() -> Self {
        Self {
            binary: PathBuf::from(protocol::PROVIDER_BINARY),
            connection: Arc::default(),
        }
    }

    /// Explicit development/test executable selection. The installed App
    /// entrypoints use `installed`; selecting a program conveys no authority.
    pub fn with_binary(binary: impl AsRef<Path>) -> Result<Self, ClientError> {
        let binary = binary.as_ref();
        if !binary.is_absolute() {
            return Err(ClientError::Protocol(
                "Provider path must be absolute.".into(),
            ));
        }
        Ok(Self {
            binary: binary.to_path_buf(),
            connection: Arc::default(),
        })
    }

    pub async fn calendar_day(
        &self,
        date: CalendarDate,
    ) -> Result<Vec<CalendarEvent>, ClientError> {
        match self.request(Operation::CalendarDay { date }).await? {
            Data::Calendar { events } => Ok(events),
            _ => unreachable!("the response was matched to the request"),
        }
    }

    pub async fn calendar_today(&self) -> Result<Vec<CalendarEvent>, ClientError> {
        match self.request(Operation::CalendarToday {}).await? {
            Data::Calendar { events } => Ok(events),
            _ => unreachable!("the response was matched to the request"),
        }
    }

    pub async fn tasks(&self) -> Result<Vec<Task>, ClientError> {
        match self.request(Operation::Tasks {}).await? {
            Data::Tasks { tasks } => Ok(tasks),
            _ => unreachable!("the response was matched to the request"),
        }
    }

    pub async fn system(&self) -> Result<SystemSummary, ClientError> {
        match self.request(Operation::System {}).await? {
            Data::System { summary } => Ok(summary),
            _ => unreachable!("the response was matched to the request"),
        }
    }

    pub async fn require_history(&self, permission: HistoryPermission) -> Result<(), ClientError> {
        self.request(Operation::HistoryCheck { permission }).await?;
        Ok(())
    }

    async fn request(&self, operation: Operation) -> Result<Data, ClientError> {
        self.request_with_timeout(operation, REQUEST_TIMEOUT).await
    }

    async fn request_with_timeout(
        &self,
        operation: Operation,
        duration: Duration,
    ) -> Result<Data, ClientError> {
        operation.validate()?;
        let deadline = Instant::now() + duration;
        let mut slot = timeout_at(deadline, self.connection.lock())
            .await
            .map_err(|_| ClientError::Timeout)?;
        // A cancelled exchange owns the connection, so it cannot leave a stale
        // reply for another clone. Cancelling a waiter does not stop the owner.
        let mut connection = match slot.take() {
            Some(connection) => connection,
            None => Connection::spawn(&self.binary)?,
        };
        match timeout_at(deadline, connection.exchange(operation)).await {
            Ok(Ok(outcome)) => {
                *slot = Some(connection);
                match outcome {
                    Outcome::Ok { data } => Ok(data),
                    Outcome::Error { error } => Err(ClientError::Provider(error)),
                }
            }
            failed => {
                let error = match failed {
                    Ok(Err(error)) => error,
                    Err(_) => ClientError::Timeout,
                    Ok(Ok(_)) => unreachable!(),
                };
                connection
                    .stop()
                    .await
                    .map_err(|cleanup| ClientError::Cleanup {
                        operation: error.to_string(),
                        cleanup,
                    })?;
                Err(error)
            }
        }
    }
}

impl Connection {
    fn spawn(binary: &Path) -> Result<Self, ClientError> {
        let mut command = Command::new(binary);
        command
            .arg(protocol::PROVIDER_ARGUMENT)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.as_std_mut().process_group(0);
        }
        let mut child = command.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ClientError::Unavailable(error)
            } else {
                ClientError::Io(error)
            }
        })?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| ClientError::Protocol("Provider stdin is unavailable.".into()))?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| ClientError::Protocol("Provider stdout is unavailable.".into()))?;
        Ok(Self {
            child,
            input,
            output: BufReader::new(output),
            next_id: 0,
        })
    }

    async fn exchange(&mut self, operation: Operation) -> Result<Outcome, ClientError> {
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| ClientError::Protocol("Provider request IDs are exhausted.".into()))?;
        let request = Request {
            version: protocol::VERSION,
            id: self.next_id,
            operation,
        };
        let bytes = protocol::encode_frame(&request, protocol::MAX_REQUEST_BYTES)?;
        self.input.write_all(&bytes).await?;
        self.input.flush().await?;
        let bytes = protocol::read_frame(&mut self.output, protocol::MAX_RESPONSE_BYTES)
            .await?
            .ok_or_else(|| ClientError::Protocol("Provider closed without a response.".into()))?;
        let response: Response = serde_json::from_slice(&bytes)
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        if response.version != protocol::VERSION || response.id != request.id {
            return Err(ClientError::Protocol(
                "Provider response version or request ID does not match.".into(),
            ));
        }
        if let Outcome::Ok { data } = &response.outcome {
            if !request.operation.accepts(data) {
                return Err(ClientError::Protocol(
                    "Provider returned the wrong response kind.".into(),
                ));
            }
        }
        Ok(response.outcome)
    }

    fn terminate(&mut self) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            let Some(pid) = self.child.id() else {
                return Ok(());
            };
            let pid = libc::pid_t::try_from(pid).map_err(std::io::Error::other)?;
            // The unreaped direct child reserves this private process-group ID.
            if unsafe { libc::kill(-pid, libc::SIGKILL) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            self.child.start_kill()
        }
    }

    async fn stop(&mut self) -> std::io::Result<()> {
        self.terminate()?;
        timeout(CLEANUP_TIMEOUT, self.child.wait())
            .await
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "Provider termination was not confirmed within the cleanup deadline.",
                )
            })??;
        Ok(())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Err(error) = self.terminate() {
            eprintln!("Claw OS applet provider cancellation failed: {error}");
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/applet/client.rs"
    ));
}
