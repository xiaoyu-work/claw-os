use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt;
use serde::Serialize;
use tokio::io::AsyncReadExt;
use zbus::zvariant::{DynamicType, OwnedObjectPath, OwnedValue};
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
        match tokio::time::timeout(CLOSE_TIMEOUT, self.request.call::<_, _, ()>("Close", &())).await {
            Ok(Ok(())) => PortalReply { result, portal_retired: true },
            Ok(Err(_)) => PortalReply::failed("OS portal did not acknowledge Close; cleanup is pending", false),
            Err(_) => PortalReply::failed("OS portal Close timed out; cleanup is pending", false),
        }
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
