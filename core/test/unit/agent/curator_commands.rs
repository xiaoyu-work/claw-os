use super::*;

#[test]
fn curator_propose_requires_session_id() {
    let err = curator_cmd(&["propose".into()]).unwrap_err();
    assert!(err.to_lowercase().contains("usage"));
}

#[test]
fn curator_propose_rejects_flag_as_session_id() {
    // `propose --accept` without a session id must error rather
    // than silently treating "--accept" as the session id.
    let err = curator_cmd(&["propose".into(), "--accept".into()]).unwrap_err();
    assert!(err.to_lowercase().contains("usage"));
}

#[test]
fn curator_author_requires_draft_id() {
    let err = curator_cmd(&["author".into()]).unwrap_err();
    assert!(err.to_lowercase().contains("usage"));
}

#[test]
fn curator_author_rejects_flag_as_id() {
    let err = curator_cmd(&["author".into(), "--write".into()]).unwrap_err();
    assert!(err.to_lowercase().contains("usage"));
}

#[test]
fn curator_author_missing_draft_returns_helpful_error() {
    // The default DraftStore should open successfully (or fail
    // with an IO error); either way, asking for an unknown id
    // must return a string mentioning the missing id.
    let result = curator_cmd(&["author".into(), "definitely-not-real".into()]);
    let err = result.unwrap_err();
    assert!(
        err.contains("definitely-not-real") || err.contains("draft store"),
        "want missing-id or draft-store error, got: {err}"
    );
}

#[test]
fn curator_scan_returns_envelope_when_db_available() {
    // The scan command may succeed (returning an envelope with
    // zero scanned sessions) or fail with a "memory db
    // unavailable" error depending on test environment. Both
    // are acceptable; what matters is no panic and a recognised
    // outcome shape.
    match curator_cmd(&["scan".into(), "--limit".into(), "1".into()]) {
        Ok(v) => {
            assert!(v.get("scanned").is_some(), "envelope missing 'scanned'");
            assert!(v.get("results").is_some(), "envelope missing 'results'");
            assert!(v.get("drafted").is_some(), "envelope missing 'drafted'");
        }
        Err(e) => {
            assert!(
                e.contains("memory db") || e.contains("draft store"),
                "unexpected scan error: {e}"
            );
        }
    }
}

#[test]
fn curator_drafts_auto_title_rejects_invalid_seed() {
    let err = curator_drafts_cmd(&[
        "auto-title".into(),
        "some-id".into(),
        "--seed".into(),
        "bogus".into(),
    ])
    .unwrap_err();
    assert!(err.contains("--seed"));
}
