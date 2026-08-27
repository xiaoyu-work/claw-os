    use super::*;

    fn tmpdir(label: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("cos-notes-{}-{}", label, std::process::id()));
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn read_missing_returns_none() {
        let dir = tmpdir("read-missing");
        let s = NotesStore::at(&dir);
        assert_eq!(s.read("MEMORY.md").unwrap(), None);
    }

    #[test]
    fn write_then_read_roundtrips() {
        let dir = tmpdir("write-read");
        let s = NotesStore::at(&dir);
        s.write("MEMORY.md", "remember: pineapples").unwrap();
        let got = s.read("MEMORY.md").unwrap().unwrap();
        assert!(got.contains("pineapples"));
    }

    #[test]
    fn append_creates_then_extends() {
        let dir = tmpdir("append");
        let s = NotesStore::at(&dir);
        s.append("MEMORY.md", "fact: one").unwrap();
        s.append("MEMORY.md", "fact: two").unwrap();
        let got = s.read("MEMORY.md").unwrap().unwrap();
        assert!(got.contains("fact: one") && got.contains("fact: two"));
    }

    #[test]
    fn list_returns_md_files_only() {
        let dir = tmpdir("list");
        let s = NotesStore::at(&dir);
        s.write("MEMORY.md", "x").unwrap();
        s.write("USER.md", "y").unwrap();
        fs::write(dir.join("note.txt"), "ignored").ok();
        let names = s.list().unwrap();
        assert!(names.contains(&"MEMORY.md".to_string()));
        assert!(names.contains(&"USER.md".to_string()));
        assert!(!names.iter().any(|n| n == "note.txt"));
    }

    #[test]
    fn delete_is_idempotent() {
        let dir = tmpdir("delete");
        let s = NotesStore::at(&dir);
        s.write("MEMORY.md", "x").unwrap();
        s.delete("MEMORY.md").unwrap();
        s.delete("MEMORY.md").unwrap();
        assert_eq!(s.read("MEMORY.md").unwrap(), None);
    }

    #[test]
    fn assemble_for_prompt_concatenates_both_when_present() {
        let dir = tmpdir("assemble-both");
        let s = NotesStore::at(&dir);
        s.write("MEMORY.md", "I learned X").unwrap();
        s.write("USER.md", "User prefers Y").unwrap();
        let assembled = s.assemble_for_prompt().unwrap();
        assert!(assembled.contains("# MEMORY.md"));
        assert!(assembled.contains("I learned X"));
        assert!(assembled.contains("# USER.md"));
        assert!(assembled.contains("User prefers Y"));
    }

    #[test]
    fn assemble_for_prompt_skips_empty_files() {
        let dir = tmpdir("assemble-empty");
        let s = NotesStore::at(&dir);
        s.write("MEMORY.md", "   \n").unwrap();
        assert!(s.assemble_for_prompt().is_none());
    }

    #[test]
    fn assemble_for_prompt_returns_none_when_dir_missing() {
        let dir = tmpdir("assemble-missing");
        let s = NotesStore::at(&dir);
        assert!(s.assemble_for_prompt().is_none());
    }

    #[test]
    fn name_with_slash_is_rejected() {
        let dir = tmpdir("name-slash");
        let s = NotesStore::at(&dir);
        assert!(s.write("../escape.md", "x").is_err());
        assert!(s.write("a/b.md", "x").is_err());
    }

    #[test]
    fn name_without_md_extension_is_rejected() {
        let dir = tmpdir("name-ext");
        let s = NotesStore::at(&dir);
        assert!(s.write("MEMORY", "x").is_err());
        assert!(s.write("MEMORY.txt", "x").is_err());
    }

    #[test]
    fn truncate_for_prompt_passes_through_short_input() {
        let s = "hello world";
        let out = truncate_for_prompt(s, 32);
        assert_eq!(out, "hello world");
        assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn select_memory_fast_path_returns_identical_under_budget() {
        // Under budget ⇒ byte-for-byte unchanged (zero-risk common case).
        let mem = "# notes\n- [fact] a\n- [fact] b\n";
        assert_eq!(select_memory_for_prompt(mem, Some("anything"), 10_000), mem);
    }

    #[test]
    fn select_memory_keeps_relevant_bullets_over_budget() {
        // Many bullets, tiny budget, a query that matches one of them.
        // The relevant bullet must survive; an irrelevant one must be
        // dropped; the omission marker must appear.
        let mut mem = String::from("## Curated facts (auto)\n");
        for i in 0..40 {
            mem.push_str(&format!("- [fact] unrelated filler entry number {i}\n"));
        }
        mem.push_str("- [fact] the user's deployment database is named orchard\n");
        // Budget big enough for a few lines but not all 41.
        let out = select_memory_for_prompt(&mem, Some("what is the database called"), 400);
        assert!(
            out.contains("orchard"),
            "the query-relevant entry must be retained, got:\n{out}"
        );
        assert!(
            out.contains("omitted for this turn"),
            "expected an omission marker when entries were dropped"
        );
        assert!(out.chars().count() <= 400 + 200, "result should respect the cap");
    }

    #[test]
    fn select_memory_pins_always_tagged_entries() {
        // An [always] entry must survive even with zero query relevance
        // and a budget that forces most contextual bullets out.
        let mut mem = String::from("## facts\n- [always] operator name is Dana\n");
        for i in 0..50 {
            mem.push_str(&format!("- [fact] filler entry {i} about widgets\n"));
        }
        let out = select_memory_for_prompt(&mem, Some("zzz no match"), 300);
        assert!(
            out.contains("operator name is Dana"),
            "[always]-tagged entry must always be kept, got:\n{out}"
        );
    }

    #[test]
    fn truncate_for_prompt_caps_long_input() {
        let s = "x".repeat(2_000);
        let out = truncate_for_prompt(&s, 200);
        let chars: usize = out.chars().count();
        assert!(chars <= 200, "got {chars} chars, expected <= 200");
        assert!(
            out.contains("[…]"),
            "expected truncation marker, got {out:?}"
        );
        assert!(out.contains("of 2000 chars"));
    }

    #[test]
    fn truncate_for_prompt_is_multibyte_safe() {
        // 1000 four-byte chars × 1 char-budget per "char" → must
        // never panic on a UTF-8 boundary and the kept slice must
        // be valid UTF-8 (the pushed prefix is built from chars()).
        let s = "🦀".repeat(1_000);
        let out = truncate_for_prompt(&s, 100);
        // Marker present + start contains crab.
        assert!(out.starts_with('🦀'));
        assert!(out.contains("[…]"));
    }

    #[test]
    fn truncate_for_prompt_with_zero_cap_keeps_just_marker() {
        let s = "abc".repeat(100);
        let out = truncate_for_prompt(&s, 0);
        assert!(out.contains("[…]"));
        assert!(out.contains("of 300 chars"));
    }

    #[test]
    fn assemble_for_prompt_with_cap_truncates_oversized_note() {
        let dir = tmpdir("assemble-cap");
        let s = NotesStore::at(&dir);
        let big = "y".repeat(5_000);
        s.write("MEMORY.md", &big).unwrap();
        let assembled = s.assemble_for_prompt_with_cap(500).unwrap();
        assert!(assembled.contains("# MEMORY.md"));
        assert!(assembled.contains("[…]"));
        // The total must be small. Header + body + marker;
        // header is ~14 chars, marker ~80 chars, body capped to
        // (500 - 80) = 420.
        let body_chars = assembled.chars().count();
        assert!(
            body_chars < 700,
            "expected ≤ ~600 chars after capping, got {body_chars}"
        );
    }

    #[test]
    fn assemble_for_prompt_with_cap_passes_through_small_notes() {
        let dir = tmpdir("assemble-cap-small");
        let s = NotesStore::at(&dir);
        s.write("MEMORY.md", "tiny note").unwrap();
        let assembled = s.assemble_for_prompt_with_cap(1_024).unwrap();
        assert!(assembled.contains("tiny note"));
        assert!(!assembled.contains("[…]"));
    }

    #[test]
    fn assemble_for_prompt_default_uses_max_note_chars_constant() {
        // 100 KiB content, default cap 32 KiB → must truncate.
        let dir = tmpdir("assemble-default-cap");
        let s = NotesStore::at(&dir);
        let big = "z".repeat(MAX_NOTE_CHARS_FOR_PROMPT * 4);
        s.write("MEMORY.md", &big).unwrap();
        let assembled = s.assemble_for_prompt().unwrap();
        assert!(assembled.contains("[…]"));
        let chars = assembled.chars().count();
        // Header (~14) + body (≤ MAX_NOTE_CHARS_FOR_PROMPT) +
        // marker (~80). Allow some slack.
        assert!(
            chars < MAX_NOTE_CHARS_FOR_PROMPT + 256,
            "assembled prompt ({chars} chars) exceeded cap"
        );
    }

    #[test]
    fn assemble_for_prompt_per_file_cap_is_independent() {
        let dir = tmpdir("assemble-per-file");
        let s = NotesStore::at(&dir);
        // Both files individually within cap; make sure MEMORY's
        // truncation marker doesn't bleed into USER.
        let big = "a".repeat(2_000);
        s.write("MEMORY.md", &big).unwrap();
        s.write("USER.md", "small content").unwrap();
        let assembled = s.assemble_for_prompt_with_cap(500).unwrap();
        assert!(assembled.contains("# MEMORY.md"));
        assert!(assembled.contains("# USER.md"));
        assert!(assembled.contains("[…]"));
        assert!(assembled.contains("small content"));
        // Each # heading appears exactly once.
        assert_eq!(assembled.matches("# MEMORY.md").count(), 1);
        assert_eq!(assembled.matches("# USER.md").count(), 1);
    }

    #[test]
    fn project_chain_tails_keeps_only_the_latest_value_per_key() {
        let content = "\
# memory

## Curated facts (auto)
- [preference] editor.name = vim _(2026-01-01, conf 0.90)_
- [environment] shell.name = bash _(2026-01-02, conf 0.90)_
- [preference] editor.name = helix _(2026-03-01, conf 0.95)_
";
        let out = project_chain_tails(content);
        assert!(!out.contains("editor.name = vim"));
        assert!(out.contains("editor.name = helix"));
        assert!(out.contains("shell.name = bash"));
        // Structural lines survive untouched.
        assert!(out.contains("# memory"));
        assert!(out.contains("## Curated facts (auto)"));
    }

    #[test]
    fn project_chain_tails_leaves_unstructured_entries_alone() {
        // Two prose facts might or might not describe the same slot;
        // without a key we cannot tell, so both are kept.
        let content = "\
## Curated facts (auto)
- [preference] User prefers Rust _(2026-01-01, conf 0.90)_
- [preference] User prefers Go _(2026-02-01, conf 0.90)_
";
        let out = project_chain_tails(content);
        assert!(out.contains("User prefers Rust"));
        assert!(out.contains("User prefers Go"));
    }

    #[test]
    fn project_chain_tails_is_identity_without_structured_facts() {
        let content = "# memory\n\nsome prose\n- a plain bullet\n";
        assert_eq!(project_chain_tails(content), content);
    }

    #[test]
    fn project_chain_tails_preserves_document_order() {
        let content = "\
## Curated facts (auto)
- [environment] os.name = linux _(2026-01-01, conf 0.90)_
- [preference] editor.name = vim _(2026-01-01, conf 0.90)_
- [preference] editor.name = helix _(2026-03-01, conf 0.95)_
";
        let out = project_chain_tails(content);
        let os_at = out.find("os.name").unwrap();
        let ed_at = out.find("editor.name").unwrap();
        assert!(os_at < ed_at);
    }

    #[test]
    fn project_chain_tails_merges_canonical_aliases_without_rewriting_history() {
        let content = "\
## Curated facts (auto)
- [environment] operating_system.name = Ubuntu _(2026-01-01, conf 0.90)_
- [environment] os.base_distribution = Debian _(2026-02-01, conf 0.95)_
";
        let out = project_chain_tails(content);
        assert!(!out.contains("operating_system.name = Ubuntu"));
        assert!(out.contains("os.base_distribution = Debian"));
        assert!(content.contains("operating_system.name = Ubuntu"));
    }

    #[test]
    fn version_and_not_found_conflict_projects_only_the_newest_observation() {
        let version_then_missing = "\
## Curated facts (auto)
- [environment] python.version = 3.13.1 _(2026-01-01, conf 0.90)_
- [environment] python.installation = not_found _(2026-01-02, conf 0.95)_
";
        let out = project_chain_tails(version_then_missing);
        assert!(!out.contains("python.version"));
        assert!(out.contains("python.installation = not_found"));

        let missing_then_version = "\
## Curated facts (auto)
- [environment] python.installation = missing _(2026-01-01, conf 0.90)_
- [environment] python.version = 3.13.1 _(2026-01-02, conf 0.95)_
";
        let out = project_chain_tails(missing_then_version);
        assert!(!out.contains("python.installation"));
        assert!(out.contains("python.version = 3.13.1"));
    }

    #[test]
    fn expired_observations_are_excluded_without_resurrecting_superseded_state() {
        let content = "\
## Curated facts (auto)
- [environment] python.version = 3.12 _(observed_at=2026-01-01, ttl=90d, source_session=s1, source_message=1, conf=0.90, lifetime=observed)_
- [environment] python.version = 3.13 _(observed_at=2026-02-01, ttl=7d, source_session=s2, source_message=2, conf=0.95, lifetime=observed)_
- [preference] editor.name = helix _(observed_at=2026-01-01, source_session=s1, source_message=1, conf=0.95, lifetime=durable)_
";
        let now = crate::agent::memory::ontology::date_to_epoch_days("2026-02-10").unwrap();
        let out = project_chain_tails_at(content, now);
        assert!(!out.contains("python.version = 3.12"));
        assert!(!out.contains("python.version = 3.13"));
        assert!(out.contains("editor.name = helix"));
        assert!(content.contains("python.version = 3.12"));
        assert!(content.contains("python.version = 3.13"));
    }

    #[test]
    fn malformed_observed_metadata_fails_closed_but_legacy_lines_stay_visible() {
        let content = "\
# hand-written memory
Keep this prose editable.

## Curated facts (auto)
- [environment] node.version = 24 _(ttl=30d, conf=0.90, lifetime=observed)_
- [environment] shell.name = bash _(2025-01-01, conf 0.90)_
";
        let now = crate::agent::memory::ontology::date_to_epoch_days("2026-02-10").unwrap();
        let out = project_chain_tails_at(content, now);
        assert!(!out.contains("node.version = 24"));
        assert!(out.contains("shell.name = bash"));
        assert!(out.contains("Keep this prose editable."));
    }

    #[test]
    fn assemble_for_prompt_hides_superseded_facts() {
        // The end-to-end guarantee: a superseded value must never reach
        // the model, even though it is still on disk.
        let dir = std::env::temp_dir().join(format!(
            "cos-notes-tails-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let store = NotesStore::at(&dir);
        store
            .write(
                MEMORY_FILE,
                "\
## Curated facts (auto)
- [preference] editor.name = vim _(2026-01-01, conf 0.90)_
- [preference] editor.name = helix _(2026-03-01, conf 0.95)_
",
            )
            .unwrap();

        let assembled = store.assemble_for_prompt().unwrap();
        assert!(assembled.contains("helix"));
        assert!(!assembled.contains("vim"));

        // But the file itself still has the full history.
        let raw = store.read(MEMORY_FILE).unwrap().unwrap();
        assert!(raw.contains("vim"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn assemble_for_prompt_hides_expired_observation_but_raw_read_keeps_it() {
        let dir = tmpdir("assemble-expired");
        let store = NotesStore::at(&dir);
        store
            .write(
                MEMORY_FILE,
                "\
## Curated facts (auto)
- [environment] ripgrep.version = 13.0 _(observed_at=2000-01-01, ttl=30d, source_session=old, source_message=1, conf=0.90, lifetime=observed)_
- [resolution] ripgrep_search.cause = wrong glob _(observed_at=2000-01-01, source_session=old, source_message=2, conf=0.95, lifetime=durable)_
",
            )
            .unwrap();

        let assembled = store.assemble_for_prompt().unwrap();
        assert!(!assembled.contains("ripgrep.version = 13.0"));
        assert!(assembled.contains("ripgrep_search.cause = wrong glob"));

        let raw = store.read(MEMORY_FILE).unwrap().unwrap();
        assert!(raw.contains("ripgrep.version = 13.0"));

        let _ = std::fs::remove_dir_all(&dir);
    }
