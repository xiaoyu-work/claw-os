use super::*;

#[test]
fn wide_terminal_output_keeps_newest_complete_lines() {
    let output = format!("{}\nnewest line one\nnewest line two", "old".repeat(20_000));
    let context = bounded_explain_context(&output, Some("/work")).unwrap();

    assert_eq!(context.output, "newest line one\nnewest line two");
    assert!(context.truncated);
    let encoded = ask_claw::serialize_context(&context).unwrap();
    let decoded: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded["app"], "cosmic-term");
    assert_eq!(decoded["mode"], "explain_output");
    assert_eq!(decoded["cwd"], "/work");
    assert_eq!(decoded["truncated"], true);
}

#[test]
fn multibyte_terminal_output_is_truncated_on_a_character_boundary() {
    let output = "界🙂".repeat(10_000);
    let context = bounded_explain_context(&output, None).unwrap();

    assert!(context.truncated);
    assert!(output.ends_with(context.output));
    assert!(context.output.starts_with(['界', '🙂']));
    assert!(ask_claw::context_fits(&context).unwrap());
}
