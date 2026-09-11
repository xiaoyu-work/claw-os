//! Shared report delivery; a recording failure must never repeat App execution.

use std::sync::Arc;

use crate::activities::ReceiptReport;
use crate::operations::{receipts, reporting::ReceiptRecorder};

use super::ToolResult;

pub(super) async fn deliver(
    mut output: ToolResult,
    recorder: Arc<dyn ReceiptRecorder>,
    report: ReceiptReport,
) -> ToolResult {
    let submitted = report.clone();
    let recorded = match tokio::task::spawn_blocking(move || recorder.record(submitted)).await {
        Ok(Ok(id)) if id == report.id => return output,
        Ok(Ok(_)) => "receipt recorder acknowledged a different report".to_string(),
        Ok(Err(error)) => receipts::diagnostic(&error),
        Err(error) => receipts::diagnostic(&format!("receipt recorder failed: {error}")),
    };
    let retry = match serde_json::to_string(&report) {
        Ok(retry) => retry,
        Err(error) => {
            output.content.push_str(&format!(
                "\n\nActivity receipt recording failed: {recorded}. The report could not be encoded: {error}. Do not repeat the App operation."
            ));
            output.is_error = true;
            return output;
        }
    };
    output.content.push_str(&format!(
        "\n\nActivity receipt recording failed: {recorded}.\nDo not repeat the App operation to repair recording. Retry only this report through `cos activity record-receipt <activity-id> --stdin`:\n{retry}"
    ));
    output.is_error = true;
    output
}
