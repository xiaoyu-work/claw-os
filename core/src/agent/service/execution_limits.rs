//! Live Activity limits plus a monotonic deadline independent of heartbeats.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;

use crate::activities::ActivityService;
use crate::agent::service::Job;

pub(crate) struct ExecutionLimitsGuard {
    service: Option<Arc<dyn ActivityService>>,
    deadline: Option<Instant>,
    next_policy_check: Instant,
}

impl ExecutionLimitsGuard {
    pub(crate) fn check_now(&mut self, job: &Job) -> Result<Option<String>, String> {
        self.next_policy_check = Instant::now();
        self.check(job)
    }

    pub(crate) fn new(job: &Job) -> Result<Self, String> {
        let service: Option<Arc<dyn ActivityService>> = if job.activity_id.is_some() {
            Some(Arc::new(
                crate::activities::open_default().map_err(|error| error.to_string())?,
            ))
        } else {
            None
        };
        Self::with_service(job, service)
    }

    fn with_service(job: &Job, service: Option<Arc<dyn ActivityService>>) -> Result<Self, String> {
        let deadline = job
            .execution_reservation
            .as_ref()
            .map(|reservation| {
                let expires = chrono::DateTime::parse_from_rfc3339(&reservation.expires_at)
                    .map_err(|error| format!("invalid Activity execution deadline: {error}"))?;
                let remaining = (expires.with_timezone(&Utc) - Utc::now())
                    .to_std()
                    .map_err(|_| "Activity execution limit expired".to_string())?;
                Instant::now()
                    .checked_add(remaining)
                    .ok_or_else(|| "Activity execution deadline cannot be represented".to_string())
            })
            .transpose()?;
        let mut guard = Self {
            service,
            deadline,
            next_policy_check: Instant::now(),
        };
        if let Some(reason) = guard.check(job)? {
            return Err(reason);
        }
        Ok(guard)
    }

    pub(crate) fn check(&mut self, job: &Job) -> Result<Option<String>, String> {
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Ok(Some("Activity execution limit expired".into()));
        }
        if Instant::now() < self.next_policy_check {
            return Ok(None);
        }
        self.next_policy_check = Instant::now() + Duration::from_secs(1);
        let Some(service) = &self.service else {
            return if job.execution_reservation.is_some() {
                Err("execution reservation has no Activity association".into())
            } else {
                Ok(None)
            };
        };
        let activity = job
            .activity_id
            .as_deref()
            .ok_or_else(|| "Activity execution lost its association".to_string())?;
        let owner = job
            .owner_uid
            .ok_or_else(|| "Activity execution has no owner".to_string())?;
        let policy = service
            .execution_limits(owner, activity)
            .map_err(|error| format!("read live Activity execution limits: {error}"))?;
        match (&job.execution_reservation, policy) {
            (None, None) => Ok(None),
            (None, Some(_)) => Ok(Some(
                "Activity now requires a reserved bounded attempt".into(),
            )),
            (Some(_), None) => Err("reserved Activity execution lost its policy".into()),
            (Some(reservation), Some(policy)) => {
                if reservation.owner_uid != owner
                    || reservation.activity_id != activity
                    || reservation.job_id != job.id
                    || policy.owner_uid != owner
                    || policy.activity_id != activity
                    || reservation.max_turns == 0
                    || reservation.max_turns > 100
                {
                    return Err(
                        "Activity execution reservation identity or bounds are invalid".into(),
                    );
                }
                if !policy.enabled {
                    return Ok(Some("Activity execution limits are disabled".into()));
                }
                if reservation.policy_revision != policy.revision {
                    return Ok(Some(
                        "Activity execution policy changed; a fresh attempt is required".into(),
                    ));
                }
                let expires = chrono::DateTime::parse_from_rfc3339(&policy.limits.expires_at)
                    .map_err(|error| format!("invalid live Activity execution expiry: {error}"))?;
                if expires.with_timezone(&Utc) <= Utc::now() {
                    return Ok(Some("Activity execution limit expired".into()));
                }
                if reservation.max_turns > policy.limits.max_turns_per_attempt
                    || reservation.expires_at != policy.limits.expires_at
                {
                    return Err("Activity execution reservation exceeds the current policy".into());
                }
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/service/execution_limits.rs"
    ));
}
