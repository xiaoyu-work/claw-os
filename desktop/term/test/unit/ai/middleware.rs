use super::*;

#[test]
fn strip_csi_removes_arrow_keys() {
    assert_eq!(strip_csi_ss3("\x1b[A"), "");
    assert_eq!(strip_csi_ss3("\x1b[Bhello"), "hello");
    assert_eq!(strip_csi_ss3("hi\x1b[Cworld"), "hiworld");
    assert_eq!(strip_csi_ss3("\x1bOP"), "");
    assert_eq!(strip_csi_ss3("plain"), "plain");
}

#[test]
fn pop_char_handles_unicode() {
    let mut s = String::from("héllo");
    pop_char(&mut s);
    assert_eq!(s, "héll");
    let mut s = String::from("a你");
    pop_char(&mut s);
    assert_eq!(s, "a");
}

#[test]
fn split_placeholders_basic() {
    let out = split_with_placeholders("hello [Pasted Text: 3 lines #1] world");
    assert_eq!(out, vec!["hello ", "[Pasted Text: 3 lines #1]", " world"]);
}

#[test]
fn split_placeholders_no_match() {
    let out = split_with_placeholders("just text");
    assert_eq!(out, vec!["just text"]);
}
