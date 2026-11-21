//! Private owner-side client of the actual OS panel descriptor and managed App.

use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::time::Duration;

use claw_notification_presentation::{Card as PresentationCard, Preferences, PRESENTER_FD_ENV};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::io::AsyncBufReadExt;
use zbus::{connection::Builder, proxy};

#[proxy(
    interface = "com.yuos.NotificationsSocket",
    default_service = "com.yuos.NotificationsSocket",
    default_path = "/com/yuos/NotificationsSocket"
)]
trait Socket {
    fn get_fd(&self) -> zbus::Result<zbus::zvariant::OwnedFd>;
}

#[proxy(
    interface = "com.yuos.NotificationPresentation1",
    default_service = "com.yuos.NotificationPresentation1",
    default_path = "/com/yuos/NotificationPresentation1"
)]
trait Presentation {
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

#[proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Closed {
    #[zbus(signal)]
    fn notification_closed(&self, id: u32, reason: u32) -> zbus::Result<()>;
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if unsafe { libc::geteuid() } == 0
        || std::env::var("CLAW_NOTIFICATION_PRIVATE_FIXTURE").as_deref() != Ok("1")
    {
        return Err("run only as a synthetic owner in the isolated native fixture".into());
    }
    let (mine, theirs) = std::os::unix::net::UnixStream::pair()?;
    let raw = theirs.as_raw_fd();
    let mut command = std::process::Command::new("/usr/lib/cos/bin/claw-notification-panel");
    command
        .env(PRESENTER_FD_ENV, raw.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit());
    unsafe {
        command.pre_exec(move || {
            let flags = libc::fcntl(raw, libc::F_GETFD);
            if flags < 0 || libc::fcntl(raw, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut command = tokio::process::Command::from(command);
    command.kill_on_drop(true);
    let mut helper = command.spawn()?;
    drop(theirs);
    mine.set_nonblocking(true)?;
    let socket = tokio::time::timeout(
        Duration::from_secs(3),
        Builder::socket(tokio::net::UnixStream::from_std(mine)?)
            .p2p()
            .build(),
    )
    .await??;
    let socket_proxy = SocketProxy::new(&socket).await?;
    let descriptor: OwnedFd = socket_proxy.get_fd().await?.into();
    let stream = std::os::unix::net::UnixStream::from(descriptor);
    stream.set_nonblocking(true)?;
    let connection = Builder::socket(tokio::net::UnixStream::from_std(stream)?)
        .p2p()
        .build()
        .await?;
    let proxy = PresentationProxy::new(&connection).await?;
    assert_eq!(proxy.version().await?, 1);
    let mut cards = proxy.receive_card().await?;
    let mut preferences = proxy.receive_preferences_changed().await?;
    let mut failures = proxy.receive_failure().await?;
    let closed = ClosedProxy::new(&connection).await?;
    let mut closures = closed.receive_notification_closed().await?;
    println!("{}", json!({"ready":true,"helper_pid":helper.id()}));
    let mut input = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    loop {
        tokio::select! {
            message = cards.next() => {
                let message = message.ok_or("panel card descriptor closed")?;
                let card = PresentationCard::from_json(message.args()?.payload)?;
                println!("{}", json!({"card":card}));
            }
            message = preferences.next() => {
                let message = message.ok_or("panel preferences descriptor closed")?;
                println!("{}", json!({"preferences":Preferences::from_json(message.args()?.payload)?}));
            }
            message = failures.next() => {
                let message = message.ok_or("panel failure descriptor closed")?;
                println!("{}", json!({"unavailable":message.args()?.message}));
            }
            message = closures.next() => {
                let message = message.ok_or("panel close descriptor closed")?;
                let args = message.args()?;
                println!("{}", json!({"closed":args.id,"reason":args.reason}));
            }
            line = input.next_line() => {
                let Some(line) = line? else { break; };
                let request: Value = serde_json::from_str(&line)?;
                let id = request["id"].as_u64().unwrap_or(0);
                let result = match request["action"].as_str() {
                    Some("claim") | Some("availability") => {
                        let (command, params) = if request["action"] == "claim" {
                            (cos::clawd::routes::Command::NotificationDeliveryClaim,
                                json!({"channel":"desktop","limit":1}))
                        } else {
                            (cos::clawd::routes::Command::parse("notification.presentation.control")
                                .ok_or("Root notification panel route is unavailable")?,
                                json!({"action":"snapshot","after":0}))
                        };
                        let response = cos::clawd::client::request(cos::clawd::config::socket_path(),
                            cos::clawd::protocol::Request::new(command, params)).await?;
                        json!({"ok":response.ok,"result":response.result,"error":response.error})
                    }
                    Some("preferences") => json!({"preferences":Preferences::from_json(&proxy.preferences().await?)?}),
                    Some("muted") => {
                        let preferences = Preferences::new(request["muted"].as_bool().ok_or("muted must be boolean")?);
                        json!({"preferences":Preferences::from_json(&proxy.set_preferences(&preferences.to_json()?).await?)?})
                    }
                    Some("dismiss") => {
                        proxy.dismiss(u32::try_from(id)?).await?;
                        json!({"dismissed":id})
                    }
                    Some("activate") => {
                        proxy.invoke_action(u32::try_from(id)?, "default").await?;
                        json!({"activated":id})
                    }
                    _ => return Err("unknown private panel fixture action".into()),
                };
                println!("{}", json!({"response":result}));
            }
        }
    }
    drop(closures);
    drop(closed);
    drop(failures);
    drop(preferences);
    drop(cards);
    drop(proxy);
    drop(connection);
    drop(socket_proxy);
    drop(socket);
    if !tokio::time::timeout(Duration::from_secs(3), helper.wait())
        .await??
        .success()
    {
        return Err("OS notification panel transport did not retire".into());
    }
    Ok(())
}
