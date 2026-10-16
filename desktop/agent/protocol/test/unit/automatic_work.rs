use super::*;
use serde_json::json;

#[test]
fn automatic_work_requests_are_closed_owner_free_and_revision_checked() {
    let value = json!({"expected_revision":null,"work":{
        "task":"Prepare a draft","check_interval_secs":3600,"wake_event":null,
    }});
    let request: AutoWorkSetRequest = serde_json::from_value(value.clone()).unwrap();
    request.validate().unwrap();
    for field in [
        "owner_uid",
        "generation",
        "job_id",
        "approved",
        "checkpoint",
    ] {
        let mut forged = value.clone();
        forged[field] = json!(true);
        assert!(serde_json::from_value::<AutoWorkSetRequest>(forged).is_err());
    }
    let request = AutoWorkEnabledRequest {
        expected_revision: 9_007_199_254_740_993,
        enabled: true,
        next_task: Some("Inspect before continuing".into()),
        review_note: Some("Checked the previous result".into()),
    };
    request.validate().unwrap();
    let roundtrip: AutoWorkEnabledRequest =
        serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
    assert_eq!(roundtrip, request);
    assert!(
        AutoWorkEnabledRequest {
            enabled: false,
            ..request
        }
        .validate()
        .is_err()
    );
    assert!(
        serde_json::from_value::<ActivityAutoWorkResponse>(
            json!({"schema":1,"activity_id":"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee","history":[]})
        )
        .is_err()
    );
}
