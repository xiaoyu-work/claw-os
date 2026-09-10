//! Client declarations for an existing private presentation connection.
//! Constructing a proxy neither opens a session bus nor chooses an App.

use zbus::proxy;

#[proxy(
    default_service = "com.clawos.NotificationPresentation1",
    interface = "com.clawos.NotificationPresentation1",
    default_path = "/com/clawos/NotificationPresentation1"
)]
pub trait NotificationPresentation {
    fn version(&self) -> zbus::Result<u32>;
    fn preferences(&self) -> zbus::Result<String>;
    fn set_preferences(&self, payload: &str) -> zbus::Result<String>;
    fn dismiss(&self, id: u32) -> zbus::Result<()>;
    fn invoke_action(&self, id: u32, action: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    fn card(&self, payload: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    fn preferences_changed(&self, payload: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    fn failure(&self, message: &str) -> zbus::Result<()>;
}
