// SPDX-License-Identifier: GPL-3.0-only

use claw_applet_services::{calendar, system, tasks};
use claw_applet_widget_rail::{CalendarEvent, Providers, SystemSummary, Task, Usage};
use std::sync::Arc;
use tokio::sync::Mutex;

pub fn providers() -> Providers {
    // Sampling deltas are process-local provider state, never a new data store.
    let sample = Arc::new(Mutex::new(system::RawSample::default()));
    Providers::new(
        || Box::pin(async { calendar::load_today().await.map(|events| {
            events.into_iter().map(calendar_event).collect()
        }) }),
        || Box::pin(async { tasks::observe().await.map(|tasks| {
            tasks.into_iter().map(task).collect()
        }) }),
        move || {
            let sample = Arc::clone(&sample);
            Box::pin(async move {
                let mut previous = sample.lock().await;
                let (summary, current) = system::load(*previous).await?;
                *previous = current;
                Ok(system_summary(summary))
            })
        },
    )
}

fn calendar_event(event: calendar::CalendarEvent) -> CalendarEvent {
    CalendarEvent {
        id: event.id,
        title: event.title,
        start: event.start,
        end: event.end,
        location: event.location,
    }
}

fn task(task: tasks::Task) -> Task {
    Task {
        id: task.id,
        purpose: task.purpose,
        status: task.status,
        created_at: task.created_at,
    }
}

fn system_summary(summary: system::SystemSummary) -> SystemSummary {
    let usage = |value: system::Usage| Usage {
        used_mb: value.used_mb,
        total_mb: value.total_mb,
    };
    SystemSummary {
        cpu_percent: summary.cpu_percent,
        memory: summary.memory.map(usage),
        storage: summary.storage.map(usage),
        network_down_bps: summary.network_down_bps,
        network_up_bps: summary.network_up_bps,
        fallback: summary.fallback,
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/widget_rail.rs"));
}
