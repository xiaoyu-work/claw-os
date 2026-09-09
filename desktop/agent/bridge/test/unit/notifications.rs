use super::*;

#[test]
fn presentation_retirement_releases_handles_without_acknowledging_or_dismissing() {
    for reason in [1, 3] {
        let mut visible = HashMap::from([("retired".into(), 1), ("other".into(), 2)]);
        assert_eq!(close_action(&mut visible, 1, reason), None);
        assert_eq!(visible, HashMap::from([("other".into(), 2)]));
    }
    let mut visible = HashMap::from([("human-dismissed".into(), 1)]);
    assert_eq!(
        close_action(&mut visible, 1, 2),
        Some((1, Command::NotificationDismiss))
    );
    assert!(visible.contains_key("human-dismissed"));
}

#[test]
fn severity_maps_to_freedesktop_urgency() {
    assert_eq!(urgency("info"), 0);
    assert_eq!(urgency("warning"), 1);
    assert_eq!(urgency("error"), 2);
    assert_eq!(urgency("critical"), 2);
}

#[test]
fn delivery_envelope_rejects_missing_notification_fields() {
    assert!(
        serde_json::from_value::<DeliveryEnvelope>(json!({
            "deliveries": [{ "notification": { "id": "n-1" } }]
        }))
        .is_err()
    );
}

#[test]
fn model_content_is_plain_text_and_presentation_labels_are_not_identity() {
    assert_eq!(
        plain_body("<b>text</b> & <img src=\"file:///private\">"),
        "&lt;b&gt;text&lt;/b&gt; &amp; &lt;img src=\"file:///private\"&gt;"
    );
    let envelope: DeliveryEnvelope = serde_json::from_value(json!({
        "deliveries":[{"notification":{
            "id":"notif-example","state":"unread","severity":"info","title":"Title","body":"",
            "source":"app:cosmic-notifications",
            "presentation":{"app_name":"Untrusted label","icon":"name","expire_ms":0,"transient":true}
        }}]
    })).unwrap();
    let presentation = envelope.deliveries[0]
        .notification
        .presentation
        .as_ref()
        .unwrap();
    assert_eq!(presentation.app_name, "Untrusted label");
    assert_eq!(presentation.expire_ms, 0);
    assert!(presentation.transient);
}
