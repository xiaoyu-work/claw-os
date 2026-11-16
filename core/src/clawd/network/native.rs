use std::collections::HashMap;
use std::future::Future;
use std::io::Write;
use std::net::Ipv4Addr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use zbus::fdo::DBusProxy;
use zbus::names::{BusName, OwnedUniqueName};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Str};
use zbus::{Connection, Proxy};

use crate::caps::{Scope, Verb};
use crate::clawd::authority::Decision;
use crate::clawd::client_identity::ClientIdentity;
use crate::clawd::wire::bounded::{FileText, Name, Text, Token};

const SERVICE: &str = "org.freedesktop.NetworkManager";
const BASE: &str = "/org/freedesktop/NetworkManager";
const SETTINGS: &str = "/org/freedesktop/NetworkManager/Settings";
const CALL: Duration = Duration::from_secs(8);
pub const MAX_INPUT: usize = 196 * 1024;
type Settings = HashMap<String, HashMap<String, OwnedValue>>;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    session: Token,
    action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target: Option<Text<1024>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    credential: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    interface: Option<Text<15>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile: Option<Profile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    config: Option<Import>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Kind {
    Ethernet,
    Wifi,
    Vpn,
    Wireguard,
}
impl Kind {
    fn scope(self) -> &'static str {
        match self {
            Self::Ethernet => "ethernet",
            Self::Wifi => "wifi",
            Self::Vpn | Self::Wireguard => "vpn",
        }
    }
    fn nm(self) -> &'static str {
        match self {
            Self::Ethernet => "802-3-ethernet",
            Self::Wifi => "802-11-wireless",
            Self::Vpn => "vpn",
            Self::Wireguard => "wireguard",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "802-3-ethernet" | "ethernet" => Self::Ethernet,
            "802-11-wireless" | "wifi" => Self::Wifi,
            "vpn" => Self::Vpn,
            "wireguard" => Self::Wireguard,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Profile {
    #[serde(default)]
    uuid: Option<uuid::Uuid>,
    #[serde(default)]
    revision: Option<Text<64>>,
    kind: Kind,
    name: Text<255>,
    #[serde(default)]
    interface: Option<Text<15>>,
    autoconnect: bool,
    ipv4_method: Text<16>,
    #[serde(default)]
    ipv4_addresses: Vec<Text<32>>,
    #[serde(default)]
    ipv4_gateway: Option<Ipv4Addr>,
    #[serde(default)]
    ipv4_dns: Vec<Ipv4Addr>,
    #[serde(default)]
    ssid: Option<Text<32>>,
    #[serde(default)]
    security: Option<Text<16>>,
    #[serde(default)]
    identity: Option<Text<255>>,
    #[serde(default)]
    username: Option<Text<255>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Import {
    kind: Kind,
    name: Text<255>,
    content: FileText,
}

pub fn cli_request(session: &str, action: &str, input: &str) -> Result<Value, String> {
    if input.len() > MAX_INPUT {
        return Err("network request exceeds its byte limit".into());
    }
    let mut value: Value =
        serde_json::from_str(input).map_err(|_| "invalid network request JSON")?;
    let object = value
        .as_object_mut()
        .ok_or("network request must be an object")?;
    if object.contains_key("session") || object.contains_key("action") {
        return Err("network session/action are derived by the CLI".into());
    }
    object.insert("session".into(), json!(session));
    object.insert("action".into(), json!(action));
    let request: Request =
        serde_json::from_value(value).map_err(|_| "invalid closed network request")?;
    validate(&request)?;
    serde_json::to_value(request).map_err(|_| "encode network request".into())
}

pub(super) fn handles(action: &str) -> bool {
    matches!(
        action,
        "native-status"
            | "profile-read"
            | "profile-save"
            | "profile-import"
            | "wifi-share"
            | "ethernet-up"
            | "ethernet-down"
            | "ethernet-remove"
            | "wifi-up"
            | "wifi-down"
            | "wifi-remove"
            | "vpn-activate"
            | "vpn-deactivate"
            | "vpn-remove"
            | "wifi-join"
            | "vpn-connect"
    )
}

fn validate(request: &Request) -> Result<(), String> {
    let action = request.action.as_str();
    if request.state.is_some() {
        return Err("native network requests do not accept state".into());
    }
    if let Some(interface) = &request.interface {
        validate_interface(interface.as_str())?;
        if !matches!(action, "wifi-up" | "wifi-join" | "ethernet-up") {
            return Err("only Wi-Fi/Ethernet activation accepts an interface".into());
        }
    }
    match action {
        "native-status"
            if request.target.is_none()
                && request.profile.is_none()
                && request.config.is_none()
                && request.credential.is_none() =>
        {
            Ok(())
        }
        "profile-save"
            if request.target.is_none()
                && request.config.is_none()
                && request.credential.is_none() =>
        {
            validate_profile(request.profile.as_ref().ok_or("profile is required")?)
        }
        "profile-import"
            if request.target.is_none()
                && request.profile.is_none()
                && request.credential.is_none() =>
        {
            validate_import(request.config.as_ref().ok_or("config is required")?)
        }
        "wifi-join" if request.target.is_none() && request.config.is_none() => {
            let profile = request
                .profile
                .as_ref()
                .ok_or("Wi-Fi profile is required")?;
            validate_profile(profile)?;
            if profile.kind != Kind::Wifi {
                return Err("Wi-Fi activation cannot modify another profile category".into());
            }
            match &request.credential {
                Some(reference) => {
                    super::parse_credential_ref(reference.as_str())?;
                }
                None if profile.security.as_ref().map(Text::as_str) != Some("open") => {
                    return Err("protected Wi-Fi requires a credential reference".into())
                }
                None => {}
            }
            Ok(())
        }
        "vpn-connect" if request.config.is_none() && request.interface.is_none() => {
            let target = uuid::Uuid::parse_str(
                request
                    .target
                    .as_ref()
                    .ok_or("VPN UUID is required")?
                    .as_str(),
            )
            .map_err(|_| "invalid VPN UUID")?;
            if let Some(profile) = &request.profile {
                validate_profile(profile)?;
                if profile.kind.scope() != "vpn" || profile.uuid != Some(target) {
                    return Err("VPN edits must match the selected profile".into());
                }
            }
            if let Some(reference) = &request.credential {
                super::parse_credential_ref(reference.as_str())?;
            }
            Ok(())
        }
        "profile-read" | "wifi-share" | "ethernet-up" | "ethernet-down" | "ethernet-remove"
        | "wifi-up" | "wifi-down" | "wifi-remove" | "vpn-activate" | "vpn-deactivate"
        | "vpn-remove"
            if request.profile.is_none() && request.config.is_none() =>
        {
            uuid::Uuid::parse_str(
                request
                    .target
                    .as_ref()
                    .ok_or("profile UUID is required")?
                    .as_str(),
            )
            .map_err(|_| "invalid profile UUID")?;
            if let Some(credential) = &request.credential {
                if !matches!(action, "wifi-up" | "vpn-activate") {
                    return Err("this network action does not accept a credential".into());
                }
                super::parse_credential_ref(credential.as_str())?;
            }
            Ok(())
        }
        _ => Err("unsupported or conflicting native network request fields".into()),
    }
}

fn validate_profile(profile: &Profile) -> Result<(), String> {
    super::validate_text("profile name", profile.name.as_str(), 255)?;
    if let Some(interface) = &profile.interface {
        validate_interface(interface.as_str())?;
    }
    if profile.uuid.is_some() != profile.revision.is_some() {
        return Err("existing profiles require the displayed revision".into());
    }
    if profile.revision.as_ref().is_some_and(|revision| {
        revision.as_str().len() != 64
            || !revision
                .as_str()
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
    }) {
        return Err("invalid profile revision".into());
    }
    if profile.uuid.is_none() && matches!(profile.kind, Kind::Vpn | Kind::Wireguard) {
        return Err("new VPN profiles require the bounded import operation".into());
    }
    if !matches!(
        profile.ipv4_method.as_str(),
        "auto" | "manual" | "disabled" | "link-local" | "shared"
    ) || profile.ipv4_addresses.len() > 8
        || profile.ipv4_dns.len() > 8
        || (profile.ipv4_method.as_str() == "manual" && profile.ipv4_addresses.is_empty())
    {
        return Err("invalid or unbounded IPv4 settings".into());
    }
    for address in &profile.ipv4_addresses {
        parse_address(address.as_str())?;
    }
    match profile.kind {
        Kind::Wifi => {
            super::validate_text(
                "SSID",
                profile
                    .ssid
                    .as_ref()
                    .ok_or("Wi-Fi profile requires SSID")?
                    .as_str(),
                32,
            )?;
            if !matches!(
                profile.security.as_ref().map(Text::as_str),
                Some("open" | "psk" | "eap")
            ) {
                return Err("unsupported Wi-Fi security".into());
            }
            if profile.uuid.is_none()
                && profile.security.as_ref().map(Text::as_str) == Some("eap")
                && profile
                    .identity
                    .as_ref()
                    .is_none_or(|value| value.as_str().is_empty())
            {
                return Err("EAP identity is required".into());
            }
        }
        _ if profile.ssid.is_some() || profile.security.is_some() || profile.identity.is_some() => {
            return Err("Wi-Fi fields require a Wi-Fi profile".into())
        }
        _ => {}
    }
    if profile.username.is_some() && profile.kind != Kind::Vpn {
        return Err("VPN username requires an OpenVPN profile".into());
    }
    Ok(())
}

fn validate_interface(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 15
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        || value.starts_with(['.', '-'])
    {
        return Err("invalid network interface name".into());
    }
    Ok(())
}
fn parse_address(value: &str) -> Result<(Ipv4Addr, u32), String> {
    let (address, prefix) = value
        .split_once('/')
        .ok_or("IPv4 address requires a prefix")?;
    let address = address.parse().map_err(|_| "invalid IPv4 address")?;
    let prefix = prefix
        .parse::<u32>()
        .ok()
        .filter(|value| *value <= 32)
        .ok_or("invalid IPv4 prefix")?;
    Ok((address, prefix))
}

pub(super) async fn control(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, String> {
    let request: Request =
        serde_json::from_value(params).map_err(|_| "invalid closed network request")?;
    validate(&request)?;
    let action = request.action.as_str();
    let category = category(&request);
    let (session, _proof) = authorize(&request, authority)?;
    let _guard = super::NETWORK_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let backend = Backend::system().await?;
    if action == "native-status" {
        return backend.snapshot().await;
    }
    if action == "profile-save" {
        return backend.save(request.profile.unwrap()).await;
    }
    if action == "profile-import" {
        return import_profile(&backend, request.config.unwrap()).await;
    }
    control_profile(request, client, session, backend, category).await
}

fn category(request: &Request) -> &'static str {
    let action = request.action.as_str();
    match action {
        "profile-save" => request.profile.as_ref().unwrap().kind.scope(),
        "profile-import" => "vpn",
        value if value.starts_with("ethernet-") => "ethernet",
        value if value.starts_with("wifi-") => "wifi",
        _ => "vpn",
    }
}

fn authorize(
    request: &Request,
    authority: &Decision,
) -> Result<
    (
        crate::proc::SessionInfo,
        crate::clawd::authority::Authorized,
    ),
    String,
> {
    validate(request)?;
    let action = request.action.as_str();
    let target = request.target.as_ref().map(Text::as_str);
    let read = matches!(action, "native-status" | "profile-read" | "wifi-share");
    let category = category(request);
    let share_ref = if action == "wifi-share" {
        Some(format!("network.profile/{}", target.unwrap()))
    } else {
        None
    };
    let reference = share_ref
        .as_deref()
        .or_else(|| request.credential.as_ref().map(Name::as_str));
    super::authorize_session(
        authority,
        if read {
            Verb::SYS_OBSERVE
        } else {
            Verb::NET_MANAGE
        },
        Scope::name(if read { "network" } else { category }),
        reference,
    )
}

async fn control_profile(
    request: Request,
    client: &ClientIdentity,
    session: crate::proc::SessionInfo,
    backend: Backend,
    category: &str,
) -> Result<Value, String> {
    let action = request.action.as_str();
    let target = request.target.as_ref().map(Text::as_str);
    let password = match request.credential.as_ref() {
        Some(reference) => {
            let (namespace, name) = super::parse_credential_ref(reference.as_str())?;
            let password = crate::paths::with_user_override(
                client.require_uid()?,
                client.require_home_dir()?,
                async {
                    crate::credential::load_for_broker(
                        &name,
                        &namespace,
                        session.tier.unwrap_or(u8::MAX),
                    )
                },
            )
            .await?;
            if password.is_empty()
                || password.len() > 1024
                || password.chars().any(char::is_control)
            {
                return Err("stored network credential must be one bounded line".into());
            }
            Some(password)
        }
        None => None,
    };
    if action == "wifi-join" {
        let requested = request.profile.unwrap();
        let security = requested
            .security
            .as_ref()
            .map(Text::as_str)
            .map(str::to_string);
        if let Some(interface) = &request.interface {
            backend.device(interface.as_str(), Kind::Wifi, None).await?;
        }
        let saved = backend.save(requested).await?;
        let id = saved["profile"]["uuid"]
            .as_str()
            .ok_or("indeterminate: saved Wi-Fi UUID unavailable")?;
        activate(
            id,
            Kind::Wifi,
            security.as_deref(),
            password.as_deref(),
            request.interface.as_ref().map(Text::as_str),
        )
        .await
        .map_err(|_| {
            "indeterminate: Wi-Fi profile was saved; connection failed or is unconfirmed. Refresh before retrying."
        })?;
        let after = backend.target_state(id).await?;
        if after["active"] != true {
            return Err(
                "indeterminate: Wi-Fi profile was saved but the connection is not active".into(),
            );
        }
        return Ok(json!({"action":action,"target":id,"status":"applied","after":after}));
    }
    let (path, raw) = backend.find(target.unwrap()).await?;
    if action == "profile-read" {
        return Ok(json!({"profile":project(&raw)?}));
    }
    let profile = metadata(&raw)?;
    let kind = string(&raw, "connection", "type")
        .as_deref()
        .and_then(Kind::parse)
        .ok_or("unsupported profile type")?;
    if kind.scope() != category {
        return Err("profile belongs to another independently authorized network category".into());
    }
    if action == "wifi-share" {
        let proxy = backend
            .proxy(&path, "org.freedesktop.NetworkManager.Settings.Connection")
            .await?;
        let eap = profile["security"] == "eap";
        let group = if eap {
            "802-1x"
        } else {
            "802-11-wireless-security"
        };
        let secrets: Settings = query(proxy.call("GetSecrets", &(group,))).await?;
        let password = string(&secrets, group, if eap { "password" } else { "psk" });
        if profile["security"] != "open" && password.is_none() {
            return Err("NetworkManager did not return an exportable Wi-Fi password".into());
        }
        return Ok(
            json!({"uuid":profile["uuid"],"ssid":profile["ssid"],"security":profile["security"],"password":password}),
        );
    }
    if action == "vpn-connect" {
        if let Some(edit) = request.profile {
            backend.save(edit).await?;
        }
    }
    if let Some(interface) = &request.interface {
        backend
            .device(interface.as_str(), kind, Some(&path))
            .await?;
    }
    backend.current().await?;
    let output = if action.ends_with("-remove") {
        super::run_nmcli(&[
            "--wait",
            "30",
            "connection",
            "delete",
            "uuid",
            target.unwrap(),
        ])
        .await
    } else if action.ends_with("-down") || action == "vpn-deactivate" {
        super::run_nmcli(&[
            "--wait",
            "30",
            "connection",
            "down",
            "uuid",
            target.unwrap(),
        ])
        .await
    } else {
        activate(
            target.unwrap(),
            kind,
            profile["security"].as_str(),
            password.as_deref(),
            request.interface.as_ref().map(Text::as_str),
        )
        .await
    };
    let output = output.map_err(|_| {
        "indeterminate: network dispatch failed or is unconfirmed; refresh state before retrying"
    })?;
    backend
        .current()
        .await
        .map_err(|_| "indeterminate: NetworkManager changed after dispatch")?;
    let after = backend
        .target_state(target.unwrap())
        .await
        .map_err(|_| "indeterminate: network effect dispatched but readback failed")?;
    let expected = if action.ends_with("-remove") {
        after["exists"] == false
    } else if action.ends_with("-down") || action == "vpn-deactivate" {
        after["active"] == false
    } else {
        after["active"] == true
    };
    if !expected {
        return Err("indeterminate: NetworkManager did not confirm the requested state; refresh before retrying".into());
    }
    Ok(
        json!({"action":action,"target":target,"status":"applied","before":profile,"after":after,"stdout_tail":output}),
    )
}

struct Backend {
    connection: Connection,
    owner: OwnedUniqueName,
}
impl Backend {
    async fn system() -> Result<Self, String> {
        let connection = query(
            zbus::connection::Builder::address("unix:path=/run/dbus/system_bus_socket")
                .map_err(error)?
                .build(),
        )
        .await?;
        let bus = DBusProxy::new(&connection).await.map_err(error)?;
        for name in ["org.freedesktop.DBus", SERVICE] {
            if query(async {
                bus.get_connection_unix_user(BusName::try_from(name).map_err(zbus::Error::from)?)
                    .await
                    .map_err(Into::into)
            })
            .await?
                != 0
            {
                return Err("NetworkManager and its system bus must be root-owned".into());
            }
        }
        let owner = query(async {
            bus.get_name_owner(BusName::try_from(SERVICE).map_err(zbus::Error::from)?)
                .await
                .map_err(Into::into)
        })
        .await?;
        if query(async {
            bus.get_connection_unix_user(owner.clone().into())
                .await
                .map_err(Into::into)
        })
        .await?
            != 0
        {
            return Err("NetworkManager's selected unique service owner is not root".into());
        }
        Ok(Self { connection, owner })
    }
    async fn current(&self) -> Result<(), String> {
        let bus = DBusProxy::new(&self.connection).await.map_err(error)?;
        let owner = query(async {
            bus.get_name_owner(BusName::try_from(SERVICE).map_err(zbus::Error::from)?)
                .await
                .map_err(Into::into)
        })
        .await?;
        if owner != self.owner {
            return Err("NetworkManager service changed".into());
        }
        Ok(())
    }
    async fn proxy(&self, path: &str, interface: &str) -> Result<Proxy<'static>, String> {
        zbus::proxy::Builder::<Proxy<'static>>::new(&self.connection)
            .destination(self.owner.clone())
            .map_err(error)?
            .path(path.to_string())
            .map_err(error)?
            .interface(interface.to_string())
            .map_err(error)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
            .map_err(error)
    }
    async fn profiles(&self) -> Result<Vec<(String, Settings)>, String> {
        let proxy = self
            .proxy(SETTINGS, "org.freedesktop.NetworkManager.Settings")
            .await?;
        let paths: Vec<OwnedObjectPath> = query(proxy.call("ListConnections", &())).await?;
        if paths.len() > 256 {
            return Err("network profile inventory exceeds its bound".into());
        }
        let mut values = Vec::new();
        for path in paths {
            let proxy = self
                .proxy(
                    path.as_str(),
                    "org.freedesktop.NetworkManager.Settings.Connection",
                )
                .await?;
            let raw: Settings = query(proxy.call("GetSettings", &())).await?;
            if string(&raw, "connection", "type")
                .as_deref()
                .and_then(Kind::parse)
                .is_some()
            {
                values.push((path.to_string(), raw));
            }
        }
        Ok(values)
    }
    async fn find(&self, uuid: &str) -> Result<(String, Settings), String> {
        self.profiles()
            .await?
            .into_iter()
            .find(|(_, raw)| string(raw, "connection", "uuid").as_deref() == Some(uuid))
            .ok_or_else(|| "network profile is no longer available".into())
    }
    async fn device(
        &self,
        interface: &str,
        kind: Kind,
        profile: Option<&str>,
    ) -> Result<(), String> {
        let nm = self.proxy(BASE, SERVICE).await?;
        let path: OwnedObjectPath = query(nm.call("GetDeviceByIpIface", &(interface,))).await?;
        let device = self
            .proxy(path.as_str(), "org.freedesktop.NetworkManager.Device")
            .await?;
        let actual: u32 = query(device.get_property("DeviceType")).await?;
        if !matches!((kind, actual), (Kind::Wifi, 2) | (Kind::Ethernet, 1)) {
            return Err(
                "selected interface belongs to another independently authorized category".into(),
            );
        }
        if let Some(profile) = profile {
            let available: Vec<OwnedObjectPath> =
                query(device.get_property("AvailableConnections")).await?;
            if !available.iter().any(|path| path.as_str() == profile) {
                return Err("profile is not available on the selected interface".into());
            }
        }
        Ok(())
    }
    async fn target_state(&self, uuid: &str) -> Result<Value, String> {
        let snapshot = self
            .snapshot()
            .await
            .map_err(|_| "indeterminate: network effect dispatched but readback failed")?;
        let exists = snapshot["profiles"].as_array().is_some_and(|profiles| {
            profiles
                .iter()
                .any(|profile| profile["uuid"].as_str() == Some(uuid))
        });
        let active = snapshot["active"].as_array().is_some_and(|profiles| {
            profiles
                .iter()
                .any(|profile| profile["uuid"].as_str() == Some(uuid) && profile["state"] == 2)
        });
        Ok(json!({"uuid":uuid,"exists":exists,"active":active}))
    }
    async fn snapshot(&self) -> Result<Value, String> {
        let nm = self.proxy(BASE, SERVICE).await?;
        let wifi: bool = query(nm.get_property("WirelessEnabled")).await?;
        let wwan: bool = query(nm.get_property("WwanEnabled")).await?;
        let connectivity: u32 = query(nm.get_property("Connectivity")).await?;
        let profiles = self.profiles().await?;
        let mut projected = Vec::new();
        for (path, raw) in &profiles {
            let mut profile = metadata(raw)?;
            profile["path"] = json!(path);
            if let Some(data) = raw.get("vpn").and_then(|values| values.get("data")) {
                let data = HashMap::<String, String>::try_from(data.try_clone().map_err(error)?)
                    .map_err(error)?;
                profile["password_required"] = json!(data
                    .get("connection-type")
                    .is_some_and(|value| value.starts_with("password")));
                profile["password_flag"] = json!(data
                    .get("password-flags")
                    .and_then(|value| value.parse::<u32>().ok())
                    .unwrap_or(0));
            }
            projected.push(profile);
        }
        let paths: Vec<OwnedObjectPath> = query(nm.call("GetDevices", &())).await?;
        if paths.len() > 32 {
            return Err("network device inventory exceeds its bound".into());
        }
        let mut devices = Vec::new();
        let mut aps = Vec::new();
        for path in paths {
            let device = self
                .proxy(path.as_str(), "org.freedesktop.NetworkManager.Device")
                .await?;
            let kind: u32 = query(device.get_property("DeviceType")).await?;
            if !matches!(kind, 1 | 2 | 29) {
                continue;
            }
            let interface: String = query(device.get_property("Interface")).await?;
            let state: u32 = query(device.get_property("State")).await?;
            let active: OwnedObjectPath = query(device.get_property("ActiveConnection")).await?;
            let available: Vec<OwnedObjectPath> =
                query(device.get_property("AvailableConnections")).await?;
            if available.len() > 256 {
                return Err("available profile inventory exceeds its bound".into());
            }
            let subtype = match kind {
                1 => "Wired",
                2 => "Wireless",
                _ => "WireGuard",
            };
            let specialized = self
                .proxy(
                    path.as_str(),
                    &format!("org.freedesktop.NetworkManager.Device.{subtype}"),
                )
                .await?;
            let hw: String = if kind == 29 {
                String::new()
            } else {
                query(specialized.get_property("HwAddress")).await?
            };
            let speed: u32 = if kind == 1 {
                query(specialized.get_property("Speed")).await?
            } else {
                0
            };
            devices.push(json!({"path":path,"kind":kind,"interface":interface,"state":state,"active":active,"available":available,"hw_address":hw,"speed":speed}));
            if kind == 2 {
                let paths: Vec<OwnedObjectPath> =
                    query(specialized.call("GetAllAccessPoints", &())).await?;
                if paths.len() + aps.len() > 512 {
                    return Err("Wi-Fi access point inventory exceeds its bound".into());
                }
                for ap in paths {
                    let proxy = self
                        .proxy(ap.as_str(), "org.freedesktop.NetworkManager.AccessPoint")
                        .await?;
                    let ssid: Vec<u8> = query(proxy.get_property("Ssid")).await?;
                    let strength: u8 = query(proxy.get_property("Strength")).await?;
                    let flags: u32 = query(proxy.get_property("Flags")).await?;
                    let rsn: u32 = query(proxy.get_property("RsnFlags")).await?;
                    let wpa: u32 = query(proxy.get_property("WpaFlags")).await?;
                    let security = if rsn & 0x200 != 0 {
                        "eap"
                    } else if (rsn | wpa) & (0x100 | 0x400) != 0 {
                        "psk"
                    } else if flags & 1 == 0 || rsn & 0x800 != 0 {
                        "open"
                    } else {
                        "unsupported"
                    };
                    aps.push(json!({"path":ap,"ssid":String::from_utf8_lossy(&ssid),"strength":strength,"device":path,"hw_address":hw,"interface":interface,"state":state,"security":security,"secured":flags&1!=0,"wps_push":flags&4!=0}));
                }
            }
        }
        let paths: Vec<OwnedObjectPath> = query(nm.get_property("ActiveConnections")).await?;
        if paths.len() > 128 {
            return Err("active network inventory exceeds its bound".into());
        }
        let mut active = Vec::new();
        for path in paths {
            let proxy = self
                .proxy(
                    path.as_str(),
                    "org.freedesktop.NetworkManager.Connection.Active",
                )
                .await?;
            let uuid: String = query(proxy.get_property("Uuid")).await?;
            let name: String = query(proxy.get_property("Id")).await?;
            let kind: String = query(proxy.get_property("Type")).await?;
            if Kind::parse(&kind).is_none() {
                continue;
            }
            let state: u32 = query(proxy.get_property("State")).await?;
            let ip4: OwnedObjectPath = query(proxy.get_property("Ip4Config")).await?;
            let addresses = if ip4.as_str() == "/" {
                Vec::new()
            } else {
                let proxy = self
                    .proxy(ip4.as_str(), "org.freedesktop.NetworkManager.IP4Config")
                    .await?;
                let data: Vec<HashMap<String, OwnedValue>> =
                    query(proxy.get_property("AddressData")).await?;
                if data.len() > 32 {
                    return Err("network address inventory exceeds its bound".into());
                }
                data.iter()
                    .filter_map(|entry| {
                        entry
                            .get("address")
                            .and_then(|value| <&str>::try_from(value).ok())
                            .map(str::to_string)
                    })
                    .collect::<Vec<_>>()
            };
            active.push(json!({"path":path,"uuid":uuid,"name":name,"kind":Kind::parse(&kind),"state":state,"ip_addresses":addresses}));
        }
        self.current().await?;
        Ok(
            json!({"version":1,"provider":"networkmanager","wifi_enabled":wifi,"airplane_mode":!wifi&&!wwan,"connectivity":connectivity,"profiles":projected,"devices":devices,"access_points":aps,"active":active}),
        )
    }
    async fn save(&self, profile: Profile) -> Result<Value, String> {
        let (path, mut raw) = if let Some(uuid) = profile.uuid {
            let (path, raw) = self.find(&uuid.to_string()).await?;
            let before = project(&raw)?;
            if before.kind != profile.kind
                || before.revision.as_ref().map(Text::as_str)
                    != profile.revision.as_ref().map(Text::as_str)
            {
                return Err(
                    "network profile changed since it was displayed; reload before applying".into(),
                );
            }
            (Some(path), raw)
        } else {
            (None, Settings::new())
        };
        let uuid = profile.uuid.unwrap_or_else(uuid::Uuid::new_v4);
        patch(&mut raw, &profile, &uuid.to_string())?;
        self.current().await?;
        if let Some(path) = path {
            let proxy = self
                .proxy(&path, "org.freedesktop.NetworkManager.Settings.Connection")
                .await?;
            query_mutation(proxy.call::<_, _, ()>("Update", &(raw,))).await?;
        } else {
            let proxy = self
                .proxy(SETTINGS, "org.freedesktop.NetworkManager.Settings")
                .await?;
            query_mutation(proxy.call::<_, _, OwnedObjectPath>("AddConnection", &(raw,))).await?;
        }
        self.current()
            .await
            .map_err(|_| "indeterminate: NetworkManager changed after saving")?;
        let (_, raw) = self
            .find(&uuid.to_string())
            .await
            .map_err(|_| "indeterminate: saved network profile readback failed")?;
        let after = project(&raw)?;
        let mut expected = profile;
        expected.uuid = Some(uuid);
        expected.revision = after.revision.clone();
        if after != expected {
            return Err("indeterminate: saved network profile differs from the request".into());
        }
        Ok(json!({"action":"profile-save","status":"applied","profile":after}))
    }
}

fn string(raw: &Settings, group: &str, key: &str) -> Option<String> {
    raw.get(group)?
        .get(key)
        .and_then(|value| <&str>::try_from(value).ok())
        .map(str::to_string)
}
fn metadata(raw: &Settings) -> Result<Value, String> {
    let kind = string(raw, "connection", "type")
        .as_deref()
        .and_then(Kind::parse)
        .ok_or("unsupported profile type")?;
    let uuid = string(raw, "connection", "uuid").ok_or("profile UUID is unavailable")?;
    uuid::Uuid::parse_str(&uuid).map_err(|_| "invalid provider profile UUID")?;
    let name = string(raw, "connection", "id").ok_or("profile name is unavailable")?;
    if name.len() > 255 {
        return Err("provider profile name exceeds its bound".into());
    }
    Ok(json!({
        "uuid":uuid,"kind":kind,"name":name,
        "interface":string(raw,"connection","interface-name").filter(|value|!value.is_empty()),
        "ssid":if kind==Kind::Wifi{Some(ssid(raw)?)}else{None},
        "security":if kind==Kind::Wifi{Some(match string(raw,"802-11-wireless-security","key-mgmt").as_deref(){None=>"open",Some("wpa-psk"|"sae")=>"psk",Some("wpa-eap"|"ieee8021x")=>"eap",_=>"unsupported"})}else{None},
        "username":string(raw,"vpn","user-name")
    }))
}

fn project(raw: &Settings) -> Result<Profile, String> {
    let kind = string(raw, "connection", "type")
        .as_deref()
        .and_then(Kind::parse)
        .ok_or("unsupported profile type")?;
    let field = |group, key| -> Result<Text<255>, String> {
        serde_json::from_value(json!(
            string(raw, group, key).ok_or("required network profile field is unavailable")?
        ))
        .map_err(|_| "invalid network profile field".into())
    };
    let mut profile:Profile=serde_json::from_value(json!({
        "uuid":string(raw,"connection","uuid").ok_or("profile UUID is unavailable")?,
        "kind":kind,"name":field("connection","id")?,"interface":string(raw,"connection","interface-name").filter(|value|!value.is_empty()),
        "autoconnect":raw.get("connection").and_then(|group|group.get("autoconnect")).map(bool::try_from).transpose().map_err(error)?.unwrap_or(true),
        "ipv4_method":string(raw,"ipv4","method").unwrap_or_else(||"auto".into()),
        "ipv4_gateway":string(raw,"ipv4","gateway").filter(|value|!value.is_empty()),
        "ssid":if kind==Kind::Wifi {Some(ssid(raw)?)}else{None},
        "security":if kind==Kind::Wifi {Some(match string(raw,"802-11-wireless-security","key-mgmt").as_deref(){None=>"open",Some("wpa-psk"|"sae")=>"psk",Some("wpa-eap"|"ieee8021x")=>"eap",_=>"unsupported"})}else{None},
        "identity":string(raw,"802-1x","identity"),"username":string(raw,"vpn","user-name")
    })).map_err(|_|"invalid network profile metadata")?;
    if let Some(value) = raw.get("ipv4").and_then(|group| group.get("address-data")) {
        let addresses =
            Vec::<HashMap<String, OwnedValue>>::try_from(value.try_clone().map_err(error)?)
                .map_err(error)?;
        if addresses.len() > 8 {
            return Err("profile address inventory exceeds its bound".into());
        }
        for address in addresses {
            let text = <&str>::try_from(address.get("address").ok_or("missing profile address")?)
                .map_err(error)?;
            let prefix = u32::try_from(address.get("prefix").ok_or("missing profile prefix")?)
                .map_err(error)?;
            profile
                .ipv4_addresses
                .push(serde_json::from_value(json!(format!("{text}/{prefix}"))).map_err(error)?);
        }
    }
    if let Some(value) = raw.get("ipv4").and_then(|group| group.get("dns-data")) {
        let addresses =
            Vec::<String>::try_from(value.try_clone().map_err(error)?).map_err(error)?;
        if addresses.len() > 8 {
            return Err("profile DNS inventory exceeds its bound".into());
        }
        for address in addresses {
            profile
                .ipv4_dns
                .push(address.parse().map_err(|_| "invalid profile DNS address")?);
        }
    } else if let Some(value) = raw.get("ipv4").and_then(|group| group.get("dns")) {
        let addresses = Vec::<u32>::try_from(value.try_clone().map_err(error)?).map_err(error)?;
        if addresses.len() > 8 {
            return Err("profile DNS inventory exceeds its bound".into());
        }
        profile.ipv4_dns = addresses
            .into_iter()
            .map(|value| Ipv4Addr::from(value.to_ne_bytes()))
            .collect();
    }
    profile.revision = Some(
        serde_json::from_value(json!(crate::crypto::sha256_hex(
            &serde_json::to_vec(&profile).map_err(error)?
        )))
        .map_err(error)?,
    );
    Ok(profile)
}
fn ssid(raw: &Settings) -> Result<String, String> {
    let value = raw
        .get("802-11-wireless")
        .and_then(|group| group.get("ssid"))
        .ok_or("profile SSID unavailable")?;
    let bytes = Vec::<u8>::try_from(value.try_clone().map_err(error)?).map_err(error)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
fn text(value: impl Into<String>) -> OwnedValue {
    OwnedValue::from(Str::from(value.into()))
}
fn patch(raw: &mut Settings, profile: &Profile, uuid: &str) -> Result<(), String> {
    let new = raw.is_empty();
    let previous_security = if new {
        None
    } else {
        project(raw)?
            .security
            .map(|value| value.as_str().to_string())
    };
    let connection = raw.entry("connection".into()).or_default();
    connection.insert("id".into(), text(profile.name.as_str()));
    connection.insert("uuid".into(), text(uuid));
    connection.insert("type".into(), text(profile.kind.nm()));
    connection.insert("autoconnect".into(), OwnedValue::from(profile.autoconnect));
    if let Some(interface) = &profile.interface {
        connection.insert("interface-name".into(), text(interface.as_str()));
    } else {
        connection.remove("interface-name");
    }
    let ipv4 = raw.entry("ipv4".into()).or_default();
    ipv4.insert("method".into(), text(profile.ipv4_method.as_str()));
    ipv4.remove("addresses");
    let mut addresses = Vec::new();
    for address in &profile.ipv4_addresses {
        let (address, prefix) = parse_address(address.as_str())?;
        addresses.push(HashMap::from([
            ("address".to_string(), text(address.to_string())),
            ("prefix".to_string(), OwnedValue::from(prefix)),
        ]));
    }
    ipv4.insert(
        "address-data".into(),
        OwnedValue::try_from(zbus::zvariant::Value::new(addresses)).map_err(error)?,
    );
    ipv4.insert(
        "dns".into(),
        OwnedValue::try_from(zbus::zvariant::Value::new(
            profile
                .ipv4_dns
                .iter()
                .map(|value| u32::from_ne_bytes(value.octets()))
                .collect::<Vec<_>>(),
        ))
        .map_err(error)?,
    );
    ipv4.remove("dns-data");
    if let Some(gateway) = profile.ipv4_gateway {
        ipv4.insert("gateway".into(), text(gateway.to_string()));
    } else {
        ipv4.remove("gateway");
    }
    if new {
        raw.insert(
            "ipv6".into(),
            HashMap::from([("method".into(), text("auto"))]),
        );
    }
    if profile.kind == Kind::Wifi {
        let wifi = raw.entry("802-11-wireless".into()).or_default();
        wifi.insert(
            "ssid".into(),
            OwnedValue::try_from(zbus::zvariant::Value::new(
                profile.ssid.as_ref().unwrap().as_str().as_bytes().to_vec(),
            ))
            .map_err(error)?,
        );
        wifi.insert("mode".into(), text("infrastructure"));
        let security = profile.security.as_ref().unwrap().as_str();
        if security == "open" {
            raw.remove("802-11-wireless-security");
            raw.remove("802-1x");
        } else {
            let secured = raw.entry("802-11-wireless-security".into()).or_default();
            if new || previous_security.as_deref() != Some(security) {
                secured.insert(
                    "key-mgmt".into(),
                    text(if security == "eap" {
                        "wpa-eap"
                    } else {
                        "wpa-psk"
                    }),
                );
            }
            if new {
                secured.insert("psk-flags".into(), OwnedValue::from(2u32));
            }
            if security == "eap" {
                let eap = raw.entry("802-1x".into()).or_default();
                eap.insert(
                    "identity".into(),
                    text(profile.identity.as_ref().unwrap().as_str()),
                );
                if new {
                    eap.insert(
                        "eap".into(),
                        OwnedValue::try_from(zbus::zvariant::Value::new(vec!["peap".to_string()]))
                            .map_err(error)?,
                    );
                    eap.insert("phase2-auth".into(), text("mschapv2"));
                    eap.insert("password-flags".into(), OwnedValue::from(2u32));
                    eap.insert("system-ca-certs".into(), OwnedValue::from(true));
                }
            } else {
                raw.remove("802-1x");
            }
        }
    }
    if profile.kind == Kind::Vpn {
        let vpn = raw.entry("vpn".into()).or_default();
        if let Some(username) = &profile.username {
            vpn.insert("user-name".into(), text(username.as_str()));
        }
    }
    Ok(())
}

async fn activate(
    uuid: &str,
    kind: Kind,
    security: Option<&str>,
    password: Option<&str>,
    interface: Option<&str>,
) -> Result<String, String> {
    let mut args = vec!["--wait", "30", "connection", "up", "uuid", uuid];
    if let Some(interface) = interface {
        args.extend(["ifname", interface]);
    }
    let Some(password) = password else {
        return super::run_nmcli(&args).await;
    };
    if password.is_empty() || password.len() > 1024 || password.chars().any(char::is_control) {
        return Err("stored network credential must be one bounded line".into());
    }
    let key = if kind == Kind::Vpn {
        "vpn.secrets.password"
    } else if security == Some("eap") {
        "802-1x.password"
    } else {
        "802-11-wireless-security.psk"
    };
    let mut command = tokio::process::Command::new(super::nmcli_path());
    command
        .args(["--passwd-file", "/proc/self/fd/0"])
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LC_ALL", "C.UTF-8")
        .kill_on_drop(true)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|_| "failed to start network authentication")?;
    let input = format!("{key}:{password}\n");
    use tokio::io::AsyncWriteExt;
    let mut stdin = child
        .stdin
        .take()
        .ok_or("network authentication stdin unavailable")?;
    stdin
        .write_all(input.as_bytes())
        .await
        .map_err(|_| "network authentication input is unconfirmed")?;
    drop(stdin);
    let output = tokio::time::timeout(super::NMCLI_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| "network authentication timed out")?
        .map_err(|_| "network authentication outcome is unconfirmed")?;
    if !output.status.success() {
        return Err("NetworkManager refused the connection or authentication".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn validate_import(config: &Import) -> Result<(), String> {
    if config.kind == Kind::Wireguard {
        validate_interface(config.name.as_str())?;
    } else {
        super::validate_text("VPN profile name", config.name.as_str(), 255)?;
    }
    let content = config.content.as_str();
    if content.is_empty() || content.len() > 128 * 1024 || content.contains('\0') {
        return Err("VPN configuration exceeds its text bound".into());
    }
    match config.kind {
        Kind::Wireguard => {
            let mut section = "";
            for line in content
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with(['#', ';']))
            {
                if line.starts_with('[') {
                    if !matches!(line, "[Interface]" | "[Peer]") {
                        return Err("unsupported WireGuard section".into());
                    }
                    section = line;
                    continue;
                }
                let (key, _) = line.split_once('=').ok_or("invalid WireGuard setting")?;
                if !match section {
                    "[Interface]" => matches!(
                        key.trim(),
                        "PrivateKey" | "Address" | "DNS" | "ListenPort" | "MTU"
                    ),
                    "[Peer]" => matches!(
                        key.trim(),
                        "PublicKey"
                            | "PresharedKey"
                            | "AllowedIPs"
                            | "Endpoint"
                            | "PersistentKeepalive"
                    ),
                    _ => false,
                } {
                    return Err("unsupported WireGuard setting; executable hooks and external references are forbidden".into());
                }
            }
        }
        Kind::Vpn => {
            let mut block: Option<&str> = None;
            for line in content
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with(['#', ';']))
            {
                if let Some(tag) = block {
                    if line == format!("</{tag}>") {
                        block = None;
                    } else if line.contains(['<', '>']) {
                        return Err("ambiguous OpenVPN inline block delimiter".into());
                    }
                    continue;
                }
                if line.starts_with('<') {
                    let tag = line
                        .strip_prefix('<')
                        .and_then(|value| value.strip_suffix('>'))
                        .ok_or("invalid OpenVPN inline block")?;
                    if !matches!(
                        tag,
                        "ca" | "cert" | "key" | "tls-auth" | "tls-crypt" | "pkcs12"
                    ) {
                        return Err("unsupported OpenVPN inline block".into());
                    }
                    block = Some(tag);
                    continue;
                }
                let mut words = line.split_ascii_whitespace();
                let key = words.next().ok_or("invalid OpenVPN option")?;
                if line.contains(['\\', '\0'])
                    || !matches!(
                        key,
                        "client"
                            | "dev"
                            | "dev-type"
                            | "proto"
                            | "remote"
                            | "remote-random"
                            | "remote-random-hostname"
                            | "resolv-retry"
                            | "nobind"
                            | "persist-key"
                            | "persist-tun"
                            | "cipher"
                            | "data-ciphers"
                            | "data-ciphers-fallback"
                            | "auth"
                            | "auth-nocache"
                            | "auth-retry"
                            | "auth-user-pass"
                            | "tls-client"
                            | "tls-version-min"
                            | "tls-version-max"
                            | "remote-cert-tls"
                            | "verify-x509-name"
                            | "key-direction"
                            | "reneg-sec"
                            | "hand-window"
                            | "tran-window"
                            | "ping"
                            | "ping-restart"
                            | "keepalive"
                            | "connect-retry"
                            | "connect-timeout"
                            | "connect-retry-max"
                            | "float"
                            | "explicit-exit-notify"
                            | "mute-replay-warnings"
                            | "verb"
                            | "mute"
                            | "sndbuf"
                            | "rcvbuf"
                            | "tun-mtu"
                            | "mssfix"
                            | "fragment"
                            | "comp-lzo"
                            | "compress"
                            | "allow-compression"
                            | "route"
                            | "route-ipv6"
                            | "route-metric"
                            | "route-nopull"
                            | "redirect-gateway"
                            | "redirect-private"
                            | "dhcp-option"
                            | "pull"
                            | "pull-filter"
                            | "topology"
                    )
                    || (key == "auth-user-pass" && words.next().is_some())
                {
                    return Err("unsupported OpenVPN option; executable hooks and external file references are forbidden".into());
                }
            }
            if block.is_some() {
                return Err("unterminated OpenVPN inline block".into());
            }
        }
        _ => return Err("only OpenVPN/WireGuard configurations may be imported".into()),
    }
    Ok(())
}
async fn import_profile(backend: &Backend, config: Import) -> Result<Value, String> {
    let before = backend.profiles().await?;
    let directory = crate::paths::runtime_dir().join("network-imports");
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    match std::fs::DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err("OS network import directory is unavailable".into()),
    }
    let metadata = std::fs::symlink_metadata(&directory).map_err(error)?;
    use std::os::unix::fs::MetadataExt;
    if !metadata.is_dir() || metadata.uid() != 0 || metadata.permissions().mode() & 0o077 != 0 {
        return Err("OS network import directory is not protected".into());
    }
    backend.current().await?;
    let staging = directory.join(uuid::Uuid::new_v4().simple().to_string());
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&staging)
        .map_err(|_| "create protected network import directory")?;
    let path = staging.join(format!(
        "{}.conf",
        if config.kind == Kind::Wireguard {
            config.name.as_str()
        } else {
            "source"
        }
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|_| "create protected network import")?;
    let write = file
        .write_all(config.content.as_str().as_bytes())
        .and_then(|_| file.sync_all());
    if write.is_err() {
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&staging);
        return Err("write protected network import".into());
    }
    drop(file);
    let output = super::run_nmcli(&[
        "--wait",
        "30",
        "connection",
        "import",
        "type",
        if config.kind == Kind::Vpn {
            "openvpn"
        } else {
            "wireguard"
        },
        "file",
        path.to_str().ok_or("network import path is not UTF-8")?,
    ])
    .await;
    let retired = std::fs::remove_file(&path).is_ok() && std::fs::remove_dir(&staging).is_ok();
    if !retired {
        return Err("indeterminate: network import source cleanup is unconfirmed".into());
    }
    output.map_err(|_| {
        "indeterminate: VPN import failed or is unconfirmed; refresh before retrying"
    })?;
    let after = backend
        .profiles()
        .await
        .map_err(|_| "indeterminate: VPN import readback failed")?;
    let mut added = after
        .into_iter()
        .filter(|(_, raw)| {
            !before.iter().any(|(_, old)| {
                string(raw, "connection", "uuid") == string(old, "connection", "uuid")
            })
        })
        .collect::<Vec<_>>();
    if added.len() != 1 {
        return Err("indeterminate: imported VPN profile could not be correlated".into());
    }
    let (path, mut raw) = added.remove(0);
    if project(&raw)?.kind != config.kind {
        return Err("indeterminate: imported VPN has an unexpected type".into());
    }
    raw.entry("connection".into())
        .or_default()
        .insert("id".into(), text(config.name.as_str()));
    if config.kind == Kind::Wireguard {
        raw.entry("connection".into())
            .or_default()
            .insert("interface-name".into(), text(config.name.as_str()));
    }
    let proxy = backend
        .proxy(&path, "org.freedesktop.NetworkManager.Settings.Connection")
        .await?;
    query_mutation(proxy.call::<_, _, ()>("Update", &(raw,))).await?;
    let raw: Settings = query(proxy.call("GetSettings", &())).await?;
    Ok(json!({"action":"profile-import","status":"applied","profile":project(&raw)?}))
}
fn error(error: impl std::fmt::Display) -> String {
    format!("NetworkManager provider unavailable: {error}")
}
async fn query<T>(future: impl Future<Output = zbus::Result<T>>) -> Result<T, String> {
    tokio::time::timeout(CALL, future)
        .await
        .map_err(|_| "NetworkManager query timed out")?
        .map_err(error)
}
async fn query_mutation<T>(future: impl Future<Output = zbus::Result<T>>) -> Result<T, String> {
    tokio::time::timeout(CALL, future)
        .await
        .map_err(|_| "indeterminate: network mutation timed out; refresh before retrying")?
        .map_err(|_| {
            "indeterminate: network mutation failed or is unconfirmed; refresh before retrying"
                .into()
        })
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/network/native.rs"
    ));
}
