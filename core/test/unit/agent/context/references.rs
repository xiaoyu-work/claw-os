use super::*;

#[test]
fn extracts_simple_path_reference() {
    let refs = extract("please read @notes.md");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].raw, "notes.md");
    assert_eq!(refs[0].kind, ReferenceKind::Path);
}

#[test]
fn classifies_url_reference() {
    let refs = extract("see @https://example.com/page");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].raw, "https://example.com/page");
    assert_eq!(refs[0].kind, ReferenceKind::Url);
}

#[test]
fn classifies_relative_and_absolute_paths() {
    let refs = extract("a @./local b @/etc/hosts c @../up");
    assert_eq!(refs.len(), 3);
    assert_eq!(refs[0].kind, ReferenceKind::RelativePath);
    assert_eq!(refs[1].kind, ReferenceKind::AbsolutePath);
    assert_eq!(refs[2].kind, ReferenceKind::RelativePath);
}

#[test]
fn classifies_windows_drive_paths() {
    let refs = extract("look at @C:\\Users\\me\\file.txt");
    assert_eq!(refs.len(), 1);
    // Windows path: trailing chars are not in TRIM_TAIL until '.txt' so
    // the 't' isn't stripped. End-trim only fires on punctuation.
    assert_eq!(refs[0].raw, "C:\\Users\\me\\file.txt");
    assert_eq!(refs[0].kind, ReferenceKind::AbsolutePath);
}

#[test]
fn skips_email_addresses() {
    let refs = extract("ping me at user@example.com please");
    assert!(refs.is_empty(), "got: {refs:?}");
}

#[test]
fn skips_doubled_at_escape() {
    let refs = extract("write @@foo as a literal");
    assert!(refs.is_empty(), "got: {refs:?}");
}

#[test]
fn quoted_path_keeps_spaces() {
    let refs = extract("look at @\"my notes.md\" please");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].raw, "my notes.md");
    assert_eq!(refs[0].kind, ReferenceKind::Path);
}

#[test]
fn quoted_unterminated_emits_nothing() {
    let refs = extract("look at @\"notes.md without close quote");
    assert!(refs.is_empty(), "got: {refs:?}");
}

#[test]
fn empty_quoted_emits_nothing() {
    let refs = extract("@\"\" ignored");
    assert!(refs.is_empty(), "got: {refs:?}");
}

#[test]
fn trims_trailing_sentence_punctuation() {
    let refs = extract("Read @notes.md, then @summary.txt.");
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].raw, "notes.md");
    assert_eq!(refs[1].raw, "summary.txt");
}

#[test]
fn trims_trailing_brackets_and_parens() {
    let refs = extract("(see @doc.md) and [also @other.md]");
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].raw, "doc.md");
    assert_eq!(refs[1].raw, "other.md");
}

#[test]
fn keeps_url_query_and_fragment() {
    let refs = extract("@https://example.com/x?a=1&b=2#frag");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].raw, "https://example.com/x?a=1&b=2#frag");
}

#[test]
fn at_start_of_input_works() {
    let refs = extract("@README.md");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].raw, "README.md");
}

#[test]
fn span_offsets_match_original_text() {
    let s = "x @abc";
    let refs = extract(s);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].start, 2);
    assert_eq!(refs[0].end, 6);
    assert_eq!(&s[refs[0].start..refs[0].end], "@abc");
}

#[test]
fn quoted_span_includes_closing_quote() {
    let s = "@\"a b\"";
    let refs = extract(s);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].start, 0);
    assert_eq!(refs[0].end, s.len());
    assert_eq!(refs[0].raw, "a b");
}

#[test]
fn multiple_references_preserve_order_and_offsets() {
    let s = "@one and @two and @three";
    let refs = extract(s);
    let raws: Vec<&str> = refs.iter().map(|r| r.raw.as_str()).collect();
    assert_eq!(raws, vec!["one", "two", "three"]);
    assert!(refs[0].start < refs[1].start);
    assert!(refs[1].start < refs[2].start);
}

#[test]
fn extract_unique_dedupes_preserving_first_position() {
    let s = "@a @b @a @b @c";
    let refs = extract_unique(s);
    let raws: Vec<&str> = refs.iter().map(|r| r.raw.as_str()).collect();
    assert_eq!(raws, vec!["a", "b", "c"]);
}

#[test]
fn extract_unique_treats_different_kinds_as_distinct() {
    let s = "@./README.md and @README.md";
    let refs = extract_unique(s);
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].kind, ReferenceKind::RelativePath);
    assert_eq!(refs[1].kind, ReferenceKind::Path);
}

#[test]
fn empty_input_returns_empty() {
    assert!(extract("").is_empty());
    assert!(extract_unique("").is_empty());
}

#[test]
fn no_references_returns_empty() {
    assert!(extract("just plain text, no refs").is_empty());
}

#[test]
fn lone_at_with_no_body_emits_nothing() {
    let refs = extract("hello @ world");
    assert!(refs.is_empty(), "got: {refs:?}");
}

#[test]
fn url_classification_is_case_insensitive() {
    let refs = extract("@HTTPS://EXAMPLE.COM/x");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].kind, ReferenceKind::Url);
}

#[test]
fn ref_inside_backticks_still_extracted() {
    let refs = extract("see `@notes.md` for details");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].raw, "notes.md");
}

#[test]
fn boundary_after_period_in_word_does_not_trigger_ref() {
    // Regression guard: 'foo.com@bar' should not split into a ref
    // because the '@' is preceded by a word/period character.
    let refs = extract("foo.com@bar");
    assert!(refs.is_empty(), "got: {refs:?}");
}
