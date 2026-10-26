//! owlyd's system D-Bus API, `io.github.t4t5.Owly.Radio1`, as its clients
//! see it: the names, the proxy, and the values it trades in.
//!
//! | Member | |
//! |---|---|
//! | `Probe() → s` | the capability report, as JSON. Anyone may call it |
//! | `Acquire(u duration) → (t lease, a{sv} link)` | brings the radio up and leases it to the caller, for `duration` seconds or, with 0, until released. polkit decides who may (`io.github.t4t5.Owly.acquire`); root may |
//! | `AcquireDetached(u duration) → (t lease, a{sv} link)` | the same, for root only, with a lease that outlives its caller on the bus: for omdrop's hooks, which take the radio in one process and give it back in another |
//! | `Release(t lease)` | ends the lease, the caller's own or, for root, a detached one; returns once Wi-Fi is back |
//! | signal `StateChanged(t lease, s state, s reason)` | the radio's state changed, and why |
//! | property `State` (s) | `idle`, `starting`, `up` or `stopping` |
//! | property `Lease` (t) | the lease held, or 0 |
//! | property `Link` (a{sv}) | the lease's link, as `Acquire` returns it, or empty |
//! | property `Peers` (aa{sv}) | the AWDL peers: `mac` (s), and `rssi` (n) when known |
//! | property `LastEnd` (ts) | the last lease to end, and why: the reason its `StateChanged` to `idle` gave |
//!
//! A lease ends on `Release`, when its duration runs out, when its owner
//! drops off the bus (unless it's detached), or when the link is lost. Each has a fresh number,
//! and calls naming another fail, so a client that reconnects can't act on
//! a session it no longer owns. The link dictionary holds `interface` (s),
//! `index` (u), `mac` (s), `address` (s) and, when receivers on the link
//! repeat their mDNS announcements in bursts (MDNS-7), `announce` as
//! `(uuu)`: copies, microseconds between them, microseconds between bursts.

use std::collections::HashMap;
use std::time::Duration;

use owly_radio::radio::Announce;
use zbus::zvariant::{OwnedValue, Value};

pub const NAME: &str = "io.github.t4t5.Owly";
pub const PATH: &str = "/io/github/t4t5/Owly";
pub const INTERFACE: &str = "io.github.t4t5.Owly.Radio1";
/// The polkit action that `Acquire` checks.
pub const ACQUIRE: &str = "io.github.t4t5.Owly.acquire";
/// The prefix of the API's errors.
pub const ERRORS: &str = "io.github.t4t5.Owly.Error";

#[zbus::proxy(
    interface = "io.github.t4t5.Owly.Radio1",
    default_service = "io.github.t4t5.Owly",
    default_path = "/io/github/t4t5/Owly"
)]
pub trait Radio {
    fn probe(&self) -> zbus::Result<String>;
    fn acquire(&self, duration: u32) -> zbus::Result<(u64, HashMap<String, OwnedValue>)>;
    fn acquire_detached(&self, duration: u32) -> zbus::Result<(u64, HashMap<String, OwnedValue>)>;
    fn release(&self, lease: u64) -> zbus::Result<()>;

    #[zbus(signal, name = "StateChanged")]
    fn radio_state(&self, lease: u64, state: String, reason: String) -> zbus::Result<()>;

    #[zbus(property, name = "State")]
    fn current_state(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn lease(&self) -> zbus::Result<u64>;
    #[zbus(property)]
    fn link(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
    #[zbus(property)]
    fn last_end(&self) -> zbus::Result<(u64, String)>;
    #[zbus(property)]
    fn peers(&self) -> zbus::Result<Vec<HashMap<String, OwnedValue>>>;
}

/// A connection to the bus owlyd is on: the system bus, or `address`.
pub async fn connect(address: Option<&str>) -> zbus::Result<zbus::Connection> {
    let builder = match address {
        Some(a) => zbus::connection::Builder::address(a)?,
        None => zbus::connection::Builder::system()?,
    };
    builder.build().await
}

/// The radio's proxy, reading properties from owlyd on every call rather
/// than from a cache the signals keep, which can lag behind a call that
/// just returned.
pub async fn radio(conn: &zbus::Connection) -> zbus::Result<RadioProxy<'static>> {
    RadioProxy::builder(conn)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
}

/// The up link a lease carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkInfo {
    pub interface: String,
    pub index: u32,
    pub mac: String,
    pub address: String,
    pub announce: Option<Announce>,
}

impl LinkInfo {
    pub fn to_dict(&self) -> HashMap<String, OwnedValue> {
        let mut d = HashMap::new();
        let mut put = |k: &str, v: Value<'_>| {
            d.insert(
                k.to_string(),
                v.try_to_owned().expect("no file descriptors"),
            );
        };
        put("interface", Value::from(self.interface.as_str()));
        put("index", Value::from(self.index));
        put("mac", Value::from(self.mac.as_str()));
        put("address", Value::from(self.address.as_str()));
        if let Some(a) = self.announce {
            put(
                "announce",
                Value::from((
                    a.count,
                    a.interval.as_micros() as u32,
                    a.pause.as_micros() as u32,
                )),
            );
        }
        d
    }

    pub fn from_dict(d: &HashMap<String, OwnedValue>) -> Option<LinkInfo> {
        let s = |k: &str| -> Option<String> { d.get(k)?.downcast_ref::<String>().ok() };
        let announce = d
            .get("announce")
            .and_then(|v| <(u32, u32, u32)>::try_from(v.try_clone().ok()?).ok())
            .map(|(count, interval, pause)| Announce {
                count,
                interval: Duration::from_micros(interval.into()),
                pause: Duration::from_micros(pause.into()),
            });
        Some(LinkInfo {
            interface: s("interface")?,
            index: d.get("index")?.downcast_ref::<u32>().ok()?,
            mac: s("mac")?,
            address: s("address")?,
            announce,
        })
    }
}

/// A peer, from the `Peers` property.
pub fn peer_line(d: &HashMap<String, OwnedValue>) -> Option<String> {
    let mac = d.get("mac")?.downcast_ref::<String>().ok()?;
    Some(
        match d.get("rssi").and_then(|v| v.downcast_ref::<i16>().ok()) {
            Some(rssi) => format!("{mac} rssi={rssi}"),
            None => format!("{mac} rssi=?"),
        },
    )
}

/// Whether a call failed because owlyd isn't there to answer.
pub fn unreachable(e: &zbus::Error) -> bool {
    match e {
        zbus::Error::MethodError(name, _, _) => {
            let name = name.as_str();
            name == "org.freedesktop.DBus.Error.ServiceUnknown"
                || name == "org.freedesktop.DBus.Error.NameHasNoOwner"
                || name == "org.freedesktop.DBus.Error.Spawn.ServiceNotFound"
        }
        zbus::Error::FDO(e) => matches!(
            **e,
            zbus::fdo::Error::ServiceUnknown(_) | zbus::fdo::Error::NameHasNoOwner(_)
        ),
        zbus::Error::InputOutput(_) | zbus::Error::Address(_) => true,
        _ => false,
    }
}

/// The API's own error, by its short name (`Busy`, `NotNow`, `NoRadio`,
/// `StaleLease`, `NotAuthorized`), and its message.
pub fn api_error(e: &zbus::Error) -> Option<(&str, String)> {
    let zbus::Error::MethodError(name, message, _) = e else {
        return None;
    };
    let short = name.as_str().strip_prefix(ERRORS)?.strip_prefix('.')?;
    Some((short, message.clone().unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_round_trips_as_a_dictionary() {
        let link = LinkInfo {
            interface: "awdl0".into(),
            index: 7,
            mac: "aa:bb:cc:dd:ee:01".into(),
            address: "fe80::a8bb:ccff:fedd:ee01".into(),
            announce: Some(Announce::FIXED),
        };
        assert_eq!(LinkInfo::from_dict(&link.to_dict()), Some(link.clone()));
        let plain = LinkInfo {
            announce: None,
            ..link
        };
        assert_eq!(LinkInfo::from_dict(&plain.to_dict()), Some(plain));
    }
}
