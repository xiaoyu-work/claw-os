use super::*;

#[test]
fn truncates_unicode_without_splitting_code_points() {
    assert_eq!(truncate("hello", 5), "hello");
    assert_eq!(truncate("日程表です", 3), "日程表…");
}
