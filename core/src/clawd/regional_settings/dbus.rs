use std::future::Future;
use std::time::Duration;

use serde_json::{json, Value};
use zbus::{
    fdo::DBusProxy,
    names::{BusName, WellKnownName},
    Connection, Proxy,
};

use crate::clawd::authority::{Authorized, Decision};
use crate::clawd::protocol::BrokerError;

use super::request::{locale_confirmed, locale_values, Body, Request};

const CALL_TIMEOUT: Duration = Duration::from_secs(5);
const LOCALE: &str = "org.freedesktop.locale1";
const HOSTNAME: &str = "org.freedesktop.hostname1";
const ACCOUNTS: &str = "org.freedesktop.Accounts";

pub(super) struct Backend {
    connection: Connection,
}

impl Backend {
    pub(super) async fn system() -> Result<Self, BrokerError> {
        let connection = before("connect system bus", async {
            zbus::connection::Builder::address("unix:path=/run/dbus/system_bus_socket")?
                .build()
                .await
        })
        .await?;
        Self::from_connection(connection).await
    }

    async fn from_connection(connection: Connection) -> Result<Self, BrokerError> {
        let bus = DBusProxy::new(&connection).await.map_err(unavailable)?;
        let uid = before("authenticate system bus", async {
            bus.get_connection_unix_user(BusName::try_from("org.freedesktop.DBus")?)
                .await
                .map_err(Into::into)
        })
        .await?;
        if uid != 0 {
            return Err(BrokerError::unavailable(
                "regional settings require a root-owned system bus",
            ));
        }
        Ok(Self { connection })
    }

    async fn proxy(
        &self,
        service: &'static str,
        path: &str,
        interface: &'static str,
    ) -> Result<Proxy<'static>, BrokerError> {
        let bus = DBusProxy::new(&self.connection)
            .await
            .map_err(unavailable)?;
        let name = WellKnownName::try_from(service).map_err(unavailable)?;
        let owner = before("identify regional backend", async {
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
        let uid = before("authenticate regional backend", async {
            bus.get_connection_unix_user(owner.clone().into())
                .await
                .map_err(Into::into)
        })
        .await?;
        if uid != 0 {
            return Err(BrokerError::unavailable(
                "regional backend is not root-owned",
            ));
        }
        zbus::proxy::Builder::<Proxy<'static>>::new(&self.connection)
            .destination(owner)
            .map_err(unavailable)?
            .path(path.to_string())
            .map_err(unavailable)?
            .interface(interface)
            .map_err(unavailable)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
            .map_err(unavailable)
    }

    pub(super) async fn apply(
        &self,
        request: &Request,
        owner: u32,
        authority: &Decision,
    ) -> Result<Value, BrokerError> {
        match &request.0 {
            Body::SystemLocale { lang, region, .. } => {
                let values = locale_values(lang.as_str(), region.as_str());
                let proxy = self
                    .proxy(LOCALE, "/org/freedesktop/locale1", LOCALE)
                    .await?;
                self.require_current(LOCALE, &proxy).await?;
                let authorized = authority
                    .require(request.required_cap())
                    .map_err(BrokerError::authorization)?;
                mutate(
                    &authorized,
                    "set system locale",
                    proxy.call::<_, _, ()>("SetLocale", &(&values, false)),
                )
                .await?;
                let observed: Vec<String> =
                    after("read system locale", proxy.get_property("Locale")).await?;
                if !locale_confirmed(&observed, lang.as_str(), region.as_str()) {
                    return Err(BrokerError::indeterminate("system locale write was acknowledged but readback did not confirm every requested value; refresh before retrying"));
                }
                Ok(json!({"action":request.action(),"status":"applied","locale":values}))
            }
            Body::OwnerLanguage { languages, .. } => {
                let accounts = self
                    .proxy(ACCOUNTS, "/org/freedesktop/Accounts", ACCOUNTS)
                    .await?;
                let path: zbus::zvariant::OwnedObjectPath = before(
                    "resolve owner account",
                    accounts.call("FindUserById", &(i64::from(owner),)),
                )
                .await?;
                let user = zbus::proxy::Builder::<Proxy<'static>>::new(&self.connection)
                    .destination(accounts.destination().to_owned())
                    .map_err(unavailable)?
                    .path(path)
                    .map_err(unavailable)?
                    .interface("org.freedesktop.Accounts.User")
                    .map_err(unavailable)?
                    .cache_properties(zbus::proxy::CacheProperties::No)
                    .build()
                    .await
                    .map_err(unavailable)?;
                let resolved: u64 =
                    before("verify owner account", user.get_property("Uid")).await?;
                if resolved != u64::from(owner) {
                    return Err(BrokerError::authorization(
                        "AccountsService returned a different owner",
                    ));
                }
                self.require_current(ACCOUNTS, &accounts).await?;
                let authorized = authority
                    .require(request.required_cap())
                    .map_err(BrokerError::authorization)?;
                mutate(
                    &authorized,
                    "set owner language",
                    user.call::<_, _, ()>("SetLanguage", &(languages.as_str(),)),
                )
                .await?;
                let observed: String =
                    after("read owner language", user.get_property("Language")).await?;
                if observed != languages.as_str() {
                    return Err(BrokerError::indeterminate(
                        "owner language readback differs after the write; refresh before retrying",
                    ));
                }
                Ok(json!({"action":request.action(),"status":"applied","language":observed}))
            }
            Body::StaticHostname { hostname, .. } => {
                let proxy = self
                    .proxy(HOSTNAME, "/org/freedesktop/hostname1", HOSTNAME)
                    .await?;
                self.require_current(HOSTNAME, &proxy).await?;
                let authorized = authority
                    .require(request.required_cap())
                    .map_err(BrokerError::authorization)?;
                mutate(
                    &authorized,
                    "set static hostname",
                    proxy.call::<_, _, ()>("SetStaticHostname", &(hostname.as_str(), false)),
                )
                .await?;
                let observed: String =
                    after("read static hostname", proxy.get_property("StaticHostname")).await?;
                if observed != hostname.as_str() {
                    return Err(BrokerError::indeterminate(
                        "hostname readback differs after the write; refresh before retrying",
                    ));
                }
                Ok(json!({"action":request.action(),"status":"applied","hostname":observed}))
            }
        }
    }

    async fn require_current(&self, service: &str, proxy: &Proxy<'_>) -> Result<(), BrokerError> {
        let bus = DBusProxy::new(&self.connection)
            .await
            .map_err(unavailable)?;
        let name = BusName::try_from(service).map_err(unavailable)?;
        let current = before("revalidate regional backend", async {
            bus.get_name_owner(name).await.map_err(Into::into)
        })
        .await?;
        if current.as_str() != proxy.destination().as_str() {
            return Err(BrokerError::unavailable(
                "regional backend changed before mutation",
            ));
        }
        Ok(())
    }
}

fn unavailable(error: impl std::fmt::Display) -> BrokerError {
    BrokerError::unavailable(format!(
        "regional backend unavailable before mutation: {error}"
    ))
}

async fn before<T>(
    phase: &str,
    future: impl Future<Output = zbus::Result<T>>,
) -> Result<T, BrokerError> {
    tokio::time::timeout(CALL_TIMEOUT, future)
        .await
        .map_err(|_| BrokerError::unavailable(format!("{phase} timed out before mutation")))?
        .map_err(|error| {
            BrokerError::unavailable(format!("{phase} failed before mutation: {error}"))
        })
}

async fn mutate<T>(
    _authorized: &Authorized,
    phase: &str,
    future: impl Future<Output = zbus::Result<T>>,
) -> Result<T, BrokerError> {
    match tokio::time::timeout(CALL_TIMEOUT, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(zbus::Error::MethodError(name, _, _)))
            if matches!(
                name.as_str(),
                "org.freedesktop.DBus.Error.AccessDenied" | "org.freedesktop.DBus.Error.AuthFailed"
            ) =>
        {
            Err(BrokerError::authorization(format!(
                "{phase} refused by the OS backend"
            )))
        }
        Ok(Err(error)) => Err(BrokerError::indeterminate(format!(
            "{phase} outcome is unknown: {error}; refresh before retrying"
        ))),
        Err(_) => Err(BrokerError::indeterminate(format!(
            "{phase} timed out; the change may have applied; refresh before retrying"
        ))),
    }
}

async fn after<T>(
    phase: &str,
    future: impl Future<Output = zbus::Result<T>>,
) -> Result<T, BrokerError> {
    tokio::time::timeout(CALL_TIMEOUT, future)
        .await
        .map_err(|_| {
            BrokerError::indeterminate(format!(
                "{phase} timed out after mutation; refresh before retrying"
            ))
        })?
        .map_err(|error| {
            BrokerError::indeterminate(format!(
                "{phase} failed after mutation: {error}; refresh before retrying"
            ))
        })
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/regional_settings/dbus.rs"
    ));
}
