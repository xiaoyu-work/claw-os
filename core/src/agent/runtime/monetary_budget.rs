//! Injected per-turn Activity monetary accounting.

use crate::activities::{
    ActivityService, MonetaryReservation, MonetaryReservationRequest, MonetarySettlement,
};
use crate::agent::llm::{ChatRequest, Usage};

pub(crate) trait MonetaryBudgetController: Send + Sync {
    fn reserve(
        &self,
        request: MonetaryReservationRequest,
    ) -> Result<Option<MonetaryReservation>, String>;

    fn settle(
        &self,
        reservation: &MonetaryReservation,
        settlement: MonetarySettlement,
    ) -> Result<(), String>;
}

pub(crate) struct DirectMonetaryBudgetController {
    service: std::sync::Arc<dyn ActivityService>,
    owner_uid: u32,
    activity_id: String,
    job_id: String,
    session_id: Option<String>,
}

impl DirectMonetaryBudgetController {
    pub(crate) fn new(
        service: std::sync::Arc<dyn ActivityService>,
        owner_uid: u32,
        activity_id: String,
        job_id: String,
        session_id: Option<String>,
    ) -> Self {
        Self {
            service,
            owner_uid,
            activity_id,
            job_id,
            session_id,
        }
    }
}

impl MonetaryBudgetController for DirectMonetaryBudgetController {
    fn reserve(
        &self,
        mut request: MonetaryReservationRequest,
    ) -> Result<Option<MonetaryReservation>, String> {
        request.job_id = self.job_id.clone();
        request.session_id = self.session_id.clone();
        self.service
            .reserve_monetary(self.owner_uid, &self.activity_id, request)
            .map_err(|error| error.to_string())
    }

    fn settle(
        &self,
        reservation: &MonetaryReservation,
        settlement: MonetarySettlement,
    ) -> Result<(), String> {
        self.service
            .settle_monetary(
                self.owner_uid,
                &self.activity_id,
                &reservation.call_id,
                &self.job_id,
                self.session_id.as_deref(),
                reservation.turn_index,
                settlement,
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

pub(crate) fn serialized_input_upper_bound(request: &ChatRequest) -> Result<u64, String> {
    let bytes = serde_json::to_vec(request).map_err(|error| {
        format!("serialize outgoing model request for monetary budget: {error}")
    })?;
    u64::try_from(bytes.len())
        .map_err(|_| "outgoing model request byte length is not representable".to_string())
}

pub(crate) fn actual_settlement(
    usage: &Usage,
    provider: String,
    model: String,
) -> MonetarySettlement {
    MonetarySettlement {
        input_tokens: Some(usage.input_tokens),
        output_tokens: Some(usage.output_tokens),
        cache_read_tokens: Some(usage.cache_read_tokens),
        cache_write_tokens: Some(usage.cache_write_tokens),
        provider,
        model,
        conservative: false,
    }
}

pub(crate) fn conservative_settlement(provider: String, model: String) -> MonetarySettlement {
    MonetarySettlement {
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        provider,
        model,
        conservative: true,
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/runtime/monetary_budget.rs"
    ));
}
