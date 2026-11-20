use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt;
use serde::Serialize;
use tokio::io::AsyncReadExt;
use zbus::zvariant::{DynamicType, OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, Proxy};

const SETUP_TIMEOUT: Duration = Duration::from_secs(10);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
const DESTINATION: &str = "org.freedesktop.portal.Desktop";
const REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";

pub(in crate::clawd) enum PortalResult {
    Success(HashMap<String, OwnedValue>),
    Cancelled,
    Failed(String),
}

pub(in crate::clawd) struct PortalReply {
    pub result: PortalResult,
    pub portal_retired: bool,
}

impl PortalReply {
    fn failed(error: impl Into<String>, portal_retired: bool) -> Self {
        Self { result: PortalResult::Failed(error.into()), portal_retired }
    }

    fn cancelled() -> Self {
        Self { result: PortalResult::Cancelled, portal_retired: true }
    }
}

struct Prepared {
    portal: Proxy<'static>,
    request: Proxy<'static>,
    responses: zbus::proxy::SignalStream<'static>,
    expected: OwnedObjectPath,
    token: String,
}

impl Prepared {
    async fn new(interface: &'static str, minimum_version: u32) -> Result<Self, String> {
        let connection = Connection::session().await.map_err(|error| error.to_string())?;
        let portal = Proxy::new(&connection, DESTINATION, "/org/freedesktop/portal/desktop", interface)
            .await.map_err(|error| error.to_string())?;
        let version = portal.get_property::<u32>("version").await.map_err(|error| error.to_string())?;
        if version < minimum_version { return Err("OS portal does not support this interaction".into()); }
        let owner = zbus::fdo::DBusProxy::new(&connection).await.map_err(|error| error.to_string())?
            .get_name_owner(DESTINATION.try_into().expect("fixed portal bus name"))
            .await.map_err(|error| error.to_string())?;
        let portal = Proxy::new(&connection, owner.clone(), "/org/freedesktop/portal/desktop", interface)
            .await.map_err(|error| error.to_string())?;
        let sender = connection.unique_name().ok_or("OS portal has no unique name")?
            .as_str().trim_start_matches(':').replace('.', "_");
        let token = format!("claw_portal_{}", uuid::Uuid::new_v4().simple());
        let expected = OwnedObjectPath::try_from(format!(
            "/org/freedesktop/portal/desktop/request/{sender}/{token}",
        )).map_err(|error| error.to_string())?;
        let request = Proxy::new(&connection, owner, expected.clone(), REQUEST_INTERFACE)
            .await.map_err(|error| error.to_string())?;
        let responses = request.receive_signal("Response").await.map_err(|error| error.to_string())?;
        Ok(Self { portal, request, responses, expected, token })
    }

    async fn close(&self, result: PortalResult) -> PortalReply {
        close_request(&self.request, result).await
    }
}

/// Caller-owned authority admits byte1 before entering this helper. The returned
/// evidence is only portal retirement; the parent must separately observe exit.
pub(in crate::clawd) async fn owner_portal_request<B: Serialize + DynamicType + Sync>(
    control: &mut tokio::net::unix::OwnedReadHalf,
    interface: &'static str,
    minimum_version: u32,
    method: &'static str,
    arguments: impl FnOnce(String) -> B,
) -> PortalReply {
    let cancellation = async { let _ = control.read_u8().await; };
    tokio::pin!(cancellation);
    let mut request = tokio::select! {
        biased;
        _ = &mut cancellation => return PortalReply::cancelled(),
        prepared = tokio::time::timeout(SETUP_TIMEOUT, Prepared::new(interface, minimum_version)) => match prepared {
            Ok(Ok(request)) => request,
            Ok(Err(error)) => return PortalReply::failed(error, true),
            Err(_) => return PortalReply::failed("OS portal setup timed out", true),
        },
    };
    let body = arguments(request.token.clone());
    // Both method reply and Response can include human interaction.
    let dispatched = {
        let dispatch = request.portal.call::<_, _, OwnedObjectPath>(method, &body);
        tokio::pin!(dispatch);
        tokio::select! {
            biased;
            result = &mut dispatch => result,
            _ = &mut cancellation => return request.close(PortalResult::Cancelled).await,
        }
    };
    match dispatched {
        Ok(actual) if actual == request.expected => {}
        Ok(_) => {
            let _ = request.close(PortalResult::Cancelled).await;
            return PortalReply::failed("OS portal returned an unrelated handle; cleanup is pending", false);
        }
        Err(error) => return request.close(PortalResult::Failed(error.to_string())).await,
    }
    let response = tokio::select! {
        biased;
        response = request.responses.next() => response,
        _ = &mut cancellation => return request.close(PortalResult::Cancelled).await,
    };
    let Some(response) = response else {
        return request.close(PortalResult::Failed("OS portal ended without a response".into())).await;
    };
    match response.body().deserialize::<(u32, HashMap<String, OwnedValue>)>() {
        Ok((0, values)) => PortalReply { result: PortalResult::Success(values), portal_retired: true },
        Ok((1, _)) => PortalReply::cancelled(),
        Ok((2, _)) => PortalReply::failed("OS portal refused the request", true),
        _ => request.close(PortalResult::Failed("OS portal returned an invalid response".into())).await,
    }
}

async fn close_request(request: &Proxy<'_>, result: PortalResult) -> PortalReply {
    match tokio::time::timeout(CLOSE_TIMEOUT, request.call::<_, _, ()>("Close", &())).await {
        Ok(Ok(())) => PortalReply { result, portal_retired: true },
        Ok(Err(_)) => PortalReply::failed("OS portal did not acknowledge Close; cleanup is pending", false),
        Err(_) => PortalReply::failed("OS portal Close timed out; cleanup is pending", false),
    }
}

/// File selection must not wait for unrelated screenshot and screencast backends.
pub(in crate::clawd) async fn owner_file_chooser_request(
    control: &mut tokio::net::unix::OwnedReadHalf,
    save: bool,
    title: &str,
    options: HashMap<&str, Value<'_>>,
) -> PortalReply {
    const GTK: &str = "org.freedesktop.impl.portal.desktop.gtk";
    const PROGRAM: &str = "/usr/libexec/xdg-desktop-portal-gtk";
    let cancellation = async { let _ = control.read_u8().await; };
    tokio::pin!(cancellation);
    let prepare = async {
        let connection = Connection::session().await.map_err(|error| error.to_string())?;
        let bus = zbus::fdo::DBusProxy::new(&connection).await.map_err(|error| error.to_string())?;
        let name = zbus::names::WellKnownName::try_from(GTK).expect("fixed chooser service");
        bus.start_service_by_name(name.clone(), 0).await.map_err(|error| error.to_string())?;
        let owner = bus.get_name_owner(name.into()).await.map_err(|error| error.to_string())?;
        let uid = bus.get_connection_unix_user(owner.clone().into()).await.map_err(|error| error.to_string())?;
        let pid = bus.get_connection_unix_process_id(owner.clone().into()).await.map_err(|error| error.to_string())?;
        let program = std::path::Path::new(PROGRAM);
        crate::provenance::fsec::require_secure_location(program, &[0]).map_err(|error| error.to_string())?;
        if uid != unsafe { libc::geteuid() }
            || std::fs::read_link(format!("/proc/{pid}/exe")).map_err(|error| error.to_string())? != program
        {
            return Err("File chooser is not the installed OS provider for this account".into());
        }
        let path = OwnedObjectPath::try_from(format!(
            "/org/freedesktop/portal/desktop/request/claw_{}", uuid::Uuid::new_v4().simple(),
        )).map_err(|error| error.to_string())?;
        let picker = Proxy::new(
            &connection, owner.clone(), "/org/freedesktop/portal/desktop", "org.freedesktop.impl.portal.FileChooser",
        ).await.map_err(|error| error.to_string())?;
        let request = Proxy::new(
            &connection, owner, path.clone(), "org.freedesktop.impl.portal.Request",
        ).await.map_err(|error| error.to_string())?;
        Ok::<_, String>((picker, request, path))
    };
    let (picker, request, path) = tokio::select! {
        biased;
        _ = &mut cancellation => return PortalReply::cancelled(),
        prepared = tokio::time::timeout(SETUP_TIMEOUT, prepare) => match prepared {
            Ok(Ok(prepared)) => prepared,
            _ => return PortalReply::failed("The system file chooser is unavailable", true),
        },
    };
    let method = if save { "SaveFile" } else { "OpenFile" };
    let args = (path, "", "", title, options);
    let reply = picker.call::<_, _, (u32, HashMap<String, OwnedValue>)>(method, &args);
    tokio::select! {
        biased;
        result = reply => match result {
            Ok((0, values)) => PortalReply { result: PortalResult::Success(values), portal_retired: true },
            Ok((1, _)) => PortalReply::cancelled(),
            Ok((_, _)) => PortalReply::failed("The system file chooser refused the request", true),
            Err(_) => close_request(&request, PortalResult::Failed("The system file chooser did not respond".into())).await,
        },
        _ = &mut cancellation => close_request(&request, PortalResult::Cancelled).await,
    }
}
