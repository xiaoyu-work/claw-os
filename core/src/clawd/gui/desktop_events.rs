//! One read-only event connection for each authenticated GUI instance.

use std::sync::atomic::{AtomicBool, Ordering};

use claw_display_control::ProcessIdentity;
use serde_json::{json, Value};
use tokio::sync::watch;

use crate::caps::{Cap, Scope, Verb};
use crate::clawd::protocol::{BrokerError, Response};
use crate::clawd::transport::PeerStream;
use crate::clawd::wire::{RequestId, MAX_REQUEST_BYTES};

use super::broker::respond;
use super::manager::{Job, Manager, Phase};

struct Subscription<'a>(&'a AtomicBool);

impl Drop for Subscription<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn authorize(job: &Job, process: &ProcessIdentity) -> Result<(), BrokerError> {
    let registration = &job.registration;
    let view = registration.snapshot(process).map_err(BrokerError::authorization)?;
    if job.stopping()
        || job.state.lock().map_err(|_| BrokerError::unavailable("GUI status lock poisoned"))?.phase != Phase::Running
        || view.expires_in.is_zero()
    {
        return Err(BrokerError::authorization("the desktop GUI is no longer live"));
    }
    if registration.desktop_shell {
        let observe = Cap::new(Verb::SYS_OBSERVE, Scope::name("desktop"));
        if !view.caps.covers(&observe) {
            return Err(BrokerError::authorization("desktop observation is not permitted"));
        }
    } else if registration.gui_entry.is_some() || job.desktop_ids.is_empty() {
        return Err(BrokerError::authorization("reopen events require an exported default GUI"));
    }
    Ok(())
}

async fn snapshot(job: &Job, observe: bool) -> Result<Value, BrokerError> {
    let registration = &job.registration;
    let control = &registration.control;
    let mut value = crate::clawd::desktop::window_snapshot(control, observe)
        .await.map_err(BrokerError::execution)?;
    let running = Manager::get().and_then(|manager| {
        manager.running_desktop_ids(registration.owner(), control.epoch)
    }).map_err(BrokerError::unavailable)?;
    value["running_apps"] = json!(running);
    Ok(value)
}

pub(super) async fn serve(
    mut stream: PeerStream,
    job: &Job,
    process: &ProcessIdentity,
    id: RequestId,
    mut shutdown: watch::Receiver<bool>,
) {
    if let Err(error) = authorize(job, process) {
        respond(&mut stream, Response::handler_error(id, error)).await;
        return;
    }
    if job.desktop_feed.swap(true, Ordering::AcqRel) {
        respond(&mut stream, Response::handler_error(id, BrokerError::authorization(
            "this GUI already has a desktop event connection",
        ))).await;
        return;
    }
    let _subscription = Subscription(&job.desktop_feed);
    let shell = job.registration.desktop_shell;
    let mut changes = if shell {
        job.registration.control.desktop_updates()
    } else {
        job.reopen.subscribe()
    };
    let mut previous = None;
    loop {
        let payload = if shell {
            tokio::select! {
                _ = shutdown.changed() => return,
                payload = snapshot(job, previous.is_none()) => payload,
            }
        } else {
            Ok(json!({"event": if previous.is_none() { "ready" } else { "reopen" }}))
        };
        let payload = match payload.and_then(|payload| {
            authorize(job, process)?;
            Ok(payload)
        }) {
            Ok(payload) => payload,
            Err(error) => {
                respond(&mut stream, Response::handler_error(id, error)).await;
                return;
            }
        };
        if !shell || previous.as_ref() != Some(&payload) {
            let response = Response::ok(id.clone(), payload.clone());
            if !tokio::select! {
                _ = shutdown.changed() => false,
                succeeded = respond(&mut stream, response) => succeeded,
            } {
                return;
            }
            previous = Some(payload);
        }
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = stream.read_request(MAX_REQUEST_BYTES) => return,
            result = changes.changed() => {
                if result.is_err() { return; }
            }
        }
    }
}
