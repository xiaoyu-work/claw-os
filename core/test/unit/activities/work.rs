use super::*;

#[test]
fn checkpoints_are_closed_bounded_reports_not_authority_or_completion() {
    let value = serde_json::json!({
        "summary":"Prepared a draft", "made_progress":true,
        "completed_steps":["Drafted outline"], "artifacts":[],
        "next":{"kind":"continue","task":"Check the outline"}
    });
    let checkpoint: ActivityWorkCheckpoint = serde_json::from_value(value.clone()).unwrap();
    checkpoint.validate().unwrap();
    for field in [
        "owner_uid",
        "capabilities",
        "approved",
        "completed",
        "next_job_id",
    ] {
        let mut forged = value.clone();
        forged[field] = serde_json::json!(true);
        assert!(serde_json::from_value::<ActivityWorkCheckpoint>(forged).is_err());
    }
    let mut forged = value;
    forged["next"] = serde_json::json!({"kind":"complete"});
    assert!(serde_json::from_value::<ActivityWorkCheckpoint>(forged).is_err());
}
