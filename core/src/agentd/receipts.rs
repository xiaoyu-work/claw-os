//! Broker-side association of worker reports with the broker's own task.

use crate::activities::ReceiptReport;
use crate::agent::service::{Job, Store};
use crate::operations::receipts::diagnostic;

use super::protocol::{self, ReceiptReply};

pub(super) fn record(
    used: &mut u32,
    store: &Store,
    owner_uid: u32,
    task_id: &str,
    job: &Job,
    report: ReceiptReport,
) -> ReceiptReply {
    *used = used.saturating_add(1);
    let result = if *used > protocol::MAX_RECEIPT_REPORTS {
        Err("receipt-reporting budget exhausted for this task".to_string())
    } else {
        record_owned(store, owner_uid, task_id, job, report)
    };
    match result {
        Ok(receipt_id) => ReceiptReply::Recorded { receipt_id },
        Err(error) => {
            tracing::warn!(task = %task_id, owner_uid, error = %error, "Activity receipt report refused");
            ReceiptReply::Refused {
                message: diagnostic(&error),
            }
        }
    }
}

fn record_owned(
    store: &Store,
    owner_uid: u32,
    task_id: &str,
    job: &Job,
    report: ReceiptReport,
) -> Result<String, String> {
    protocol::validate_receipt_report(&report)?;
    if job.id != task_id || job.owner_uid != Some(owner_uid) {
        return Err("receipt report does not match the leased task owner".to_string());
    }
    let activity_id = job
        .activity_id
        .as_deref()
        .ok_or_else(|| "leased task has no Activity association".to_string())?;
    let receipt = crate::clawd::activity_receipts::record_for_owner(owner_uid, activity_id, report)
        .map_err(|error| error.to_string())?;
    store
        .append_stream_progress(
            task_id,
            serde_json::json!({
                "kind": "activity_receipt",
                "activity_id": receipt.activity_id,
                "receipt_id": receipt.id,
                "source": receipt.source,
                "reported_outcome": receipt.report.outcome,
            }),
        )
        .map_err(|error| {
            format!(
                "receipt {} was stored but its task reporting link could not be recorded: {error}",
                receipt.id
            )
        })?;
    Ok(receipt.id)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agentd/receipts.rs"
    ));
}
