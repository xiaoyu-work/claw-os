//! Version 1 of the public App data service's bounded JSON-lines protocol.

use std::io;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

pub const VERSION: u32 = 1;
pub const PROVIDER_BINARY: &str = "/usr/libexec/claw-os-applet-provider";
pub const PROVIDER_ARGUMENT: &str = "--stdio-v1";
pub const MAX_REQUEST_BYTES: usize = 4096;
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarDate {
    pub year: i16,
    pub month: i8,
    pub day: i8,
}

impl CalendarDate {
    pub fn validate(self) -> Result<(), Failure> {
        let leap = self.year % 4 == 0 && (self.year % 100 != 0 || self.year % 400 == 0);
        let days = match self.month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => 0,
        };
        if !(-9999..=9999).contains(&self.year) || self.day < 1 || self.day > days {
            return Err(Failure::new(
                ErrorCode::InvalidRequest,
                "Calendar date is invalid.",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HistoryPermission {
    Read,
    Write,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Operation {
    CalendarDay { date: CalendarDate },
    CalendarToday {},
    Tasks {},
    System {},
    HistoryCheck { permission: HistoryPermission },
}

impl Operation {
    pub fn validate(&self) -> Result<(), Failure> {
        if let Self::CalendarDay { date } = self {
            date.validate()?;
        }
        Ok(())
    }

    pub fn accepts(&self, data: &Data) -> bool {
        matches!(
            (self, data),
            (
                Self::CalendarDay { .. } | Self::CalendarToday {},
                Data::Calendar { .. }
            ) | (Self::Tasks {}, Data::Tasks { .. })
                | (Self::System {}, Data::System { .. })
                | (Self::HistoryCheck { .. }, Data::HistoryAllowed {})
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub id: u64,
    pub operation: Operation,
}

impl Request {
    pub fn validate(&self) -> Result<(), Failure> {
        if self.version != VERSION {
            return Err(Failure::new(
                ErrorCode::UnsupportedVersion,
                "The applet service protocol version is unsupported.",
            ));
        }
        if self.id == 0 {
            return Err(Failure::new(
                ErrorCode::InvalidRequest,
                "Applet request IDs must be positive.",
            ));
        }
        self.operation.validate()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub version: u32,
    pub id: u64,
    pub outcome: Outcome,
}

impl<'de> Deserialize<'de> for Response {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        use serde_json::value::RawValue;

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Envelope {
            version: u32,
            id: u64,
            outcome: Box<RawValue>,
        }
        #[derive(Deserialize)]
        struct RawOutcome {
            data: Box<RawValue>,
        }
        #[derive(Deserialize)]
        struct RawData {
            summary: Box<RawValue>,
        }
        #[derive(Deserialize)]
        struct RawSummary {
            #[serde(rename = "cpu_percent")]
            _cpu_percent: Option<f32>,
        }

        let envelope = Envelope::deserialize(decoder)?;
        let outcome =
            serde_json::from_str(envelope.outcome.get()).map_err(serde::de::Error::custom)?;
        if matches!(
            &outcome,
            Outcome::Ok {
                data: Data::System { .. }
            }
        ) {
            // Validate the original JSON token before tagged-enum buffering can
            // confuse an object with serde_json's private number representation.
            let raw: RawOutcome =
                serde_json::from_str(envelope.outcome.get()).map_err(serde::de::Error::custom)?;
            let data: RawData =
                serde_json::from_str(raw.data.get()).map_err(serde::de::Error::custom)?;
            serde_json::from_str::<RawSummary>(data.summary.get())
                .map_err(serde::de::Error::custom)?;
        }
        Ok(Self {
            version: envelope.version,
            id: envelope.id,
            outcome,
        })
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Outcome {
    Ok { data: Data },
    Error { error: Failure },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Data {
    Calendar { events: Vec<CalendarEvent> },
    Tasks { tasks: Vec<Task> },
    System { summary: SystemSummary },
    HistoryAllowed {},
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarEvent {
    pub id: String,
    pub title: String,
    pub start: String,
    pub end: Option<String>,
    pub location: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub id: String,
    pub purpose: String,
    pub status: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SystemSummary {
    #[serde(
        default,
        deserialize_with = "cpu_percent",
        serialize_with = "serialize_cpu_percent"
    )]
    pub cpu_percent: Option<f32>,
    pub memory: Option<Usage>,
    pub storage: Option<Usage>,
    pub network_down_bps: Option<u64>,
    pub network_up_bps: Option<u64>,
    pub fallback: bool,
}

fn cpu_percent<'de, D: serde::Deserializer<'de>>(decoder: D) -> Result<Option<f32>, D::Error> {
    // Tagged enums buffer fields. Number also decodes serde_json's buffered
    // arbitrary-precision representation, which a bare f32 cannot consume.
    let value = Option::<serde_json::Number>::deserialize(decoder)?
        .map(|number| {
            number
                .to_string()
                .parse::<f32>()
                .map_err(serde::de::Error::custom)
        })
        .transpose()?;
    checked_cpu_percent(value).map_err(serde::de::Error::custom)
}

fn checked_cpu_percent(value: Option<f32>) -> Result<Option<f32>, &'static str> {
    if value.is_some_and(|value| !value.is_finite() || !(0.0..=100.0).contains(&value)) {
        return Err("CPU percentage must be finite and between 0 and 100.");
    }
    Ok(value)
}

fn serialize_cpu_percent<S: serde::Serializer>(
    value: &Option<f32>,
    encoder: S,
) -> Result<S::Ok, S::Error> {
    checked_cpu_percent(*value)
        .map_err(serde::ser::Error::custom)?
        .serialize(encoder)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    pub used_mb: u64,
    pub total_mb: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
    InvalidRequest,
    UnsupportedVersion,
    PermissionDenied,
    ProviderUnavailable,
    ProviderFailure,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, thiserror::Error)]
#[error("{message}")]
#[serde(deny_unknown_fields)]
pub struct Failure {
    pub code: ErrorCode,
    pub message: String,
}

impl Failure {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// The limit includes the terminating newline. An oversized or unterminated
/// frame poisons the connection; callers must not try to drain arbitrary input.
pub async fn read_frame(
    reader: &mut (impl AsyncBufRead + Unpin),
    limit: usize,
) -> io::Result<Option<Vec<u8>>> {
    let mut frame = Vec::new();
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            return if frame.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Unterminated applet service frame.",
                ))
            };
        }
        let end = buffer.iter().position(|byte| *byte == b'\n');
        let count = end.map_or(buffer.len(), |index| index + 1);
        if count > limit.saturating_sub(frame.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Applet service frame is too large.",
            ));
        }
        frame.extend_from_slice(&buffer[..count]);
        reader.consume(count);
        if end.is_some() {
            frame.pop();
            return Ok(Some(frame));
        }
    }
}

/// Serialize without allocating an unbounded intermediate response.
pub fn encode_frame(value: &impl Serialize, limit: usize) -> io::Result<Vec<u8>> {
    struct Buffer {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl io::Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Applet service frame is too large.",
                ));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut buffer = Buffer {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut buffer, value).map_err(io::Error::other)?;
    io::Write::write_all(&mut buffer, b"\n")?;
    Ok(buffer.bytes)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/applet/protocol.rs"
    ));
}
