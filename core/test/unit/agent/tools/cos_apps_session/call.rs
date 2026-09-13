use super::*;
use crate::activities::{ReceiptOutcome, ReceiptReport};
use crate::operations::reporting::{self, ReceiptRecorder};
use std::sync::Mutex;

#[derive(Default)]
struct Recorder {
    reports: Mutex<Vec<ReceiptReport>>,
    fail: bool,
}

impl ReceiptRecorder for Recorder {
    fn record(&self, report: ReceiptReport) -> Result<String, String> {
        let id = report.id.clone();
        self.reports.lock().unwrap().push(report);
        if self.fail {
            Err("receipt storage unavailable".into())
        } else {
            Ok(id)
        }
    }
}

#[tokio::test]
async fn session_reports_keep_results_separate_from_cleanup_and_storage_failures() {
    let digest = format!("sha256:{}", "a".repeat(64));
    let recorder = Arc::new(Recorder::default());
    for (result, outcome) in [
        (
            Ok(("original result\n".to_string(), false)),
            ReceiptOutcome::Returned,
        ),
        (
            Ok(("App refused".into(), true)),
            ReceiptOutcome::ReportedError,
        ),
        (
            Err("transport lost".to_string()),
            ReceiptOutcome::Indeterminate,
        ),
    ] {
        let expected = match &result {
            Ok((text, _)) => text.as_str(),
            Err(error) => error.as_str(),
        }
        .to_string();
        let output = reporting::with_recorder(
            Some(recorder.clone()),
            report(
                "demo",
                "demo.run",
                &digest,
                result,
                Some("clear failed".into()),
            ),
        )
        .await;
        assert!(output.is_error);
        assert!(output.content.starts_with(&expected));
        assert!(output.content.contains("could not be cleared"));
        let saved = recorder.reports.lock().unwrap().last().unwrap().clone();
        saved.validate().unwrap();
        assert_eq!(saved.outcome, outcome);
        if let Some(result) = saved.result {
            assert_eq!(result.bytes, expected.len() as u64);
            assert!(!result.preview.contains("clear failed"));
        }
    }

    let recorder = Arc::new(Recorder {
        fail: true,
        ..Default::default()
    });
    let output = reporting::with_recorder(
        Some(recorder.clone()),
        report(
            "demo",
            "demo.run",
            &digest,
            Ok(("returned once".into(), false)),
            None,
        ),
    )
    .await;
    assert!(output.is_error);
    assert!(output.content.starts_with("returned once\n\n"));
    assert!(output.content.contains("Do not repeat the App operation"));
    let retry: ReceiptReport =
        serde_json::from_str(output.content.lines().last().unwrap()).unwrap();
    assert_eq!(recorder.reports.lock().unwrap().as_slice(), &[retry]);
    assert!(reporting::current().is_none());
    let output = report(
        "demo",
        "demo.run",
        &digest,
        Ok(("unassociated".into(), false)),
        None,
    )
    .await;
    assert!(!output.is_error);
    assert_eq!(output.content, "unassociated");
}
