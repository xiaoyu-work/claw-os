use super::*;
use serde_json::json;

#[test]
fn parses_snapshot_deduplicates_truncates_and_rejects_binary_text() {
    let mut values = vec![
        json!({"identity": "first", "text": "first"}),
        json!({"identity": "first", "text": "first"}),
        json!({"identity": "binary", "text": "bad\u{0}value"}),
        json!({"identity": "second", "text": "second\nline", "copied_at": " 123 "}),
    ];
    for index in 0..25 {
        values.push(json!({
            "identity": format!("item-{index}"),
            "text": format!("value {index}")
        }));
    }

    let entries = parse_snapshot(&serde_json::to_vec(&values).unwrap()).unwrap();
    assert_eq!(entries.len(), MAX_ITEMS);
    assert_eq!(entries[0].identity, "first");
    assert_eq!(entries[1].identity, "second");
    assert_eq!(entries[1].preview, "second line");
    assert_eq!(entries[1].copied_at.as_deref(), Some("123"));
    assert!(!entries.iter().any(|entry| entry.identity == "binary"));
}

#[test]
fn rejects_non_utf8_json_snapshot() {
    assert!(parse_snapshot(&[0xff, 0xfe]).is_err());
}

#[test]
fn truncates_unicode_preview_safely() {
    assert_eq!(preview("日 程 表 です", 3), "日 程…");
    assert_eq!(preview("short", 20), "short");
}

#[test]
fn constructs_eval_commands_without_identity_or_shell_arguments() {
    for command in [
        CopyqCommand::Snapshot,
        CopyqCommand::Restore,
        CopyqCommand::Remove,
        CopyqCommand::Clear,
    ] {
        let args = command.args();
        assert_eq!(args[0], OsString::from("eval"));
        assert_eq!(args.len(), 2);
        assert_eq!(args[1], OsString::from(command.script()));
    }
    assert!(
        !CopyqCommand::Restore
            .args()
            .iter()
            .any(|arg| arg == "opaque-identity")
    );
}

#[test]
fn snapshot_script_filters_text_and_computes_stable_identity() {
    assert!(SNAPSHOT_SCRIPT.contains("getItem(row)"));
    assert!(SNAPSHOT_SCRIPT.contains("mimeText in item"));
    assert!(SNAPSHOT_SCRIPT.contains("toBase64(sha256sum(data))"));
    assert!(SNAPSHOT_SCRIPT.contains("toBase64(sha256sum(pack(item)))"));
    assert!(SNAPSHOT_SCRIPT.contains("entries.length < 20"));
    assert!(SNAPSHOT_SCRIPT.contains("textIdentity === previousTextIdentity"));
    assert!(SNAPSHOT_SCRIPT.contains("no-clipboard-tab"));
}

#[test]
fn action_scripts_relocate_identity_before_supported_operations() {
    for script in [RESTORE_SCRIPT, REMOVE_SCRIPT] {
        assert!(script.contains("str(input())"));
        assert!(script.contains("toBase64(sha256sum(pack(item)))"));
        assert!(script.contains("not-found"));
        assert!(script.contains("no-clipboard-tab"));
    }
    assert!(RESTORE_SCRIPT.contains("select(currentRow)"));
    assert!(REMOVE_SCRIPT.contains("remove(currentRow)"));
    assert!(CLEAR_SCRIPT.contains("tab(clipboardTab)"));
    assert!(CLEAR_SCRIPT.contains("remove(row)"));
    assert!(CLEAR_SCRIPT.contains("no-clipboard-tab"));
    assert!(!CLEAR_SCRIPT.contains("clear"));
}
