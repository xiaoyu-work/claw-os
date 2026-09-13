//! Task-scoped delivery of App reports, without an execution or storage bypass.

use std::future::Future;
use std::sync::Arc;

use crate::activities::ReceiptReport;

pub trait ReceiptRecorder: Send + Sync {
    /// Record only this report and return its canonical receipt ID.
    fn record(&self, report: ReceiptReport) -> Result<String, String>;
}

tokio::task_local! {
    static RECORDER: Option<Arc<dyn ReceiptRecorder>>;
}

pub(crate) fn with_recorder<F: Future>(
    recorder: Option<Arc<dyn ReceiptRecorder>>,
    future: F,
) -> impl Future<Output = F::Output> {
    RECORDER.scope(recorder, future)
}

pub(crate) fn current() -> Option<Arc<dyn ReceiptRecorder>> {
    RECORDER.try_with(Clone::clone).ok().flatten()
}
