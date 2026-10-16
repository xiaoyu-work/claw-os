use super::{Job, JobStatus, Store};
use std::collections::BTreeSet;
use std::io;

pub(super) enum Dependency {
    Ready,
    Blocked,
    Invalid(String),
}

pub(super) fn state(store: &Store, job: &Job) -> io::Result<Dependency> {
    let mut next = job.after_task_id.clone();
    let mut seen = BTreeSet::from([job.id.clone()]);
    while let Some(id) = next {
        if seen.len() > 1024 || !seen.insert(id.clone()) {
            return Ok(Dependency::Invalid(
                "queued predecessor chain is cyclic or exceeds its bound".into(),
            ));
        }
        let Some((_, predecessor)) = store.locate(&id)? else {
            return Ok(Dependency::Invalid(format!(
                "queued predecessor task is unavailable: {id}"
            )));
        };
        if predecessor.owner_uid != job.owner_uid || predecessor.session_id != job.session_id {
            return Ok(Dependency::Invalid(
                "queued predecessor has a different owner or conversation".into(),
            ));
        }
        if !matches!(
            predecessor.status,
            JobStatus::Ok | JobStatus::Error | JobStatus::Cancelled
        ) {
            return Ok(Dependency::Blocked);
        }
        next = predecessor.after_task_id;
    }
    Ok(Dependency::Ready)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/service/dependencies.rs"
    ));
}
