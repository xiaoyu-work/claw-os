use super::*;

fn signature(secret: &str, input: &[u8], prefix: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(input);
    format!("{prefix}{}", hex::encode(mac.finalize().into_bytes()))
}

#[test]
fn slack_signatures_bind_exact_raw_bytes_and_reject_old_or_ambiguous_headers() {
    let now = 1_800_000_000_000;
    let raw = br#"{"type":"event_callback","event":{"text":"synthetic"}}"#;
    let mut headers = HeaderMap::new();
    headers.insert("X-Slack-Request-Timestamp", "1800000000".parse().unwrap());
    let mut signed = b"v0:1800000000:".to_vec();
    signed.extend_from_slice(raw);
    headers.insert(
        "X-Slack-Signature",
        signature("synthetic-signing-key", &signed, "v0=")
            .parse()
            .unwrap(),
    );
    assert!(verify_signature(Platform::Slack, "synthetic-signing-key", &headers, raw, now).is_ok());
    assert!(verify_signature(
        Platform::Slack,
        "synthetic-signing-key",
        &headers,
        b"{}",
        now
    )
    .is_err());
    assert!(verify_signature(Platform::Slack, "wrong", &headers, raw, now).is_err());
    assert!(verify_signature(
        Platform::Slack,
        "synthetic-signing-key",
        &headers,
        raw,
        now + FRESH_MS + 1
    )
    .is_err());
    headers.append("x-slack-signature", "v0=duplicate".parse().unwrap());
    assert!(
        verify_signature(Platform::Slack, "synthetic-signing-key", &headers, raw, now).is_err()
    );
}

#[test]
fn whatsapp_signature_authenticates_body_before_any_provider_identity_is_trusted() {
    let raw = br#"{"object":"whatsapp_business_account"}"#;
    let mut headers = HeaderMap::new();
    headers.insert(
        "X-Hub-Signature-256",
        signature("synthetic-secret", raw, "sha256=")
            .parse()
            .unwrap(),
    );
    assert!(verify_signature(Platform::Whatsapp, "synthetic-secret", &headers, raw, 0).is_ok());
    assert!(verify_signature(Platform::Whatsapp, "synthetic-secret", &headers, b"{}", 0).is_err());
    assert!(verify_signature(Platform::Telegram, "synthetic-secret", &headers, raw, 0).is_err());
}

fn profile(platform: Platform) -> Profile {
    Profile {
        schema: 1,
        owner_uid: 1000,
        platform,
        account_id: "900".into(),
        channel_id: "800".into(),
        sender_id: "US.synthetic123".into(),
        trigger_id: "fixture".into(),
        trigger_generation: "generation".into(),
        credential_tier: 0,
        credential_fingerprints: BTreeMap::new(),
        revision: 1,
        enabled: true,
        cursor: None,
        window_ms: 0,
        window_count: 0,
        recent: BTreeMap::new(),
        pending: None,
        replies: BTreeMap::new(),
        last_error: None,
    }
}

#[test]
fn whatsapp_bsuid_without_phone_number_survives_verified_ingress_and_reply_unchanged() {
    let profile = profile(Platform::Whatsapp);
    let webhook = json!({
        "object":"whatsapp_business_account","entry":[{"id":"900","changes":[{"field":"messages","value":{
            "metadata":{"phone_number_id":"800"},"messages":[{"id":"wamid.synthetic","from_user_id":"US.synthetic123",
                "timestamp":"1800000000","type":"text","text":{"body":"Synthetic message"}}],
        }}]}],
    });
    let messages = whatsapp_messages(&profile, &webhook).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].sender_id, "US.synthetic123");
    let body = reply_body(Platform::Whatsapp, &messages[0], "reply");
    assert_eq!(body["recipient"], "US.synthetic123");
    assert!(body.get("to").is_none());
    let mut forged = webhook.clone();
    forged["entry"][0]["changes"][0]["value"]["metadata"]["phone_number_id"] = json!("foreign");
    assert!(whatsapp_messages(&profile, &forged).is_err());
    forged = webhook;
    forged["entry"][0]["id"] = json!("foreign");
    assert!(whatsapp_messages(&profile, &forged).is_err());
}

#[test]
fn slack_messages_require_the_exact_workspace_and_app_and_keep_reply_thread() {
    let mut profile = profile(Platform::Slack);
    profile.account_id = "T123/A456".into();
    let webhook = json!({
        "type":"event_callback","team_id":"T123","api_app_id":"A456","event_id":"Ev789",
        "event":{"type":"message","user":"U123","channel":"C123","ts":"1800000000.123","thread_ts":"1799999999.456","text":"Synthetic"},
    });
    let messages = slack_messages(&profile, &webhook).unwrap();
    assert_eq!(
        reply_body(Platform::Slack, &messages[0], "reply")["thread_ts"],
        "1799999999.456"
    );
    let mut forged = webhook.clone();
    forged["api_app_id"] = json!("foreign");
    assert!(slack_messages(&profile, &forged).is_err());
    let mut bot = webhook;
    bot["event"]["bot_id"] = json!("B123");
    assert!(slack_messages(&profile, &bot).unwrap().is_empty());
}

#[test]
fn telegram_polling_uses_sender_identity_separately_from_group_chat_and_ignores_edits() {
    let update = json!({"message":{"message_id":123,"from":{"id":42,"is_bot":false},
        "chat":{"id":-100123},"date":1800000000,"text":"Synthetic","message_thread_id":17}});
    let message = telegram_message(&update).unwrap().unwrap();
    assert_eq!(message.sender_id, "42");
    assert_eq!(message.channel_id, "-100123");
    let body = reply_body(Platform::Telegram, &message, "reply");
    assert_eq!(body["message_thread_id"], 17);
    assert_eq!(body["reply_parameters"]["message_id"], 123);
    assert!(
        telegram_message(&json!({"edited_message":update["message"]}))
            .unwrap()
            .is_none()
    );
}

#[test]
fn channel_identifiers_never_accept_wildcards_or_rewrite_opaque_bsuid() {
    assert!(validate_identifiers(Platform::Discord, "100", "200", "300").is_ok());
    assert!(validate_identifiers(Platform::Discord, "100", "200", "*").is_err());
    assert!(validate_identifiers(Platform::Whatsapp, "900", "800", "US.ENT.synthetic123").is_ok());
    assert!(validate_identifiers(Platform::Whatsapp, "900", "800", "US.synthetic/123").is_err());
}
