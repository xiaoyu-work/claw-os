use std::collections::HashMap;
use std::ffi::CStr;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use clawd_client::system_review::{ReviewDecision, ReviewStatus, SystemReview};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use zbus::fdo::DBusProxy;
use zbus::names::{BusName, OwnedUniqueName};
use zbus::zvariant::{OwnedValue, Value};
use zbus::{Connection, Proxy};

use super::{Failure, Session, AUTHORIZATION_TIMEOUT};

const ACTION: &str = "org.clawos.approval.decide";
const AUTHORITY: &str = "org.freedesktop.PolicyKit1";
const OBJECT_PATH: &str = "/org/clawos/AgentWeb/Authentication";
const BUS_TIMEOUT: Duration = Duration::from_secs(5);

struct Agent {
    session: Arc<Session>,
    authority: OwnedUniqueName,
}

#[zbus::interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl Agent {
    async fn begin_authentication(
        &self,
        action_id: &str,
        _message: &str,
        _icon_name: &str,
        _details: HashMap<String, String>,
        cookie: &str,
        identities: Vec<(String, HashMap<String, OwnedValue>)>,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        if header.sender().map(|sender| sender.as_str()) != Some(self.authority.as_str())
            || action_id != ACTION
            || cookie.len() > 4096
            || cookie
                .bytes()
                .any(|byte| matches!(byte, b'\0' | b'\r' | b'\n'))
            || !identities.iter().any(|(kind, fields)| {
                kind == "unix-user"
                    && fields
                        .get("uid")
                        .and_then(|value| u32::try_from(value).ok())
                        == Some(self.session.owner_uid)
            })
        {
            return Err(zbus::fdo::Error::AccessDenied(
                "OS authentication identity mismatch.".into(),
            ));
        }
        let name = username(self.session.owner_uid)
            .map_err(|_| zbus::fdo::Error::Failed("OS account could not be resolved.".into()))?;
        conversation(&self.session, &name, cookie)
            .await
            .map_err(|_| zbus::fdo::Error::Failed("OS authentication did not complete.".into()))
    }

    async fn cancel_authentication(
        &self,
        _cookie: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        if header.sender().map(|sender| sender.as_str()) != Some(self.authority.as_str()) {
            return Err(zbus::fdo::Error::AccessDenied(
                "Untrusted OS authentication caller.".into(),
            ));
        }
        self.session.cancel.send_replace(true);
        Ok(())
    }
}

pub(super) async fn authorize(
    session: Arc<Session>,
    review: &SystemReview,
    decision: &ReviewDecision,
) -> Result<(), Failure> {
    let registered = tokio::time::timeout(BUS_TIMEOUT, register(session.clone()))
        .await
        .map_err(|_| Failure::Unavailable("The OS authentication service did not respond."))??;
    let (connection, authority) = registered;
    let result = run_helper(session, review, decision).await;
    let unregister_body = (subject()?, OBJECT_PATH);
    match tokio::time::timeout(
        BUS_TIMEOUT,
        authority.call::<_, _, ()>("UnregisterAuthenticationAgent", &unregister_body),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(_)) => tracing::warn!("OS authentication agent could not unregister"),
        Err(_) => tracing::warn!("OS authentication agent cleanup timed out"),
    }
    drop(authority);
    match tokio::time::timeout(BUS_TIMEOUT, connection.close()).await {
        Ok(Ok(())) => {}
        _ => tracing::warn!("OS authentication connection did not close cleanly"),
    }
    result
}

async fn register(session: Arc<Session>) -> Result<(Connection, Proxy<'static>), Failure> {
    let stream = tokio::net::UnixStream::connect("/run/dbus/system_bus_socket")
        .await
        .map_err(|_| Failure::Unavailable("The OS system bus is unavailable."))?;
    let peer = stream
        .peer_cred()
        .map_err(|_| Failure::Unavailable("The OS system bus identity is unavailable."))?;
    if peer.uid() != 0 && peer.uid() != account_uid(c"messagebus")? {
        return Err(Failure::Unavailable(
            "The OS system bus identity is not trusted.",
        ));
    }
    let connection = zbus::connection::Builder::unix_stream(stream)
        .build()
        .await
        .map_err(|_| Failure::Unavailable("Could not connect to OS authentication."))?;
    let bus = DBusProxy::new(&connection)
        .await
        .map_err(|_| Failure::Unavailable("Could not inspect OS authentication."))?;
    let service = BusName::try_from(AUTHORITY)
        .map_err(|_| Failure::Unavailable("Invalid OS authentication service."))?;
    let owner = match bus.get_name_owner(service.clone()).await {
        Ok(owner) => owner,
        Err(zbus::fdo::Error::NameHasNoOwner(_)) => {
            bus.start_service_by_name(
                zbus::names::WellKnownName::try_from(AUTHORITY)
                    .map_err(|_| Failure::Unavailable("Invalid OS authentication service."))?,
                0,
            )
            .await
            .map_err(|_| Failure::Unavailable("The OS authentication service could not start."))?;
            bus.get_name_owner(service)
                .await
                .map_err(|_| Failure::Unavailable("The OS authentication service has no owner."))?
        }
        Err(_) => {
            return Err(Failure::Unavailable(
                "The OS authentication service is unavailable.",
            ))
        }
    };
    let uid = bus
        .get_connection_unix_user(owner.clone().into())
        .await
        .map_err(|_| Failure::Unavailable("The OS authentication identity is unavailable."))?;
    if uid != 0 && uid != account_uid(c"polkitd")? {
        return Err(Failure::Unavailable(
            "The OS authentication service is not trusted.",
        ));
    }
    connection
        .object_server()
        .at(
            OBJECT_PATH,
            Agent {
                session,
                authority: owner.clone(),
            },
        )
        .await
        .map_err(|_| Failure::Unavailable("Could not create the Web OS authentication agent."))?;
    let authority = zbus::proxy::Builder::<Proxy<'static>>::new(&connection)
        .destination(owner)
        .map_err(|_| Failure::Unavailable("Invalid OS authentication owner."))?
        .path("/org/freedesktop/PolicyKit1/Authority")
        .map_err(|_| Failure::Unavailable("Invalid OS authentication path."))?
        .interface("org.freedesktop.PolicyKit1.Authority")
        .map_err(|_| Failure::Unavailable("Invalid OS authentication interface."))?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .map_err(|_| Failure::Unavailable("Could not address OS authentication."))?;
    authority
        .call::<_, _, ()>(
            "RegisterAuthenticationAgent",
            &(subject()?, "C.UTF-8", OBJECT_PATH),
        )
        .await
        .map_err(|_| {
            Failure::Unavailable("Could not register the process-bound OS authentication agent.")
        })?;
    drop(bus);
    Ok((connection, authority))
}

fn subject() -> Result<(String, HashMap<String, Value<'static>>), Failure> {
    let pid = std::process::id();
    let start = crate::proc::read_start_time_ticks_pub(pid).ok_or(Failure::Unavailable(
        "The Web process identity is unavailable.",
    ))?;
    let mut fields = HashMap::new();
    fields.insert("pid".into(), Value::U32(pid));
    fields.insert("start-time".into(), Value::U64(start));
    fields.insert("uid".into(), Value::U32(unsafe { libc::geteuid() }));
    Ok(("unix-process".into(), fields))
}

async fn run_helper(
    session: Arc<Session>,
    review: &SystemReview,
    decision: &ReviewDecision,
) -> Result<(), Failure> {
    if *session.cancel.borrow() {
        return Err(Failure::Cancelled);
    }
    let bytes = decision
        .encode_for(review)
        .map_err(|_| Failure::Unavailable("The displayed OS review is no longer valid."))?;
    let mut child = tokio::process::Command::new("/usr/bin/pkexec")
        .arg("--disable-internal-agent")
        .arg("/usr/local/bin/claw-approval-helper")
        .arg("--system-review-json")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| Failure::Unavailable("Could not launch the OS approval helper."))?;
    let mut input = child
        .stdin
        .take()
        .ok_or(Failure::Unavailable("The OS approval helper has no input."))?;
    input
        .write_all(&bytes)
        .await
        .map_err(|_| Failure::Unavailable("Could not send the displayed OS decision."))?;
    drop(input);
    let mut cancel = session.cancel.subscribe();
    let output = tokio::select! {
        output = child.wait_with_output() => output
            .map_err(|_| Failure::Unavailable("The OS approval helper did not return an outcome."))?,
        _ = cancel.changed() => return Err(Failure::Cancelled),
        _ = tokio::time::sleep(AUTHORIZATION_TIMEOUT) => {
            session.cancel.send_replace(true);
            return Err(Failure::Expired);
        }
    };
    if !output.status.success() {
        return Err(Failure::Unavailable(
            "OS authentication was cancelled or the password was not accepted.",
        ));
    }
    let returned: SystemReview = serde_json::from_slice(&output.stdout)
        .map_err(|_| Failure::Unavailable("The OS approval helper returned an invalid outcome."))?;
    returned
        .validate()
        .map_err(|_| Failure::Unavailable("The OS approval helper returned an invalid review."))?;
    if returned.id != review.id || returned.status == ReviewStatus::Pending {
        return Err(Failure::Unavailable(
            "The OS decision was not confirmed. Refresh before deciding again.",
        ));
    }
    Ok(())
}

async fn conversation(session: &Session, name: &str, cookie: &str) -> Result<(), Failure> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    let socket = match std::fs::symlink_metadata("/run/polkit/agent-helper.socket") {
        Ok(metadata) if metadata.file_type().is_socket() && metadata.uid() == 0 => true,
        Ok(_) => {
            return Err(Failure::Unavailable(
                "The OS password verifier socket is not trusted.",
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => {
            return Err(Failure::Unavailable(
                "The OS password verifier socket could not be inspected.",
            ))
        }
    };
    if socket {
        let stream = tokio::net::UnixStream::connect("/run/polkit/agent-helper.socket")
            .await
            .map_err(|_| Failure::Unavailable("Could not connect to the OS password verifier."))?;
        if stream
            .peer_cred()
            .map_err(|_| Failure::Unavailable("OS verifier identity is unavailable."))?
            .uid()
            != 0
        {
            return Err(Failure::Unavailable(
                "The OS password verifier is not trusted.",
            ));
        }
        let (read, mut write) = stream.into_split();
        write
            .write_all(name.as_bytes())
            .await
            .map_err(|_| Failure::Unavailable("Could not identify the OS account."))?;
        write
            .write_all(b"\n")
            .await
            .map_err(|_| Failure::Unavailable("Could not identify the OS account."))?;
        return exchange(session, cookie, BufReader::new(read), write).await;
    }
    let mut selected = None;
    for path in [
        "/usr/lib/polkit-1/polkit-agent-helper-1",
        "/usr/libexec/polkit-agent-helper-1",
    ] {
        match std::fs::symlink_metadata(path) {
            Ok(metadata)
                if metadata.is_file()
                    && metadata.uid() == 0
                    && metadata.mode() & 0o022 == 0
                    && metadata.mode() & 0o4000 != 0 =>
            {
                selected = Some(path);
                break;
            }
            Ok(_) => {
                return Err(Failure::Unavailable(
                    "The OS password verifier has unsafe ownership or permissions.",
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(Failure::Unavailable(
                    "The OS password verifier could not be inspected.",
                ))
            }
        }
    }
    let path = selected.ok_or(Failure::Unavailable(
        "The OS password verifier is not installed.",
    ))?;
    let mut child = tokio::process::Command::new(path)
        .arg(name)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| Failure::Unavailable("Could not launch the OS password verifier."))?;
    let input = child
        .stdin
        .take()
        .ok_or(Failure::Unavailable("OS verifier input is unavailable."))?;
    let output = child
        .stdout
        .take()
        .ok_or(Failure::Unavailable("OS verifier output is unavailable."))?;
    exchange(session, cookie, BufReader::new(output), input).await
}

async fn exchange<R, W>(
    session: &Session,
    cookie: &str,
    mut read: R,
    mut write: W,
) -> Result<(), Failure>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    write
        .write_all(cookie.as_bytes())
        .await
        .map_err(|_| Failure::Unavailable("Could not begin OS authentication."))?;
    write
        .write_all(b"\n")
        .await
        .map_err(|_| Failure::Unavailable("Could not begin OS authentication."))?;
    write
        .flush()
        .await
        .map_err(|_| Failure::Unavailable("Could not begin OS authentication."))?;
    let mut cancel = session.cancel.subscribe();
    loop {
        if *cancel.borrow() {
            return Err(Failure::Cancelled);
        }
        let mut line = Vec::new();
        let mut bounded = (&mut read).take(4097);
        let size = tokio::select! {
            size = bounded.read_until(b'\n', &mut line) => size
                .map_err(|_| Failure::Unavailable("The OS verifier conversation failed."))?,
            _ = cancel.changed() => return Err(Failure::Cancelled),
        };
        if size == 0 || size > 4096 {
            return Err(Failure::Unavailable(
                "The OS verifier returned an invalid conversation.",
            ));
        }
        let text = std::str::from_utf8(&line)
            .map_err(|_| Failure::Unavailable("The OS verifier returned invalid text."))?;
        let (event, message) = text
            .trim_end()
            .split_once(' ')
            .unwrap_or((text.trim_end(), ""));
        match event {
            "PAM_PROMPT_ECHO_OFF" | "PAM_PROMPT_ECHO_ON" => {
                let response = session
                    .prompt(message.to_string())
                    .await
                    .map_err(|_| Failure::Unavailable("The OS password prompt was closed."))?;
                write.write_all(response.as_bytes()).await.map_err(|_| {
                    Failure::Unavailable("Could not send the response to the OS verifier.")
                })?;
                write.write_all(b"\n").await.map_err(|_| {
                    Failure::Unavailable("Could not send the response to the OS verifier.")
                })?;
                write.flush().await.map_err(|_| {
                    Failure::Unavailable("Could not send the response to the OS verifier.")
                })?;
            }
            "PAM_ERROR_MSG" | "PAM_TEXT_INFO" => {}
            "SUCCESS" => return Ok(()),
            "FAILURE" => return Err(Failure::Unavailable("The OS password was not accepted.")),
            _ => {
                return Err(Failure::Unavailable(
                    "The OS verifier protocol was not recognized.",
                ))
            }
        }
    }
}

fn username(uid: u32) -> Result<String, Failure> {
    let mut record = std::mem::MaybeUninit::<libc::passwd>::zeroed();
    let mut buffer = vec![0_u8; 65536];
    let mut result = std::ptr::null_mut();
    let code = unsafe {
        libc::getpwuid_r(
            uid,
            record.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if code != 0 || result.is_null() {
        return Err(Failure::Unavailable(
            "The authenticated OS account could not be resolved.",
        ));
    }
    let record = unsafe { record.assume_init() };
    let name = unsafe { CStr::from_ptr(record.pw_name) }
        .to_str()
        .map_err(|_| Failure::Unavailable("The OS account name is invalid."))?;
    if name
        .bytes()
        .any(|byte| matches!(byte, b'\0' | b'\r' | b'\n'))
    {
        return Err(Failure::Unavailable("The OS account name is invalid."));
    }
    Ok(name.to_string())
}

fn account_uid(name: &CStr) -> Result<u32, Failure> {
    let mut record = std::mem::MaybeUninit::<libc::passwd>::zeroed();
    let mut buffer = vec![0_u8; 65536];
    let mut result = std::ptr::null_mut();
    let code = unsafe {
        libc::getpwnam_r(
            name.as_ptr(),
            record.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if code != 0 || result.is_null() {
        return Err(Failure::Unavailable(
            "The OS service account could not be resolved.",
        ));
    }
    Ok(unsafe { record.assume_init() }.pw_uid)
}
