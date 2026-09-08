use super::*;
use claw_applet_clipboard::HistoryPermission;
use claw_applet_services::policy::Scope;

#[test]
fn clipboard_provider_preserves_exact_history_permissions() {
    for (permission, expected) in [
        (HistoryPermission::Read, "clipboard.read"),
        (HistoryPermission::Write, "clipboard.write"),
    ] {
        let (verb, scope) = clipboard_request(permission);
        assert_eq!(verb, expected);
        assert!(matches!(scope, Scope::Name("history")));
    }
}
