// SPDX-License-Identifier: GPL-3.0-only

use std::{io, time::Duration};

use claw_os_sdk::applet::protocol::{
    self, CalendarEvent, Data, ErrorCode, Failure, HistoryPermission, Operation, Outcome, Request,
    Response, SystemSummary, Task, Usage,
};
use jiff::{Timestamp, civil::Date, tz::TimeZone};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt},
    time::timeout,
};

use crate::{
    calendar,
    policy::{self, Scope},
    system, tasks,
};

const FRAME_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Default)]
struct Service {
    sample: system::RawSample,
}

impl Service {
    async fn handle(&mut self, request: Request) -> Response {
        let result = match request.validate() {
            Ok(()) => self.dispatch(request.operation).await,
            Err(error) => Err(error),
        };
        Response {
            version: protocol::VERSION,
            id: request.id,
            outcome: match result {
                Ok(data) => Outcome::Ok { data },
                Err(error) => Outcome::Error { error },
            },
        }
    }

    async fn dispatch(&mut self, operation: Operation) -> Result<Data, Failure> {
        // Parsing and calendar validation precede policy, paths and subprocesses.
        let day = match &operation {
            Operation::CalendarDay { date } => Some(
                Date::new(date.year, date.month, date.day)
                    .map_err(|error| Failure::new(ErrorCode::InvalidRequest, error.to_string()))?,
            ),
            Operation::CalendarToday {} => {
                Some(Timestamp::now().to_zoned(TimeZone::system()).date())
            }
            _ => None,
        };
        let (verb, scope) = permission(&operation);
        policy::check(verb, scope).await.map_err(|error| {
            let code = match error.kind {
                policy::FailureKind::Denied => ErrorCode::PermissionDenied,
                policy::FailureKind::Unavailable => ErrorCode::ProviderUnavailable,
                policy::FailureKind::Execution => ErrorCode::ProviderFailure,
            };
            Failure::new(code, error.message)
        })?;
        match operation {
            Operation::CalendarDay { .. } | Operation::CalendarToday {} => {
                let events =
                    calendar::load_day_authorized(day.expect("validated calendar operation"))
                        .await
                        .map_err(provider_failure)?;
                Ok(Data::Calendar {
                    events: events.into_iter().map(calendar_event).collect(),
                })
            }
            Operation::Tasks {} => {
                let tasks = tasks::load_tasks_async()
                    .await
                    .map_err(|error| provider_failure(error.0))?;
                Ok(Data::Tasks {
                    tasks: tasks.into_iter().map(task).collect(),
                })
            }
            Operation::System {} => {
                let (summary, sample) = system::load_authorized(self.sample)
                    .await
                    .map_err(provider_failure)?;
                self.sample = sample;
                Ok(Data::System {
                    summary: system_summary(summary),
                })
            }
            Operation::HistoryCheck { .. } => Ok(Data::HistoryAllowed {}),
        }
    }
}

fn permission(operation: &Operation) -> (&'static str, Scope<'static>) {
    match operation {
        Operation::CalendarDay { .. } | Operation::CalendarToday {} => {
            ("data.db.read", Scope::Name("calendar"))
        }
        Operation::Tasks {} => ("agent.observe", Scope::Name("tasks")),
        Operation::System {} => ("sys.observe", Scope::Wild),
        Operation::HistoryCheck {
            permission: HistoryPermission::Read,
        } => ("clipboard.read", Scope::Name("history")),
        Operation::HistoryCheck {
            permission: HistoryPermission::Write,
        } => ("clipboard.write", Scope::Name("history")),
    }
}

fn provider_failure(message: String) -> Failure {
    Failure::new(ErrorCode::ProviderFailure, message)
}

fn calendar_event(event: calendar::CalendarEvent) -> CalendarEvent {
    CalendarEvent {
        id: event.id,
        title: event.title,
        start: event.start,
        end: event.end,
        location: event.location,
    }
}

fn task(task: tasks::Task) -> Task {
    Task {
        id: task.id,
        purpose: task.purpose,
        status: task.status,
        created_at: task.created_at,
    }
}

fn system_summary(summary: system::SystemSummary) -> SystemSummary {
    let usage = |value: system::Usage| Usage {
        used_mb: value.used_mb,
        total_mb: value.total_mb,
    };
    SystemSummary {
        cpu_percent: summary.cpu_percent,
        memory: summary.memory.map(usage),
        storage: summary.storage.map(usage),
        network_down_bps: summary.network_down_bps,
        network_up_bps: summary.network_up_bps,
        fallback: summary.fallback,
    }
}

/// One private pipe and one sampling state per client. This process has
/// the caller's existing identity/sandbox; it never acquires desktop transports.
pub async fn serve(
    mut input: impl AsyncBufRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
) -> io::Result<()> {
    let mut service = Service::default();
    loop {
        // Idle clients may live as long as their UI. Once a frame starts, its
        // entire remaining read has one deadline, not a per-byte timeout.
        if input.fill_buf().await?.is_empty() {
            return Ok(());
        }
        let frame = timeout(
            FRAME_TIMEOUT,
            protocol::read_frame(&mut input, protocol::MAX_REQUEST_BYTES),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Applet request timed out."))
        .and_then(|result| result);
        let bytes = match frame {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return Ok(()),
            Err(error) => {
                send(&mut output, refusal(0, error.to_string())).await?;
                return Err(error);
            }
        };
        let response = match serde_json::from_slice::<Request>(&bytes) {
            Ok(request) => service.handle(request).await,
            Err(error) => refusal(0, format!("Invalid applet request: {error}")),
        };
        send(&mut output, response).await?;
    }
}

fn refusal(id: u64, message: String) -> Response {
    Response {
        version: protocol::VERSION,
        id,
        outcome: Outcome::Error {
            error: Failure::new(ErrorCode::InvalidRequest, message),
        },
    }
}

async fn send(output: &mut (impl AsyncWrite + Unpin), response: Response) -> io::Result<()> {
    let bytes = match protocol::encode_frame(&response, protocol::MAX_RESPONSE_BYTES) {
        Ok(bytes) => bytes,
        Err(error) => protocol::encode_frame(
            &Response {
                version: protocol::VERSION,
                id: response.id,
                outcome: Outcome::Error {
                    error: provider_failure(format!(
                        "Could not encode bounded applet response: {error}"
                    )),
                },
            },
            protocol::MAX_RESPONSE_BYTES,
        )?,
    };
    timeout(FRAME_TIMEOUT, async {
        output.write_all(&bytes).await?;
        output.flush().await
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Applet response timed out."))?
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/service.rs"));
}
