use super::*;

#[test]
fn nul_in_path_detected() {
    let bad = "C:\\tmp\\with\0nul";
    match CString::new(bad) {
        Err(_) => {}
        Ok(_) => panic!("expected NulError for path containing NUL"),
    }
}

#[test]
fn nul_in_input_detected() {
    let bad = "hello\0world";
    match CString::new(bad) {
        Err(_) => {}
        Ok(_) => panic!("expected NulError for input containing NUL"),
    }
}

#[test]
fn error_display_smoke_test() {
    let e = OrtGenaiError::Runtime("boom".into());
    assert!(format!("{e}").contains("boom"));
    let e = OrtGenaiError::TensorTypeMismatch {
        expected: OgaElementType::Float32,
        actual: OgaElementType::Int64,
    };
    let s = format!("{e}");
    assert!(s.contains("Float32"));
    assert!(s.contains("Int64"));
}
