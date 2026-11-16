//! Closed profile control; shutdown authority and App identities confer nothing.

use super::*;
use zbus::{fdo::DBusProxy, names::BusName, zvariant::OwnedValue, Connection, Proxy};

const CALL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Profile {
    Battery,
    Balanced,
    Performance,
}

impl Profile {
    fn parse(value: &str) -> Result<Self, BrokerError> {
        match value {
            "Battery" | "battery" | "power-saver" => Ok(Self::Battery),
            "Balanced" | "balanced" => Ok(Self::Balanced),
            "Performance" | "performance" => Ok(Self::Performance),
            _ => Err(BrokerError::unavailable(
                "OS power backend returned an unknown profile",
            )),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Battery => "battery",
            Self::Balanced => "balanced",
            Self::Performance => "performance",
        }
    }
}

pub(super) fn accepts(action: &str) -> bool {
    matches!(
        action,
        "profile-status" | "profile-battery" | "profile-balanced" | "profile-performance"
    )
}

fn required(action: &str) -> Cap {
    if action == "profile-status" {
        Cap::new(Verb::SYS_OBSERVE, Scope::name("power"))
    } else {
        Cap::new(Verb::IPC_INVOKE, Scope::name("desktop.power.profile"))
    }
}

pub(super) async fn control(action: &str, authority: &Decision) -> Result<Value, BrokerError> {
    let cap = required(action);
    if !authority.caps().covers(&cap) {
        return Err(BrokerError::authorization(
            "power profiles require their exact declared capability",
        ));
    }
    let _guard = tokio::time::timeout(
        LOCK_TIMEOUT,
        POWER_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock(),
    )
    .await
    .map_err(|_| BrokerError::unavailable("power profiles are busy; no change was dispatched"))?;
    authority
        .ensure_current()
        .map_err(BrokerError::authorization)?;
    let backend = Backend::system().await?;
    if action == "profile-status" {
        let _authorized = authority.require(cap).map_err(BrokerError::authorization)?;
        return Ok(
            json!({"action":action,"profile":backend.read().await?.name(),"provider":backend.service}),
        );
    }
    let requested = Profile::parse(
        action
            .strip_prefix("profile-")
            .ok_or_else(|| BrokerError::execution("unknown power profile action"))?,
    )?;
    let before = backend.read().await?;
    backend.current().await?;
    let _authorized = authority.require(cap).map_err(BrokerError::authorization)?;
    backend.set(requested).await?;
    backend.current().await.map_err(|_| {
        BrokerError::indeterminate("power backend changed after dispatch; refresh before retrying")
    })?;
    let after = backend.read().await.map_err(|_| {
        BrokerError::indeterminate(
            "power profile readback failed after dispatch; refresh before retrying",
        )
    })?;
    if after != requested {
        return Err(BrokerError::indeterminate(
            "power profile readback differs after dispatch; refresh before retrying",
        ));
    }
    Ok(
        json!({"action":action,"status":"applied","profile":after.name(),"before":before.name(),"provider":backend.service}),
    )
}

struct Backend {
    connection: Connection,
    proxy: Proxy<'static>,
    service: &'static str,
    system76: bool,
}

impl Backend {
    async fn system() -> Result<Self, BrokerError> {
        let connection = before(
            zbus::connection::Builder::address("unix:path=/run/dbus/system_bus_socket")
                .map_err(unavailable)?
                .build(),
        )
        .await?;
        let bus = DBusProxy::new(&connection).await.map_err(unavailable)?;
        let daemon = BusName::try_from("org.freedesktop.DBus").map_err(unavailable)?;
        if before(async {
            bus.get_connection_unix_user(daemon)
                .await
                .map_err(Into::into)
        })
        .await?
            != 0
        {
            return Err(BrokerError::unavailable(
                "power profiles require a root-owned system bus",
            ));
        }
        let names = before(async { bus.list_names().await.map_err(Into::into) }).await?;
        let activatable =
            before(async { bus.list_activatable_names().await.map_err(Into::into) }).await?;
        let (service, path, interface, system76) = [
            (
                "com.system76.PowerDaemon",
                "/com/system76/PowerDaemon",
                "com.system76.PowerDaemon",
                true,
            ),
            (
                "org.freedesktop.UPower.PowerProfiles",
                "/org/freedesktop/UPower/PowerProfiles",
                "org.freedesktop.UPower.PowerProfiles",
                false,
            ),
            (
                "net.hadess.PowerProfiles",
                "/net/hadess/PowerProfiles",
                "net.hadess.PowerProfiles",
                false,
            ),
        ]
        .into_iter()
        .find(|(service, _, _, _)| {
            names.iter().any(|name| name.as_str() == *service)
                || activatable.iter().any(|name| name.as_str() == *service)
        })
        .ok_or_else(|| {
            BrokerError::unavailable("no supported OS power profile service is available")
        })?;
        let name = zbus::names::WellKnownName::try_from(service).map_err(unavailable)?;
        let owner = before(async {
            match bus.get_name_owner(name.clone().into()).await {
                Ok(owner) => Ok(owner),
                Err(zbus::fdo::Error::NameHasNoOwner(_)) => {
                    bus.start_service_by_name(name.clone(), 0).await?;
                    bus.get_name_owner(name.into()).await.map_err(Into::into)
                }
                Err(error) => Err(error.into()),
            }
        })
        .await?;
        if before(async {
            bus.get_connection_unix_user(owner.clone().into())
                .await
                .map_err(Into::into)
        })
        .await?
            != 0
        {
            return Err(BrokerError::unavailable(
                "power profile service is not root-owned",
            ));
        }
        let proxy = zbus::proxy::Builder::<Proxy<'static>>::new(&connection)
            .destination(owner)
            .map_err(unavailable)?
            .path(path)
            .map_err(unavailable)?
            .interface(interface)
            .map_err(unavailable)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
            .map_err(unavailable)?;
        Ok(Self {
            connection,
            proxy,
            service,
            system76,
        })
    }

    async fn current(&self) -> Result<(), BrokerError> {
        let bus = DBusProxy::new(&self.connection)
            .await
            .map_err(unavailable)?;
        let name = BusName::try_from(self.service).map_err(unavailable)?;
        let current = before(async { bus.get_name_owner(name).await.map_err(Into::into) }).await?;
        if current.as_str() != self.proxy.destination().as_str() {
            return Err(BrokerError::unavailable("power profile service changed"));
        }
        Ok(())
    }

    async fn read(&self) -> Result<Profile, BrokerError> {
        let value: String = if self.system76 {
            before(self.proxy.call("GetProfile", &())).await?
        } else {
            before(self.proxy.get_property("ActiveProfile")).await?
        };
        Profile::parse(&value)
    }

    async fn set(&self, profile: Profile) -> Result<(), BrokerError> {
        if self.system76 {
            let method = match profile {
                Profile::Battery => "Battery",
                Profile::Balanced => "Balanced",
                Profile::Performance => "Performance",
            };
            mutation(self.proxy.call::<_, _, ()>(method, &())).await
        } else {
            let profiles: Vec<std::collections::HashMap<String, OwnedValue>> =
                before(self.proxy.get_property("Profiles")).await?;
            let name = if profile == Profile::Battery {
                "power-saver"
            } else {
                profile.name()
            };
            if !profiles.iter().any(|entry| {
                entry
                    .get("Profile")
                    .and_then(|value| value.try_clone().ok())
                    .and_then(|value| String::try_from(value).ok())
                    .as_deref()
                    == Some(name)
            }) {
                return Err(BrokerError::unavailable(
                    "requested power profile is unavailable; no change was dispatched",
                ));
            }
            mutation(async {
                self.proxy
                    .set_property("ActiveProfile", name)
                    .await
                    .map_err(Into::into)
            })
            .await
        }
    }
}

fn unavailable(error: impl std::fmt::Display) -> BrokerError {
    BrokerError::unavailable(format!("OS power profile backend unavailable: {error}"))
}

async fn before<T>(
    future: impl std::future::Future<Output = zbus::Result<T>>,
) -> Result<T, BrokerError> {
    tokio::time::timeout(CALL, future)
        .await
        .map_err(|_| unavailable("query timed out"))?
        .map_err(unavailable)
}

async fn mutation(
    future: impl std::future::Future<Output = zbus::Result<()>>,
) -> Result<(), BrokerError> {
    match tokio::time::timeout(CALL, future).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(zbus::Error::MethodError(name, _, _)))
            if matches!(
                name.as_str(),
                "org.freedesktop.DBus.Error.AccessDenied" | "org.freedesktop.DBus.Error.AuthFailed"
            ) =>
        {
            Err(BrokerError::authorization(
                "OS power profile backend refused the change",
            ))
        }
        _ => Err(BrokerError::indeterminate(
            "OS power profile dispatch is unconfirmed; refresh before retrying",
        )),
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/power/profiles.rs"
    ));
}
