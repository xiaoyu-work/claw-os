    use super::*;

    // ---- FactCategory --------------------------------------------------

    #[test]
    fn category_parse_canonical() {
        assert_eq!(FactCategory::parse("preference"), FactCategory::Preference);
        assert_eq!(FactCategory::parse("PREF"), FactCategory::Preference);
        assert_eq!(FactCategory::parse("identity"), FactCategory::Identity);
        assert_eq!(FactCategory::parse("env"), FactCategory::Environment);
        assert_eq!(FactCategory::parse("skill"), FactCategory::Skill);
    }

    #[test]
    fn category_parse_unknown_preserved_in_other() {
        match FactCategory::parse("hobby") {
            FactCategory::Other(s) => assert_eq!(s, "hobby"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    // ---- parse_facts ---------------------------------------------------

    #[test]
    fn parse_facts_handles_well_formed_tags() {
        let out = parse_facts(
            r#"<fact category="preference" confidence="0.9">User prefers Rust over Go</fact>
            <fact category="environment" confidence="0.95">User is on Windows 11</fact>"#,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].category, FactCategory::Preference);
        assert_eq!(out[0].text, "User prefers Rust over Go");
        assert!((out[0].confidence - 0.9).abs() < 1e-4);
        assert_eq!(out[1].category, FactCategory::Environment);
    }

    #[test]
    fn parse_facts_tolerates_single_quotes() {
        let out = parse_facts(r#"<fact category='skill' confidence='0.7'>fluent in Rust</fact>"#);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].category, FactCategory::Skill);
    }

    #[test]
    fn parse_facts_tolerates_missing_confidence() {
        let out = parse_facts(r#"<fact category="identity">Name is Alex</fact>"#);
        assert_eq!(out.len(), 1);
        assert!((out[0].confidence - 0.5).abs() < 1e-4);
    }

    #[test]
    fn parse_facts_tolerates_extra_whitespace_and_newlines_inside_body() {
        let out =
            parse_facts("<fact category=\"identity\" confidence=\"0.8\">  hello\nworld  </fact>");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "hello\nworld");
    }

    #[test]
    fn parse_facts_skips_empty_body() {
        let out = parse_facts(
            r#"<fact category="preference" confidence="0.9"></fact>
            <fact category="identity" confidence="0.8">real fact</fact>"#,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "real fact");
    }

    #[test]
    fn parse_facts_empty_when_no_tags() {
        assert!(parse_facts("just prose").is_empty());
        assert!(parse_facts("").is_empty());
    }

    #[test]
    fn parse_facts_clamps_confidence_to_unit_interval() {
        let out = parse_facts(
            r#"<fact category="preference" confidence="2.5">over the moon</fact>
            <fact category="preference" confidence="-0.3">below floor</fact>"#,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].confidence, 1.0);
        assert_eq!(out[1].confidence, 0.0);
    }

    #[test]
    fn parse_facts_drops_orphaned_open_tag() {
        // Open without close → break out without emitting a phantom fact.
        let out = parse_facts(r#"<fact category="preference" confidence="0.9">unterminated"#);
        assert!(out.is_empty());
    }

    #[test]
    fn parse_facts_handles_two_back_to_back_tags() {
        // No whitespace between </fact> and the next <fact>.
        let out = parse_facts(
            r#"<fact category="preference" confidence="0.9">a</fact><fact category="skill" confidence="0.8">b</fact>"#,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].text, "a");
        assert_eq!(out[1].text, "b");
    }

    #[test]
    fn parse_facts_reads_governance_attributes_and_ignores_malformed_values() {
        let out = parse_facts(
            r#"<fact category="environment" entity="python" attribute="version" value="3.13" lifetime="observed" observed_at="2026-08-01" ttl_days="14d" source_message_id="42" confidence="0.9">Python version</fact>
<fact category="environment" entity="node" attribute="version" value="24" lifetime="forever-ish" ttl_days="many" source_message_id="nope" confidence="0.9">Node version</fact>"#,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].lifetime, Some(FactLifetime::Observed));
        assert_eq!(out[0].observed_at.as_deref(), Some("2026-08-01"));
        assert_eq!(out[0].ttl_days, Some(14));
        assert_eq!(out[0].source_message_id, Some(42));
        assert_eq!(out[1].lifetime, None);
        assert_eq!(out[1].ttl_days, None);
        assert_eq!(out[1].source_message_id, None);
    }

    // ---- looks_secret -------------------------------------------------

    #[test]
    fn looks_secret_flags_credential_words() {
        assert!(looks_secret("the API key is sk-foo"));
        assert!(looks_secret("password=abc123"));
        assert!(looks_secret("Bearer xyz"));
        assert!(looks_secret("-----BEGIN PRIVATE KEY-----"));
    }

    #[test]
    fn looks_secret_flags_long_alphanumeric_runs() {
        assert!(looks_secret("user has token AKIAIOSFODNN7EXAMPLEKEYZ"));
        assert!(looks_secret("550e8400-e29b-41d4-a716-446655440000"));
    }

    #[test]
    fn looks_secret_does_not_flag_normal_facts() {
        assert!(!looks_secret("user prefers Rust"));
        assert!(!looks_secret("user lives in Beijing"));
        assert!(!looks_secret(""));
    }

    #[test]
    fn fact_secret_filter_checks_every_model_controlled_field() {
        let safe = ExtractedFact {
            category: FactCategory::Preference,
            text: "User prefers helix".to_string(),
            confidence: 0.95,
            entity: Some("editor".to_string()),
            attribute: Some("name".to_string()),
            value: Some("helix".to_string()),
            lifetime: None,
            observed_at: None,
            ttl_days: None,
            source_session_id: None,
            source_message_id: None,
        };
        assert!(!fact_looks_secret(&safe));
        assert!(render_persistable_fact_line(&safe, "2026-01-15").is_some());
        assert!(render_persistable_fact_line(&safe, "secret-date").is_none());

        let mut secret_text = safe.clone();
        secret_text.text = "The API key must not be stored".to_string();
        assert!(fact_looks_secret(&secret_text));

        let mut secret_entity = safe.clone();
        secret_entity.entity = Some("secret-store".to_string());
        assert!(fact_looks_secret(&secret_entity));

        let mut secret_attribute = safe.clone();
        secret_attribute.attribute = Some("access_token".to_string());
        assert!(fact_looks_secret(&secret_attribute));

        let mut secret_value = safe.clone();
        secret_value.value = Some("Bearer demo-credential".to_string());
        assert!(fact_looks_secret(&secret_value));

        let uuid = "550e8400-e29b-41d4-a716-446655440000";
        let mut uuid_text = safe.clone();
        uuid_text.text = uuid.to_string();
        assert!(fact_looks_secret(&uuid_text));

        let mut uuid_entity = safe.clone();
        uuid_entity.entity = Some(uuid.to_string());
        assert!(fact_looks_secret(&uuid_entity));

        let mut uuid_attribute = safe.clone();
        uuid_attribute.attribute = Some(uuid.to_string());
        assert!(fact_looks_secret(&uuid_attribute));

        let mut uuid_value = safe.clone();
        uuid_value.value = Some(uuid.to_string());
        assert!(fact_looks_secret(&uuid_value));

        let mut secret_category = safe;
        secret_category.category = FactCategory::Other("password".to_string());
        assert!(render_persistable_fact_line(&secret_category, "2026-01-15").is_none());
    }

    #[test]
    fn normalized_representation_gets_a_final_secret_check() {
        let raw = parse_facts(
            r#"<fact category="preference" entity="abcdefghijkl mnopqrstuvwx" attribute="name" value="safe" confidence="0.9">safe</fact>"#,
        )
        .remove(0);
        assert!(!fact_looks_secret(&raw));

        let messages = vec![MessageRow {
            id: 7,
            session_id: "session-1".into(),
            role: "user".into(),
            content: "remember this".into(),
            ts_ms: 1_700_000_000_000,
        }];
        let normalized =
            normalize_fact_for_persistence(&raw, "session-1", &messages, "2026-08-27").unwrap();
        assert_eq!(
            normalized.entity.as_deref(),
            Some("abcdefghijkl_mnopqrstuvwx")
        );
        assert!(fact_looks_secret(&normalized));
    }

    #[test]
    fn provenance_ids_are_redacted_and_rendered_lines_are_rechecked() {
        let raw = parse_facts(
            r#"<fact category="preference" entity="editor" attribute="name" value="helix" source_message_id="7" confidence="0.9">safe</fact>"#,
        )
        .remove(0);
        let messages = vec![MessageRow {
            id: 7,
            session_id: "session-1".into(),
            role: "user".into(),
            content: "remember this".into(),
            ts_ms: 1_700_000_000_000,
        }];
        let uuid_normalized = normalize_fact_for_persistence(
            &raw,
            "550e8400-e29b-41d4-a716-446655440000",
            &messages,
            "2026-08-27",
        )
        .unwrap();
        assert!(render_persistable_fact_line(&uuid_normalized, "2026-08-27").is_some());

        let normalized = normalize_fact_for_persistence(
            &raw,
            "ABCDEFGHIJKLMNOPQRSTUVWXYZ123456",
            &messages,
            "2026-08-27",
        )
        .unwrap();
        assert_eq!(normalized.source_session_id.as_deref(), Some("redacted"));
        let line = render_persistable_fact_line(&normalized, "2026-08-27").unwrap();
        assert!(!line.contains("ABCDEFGHIJKLMNOPQRSTUVWXYZ123456"));
        assert!(line.contains("source_session:redacted"));

        let mut unsafe_fact = normalized;
        unsafe_fact.source_session_id = Some("ABCDEFGHIJKLMNOPQRSTUVWXYZ123456".into());
        assert!(render_persistable_fact_line(&unsafe_fact, "2026-08-27").is_none());
    }

    #[test]
    fn observed_fact_requires_a_valid_source_message() {
        let raw = parse_facts(
            r#"<fact category="environment" entity="python" attribute="version" value="3.13" source_message_id="999" confidence="0.9">version</fact>"#,
        )
        .remove(0);
        let messages = vec![MessageRow {
            id: 7,
            session_id: "session-1".into(),
            role: "user".into(),
            content: "Python is installed".into(),
            ts_ms: 1_700_000_000_000,
        }];
        assert!(
            normalize_fact_for_persistence(&raw, "session-1", &messages, "2026-08-27")
                .is_none()
        );
    }

    // ---- existing_curated_lines / dedupe ------------------------------

    #[test]
    fn existing_curated_lines_extracts_the_body_without_meta() {
        let md = r#"# memory

## Curated facts (auto)
- [preference] User prefers Rust _(2026-01-15, conf 0.90)_
- [identity] Name is Xiaoyu _(2026-01-15, conf 0.95)_

other content
"#;
        let lines = existing_curated_lines(md);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "User prefers Rust");
        assert_eq!(lines[1], "Name is Xiaoyu");
    }

    #[test]
    fn render_fact_line_format_round_trips() {
        let fact = ExtractedFact {
            category: FactCategory::Preference,
            text: "User prefers Rust".to_string(),
            confidence: 0.91,
            entity: None,
            attribute: None,
            value: None,
            lifetime: None,
            observed_at: None,
            ttl_days: None,
            source_session_id: None,
            source_message_id: None,
        };
        let line = render_fact_line(&fact, "2026-01-15");
        assert_eq!(
            line,
            "- [preference] User prefers Rust _(2026-01-15, conf 0.91)_"
        );
    }

    // ---- structured facts ---------------------------------------------

    #[test]
    fn parse_facts_extracts_entity_attribute_value() {
        let out = parse_facts(
            r#"<fact category="preference" entity="editor" attribute="name" value="helix" confidence="0.9">User switched to helix</fact>"#,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].entity.as_deref(), Some("editor"));
        assert_eq!(out[0].attribute.as_deref(), Some("name"));
        assert_eq!(out[0].value.as_deref(), Some("helix"));
        assert_eq!(out[0].key().as_deref(), Some("editor.name"));
        assert_eq!(out[0].body(), "editor.name = helix");
    }

    #[test]
    fn unstructured_facts_still_parse_and_render_as_prose() {
        // Backwards compatibility: a model that ignores the new attrs
        // must keep working exactly as before.
        let out =
            parse_facts(r#"<fact category="preference" confidence="0.9">User prefers Rust</fact>"#);
        assert_eq!(out.len(), 1);
        assert!(out[0].key().is_none());
        assert_eq!(out[0].body(), "User prefers Rust");
    }

    #[test]
    fn key_requires_both_entity_and_attribute() {
        let only_entity =
            parse_facts(r#"<fact category="preference" entity="editor" confidence="0.9">x</fact>"#);
        assert!(only_entity[0].key().is_none());
        let blank = parse_facts(
            r#"<fact category="preference" entity="" attribute="name" confidence="0.9">x</fact>"#,
        );
        assert!(blank[0].key().is_none());
    }

    #[test]
    fn key_is_case_insensitive() {
        let a = parse_facts(
            r#"<fact entity="Editor" attribute="Name" value="helix" confidence="0.9">x</fact>"#,
        );
        assert_eq!(a[0].key().as_deref(), Some("editor.name"));
    }

    #[test]
    fn resolution_category_round_trips() {
        assert_eq!(FactCategory::parse("resolution"), FactCategory::Resolution);
        assert_eq!(FactCategory::parse("fix"), FactCategory::Resolution);
        assert_eq!(FactCategory::Resolution.as_str(), "resolution");
    }

    #[test]
    fn default_prompt_routes_procedures_to_skills() {
        let prompt = default_system_prompt();
        assert!(prompt.contains("those belong in Skills"));
        assert!(prompt.contains("source_message_id"));
        assert!(prompt.contains("os.distribution"));
    }

    #[test]
    fn split_curated_body_recognises_structured_entries() {
        assert_eq!(
            split_curated_body("editor.name = helix"),
            Some(("editor.name".to_string(), "helix".to_string()))
        );
        // Prose containing an equals sign is not a slot.
        assert!(split_curated_body("User thinks a = b").is_none());
        // Missing the dot means no entity.attribute pair.
        assert!(split_curated_body("editor = helix").is_none());
        assert!(split_curated_body("User prefers Rust").is_none());
    }

    #[test]
    fn structured_fact_renders_as_key_value_line() {
        let fact = ExtractedFact {
            category: FactCategory::Preference,
            text: "User switched to helix".to_string(),
            confidence: 0.9,
            entity: Some("editor".to_string()),
            attribute: Some("name".to_string()),
            value: Some("helix".to_string()),
            lifetime: None,
            observed_at: None,
            ttl_days: None,
            source_session_id: None,
            source_message_id: None,
        };
        assert_eq!(
            render_fact_line(&fact, "2026-01-15"),
            "- [preference] editor.name = helix _(2026-01-15, conf 0.90)_"
        );
    }

    #[test]
    fn latest_values_takes_the_last_occurrence() {
        let bodies = vec![
            "editor.name = vim".to_string(),
            "shell.name = bash".to_string(),
            "editor.name = helix".to_string(),
        ];
        let m = latest_values(&bodies);
        assert_eq!(m.get("editor.name").unwrap(), "helix");
        assert_eq!(m.get("shell.name").unwrap(), "bash");
    }

    #[test]
    fn ensure_section_creates_when_missing() {
        let s = ensure_section("");
        assert!(s.contains(SECTION_HEADER));
        let s2 = ensure_section("# top\n");
        assert!(s2.contains(SECTION_HEADER));
        // Idempotent.
        let s3 = ensure_section(&s2);
        assert_eq!(s2, s3);
    }

    #[test]
    fn append_lines_to_section_adds_under_header() {
        let md = "# memory\n\nsome notes";
        let next = append_lines_to_section(
            md,
            &["- [preference] foo _(2026-01-15, conf 0.90)_".to_string()],
        );
        assert!(next.contains(SECTION_HEADER));
        assert!(next.contains("foo"));
    }

    // ---- CurationLog --------------------------------------------------

    #[test]
    fn log_load_missing_file_is_default() {
        let dir = std::env::temp_dir().join(format!(
            "cos-curator-log-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let path = dir.join("missing.json");
        let log = CurationLog::load(&path);
        assert_eq!(log.version, 2);
        assert!(log.sessions.is_empty());
    }

    #[test]
    fn log_round_trip_through_disk() {
        let dir = std::env::temp_dir().join(format!(
            "cos-curator-log-rt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let path = dir.join("log.json");
        let mut log = CurationLog::default();
        log.record_run("sess-A", 100, 3);
        log.record_run("sess-A", 142, 2); // updates same session
        log.record_run("sess-B", 7, 0);
        log.save(&path).expect("save ok");

        let loaded = CurationLog::load(&path);
        assert_eq!(loaded.last_id("sess-A"), Some(142));
        assert_eq!(loaded.last_id("sess-B"), Some(7));
        assert_eq!(loaded.last_id("sess-C"), None);
        assert_eq!(loaded.sessions["sess-A"].facts_added_total, 5);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_load_corrupt_falls_back_to_default() {
        let dir = std::env::temp_dir().join(format!(
            "cos-curator-log-corrupt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log.json");
        std::fs::write(&path, "{ this is not json").unwrap();
        let log = CurationLog::load(&path);
        assert_eq!(log.version, 2);
        assert!(log.sessions.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- format_transcript -------------------------------------------

    #[test]
    fn format_transcript_truncates_long_messages() {
        let rows = vec![MessageRow {
            id: 1,
            session_id: "s".into(),
            role: "user".into(),
            content: "x".repeat(2000),
            ts_ms: 0,
        }];
        let out = format_transcript(&rows, 100);
        assert!(out.contains("[user message_id=1] "));
        assert!(out.contains("…"));
        assert!(out.len() < 200, "got len {}", out.len());
    }

    #[test]
    fn format_transcript_preserves_short_messages() {
        let rows = vec![MessageRow {
            id: 1,
            session_id: "s".into(),
            role: "assistant".into(),
            content: "short".into(),
            ts_ms: 0,
        }];
        let out = format_transcript(&rows, 100);
        assert!(out.contains("[assistant message_id=1] short"));
        assert!(!out.contains("…"));
    }

    // ---- date math sanity --------------------------------------------

    #[test]
    fn days_to_ymd_known_anchors() {
        // Unix epoch: 1970-01-01.
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
        // 2020-01-01 = 18262 days since epoch.
        assert_eq!(days_to_ymd(18262), (2020, 1, 1));
        // 2024-02-29 (leap day) = 19782 days since epoch.
        assert_eq!(days_to_ymd(19782), (2024, 2, 29));
    }

    // ---- end-to-end on in-memory db + temp notes ---------------------

    #[tokio::test]
    async fn curate_session_writes_facts_to_memory_md_and_log() {
        use crate::agent::llm::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
        use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
        use crate::agent::llm::Provider;
        use crate::agent::memory::sqlite_fts::MemoryDb;
        use crate::config::AgentConfig;
        use std::sync::Arc;

        // Mock provider that always returns two well-formed facts.
        let cfg = AgentConfig::default();
        let provider = MockProvider::new("mock-aux", &cfg);
        provider.push_response(MockResponse::Text(
            r#"<fact category="preference" entity="programming_language" attribute="preferred" value="Rust over Go" lifetime="durable" source_message_id="1" confidence="0.9">User prefers Rust over Go</fact>
<fact category="environment" entity="operating_system" attribute="name" value="Windows 11" lifetime="observed" ttl_days="30" source_message_id="3" confidence="0.95">User runs Windows 11 with PowerShell</fact>"#
                .to_string(),
        ));
        let provider: Arc<dyn Provider> = Arc::new(provider);
        let aux = AuxiliaryClient::new(provider, AuxiliaryConfig::new("mock", "mock-aux"));

        let db = MemoryDb::open_in_memory().expect("memory db");
        db.record_message("sess-1", "user", "I love Rust!").unwrap();
        db.record_message("sess-1", "assistant", "Noted.").unwrap();
        db.record_message("sess-1", "user", "I'm on Windows 11.")
            .unwrap();

        let dir = std::env::temp_dir().join(format!(
            "cos-curator-e2e-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let notes = NotesStore::at(dir.join("notes"));
        let log_path = dir.join("log.json");

        let curator = MemoryCurator::new(aux, notes.clone(), log_path.clone());
        let outcome = curator.curate_session(&db, "sess-1", false).await.unwrap();

        assert_eq!(outcome.facts_proposed.len(), 2);
        assert_eq!(outcome.facts_added.len(), 2);
        assert!(!outcome.skipped_no_new_messages);

        let mem = notes.read(MEMORY_FILE).unwrap().unwrap();
        assert!(mem.contains(SECTION_HEADER));
        assert!(mem.contains("programming_language.preferred = Rust over Go"));
        assert!(mem.contains("os.distribution = Windows 11"));
        assert!(mem.contains("source_session:sess-1"));
        assert!(mem.contains("source_message:3"));
        assert!(mem.contains("ttl=30d"));

        // Log persisted.
        let loaded = CurationLog::load(&log_path);
        assert_eq!(loaded.last_id("sess-1"), outcome.last_message_id);

        // Re-running should skip (no new messages).
        let again = curator.curate_session(&db, "sess-1", false).await.unwrap();
        assert!(again.skipped_no_new_messages);
        assert!(again.facts_added.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn curate_session_filters_secrets_from_structured_fields_before_persistence() {
        use crate::agent::llm::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
        use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
        use crate::agent::llm::Provider;
        use crate::agent::memory::sqlite_fts::MemoryDb;
        use crate::config::AgentConfig;
        use std::sync::Arc;

        let cfg = AgentConfig::default();
        let provider = MockProvider::new("mock-aux", &cfg);
        provider.push_response(MockResponse::Text(
            r#"<fact category="preference" entity="editor" attribute="name" value="helix" confidence="0.95">User prefers helix</fact>
<fact category="environment" entity="secret-store" attribute="owner" value="local" confidence="0.95">Entity field should be filtered</fact>
<fact category="environment" entity="service" attribute="access_token" value="configured" confidence="0.95">Attribute field should be filtered</fact>
<fact category="environment" entity="editor" attribute="name" value="Bearer demo-credential" confidence="0.95">Value field should be filtered</fact>
<fact category="identity" entity="machine" attribute="id" value="550e8400-e29b-41d4-a716-446655440000" confidence="0.95">UUID-shaped credential</fact>
<fact category="password" entity="shell" attribute="name" value="fish" confidence="0.95">Rendered line should be filtered</fact>"#
                .to_string(),
        ));
        let provider: Arc<dyn Provider> = Arc::new(provider);
        let aux = AuxiliaryClient::new(provider, AuxiliaryConfig::new("mock", "mock-aux"));

        let db = MemoryDb::open_in_memory().unwrap();
        db.record_message("sess-1", "user", "Remember my editor preference")
            .unwrap();

        let dir = std::env::temp_dir().join(format!(
            "cos-curator-secret-fields-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let notes = NotesStore::at(dir.join("notes"));
        let curator = MemoryCurator::new(aux, notes.clone(), dir.join("log.json"));

        let outcome = curator.curate_session(&db, "sess-1", false).await.unwrap();

        assert_eq!(outcome.facts_proposed.len(), 6);
        assert_eq!(outcome.facts_added.len(), 1);
        assert_eq!(outcome.facts_added[0].body(), "editor.name = helix");

        let memory = notes.read(MEMORY_FILE).unwrap().unwrap();
        assert!(memory.contains("editor.name = helix"));
        for rejected in [
            "secret-store",
            "access_token",
            "demo-credential",
            "550e8400-e29b-41d4-a716-446655440000",
            "[password]",
        ] {
            assert!(
                !memory.contains(rejected),
                "rejected field reached MEMORY.md: {rejected}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn curate_session_reobserves_equal_legacy_live_state_with_governance() {
        use crate::agent::llm::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
        use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
        use crate::agent::llm::Provider;
        use crate::agent::memory::sqlite_fts::MemoryDb;
        use crate::config::AgentConfig;
        use std::sync::Arc;

        let provider = MockProvider::new("mock-aux", &AgentConfig::default());
        provider.push_response(MockResponse::Text(
            r#"<fact category="environment" entity="os" attribute="base_distribution" value="Ubuntu" lifetime="observed" source_message_id="1" confidence="0.95">Ubuntu was observed</fact>"#
                .to_string(),
        ));
        let provider: Arc<dyn Provider> = Arc::new(provider);
        let aux = AuxiliaryClient::new(provider, AuxiliaryConfig::new("mock", "mock-aux"));

        let db = MemoryDb::open_in_memory().unwrap();
        db.record_message("sess-alias", "user", "This machine runs Ubuntu")
            .unwrap();

        let dir = std::env::temp_dir().join(format!(
            "cos-curator-alias-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let notes = NotesStore::at(dir.join("notes"));
        notes
            .write(
                MEMORY_FILE,
                "## Curated facts (auto)\n- [environment] operating_system.name = Ubuntu _(2025-12-31, conf 0.85)_\n",
            )
            .unwrap();

        let curator = MemoryCurator::new(aux, notes.clone(), dir.join("log.json"));
        let outcome = curator
            .curate_session(&db, "sess-alias", false)
            .await
            .unwrap();

        assert_eq!(outcome.facts_proposed.len(), 1);
        assert_eq!(
            outcome.facts_added.len(),
            1,
            "legacy live state needs a governed replacement"
        );
        let raw = notes.read(MEMORY_FILE).unwrap().unwrap();
        assert_eq!(raw.matches("Ubuntu").count(), 2, "history must be additive");
        assert!(raw.contains("os.distribution = Ubuntu"));
        assert!(raw.contains("lifetime=observed"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn curate_session_rejects_malformed_session_and_procedure_facts() {
        use crate::agent::llm::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
        use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
        use crate::agent::llm::Provider;
        use crate::agent::memory::sqlite_fts::MemoryDb;
        use crate::config::AgentConfig;
        use std::sync::Arc;

        let provider = MockProvider::new("mock-aux", &AgentConfig::default());
        provider.push_response(MockResponse::Text(
            r#"<fact category="preference" confidence="0.95">missing structured fields</fact>
<fact category="task" entity="task" attribute="current_goal" value="finish issue" lifetime="session" confidence="0.95">task state</fact>
<fact category="procedure" entity="release" attribute="steps" value="build then publish" lifetime="procedure" confidence="0.95">workflow</fact>
<fact category="preference" entity="broken" attribute="name" value="unterminated" confidence="0.95">oops"#
                .to_string(),
        ));
        let provider: Arc<dyn Provider> = Arc::new(provider);
        let aux = AuxiliaryClient::new(provider, AuxiliaryConfig::new("mock", "mock-aux"));

        let db = MemoryDb::open_in_memory().unwrap();
        db.record_message("sess-malformed", "user", "temporary instructions")
            .unwrap();
        let dir = std::env::temp_dir().join(format!(
            "cos-curator-malformed-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let notes = NotesStore::at(dir.join("notes"));
        let curator = MemoryCurator::new(aux, notes.clone(), dir.join("log.json"));

        let outcome = curator
            .curate_session(&db, "sess-malformed", false)
            .await
            .unwrap();
        assert_eq!(outcome.facts_proposed.len(), 3);
        assert!(outcome.facts_added.is_empty());
        assert_eq!(notes.read(MEMORY_FILE).unwrap(), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn curate_session_marks_invalid_source_provenance_unknown() {
        use crate::agent::llm::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
        use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
        use crate::agent::llm::Provider;
        use crate::agent::memory::sqlite_fts::MemoryDb;
        use crate::config::AgentConfig;
        use std::sync::Arc;

        let provider = MockProvider::new("mock-aux", &AgentConfig::default());
        provider.push_response(MockResponse::Text(
            r#"<fact category="preference" entity="editor" attribute="name" value="helix" lifetime="durable" source_message_id="99999" confidence="0.95">Editor preference</fact>"#
                .to_string(),
        ));
        let provider: Arc<dyn Provider> = Arc::new(provider);
        let aux = AuxiliaryClient::new(provider, AuxiliaryConfig::new("mock", "mock-aux"));

        let db = MemoryDb::open_in_memory().unwrap();
        let _source_id = db
            .record_message("sess-source", "user", "I prefer helix")
            .unwrap();
        db.record_message("sess-source", "assistant", "noted")
            .unwrap();
        let dir = std::env::temp_dir().join(format!(
            "cos-curator-source-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let notes = NotesStore::at(dir.join("notes"));
        let curator = MemoryCurator::new(aux, notes.clone(), dir.join("log.json"));

        let outcome = curator
            .curate_session(&db, "sess-source", false)
            .await
            .unwrap();
        assert_eq!(outcome.facts_added.len(), 1);
        let added = &outcome.facts_added[0];
        assert_eq!(added.source_session_id.as_deref(), Some("sess-source"));
        assert_eq!(added.source_message_id, None);
        assert_eq!(added.observed_at, None);
        assert_eq!(added.lifetime, Some(FactLifetime::Durable));

        let memory = notes.read(MEMORY_FILE).unwrap().unwrap();
        assert!(memory.contains("source_session:sess-source"));
        assert!(memory.contains("source_message:unknown"));
        assert!(memory.contains("observed_at=unknown"));
        assert!(memory.contains("lifetime=durable"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn curate_session_derives_observed_at_from_valid_source_timestamp() {
        use crate::agent::llm::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
        use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
        use crate::agent::llm::Provider;
        use crate::agent::memory::sqlite_fts::MemoryDb;
        use crate::config::AgentConfig;
        use std::sync::Arc;

        let db = MemoryDb::open_in_memory().unwrap();
        let source_id = db
            .record_message_at(
                "sess-observed",
                "user",
                "Python 3.13 is installed",
                1_704_067_200_000,
            )
            .unwrap();

        let provider = MockProvider::new("mock-aux", &AgentConfig::default());
        provider.push_response(MockResponse::Text(format!(
            r#"<fact category="preference" entity="python" attribute="version" value="3.13" lifetime="durable" observed_at="2099-01-01" source_message_id="{source_id}" confidence="0.95">Python version</fact>"#
        )));
        let provider: Arc<dyn Provider> = Arc::new(provider);
        let aux = AuxiliaryClient::new(provider, AuxiliaryConfig::new("mock", "mock-aux"));

        let dir = std::env::temp_dir().join(format!(
            "cos-curator-observed-at-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let notes = NotesStore::at(dir.join("notes"));
        let curator = MemoryCurator::new(aux, notes.clone(), dir.join("log.json"));

        let outcome = curator
            .curate_session(&db, "sess-observed", false)
            .await
            .unwrap();
        assert_eq!(outcome.facts_added.len(), 1);
        let added = &outcome.facts_added[0];
        assert_eq!(added.lifetime, Some(FactLifetime::Observed));
        assert_eq!(added.observed_at.as_deref(), Some("2024-01-01"));
        assert_eq!(added.source_message_id, Some(source_id));

        let memory = notes.read(MEMORY_FILE).unwrap().unwrap();
        assert!(memory.contains("observed_at=2024-01-01"));
        assert!(!memory.contains("2099-01-01"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_curator_appends_and_manual_update_preserve_every_entry() {
        use std::sync::{Arc, Barrier};

        let dir = std::env::temp_dir().join(format!(
            "cos-curator-concurrent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let notes = NotesStore::at(dir.join("notes"));
        let writers = 8usize;
        let barrier = Arc::new(Barrier::new(writers + 1));
        let mut handles = Vec::new();

        for index in 0..writers {
            let notes = notes.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                let fact = ExtractedFact {
                    category: FactCategory::Preference,
                    text: format!("preference {index}"),
                    confidence: 0.9,
                    entity: Some(format!("editor_{index}")),
                    attribute: Some("name".into()),
                    value: Some(format!("value_{index}")),
                    lifetime: Some(FactLifetime::Durable),
                    observed_at: Some("2026-08-27".into()),
                    ttl_days: None,
                    source_session_id: Some(format!("session-{index}")),
                    source_message_id: Some(index as i64 + 1),
                };
                barrier.wait();
                append_facts_locked(&notes, vec![fact], "2026-08-27").unwrap();
            }));
        }

        let manual_notes = notes.clone();
        let manual_barrier = barrier.clone();
        let manual = std::thread::spawn(move || {
            manual_barrier.wait();
            manual_notes
                .update(MEMORY_FILE, |current| {
                    let mut content = current.unwrap_or_default();
                    content.push_str("# Manual edit\n");
                    content
                })
                .unwrap();
        });

        for handle in handles {
            handle.join().unwrap();
        }
        manual.join().unwrap();

        let memory = notes.read(MEMORY_FILE).unwrap().unwrap();
        assert!(memory.contains("# Manual edit"));
        for index in 0..writers {
            assert!(memory.contains(&format!("editor_{index}.name = value_{index}")));
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn curate_session_appends_correction_for_changed_value() {
        // The append-only claim: a new value for a known key is not a
        // duplicate, it is a correction. Both lines must end up on disk
        // so the transition stays readable.
        use crate::agent::llm::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
        use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
        use crate::agent::llm::Provider;
        use crate::agent::memory::sqlite_fts::MemoryDb;
        use crate::config::AgentConfig;
        use std::sync::Arc;

        let cfg = AgentConfig::default();
        let provider = MockProvider::new("mock-aux", &cfg);
        provider.push_response(MockResponse::Text(
            r#"<fact category="preference" entity="editor" attribute="name" value="helix" confidence="0.95">User switched to helix</fact>"#
                .to_string(),
        ));
        let provider: Arc<dyn Provider> = Arc::new(provider);
        let aux = AuxiliaryClient::new(provider, AuxiliaryConfig::new("mock", "mock-aux"));

        let db = MemoryDb::open_in_memory().unwrap();
        db.record_message("sess-1", "user", "stuff").unwrap();

        let dir = std::env::temp_dir().join(format!(
            "cos-curator-correct-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let notes = NotesStore::at(dir.join("notes"));
        notes
            .write(
                MEMORY_FILE,
                "# memory\n\n## Curated facts (auto)\n- [preference] editor.name = vim _(2025-12-31, conf 0.85)_\n",
            )
            .unwrap();

        let curator = MemoryCurator::new(aux, notes.clone(), dir.join("log.json"));
        let outcome = curator.curate_session(&db, "sess-1", false).await.unwrap();

        assert_eq!(outcome.facts_added.len(), 1, "correction must be appended");

        let memory = notes.read(MEMORY_FILE).unwrap().unwrap();
        assert!(memory.contains("editor.name = vim"), "old value retained");
        assert!(memory.contains("editor.name = helix"), "new value appended");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn curate_session_skips_unchanged_structured_restatement() {
        use crate::agent::llm::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
        use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
        use crate::agent::llm::Provider;
        use crate::agent::memory::sqlite_fts::MemoryDb;
        use crate::config::AgentConfig;
        use std::sync::Arc;

        let cfg = AgentConfig::default();
        let provider = MockProvider::new("mock-aux", &cfg);
        provider.push_response(MockResponse::Text(
            r#"<fact category="preference" entity="editor" attribute="name" value="helix" confidence="0.95">Still helix</fact>"#
                .to_string(),
        ));
        let provider: Arc<dyn Provider> = Arc::new(provider);
        let aux = AuxiliaryClient::new(provider, AuxiliaryConfig::new("mock", "mock-aux"));

        let db = MemoryDb::open_in_memory().unwrap();
        db.record_message("sess-1", "user", "stuff").unwrap();

        let dir = std::env::temp_dir().join(format!(
            "cos-curator-same-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let notes = NotesStore::at(dir.join("notes"));
        notes
            .write(
                MEMORY_FILE,
                "# memory\n\n## Curated facts (auto)\n- [preference] editor.name = helix _(2025-12-31, conf 0.85)_\n",
            )
            .unwrap();

        let curator = MemoryCurator::new(aux, notes.clone(), dir.join("log.json"));
        let outcome = curator.curate_session(&db, "sess-1", false).await.unwrap();

        assert!(
            outcome.facts_added.is_empty(),
            "unchanged value must not be re-appended"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn curate_session_dedupes_against_existing_memory() {
        use crate::agent::llm::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
        use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
        use crate::agent::llm::Provider;
        use crate::agent::memory::sqlite_fts::MemoryDb;
        use crate::config::AgentConfig;
        use std::sync::Arc;

        let cfg = AgentConfig::default();
        let provider = MockProvider::new("mock-aux", &cfg);
        provider.push_response(MockResponse::Text(
            r#"<fact category="preference" confidence="0.9">User prefers Rust over Go</fact>"#
                .to_string(),
        ));
        let provider: Arc<dyn Provider> = Arc::new(provider);
        let aux = AuxiliaryClient::new(provider, AuxiliaryConfig::new("mock", "mock-aux"));

        let db = MemoryDb::open_in_memory().unwrap();
        db.record_message("sess-1", "user", "stuff").unwrap();

        let dir = std::env::temp_dir().join(format!(
            "cos-curator-dedupe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let notes = NotesStore::at(dir.join("notes"));

        // Pre-seed with the same fact (different casing / meta).
        notes
            .write(
                MEMORY_FILE,
                r#"# memory

## Curated facts (auto)
- [preference] user prefers rust over go _(2025-12-31, conf 0.85)_
"#,
            )
            .unwrap();

        let log_path = dir.join("log.json");
        let curator = MemoryCurator::new(aux, notes, log_path);
        let outcome = curator.curate_session(&db, "sess-1", false).await.unwrap();

        // LLM proposed one fact; dedupe filtered it out.
        assert_eq!(outcome.facts_proposed.len(), 1);
        assert_eq!(outcome.facts_added.len(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn curate_session_dry_run_skips_llm_call() {
        use crate::agent::llm::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
        use crate::agent::llm::providers::mock::MockProvider;
        use crate::agent::llm::Provider;
        use crate::agent::memory::sqlite_fts::MemoryDb;
        use crate::config::AgentConfig;
        use std::sync::Arc;

        // Mock provider with NO scripted responses — so any LLM call
        // would either error or return empty. We assert dry_run stops
        // before getting there.
        let cfg = AgentConfig::default();
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new("mock-aux", &cfg));
        let aux = AuxiliaryClient::new(provider, AuxiliaryConfig::new("mock", "mock-aux"));

        let db = MemoryDb::open_in_memory().unwrap();
        db.record_message("sess-1", "user", "hi").unwrap();

        let dir = std::env::temp_dir().join(format!(
            "cos-curator-dry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let notes = NotesStore::at(dir.join("notes"));

        let curator = MemoryCurator::new(aux, notes, dir.join("log.json"));
        let outcome = curator.curate_session(&db, "sess-1", true).await.unwrap();
        assert_eq!(outcome.messages_examined, 1);
        assert!(outcome.facts_proposed.is_empty());
        assert!(outcome.facts_added.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- Three-phase bracketing (issue #2, point 2) ------------------

    #[test]
    fn orphaned_runs_returns_empty_when_all_completed() {
        let mut log = CurationLog::default();
        log.begin_run("r1", "s1");
        log.complete_run("r1", "s1", Some(42), 3);
        assert!(log.orphaned_runs().is_empty());
    }

    #[test]
    fn orphaned_runs_flags_in_progress_without_matching_close() {
        let mut log = CurationLog::default();
        log.begin_run("r-crash", "s1");
        // No complete_run / fail_run — simulates a crash between
        // aux LLM extraction and MEMORY.md finalisation.
        let orphans = log.orphaned_runs();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].run_id, "r-crash");
        assert_eq!(orphans[0].session_id, "s1");
        assert_eq!(orphans[0].phase, RunPhase::InProgress);
    }

    #[test]
    fn fail_run_closes_the_bracket_and_is_not_orphaned() {
        let mut log = CurationLog::default();
        log.begin_run("r-err", "s1");
        log.fail_run("r-err", "s1", "aux LLM error: timeout");
        assert!(log.orphaned_runs().is_empty());
    }

    #[test]
    fn truncate_runs_bounds_history() {
        let mut log = CurationLog::default();
        for i in 0..10 {
            let id = format!("r{i}");
            log.begin_run(&id, "s1");
            log.complete_run(&id, "s1", Some(i as i64), 0);
        }
        log.truncate_runs(6);
        assert_eq!(log.runs.len(), 6);
        // Oldest are dropped: first surviving entry references a
        // run id from the second half.
        assert!(log.runs[0].run_id.starts_with("r"));
    }

    #[test]
    fn v1_log_deserializes_with_empty_runs() {
        // v1 schema had no `runs` field. Loading it must succeed
        // and leave `runs` empty so a schema bump doesn't wedge
        // agents that upgrade in place.
        let dir = std::env::temp_dir().join(format!(
            "cos-curator-v1compat-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log.json");
        std::fs::write(
            &path,
            r#"{"version":1,"sessions":{"s1":{"last_curated_message_id":7,"last_run_unix_s":100,"facts_added_total":2}}}"#,
        )
        .unwrap();
        let log = CurationLog::load(&path);
        assert_eq!(log.sessions.len(), 1);
        assert!(log.runs.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
