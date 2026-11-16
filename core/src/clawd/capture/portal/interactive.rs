use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, Proxy};

use crate::clawd::wire::bounded::Text;

const SETUP_TIMEOUT: Duration = Duration::from_secs(10);
pub(in crate::clawd::capture) const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
pub(in crate::clawd::capture) const REPORT_BYTES: usize = 16 * 1024;
const DESTINATION: &str = "org.freedesktop.portal.Desktop";
const REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::clawd::capture) enum Outcome {
    Saved { path: Text<4096> },
    Clipboard,
    Cancelled,
    Failed { error: Text<2048> },
}

impl Outcome {
    pub(in crate::clawd::capture) fn label(&self) -> &'static str {
        match self {
            Self::Saved { .. } => "saved",
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

    fn cancelled() -> Self {
        Self {
            outcome: Outcome::Cancelled,
            portal_retired: true,
        }
    }
}

pub fn helper(args: &[String]) -> Result<(), String> {
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
            let report = interact(modal, &mut control).await;
            let bytes = serde_json::to_vec(&report).map_err(|error| error.to_string())?;
            if bytes.len() > REPORT_BYTES {
                return Err("Capture report exceeds its bound".into());
            }
            tokio::time::timeout(CLOSE_TIMEOUT, async {
                output.write_u32(bytes.len() as u32).await?;
                output.write_all(&bytes).await?;
                output.shutdown().await
            })
            .await
            .map_err(|_| "Capture report timed out")?
            .map_err(|error| error.to_string())
        })
}

struct Prepared {
    portal: Proxy<'static>,
    request: Proxy<'static>,
    responses: zbus::proxy::SignalStream<'static>,
    expected: OwnedObjectPath,
    token: String,
}

impl Prepared {
    async fn new() -> Result<Self, String> {
        let connection = Connection::session()
            .await
            .map_err(|error| error.to_string())?;
        let portal = Proxy::new(
            &connection,
            DESTINATION,
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Screenshot",
        )
        .await
        .map_err(|error| error.to_string())?;
        let version = portal
            .get_property::<u32>("version")
            .await
            .map_err(|error| error.to_string())?;
        if version < 2 {
            return Err("Capture portal does not support human interactive selection".into());
        }
        let destination = zbus::fdo::DBusProxy::new(&connection)
            .await
            .map_err(|error| error.to_string())?
            .get_name_owner(DESTINATION.try_into().expect("fixed portal bus name"))
            .await
            .map_err(|error| error.to_string())?;
        let portal = Proxy::new(
            &connection,
            destination.clone(),
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Screenshot",
        )
        .await
        .map_err(|error| error.to_string())?;
        let sender = connection
            .unique_name()
            .ok_or("Capture portal has no unique name")?
            .as_str()
            .trim_start_matches(':')
            .replace('.', "_");
        let token = format!("claw_capture_{}", uuid::Uuid::new_v4().simple());
        let expected = OwnedObjectPath::try_from(format!(
            "/org/freedesktop/portal/desktop/request/{sender}/{token}",
        ))
        .map_err(|error| error.to_string())?;
        let request = Proxy::new(
            &connection,
            destination,
            expected.clone(),
            REQUEST_INTERFACE,
        )
        .await
        .map_err(|error| error.to_string())?;
        let responses = request
            .receive_signal("Response")
            .await
            .map_err(|error| error.to_string())?;
        Ok(Self {
            portal,
            request,
            responses,
            expected,
            token,
        })
    }

    async fn dispatch(&self, modal: bool) -> Result<OwnedObjectPath, String> {
        let options = HashMap::from([
            ("handle_token", Value::from(self.token.as_str())),
            ("interactive", Value::Bool(true)),
            ("modal", Value::Bool(modal)),
        ]);
        self.portal
            .call("Screenshot", &("", options))
            .await
            .map_err(|error| error.to_string())
    }

    async fn close(&self, outcome: Outcome) -> Report {
        match tokio::time::timeout(CLOSE_TIMEOUT, self.request.call::<_, _, ()>("Close", &())).await
        {
            Ok(Ok(())) => Report {
                outcome,
                portal_retired: true,
            },
            Ok(Err(_)) => Report::failed(
                "Capture portal did not acknowledge Close; cleanup is pending",
                false,
            ),
            Err(_) => Report::failed("Capture portal Close timed out; cleanup is pending", false),
        }
    }
}

async fn interact(modal: bool, control: &mut tokio::net::unix::OwnedReadHalf) -> Report {
    // EOF and malformed control also request retirement; neither is consent.
    let cancellation = async {
        let _ = control.read_u8().await;
    };
    tokio::pin!(cancellation);
    let mut request = tokio::select! {
        biased;
        _ = &mut cancellation => return Report::cancelled(),
        prepared = tokio::time::timeout(SETUP_TIMEOUT, Prepared::new()) => match prepared {
            Ok(Ok(request)) => request,
            Ok(Err(error)) => return Report::failed(&error, true),
            Err(_) => return Report::failed("Capture portal setup timed out", true),
        },
    };
    // Neither Screenshot nor Response gets a short tool timeout: portals can
    // defer the method reply while showing human UI. Root owns the live lease.
    let dispatched = {
        let dispatch = request.dispatch(modal);
        tokio::pin!(dispatch);
        tokio::select! {
            biased;
            result = &mut dispatch => result,
            _ = &mut cancellation => return request.close(Outcome::Cancelled).await,
        }
    };
    match dispatched {
        Ok(actual) if actual == request.expected => {}
        Ok(_) => {
            let _ = request.close(Outcome::Cancelled).await;
            return Report::failed(
                "Capture portal returned an unrelated handle; cleanup is pending",
                false,
            );
        }
        Err(error) => {
            return request.close(Report::failed(&error, false).outcome).await;
        }
    }
    let response = tokio::select! {
        biased;
        response = request.responses.next() => response,
        _ = &mut cancellation => return request.close(Outcome::Cancelled).await,
    };
    let Some(response) = response else {
        return request
            .close(Report::failed("Capture portal ended without a response", false).outcome)
            .await;
    };
    let response: Result<(u32, HashMap<String, OwnedValue>), _> = response.body().deserialize();
    match response {
        Ok((0, mut values)) => Report {
            outcome: match values
                .remove("uri")
                .and_then(|value| String::try_from(value).ok())
                .ok_or("Capture portal omitted its destination")
                .and_then(|uri| destination(&uri))
            {
                Ok(outcome) => outcome,
                Err(error) => Report::failed(error, true).outcome,
            },
            portal_retired: true,
        },
        Ok((1, _)) => Report::cancelled(),
        Ok((2, _)) => Report::failed("Capture portal refused the request", true),
        _ => {
            request
                .close(Report::failed("Capture portal returned an invalid response", false).outcome)
                .await
        }
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
