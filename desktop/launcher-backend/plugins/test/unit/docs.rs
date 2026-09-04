use super::*;

#[test]
fn nonzero_cli_result_is_never_treated_as_a_legacy_error_envelope() {
    let error = decode_search_output(
        false,
        "exit status: 7",
        br#"{"error":"legacy envelope"}"#,
        b"tool call failed",
    )
    .expect_err("nonzero status must fail");

    assert_eq!(error, "cos failed (exit status: 7): tool call failed");
}

#[test]
fn successful_structured_result_is_decoded() {
    let result = decode_search_output(
        true,
        "exit status: 0",
        br#"{"hint":null,"results":[{"path":"/tmp/report.pdf","snippet":"budget"}]}"#,
        b"",
    )
    .expect("valid result");

    assert!(result.hint.is_none());
    assert_eq!(result.results.len(), 1);
    assert_eq!(result.results[0].path, PathBuf::from("/tmp/report.pdf"));
    assert_eq!(result.results[0].snippet, "budget");
}

#[test]
fn successful_empty_output_is_an_error() {
    let error =
        decode_search_output(true, "exit status: 0", b" \n", b"").expect_err("missing result");

    assert_eq!(error, "cos produced no output");
}
