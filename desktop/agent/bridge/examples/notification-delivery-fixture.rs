//! Explicit historical private-bus adapter fixture; production uses the GUI Host.
#[path = "../src/notifications.rs"]
mod notifications;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("fixture presenter path required"))?;
    anyhow::ensure!(
        std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some(),
        "private bus required"
    );
    let presenter = std::fs::metadata(path)?;
    let client = clawd_client::Client::from_env()?;
    let bus = zbus::Connection::session().await?;
    println!("ready");
    notifications::run_connection(&client, &bus, &presenter).await
}
