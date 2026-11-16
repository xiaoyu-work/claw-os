use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zbus::zvariant::Value;

use crate::clawd::wire::bounded::Text;

const SETUP_TIMEOUT: Duration = Duration::from_secs(10);
pub(in crate::clawd::capture) const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
pub(in crate::clawd::capture) const REPORT_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::clawd::capture) enum Outcome {
    Saved { path: Text<4096> },
    Image { length: u64 },
    Clipboard,
    Cancelled,
    Failed { error: Text<2048> },
}

impl Outcome {
    pub(in crate::clawd::capture) fn label(&self) -> &'static str {
        match self {
            Self::Saved { .. } => "saved",
            Self::Image { .. } => "image",
            Self::Clipboard => "clipboard",
            Self::Cancelled => "cancelled",
            Self::Failed { .. } => "failed",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::clawd::capture) struct Report {
    pub outcome: Outcome,
    pub portal_retired: bool,
}

impl Report {
    pub(in crate::clawd::capture) fn failed(error: &str, portal_retired: bool) -> Self {
        let error = error
            .chars()
            .filter(|ch| !ch.is_control())
            .take(512)
            .collect::<String>();
        Self {
            outcome: Outcome::Failed {
                error: Text::parse(&error).expect("bounded capture diagnostic"),
            },
            portal_retired,
        }
    }
}

pub fn helper(args: &[String]) -> Result<(), String> {
    run_helper(args, false)
}

pub fn image_helper(args: &[String]) -> Result<(), String> {
    run_helper(args, true)
}

fn run_helper(args: &[String], image: bool) -> Result<(), String> {
    let (modal, gate) = super::authenticated_gate(args)?;
    if unsafe { libc::geteuid() } == 0
        || unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) } != 1
    {
        return Err("interactive Capture requires the non-root no-new-privileges owner".into());
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?
        .block_on(async {
            let mut gate =
                tokio::net::UnixStream::from_std(gate).map_err(|error| error.to_string())?;
            if tokio::time::timeout(SETUP_TIMEOUT, gate.read_u8())
                .await
                .map_err(|_| "Capture dispatch timed out")?
                .map_err(|error| error.to_string())?
                != 1
            {
                return Err("Capture dispatch was not authorized by root clawd".into());
            }
            let (mut control, mut output) = gate.into_split();
            let mut report = interact_mode(modal, &mut control, !image).await;
            let mut png = Vec::new();
            if image {
                report = match &report.outcome {
                    Outcome::Saved { path } => {
                        match super::consume_png(std::path::Path::new(path.as_str())) {
                            Ok(bytes) => {
                                png = bytes;
                                Report {
                                    outcome: Outcome::Image {
                                        length: png.len() as u64,
                                    },
                                    portal_retired: report.portal_retired,
                                }
                            }
                            Err(error) => Report::failed(&error, report.portal_retired),
                        }
                    }
                    Outcome::Clipboard => Report::failed(
                        "non-interactive capture cannot use the clipboard",
                        report.portal_retired,
                    ),
                    _ => report,
                };
            }
            let bytes = serde_json::to_vec(&report).map_err(|error| error.to_string())?;
            if bytes.len() > REPORT_BYTES {
                return Err("Capture report exceeds its bound".into());
            }
            tokio::time::timeout(
                if image {
                    super::super::CAPTURE_TIMEOUT
                } else {
                    CLOSE_TIMEOUT
                },
                async {
                    output.write_u32(bytes.len() as u32).await?;
                    output.write_all(&bytes).await?;
                    output.write_all(&png).await?;
                    output.shutdown().await
                },
            )
            .await
            .map_err(|_| "Capture report timed out")?
            .map_err(|error| error.to_string())
        })
}

#[cfg(test)]
async fn interact(modal: bool, control: &mut tokio::net::unix::OwnedReadHalf) -> Report {
    interact_mode(modal, control, true).await
}

async fn interact_mode(
    modal: bool,
    control: &mut tokio::net::unix::OwnedReadHalf,
    interactive: bool,
) -> Report {
    use crate::clawd::desktop::{owner_portal_request, PortalReply, PortalResult};
    let response: PortalReply = owner_portal_request(
        control,
        "org.freedesktop.portal.Screenshot",
        if interactive { 2 } else { 1 },
        "Screenshot",
        move |token| {
            (
                String::new(),
                HashMap::from([
                    ("handle_token", Value::from(token)),
                    ("interactive", Value::Bool(interactive)),
                    ("modal", Value::Bool(modal)),
                ]),
            )
        },
    )
    .await;
    let outcome = match response.result {
        PortalResult::Success(mut values) => match values
            .remove("uri")
            .and_then(|value| String::try_from(value).ok())
            .ok_or("Capture portal omitted its destination")
            .and_then(|uri| destination(&uri))
        {
            Ok(outcome) => outcome,
            Err(error) => Report::failed(error, response.portal_retired).outcome,
        },
        PortalResult::Cancelled => Outcome::Cancelled,
        PortalResult::Failed(error) => Report::failed(&error, response.portal_retired).outcome,
    };
    Report {
        outcome,
        portal_retired: response.portal_retired,
    }
}

fn destination(uri: &str) -> Result<Outcome, &'static str> {
    if uri.len() > 8192 {
        return Err("Capture destination exceeds its bound");
    }
    let uri = url::Url::parse(uri).map_err(|_| "Capture portal returned an invalid destination")?;
    match uri.scheme() {
        "clipboard" => Ok(Outcome::Clipboard),
        "file" => {
            let path = uri
                .to_file_path()
                .map_err(|_| "Capture destination is not a local file")?;
            let path = path
                .to_str()
                .ok_or("Capture destination must be UTF-8 metadata")?;
            if !std::path::Path::new(path).is_absolute() || path.chars().any(char::is_control) {
                return Err("Capture destination is not an absolute file path");
            }
            Ok(Outcome::Saved {
                path: Text::parse(path).map_err(|_| "Capture destination exceeds its bound")?,
            })
        }
        _ => Err("Capture portal returned an unsupported destination"),
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/capture/portal/interactive.rs"
    ));
}
