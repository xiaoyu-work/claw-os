// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use claw_notification_presentation::{
    APPLET_FD_ENV, Card, Preferences, VERSION, proxy::NotificationPresentationProxy,
    validate_action, validate_handle,
};
use cosmic::iced::{Subscription, futures, stream};
use futures_util::{SinkExt, StreamExt};
use std::{future::Future, os::fd::FromRawFd, time::Duration};
use tokio::sync::mpsc;
use zbus::connection::Builder;

pub const CALL_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
pub enum Input {
    Activated(u32, String),
    Dismiss(u32),
    SetMuted(bool),
}

#[derive(Debug, Clone)]
pub enum Output {
    Ready(mpsc::Sender<Input>),
    Card(Card),
    Preferences(Preferences),
    PreferenceResult(Result<Preferences, String>),
    Dismissed(u32),
    RequestFailed(String),
    Disconnected(String),
    Queued,
}

pub fn notifications() -> Subscription<Output> {
    struct Presenter;
    Subscription::run_with(std::any::TypeId::of::<Presenter>(), |_| {
        stream::channel(
            50,
            |mut output: futures::channel::mpsc::Sender<Output>| async move {
                let result = async {
                    let proxy = get_proxy().await?;
                    client(&proxy, &mut output).await
                }
                .await;
                if let Err(error) = result {
                    let _ = output.send(Output::Disconnected(error)).await;
                }
                futures::future::pending::<()>().await;
            },
        )
    })
}

async fn bounded<T>(future: impl Future<Output = zbus::Result<T>>) -> Result<T, String> {
    tokio::time::timeout(CALL_TIMEOUT, future)
        .await
        .map_err(|_| "Notification presentation timed out.".to_string())?
        .map_err(|error| error.to_string())
}

async fn send(
    output: &mut futures::channel::mpsc::Sender<Output>,
    value: Output,
) -> Result<(), String> {
    output
        .send(value)
        .await
        .map_err(|_| "Notification presentation view closed.".to_string())
}

async fn client(
    proxy: &NotificationPresentationProxy<'static>,
    output: &mut futures::channel::mpsc::Sender<Output>,
) -> Result<(), String> {
    if bounded(proxy.version()).await? != VERSION {
        return Err("The selected presenter does not support notification presentation v1.".into());
    }
    let mut cards = bounded(proxy.receive_card()).await?;
    let mut preferences = bounded(proxy.receive_preferences_changed()).await?;
    let mut failures = bounded(proxy.receive_failure()).await?;
    let initial = Preferences::from_json(&bounded(proxy.preferences()).await?)
        .map_err(|error| error.to_string())?;
    let (sender, mut input) = mpsc::channel(10);
    send(output, Output::Preferences(initial)).await?;
    send(output, Output::Ready(sender)).await?;
    loop {
        tokio::select! {
            message = cards.next() => {
                let message = message.ok_or("Notification presentation connection closed.")?;
                let args = message.args().map_err(|error| error.to_string())?;
                let card = Card::from_json(args.payload).map_err(|error| error.to_string())?;
                send(output, Output::Card(card)).await?;
            }
            message = preferences.next() => {
                let message = message.ok_or("Notification preference connection closed.")?;
                let args = message.args().map_err(|error| error.to_string())?;
                let value = Preferences::from_json(args.payload).map_err(|error| error.to_string())?;
                send(output, Output::Preferences(value)).await?;
            }
            message = failures.next() => {
                let message = message.ok_or("Notification presentation connection closed.")?;
                let args = message.args().map_err(|error| error.to_string())?;
                send(output, Output::RequestFailed(args.message.chars().take(4096).collect())).await?;
            }
            message = input.recv() => {
                match message.ok_or("Notification presentation controls closed.")? {
                    Input::SetMuted(muted) => {
                        send(output, Output::PreferenceResult(set_muted(proxy, muted).await)).await?;
                    }
                    Input::Dismiss(id) => {
                        let result = match validate_handle(id) {
                            Ok(()) => bounded(proxy.dismiss(id)).await,
                            Err(error) => Err(error.to_string()),
                        };
                        let event = match result {
                            Ok(()) => Output::Dismissed(id),
                            Err(error) => Output::RequestFailed(error),
                        };
                        send(output, event).await?;
                    }
                    Input::Activated(id, action) => {
                        let result = match validate_action(id, &action) {
                            Ok(()) => bounded(proxy.invoke_action(id, &action)).await,
                            Err(error) => Err(error.to_string()),
                        };
                        if let Err(error) = result {
                            send(output, Output::RequestFailed(error)).await?;
                        }
                    }
                }
            }
        }
    }
}

async fn set_muted(
    proxy: &NotificationPresentationProxy<'static>,
    muted: bool,
) -> Result<Preferences, String> {
    let payload = Preferences::new(muted)
        .to_json()
        .map_err(|error| error.to_string())?;
    let value = Preferences::from_json(&bounded(proxy.set_preferences(&payload)).await?)
        .map_err(|error| error.to_string())?;
    if value.muted != muted {
        return Err("The presenter did not confirm the requested popup mute.".into());
    }
    Ok(value)
}

async fn get_proxy() -> Result<NotificationPresentationProxy<'static>, String> {
    let fd = std::env::var(APPLET_FD_ENV)
        .map_err(|_| "Notification presentation connection is unavailable.".to_string())?
        .parse::<i32>()
        .map_err(|_| "Invalid notification presentation descriptor.".to_string())?;
    if fd < 3 {
        return Err("Invalid notification presentation descriptor.".into());
    }
    let stream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
    stream
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    let stream = tokio::net::UnixStream::from_std(stream).map_err(|error| error.to_string())?;
    let connection = bounded(Builder::socket(stream).p2p().build()).await?;
    bounded(NotificationPresentationProxy::new(&connection)).await
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/subscriptions/notifications.rs"
    ));
}
