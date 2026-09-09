//! A synthetic surface over the actual native subscription and D-Bus server.
//! It never opens Wayland or reads/writes notification state.
#[path = "../src/config.rs"]
mod config;
mod subscriptions {
    pub mod applet {
        include!("../src/subscriptions/applet.rs");
    }
    pub mod notifications {
        include!("../src/subscriptions/notifications.rs");

        pub async fn fixture() -> anyhow::Result<()> {
            use cosmic::iced::futures::StreamExt;
            use tokio::io::{AsyncBufReadExt, BufReader};
            let (output, mut events) = mpsc::channel(100);
            let (waiting, conns) = Machine::<Start>::new(None, output)
                .exec()
                .await
                .map_err(|_| anyhow::anyhow!("fixture connection failed"))?;
            let tx = conns.tx.clone();
            tokio::spawn(waiting.exec(conns));
            let mut lines = BufReader::new(tokio::io::stdin()).lines();
            println!("ready");
            loop {
                tokio::select! {
                    event = events.next() => match event {
                        Some(Event::Notification(value) | Event::Replace(value)) => println!("{}", serde_json::json!({"rendered":value})),
                        Some(Event::CloseNotification(id)) => {
                            println!("{}", serde_json::json!({"closed":id}));
                            tx.send(Input::Closed(id, CloseReason::CloseNotification)).await?;
                        }
                        Some(_) => {}
                        None => break,
                    },
                    line = lines.next_line() => {
                        let Some(line) = line? else { break };
                        let value: serde_json::Value = serde_json::from_str(&line)?;
                        let id = u32::try_from(value["id"].as_u64().ok_or_else(|| anyhow::anyhow!("id required"))?)?;
                        match value["action"].as_str() {
                            Some("ack") => {
                                tx.send(Input::Activated { token:"fixture".into(), id, action:"default".into() }).await?;
                                tx.send(Input::Closed(id, CloseReason::Dismissed)).await?;
                            }
                            Some("dismiss") => tx.send(Input::Closed(id, CloseReason::Dismissed)).await?,
                            _ => anyhow::bail!("unknown synthetic UI action"),
                        }
                    }
                }
            }
            Ok(())
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    anyhow::ensure!(
        std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some(),
        "private bus required"
    );
    subscriptions::notifications::fixture().await
}
