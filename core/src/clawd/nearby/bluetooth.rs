//! Fixed anonymous AirDrop wake advertisement; never contact hashes or pairing.

use std::collections::HashMap;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

struct Advert {
    tag: [u8; 3],
}

#[zbus::interface(name = "org.bluez.LEAdvertisement1")]
impl Advert {
    fn release(&self) {}
    #[zbus(property, name = "Type")]
    fn kind(&self) -> &str {
        "peripheral"
    }
    #[zbus(property)]
    fn min_interval(&self) -> u32 {
        100
    }
    #[zbus(property)]
    fn max_interval(&self) -> u32 {
        100
    }
    #[zbus(property)]
    fn manufacturer_data(&self) -> HashMap<u16, Value<'_>> {
        let mut bytes = vec![5u8, 18, 0x40];
        bytes.extend_from_slice(&self.tag);
        bytes.extend_from_slice(&[0, 0, 0, 0, 3]);
        bytes.extend_from_slice(&[0; 9]);
        HashMap::from([(0x004c, Value::from(bytes))])
    }
}

pub(super) struct Registration {
    connection: zbus::Connection,
    adapter: OwnedObjectPath,
    path: OwnedObjectPath,
}

impl Registration {
    pub async fn start() -> Result<Self, String> {
        let connection = zbus::Connection::system()
            .await
            .map_err(|e| e.to_string())?;
        let objects = zbus::fdo::ObjectManagerProxy::builder(&connection)
            .destination("org.bluez")
            .map_err(|e| e.to_string())?
            .path("/")
            .map_err(|e| e.to_string())?
            .build()
            .await
            .map_err(|e| e.to_string())?
            .get_managed_objects()
            .await
            .map_err(|e| e.to_string())?;
        let adapter = objects
            .into_iter()
            .filter(|(_, interfaces)| {
                interfaces
                    .keys()
                    .any(|name| name.as_str() == "org.bluez.LEAdvertisingManager1")
            })
            .map(|(path, _)| path)
            .min_by(|a, b| a.as_str().cmp(b.as_str()))
            .ok_or("No Bluetooth LE advertising adapter; enable a supported Bluetooth adapter.")?;
        let properties = zbus::fdo::PropertiesProxy::builder(&connection)
            .destination("org.bluez")
            .map_err(|e| e.to_string())?
            .path(adapter.clone())
            .map_err(|e| e.to_string())?
            .build()
            .await
            .map_err(|e| e.to_string())?;
        let powered: bool = properties
            .get(
                "org.bluez.Adapter1"
                    .try_into()
                    .map_err(|e: zbus::names::Error| e.to_string())?,
                "Powered",
            )
            .await
            .map_err(|e| e.to_string())?
            .try_into()
            .map_err(|e: zbus::zvariant::Error| e.to_string())?;
        if !powered {
            return Err("Bluetooth is off. Enable it explicitly; nearby sharing does not silently change its power state.".into());
        }
        let id = uuid::Uuid::new_v4();
        let path: OwnedObjectPath = format!("/org/clawos/Nearby/advert_{}", id.simple())
            .try_into()
            .map_err(|e: zbus::zvariant::Error| e.to_string())?;
        let bytes = id.as_bytes();
        let tag = [bytes[0], bytes[1], bytes[2]];
        connection
            .object_server()
            .at(path.clone(), Advert { tag })
            .await
            .map_err(|e| e.to_string())?;
        connection
            .call_method(
                Some("org.bluez"),
                adapter.as_str(),
                Some("org.bluez.LEAdvertisingManager1"),
                "RegisterAdvertisement",
                &(
                    ObjectPath::from(path.clone()),
                    HashMap::<String, OwnedValue>::new(),
                ),
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self {
            connection,
            adapter,
            path,
        })
    }

    pub async fn stop(self) -> Result<(), String> {
        self.connection
            .call_method(
                Some("org.bluez"),
                self.adapter.as_str(),
                Some("org.bluez.LEAdvertisingManager1"),
                "UnregisterAdvertisement",
                &(ObjectPath::from(self.path),),
            )
            .await
            .map_err(|e| format!("Bluetooth advertisement cleanup: {e}"))?;
        Ok(())
    }
}
