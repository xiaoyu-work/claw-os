use super::*;

#[test]
fn severity_maps_to_freedesktop_urgency() {
    assert_eq!(urgency("info"), 0);
    assert_eq!(urgency("warning"), 1);
    assert_eq!(urgency("error"), 2);
    assert_eq!(urgency("critical"), 2);
}

#[test]
fn delivery_envelope_rejects_missing_notification_fields() {
    assert!(serde_json::from_value::<DeliveryEnvelope>(json!({
        "deliveries": [{ "notification": { "id": "n-1" } }]
    }))
    .is_err());
}
