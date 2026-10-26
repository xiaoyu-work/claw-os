//! Fixed system-bus radio service. Callers never choose a bus or interface.

use clawd_client::nearby::*;
use std::{collections::HashMap, time::Duration};
use zbus::zvariant::OwnedValue;

pub(super) const WORKER: &str = "/usr/lib/cos/bin/claw-nearby-worker";
pub(super) const RADIO: &str = "/usr/lib/cos/bin/claw-nearby-radio";
const NAME: &str = "io.github.t4t5.Owly";
const PATH: &str = "/io/github/t4t5/Owly";
const INTERFACE: &str = "io.github.t4t5.Owly.Radio1";

pub(super) struct Lease {
    pub proxy: zbus::Proxy<'static>,
    pub id: u64,
    pub link: LinkConfig,
}

pub(super) fn installed() -> Result<(), String> {
    for (name, path) in [("claw-nearby-worker", WORKER), ("claw-nearby-radio", RADIO)] {
        crate::agentd::spawn::validate_root_owned_executable(std::path::Path::new(path))
            .map_err(|error| format!("Nearby backend is not installed: {error}"))?;
        crate::update::runtime::enforce_component_binary(name, std::path::Path::new(path))
            .map_err(|error| error.message)?;
    }
    Ok(())
}

async fn proxy() -> Result<zbus::Proxy<'static>, String> {
    let conn = zbus::Connection::system()
        .await
        .map_err(|e| e.to_string())?;
    zbus::Proxy::new_owned(conn, NAME, PATH, INTERFACE)
        .await
        .map_err(|e| e.to_string())
}

pub(super) async fn probe() -> RadioProbe {
    let result = async {
        installed()?;
        let proxy = proxy().await?;
        let data: String = proxy.call("Probe", &()).await.map_err(|e| e.to_string())?;
        if data.len() > 64 * 1024 {
            return Err("radio probe exceeds its bound".into());
        }
        let value: serde_json::Value = serde_json::from_str(&data).map_err(|e| e.to_string())?;
        let mode = match value["mode"].as_str() {
            Some("shared") => Some(RadioMode::Shared),
            Some("exclusive") => Some(RadioMode::Exclusive),
            Some("active-monitor") | Some("active_monitor") => Some(RadioMode::ActiveMonitor),
            _ => None,
        };
        let mut diagnostics = Vec::new();
        for key in ["missing", "issues"] {
            if let Some(values) = value[key].as_array() {
                for value in values.iter().take(7) {
                    if let Some(message) = value.as_str().or_else(|| value["say"].as_str()) {
                        diagnostics.push(super::clip(message, 1024));
                    }
                }
            }
        }
        let available = value["hardware"].as_bool() == Some(true)
            && value["missing"].as_array().is_some_and(Vec::is_empty)
            && mode.is_some();
        if available {
            diagnostics.push("Host firewall rules are unchanged. AirDrop needs link-local IPv6 TCP 8770/8771 and UDP 5353 on awdl0; check scoped admission if discovery or transfer fails, never open these ports on the WAN.".into());
        }
        Ok::<_, String>(RadioProbe {
            available,
            mode,
            driver: value["driver"].as_str().map(|s| super::clip(s, 128)),
            diagnostics,
        })
    };
    match tokio::time::timeout(Duration::from_secs(10), result).await {
        Ok(Ok(probe)) => probe,
        Ok(Err(error)) => RadioProbe {
            available: false,
            mode: None,
            driver: None,
            diagnostics: vec![super::clip(&error, 1024)],
        },
        Err(_) => RadioProbe {
            available: false,
            mode: None,
            driver: None,
            diagnostics: vec!["Radio probe timed out; no radio was acquired.".into()],
        },
    }
}

impl Lease {
    pub async fn acquire(duration: u32, receive: bool) -> Result<Self, String> {
        installed()?;
        let proxy = proxy().await?;
        let reply = tokio::time::timeout(
            Duration::from_secs(60),
            proxy.call_method("Acquire", &duration),
        )
        .await
        .map_err(|_| "Radio acquisition timed out; closing its owner connection.")?
        .map_err(|e| e.to_string())?;
        let sender = reply
            .header()
            .sender()
            .ok_or("radio reply has no authenticated bus owner")?
            .to_string();
        let (id, fields): (u64, HashMap<String, OwnedValue>) =
            reply.body().deserialize().map_err(|e| e.to_string())?;
        let proxy = zbus::Proxy::new_owned(proxy.connection().clone(), sender, PATH, INTERFACE)
            .await
            .map_err(|e| e.to_string())?;
        if id == 0 {
            return Err("radio returned an invalid lease".into());
        }
        let string = |name: &str| {
            fields
                .get(name)
                .ok_or("radio link field missing")?
                .downcast_ref::<String>()
                .map_err(|_| "invalid radio link field".to_string())
        };
        let interface = string("interface")?;
        if interface != "awdl0" {
            return Err("radio returned an unexpected interface".into());
        }
        let index = fields
            .get("index")
            .and_then(|value| value.downcast_ref::<u32>().ok())
            .ok_or("radio link index missing")?;
        let address = string("address")?;
        let announce = fields
            .get("announce")
            .map(|value| {
                <(u32, u32, u32)>::try_from(value.try_clone().map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())
            })
            .transpose()?;
        Ok(Self {
            proxy,
            id,
            link: LinkConfig {
                protocol: WORKER_PROTOCOL,
                interface,
                index,
                address,
                name: "Claw OS".into(),
                receive,
                duration_secs: duration,
                announce,
            },
        })
    }

    pub async fn check_alive(&self) -> Result<(), String> {
        let checked = tokio::time::timeout(Duration::from_secs(3), async {
            let id: u64 = self
                .proxy
                .get_property("Lease")
                .await
                .map_err(|e| e.to_string())?;
            let state: String = self
                .proxy
                .get_property("State")
                .await
                .map_err(|e| e.to_string())?;
            if id != self.id || state != "up" {
                return Err("Radio lease ended or changed; no transfer is replayed.".into());
            }
            Ok(())
        })
        .await
        .map_err(|_| "Radio health check timed out".to_string())?;
        checked
    }

    pub async fn peers(&self) -> Result<Vec<String>, String> {
        let values: Vec<HashMap<String, OwnedValue>> =
            tokio::time::timeout(Duration::from_secs(3), self.proxy.get_property("Peers"))
                .await
                .map_err(|_| "Radio peer query timed out")?
                .map_err(|e| e.to_string())?;
        let mut peers = Vec::new();
        for peer in values.into_iter().take(MAX_PEERS) {
            let mac = peer
                .get("mac")
                .and_then(|value| value.downcast_ref::<String>().ok())
                .ok_or("invalid AWDL peer")?;
            let bytes = mac
                .split(':')
                .map(|part| u8::from_str_radix(part, 16))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            if bytes.len() != 6 {
                return Err("invalid AWDL MAC address".into());
            }
            let address = std::net::Ipv6Addr::new(
                0xfe80,
                0,
                0,
                0,
                u16::from_be_bytes([bytes[0] ^ 2, bytes[1]]),
                u16::from_be_bytes([bytes[2], 0xff]),
                u16::from_be_bytes([0xfe, bytes[3]]),
                u16::from_be_bytes([bytes[4], bytes[5]]),
            );
            peers.push(std::net::SocketAddrV6::new(address, 8770, 0, self.link.index).to_string());
        }
        Ok(peers)
    }

    pub async fn release(self) -> Result<(), String> {
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            self.proxy.call::<_, _, ()>("Release", &self.id),
        )
        .await
        .map_err(|_| "Radio restoration timed out".to_string())?;
        match result {
            Ok(()) => Ok(()),
            Err(zbus::Error::MethodError(name, _, _))
                if name.as_str() == "io.github.t4t5.Owly.Error.StaleLease" =>
            {
                tokio::time::timeout(Duration::from_secs(3), async {
                    let current: u64 = self
                        .proxy
                        .get_property("Lease")
                        .await
                        .map_err(|e| e.to_string())?;
                    let state: String = self
                        .proxy
                        .get_property("State")
                        .await
                        .map_err(|e| e.to_string())?;
                    let ended: (u64, String) = self
                        .proxy
                        .get_property("LastEnd")
                        .await
                        .map_err(|e| e.to_string())?;
                    if current == 0 && state == "idle" && ended == (self.id, "expired".into()) {
                        Ok(())
                    } else {
                        Err(
                            "Radio expiry did not provide a matching checked retirement receipt."
                                .into(),
                        )
                    }
                })
                .await
                .map_err(|_| "Radio expiry acknowledgement timed out".to_string())?
            }
            Err(error) => Err(format!("Radio restoration failed: {error}")),
        }
    }
}
