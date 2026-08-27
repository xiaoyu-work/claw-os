use super::*;

#[test]
fn parse_origin_known() {
    assert_eq!(parse_origin("trusted").unwrap(), PromptOrigin::Trusted);
    assert_eq!(
        parse_origin("external-content").unwrap(),
        PromptOrigin::ExternalContent
    );
}

#[test]
fn modality_derive_chat_default() {
    let req = ChatRequest {
        app_id: "x".into(),
        origin: "trusted".into(),
        prompt: Some("hi".into()),
        ..Default::default()
    };
    assert_eq!(Modality::derive(&req).unwrap(), Modality::Chat);
}

#[test]
fn modality_derive_chat_untrusted_from_external_origin() {
    let req = ChatRequest {
        app_id: "x".into(),
        origin: "external-content".into(),
        prompt: Some("hi".into()),
        ..Default::default()
    };
    assert_eq!(
        Modality::derive(&req).unwrap(),
        Modality::ChatUntrusted
    );
}

#[test]
fn modality_derive_embed() {
    let req = ChatRequest {
        app_id: "x".into(),
        origin: "trusted".into(),
        prompt: Some("hi".into()),
        embed: true,
        ..Default::default()
    };
    assert_eq!(Modality::derive(&req).unwrap(), Modality::Embed);
}

#[test]
fn modality_derive_image_generate_from_image_output() {
    let req = ChatRequest {
        app_id: "x".into(),
        origin: "trusted".into(),
        prompt: Some("a cat".into()),
        image_output: Some(PathBuf::from("/tmp/out.png")),
        ..Default::default()
    };
    assert_eq!(
        Modality::derive(&req).unwrap(),
        Modality::ImageGenerate
    );
}

#[test]
fn modality_derive_image_analyze_no_prompt() {
    let req = ChatRequest {
        app_id: "x".into(),
        origin: "trusted".into(),
        image_input: Some(PathBuf::from("/tmp/in.png")),
        ..Default::default()
    };
    assert_eq!(
        Modality::derive(&req).unwrap(),
        Modality::ImageAnalyze
    );
}

#[test]
fn modality_derive_vision_analyze_with_prompt() {
    let req = ChatRequest {
        app_id: "x".into(),
        origin: "trusted".into(),
        prompt: Some("describe this".into()),
        image_input: Some(PathBuf::from("/tmp/in.png")),
        ..Default::default()
    };
    assert_eq!(
        Modality::derive(&req).unwrap(),
        Modality::VisionAnalyze
    );
}

#[test]
fn modality_derive_audio_tts() {
    let req = ChatRequest {
        app_id: "x".into(),
        origin: "trusted".into(),
        prompt: Some("hello world".into()),
        audio_output: Some(PathBuf::from("/tmp/out.wav")),
        ..Default::default()
    };
    assert_eq!(Modality::derive(&req).unwrap(), Modality::AudioTts);
}

#[test]
fn modality_derive_audio_stt() {
    let req = ChatRequest {
        app_id: "x".into(),
        origin: "trusted".into(),
        audio_input: Some(PathBuf::from("/tmp/in.wav")),
        ..Default::default()
    };
    assert_eq!(Modality::derive(&req).unwrap(), Modality::AudioStt);
}

#[test]
fn modality_derive_video_generate() {
    let req = ChatRequest {
        app_id: "x".into(),
        origin: "trusted".into(),
        prompt: Some("a sunrise".into()),
        video_output: Some(PathBuf::from("/tmp/out.mp4")),
        ..Default::default()
    };
    assert_eq!(
        Modality::derive(&req).unwrap(),
        Modality::VideoGenerate
    );
}

#[test]
fn modality_derive_video_analyze() {
    let req = ChatRequest {
        app_id: "x".into(),
        origin: "trusted".into(),
        video_input: Some(PathBuf::from("/tmp/in.mp4")),
        ..Default::default()
    };
    assert_eq!(
        Modality::derive(&req).unwrap(),
        Modality::VideoAnalyze
    );
}

#[test]
fn modality_derive_rejects_conflicting_selectors() {
    let req = ChatRequest {
        app_id: "x".into(),
        origin: "trusted".into(),
        image_input: Some(PathBuf::from("/tmp/i.png")),
        audio_input: Some(PathBuf::from("/tmp/a.wav")),
        ..Default::default()
    };
    let err = Modality::derive(&req).unwrap_err();
    assert!(matches!(err, AiError::ModalityConflict(_)));
}

#[test]
fn modality_verbs_cover_every_variant() {
    // Sanity: every variant has a corresponding caps verb. If a
    // future modality is added without wiring caps, this matches
    // statement will fail at compile time.
    let all = [
        Modality::Chat,
        Modality::ChatUntrusted,
        Modality::Embed,
        Modality::ImageGenerate,
        Modality::ImageAnalyze,
        Modality::VisionAnalyze,
        Modality::AudioTts,
        Modality::AudioStt,
        Modality::VideoGenerate,
        Modality::VideoAnalyze,
    ];
    for m in all {
        // verb() always returns; label() always returns. Cover both.
        let _ = m.verb();
        assert!(!m.label().is_empty());
    }
}

#[test]
fn apply_safety_minimal_is_passthrough() {
    let (out, changed) = apply_safety("hello sk-FAKE", AiSafety::Minimal);
    assert_eq!(out, "hello sk-FAKE");
    assert!(!changed);
}

#[test]
fn apply_safety_strict_redacts() {
    let secret = "AKIAIOSFODNN7EXAMPLE";
    let (out, changed) = apply_safety(&format!("key={secret}"), AiSafety::Strict);
    assert!(changed);
    assert!(!out.contains(secret));
}

#[test]
fn tool_not_in_policy_has_stable_denial_token() {
    let err = AiError::ToolNotInPolicy {
        app: "demo".into(),
        tool: "fs.read_text".into(),
        allowed: vec!["kv.get".into()],
    };
    assert_eq!(denial_reason_token(&err), "tool_not_in_policy");
}

#[test]
fn tool_not_in_policy_display_mentions_app_tool_and_allowed() {
    let err = AiError::ToolNotInPolicy {
        app: "demo".into(),
        tool: "fs.read_text".into(),
        allowed: vec!["kv.get".into()],
    };
    let msg = err.to_string();
    assert!(msg.contains("demo"), "{msg}");
    assert!(msg.contains("fs.read_text"), "{msg}");
    assert!(msg.contains("kv.get"), "{msg}");
    assert!(msg.contains("ai.tools[]"), "{msg}");
}

// ---------- BudgetReservation Drop guard (audit fix) ----------

/// Build a `Store` backed by a private on-disk SQLite file under
/// a tempdir. Returns the tempdir so the file outlives the
/// store; dropping it cleans up. We override `COS_DATA_DIR` so
/// `Store::open()` uses our tempdir instead of the system path.
fn ephemeral_budget_store_via_tempdir() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("COS_DATA_DIR", dir.path());
    let store = Store::open().expect("open store in tempdir");
    (dir, store)
}

/// The `BudgetReservation` Drop guard refunds the reserved
/// units when it goes out of scope without `commit()`. This is
/// the audit fix for `ai/gate.rs HIGH`: previously a provider
/// error between `reserve` and `settle` left the units debited.
#[test]
fn budget_refunded_on_provider_error() {
    let (_dir, store) = ephemeral_budget_store_via_tempdir();

    // Take a snapshot of the starting balance so the assertion
    // is independent of any leftover rows in the in-tempdir DB.
    let before = {
        let s = Store::open().unwrap();
        s.current("test.app").unwrap().units_used
    };

    // Reserve 500 units, then drop without committing — mimics
    // a provider error path between `reserve` and `commit`.
    {
        let _r = BudgetReservation::reserve(
            store,
            "test.app".to_string(),
            500,
            10_000,
            0, // no user cap
        )
        .expect("reserve");
        // Reservation is alive here; the row should reflect the debit.
        let probe = Store::open().unwrap();
        let mid = probe.current("test.app").unwrap().units_used;
        assert_eq!(
            mid,
            before + 500,
            "reservation should debit `units_used` while alive"
        );
        // Falling out of scope drops `_r` without calling `commit`.
    }

    // After Drop, the row must be refunded back to `before`.
    let after_store = Store::open().unwrap();
    let after = after_store.current("test.app").unwrap().units_used;
    assert_eq!(
        after, before,
        "BudgetReservation::drop must refund the reservation when commit() was not called"
    );

    std::env::remove_var("COS_DATA_DIR");
}

#[tokio::test]
async fn system_stream_settles_reported_openai_usage_to_actuals() {
    use futures_util::StreamExt;

    let (_dir, store) = ephemeral_budget_store_via_tempdir();
    let reservation =
        SystemBudgetReservation::reserve(store, 500, 10_000).expect("reserve estimate");
    let inner: futures_util::stream::BoxStream<'static, crate::agent::llm::Result<StreamEvent>> =
        Box::pin(futures_util::stream::iter(vec![Ok(StreamEvent::Done {
            finish: FinishReason::Stop,
            usage: crate::agent::llm::types::Usage {
                input_tokens: 17,
                output_tokens: 3,
                ..Default::default()
            },
        })]));

    let events: Vec<_> = wrap_system_stream(inner, reservation).collect().await;
    let charged = Store::open()
        .unwrap()
        .current(SYSTEM_AGENT_BUCKET)
        .unwrap()
        .units_used;
    std::env::remove_var("COS_DATA_DIR");

    assert!(matches!(
        events.last(),
        Some(Ok(StreamEvent::Done { usage, .. }))
            if usage.input_tokens == 17 && usage.output_tokens == 3
    ));
    assert_eq!(
        charged, 20,
        "reported streaming usage must replace the conservative reservation"
    );
}

#[tokio::test]
async fn system_stream_without_compat_usage_keeps_conservative_estimate() {
    use futures_util::StreamExt;

    let (_dir, store) = ephemeral_budget_store_via_tempdir();
    let reservation =
        SystemBudgetReservation::reserve(store, 500, 10_000).expect("reserve estimate");
    let inner: futures_util::stream::BoxStream<'static, crate::agent::llm::Result<StreamEvent>> =
        Box::pin(futures_util::stream::iter(vec![Ok(StreamEvent::Done {
            finish: FinishReason::Stop,
            usage: Default::default(),
        })]));

    let events: Vec<_> = wrap_system_stream(inner, reservation).collect().await;
    let charged = Store::open()
        .unwrap()
        .current(SYSTEM_AGENT_BUCKET)
        .unwrap()
        .units_used;
    std::env::remove_var("COS_DATA_DIR");

    assert!(events.last().is_some_and(Result::is_ok));
    assert_eq!(
        charged, 500,
        "strict compatibility providers without usage must retain the safe estimate"
    );
}
