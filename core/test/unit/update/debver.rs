use super::*;

fn cmp(left: &str, right: &str) -> Ordering {
    compare(left, right).expect("comparable versions")
}

#[test]
fn generated_package_versions_are_ordered_numerically_not_lexically() {
    // The exact regression a string comparison gets wrong.
    assert_eq!(
        cmp("1:0.2.0+git100.gaaa", "1:0.2.0+git99.gaaa"),
        Ordering::Greater
    );
    assert_eq!(
        cmp("1:0.2.0+git99.gaaa", "1:0.2.0+git100.gaaa"),
        Ordering::Less
    );
}

#[test]
fn epochs_dominate_the_upstream_version() {
    assert_eq!(cmp("1:0.1.0", "9.9.9"), Ordering::Greater);
    assert_eq!(cmp("0.1.0", "1:0.0.1"), Ordering::Less);
}

#[test]
fn tilde_sorts_before_everything_including_the_empty_string() {
    assert_eq!(cmp("1.0~pr5", "1.0"), Ordering::Less);
    assert_eq!(cmp("1.0~~", "1.0~"), Ordering::Less);
    assert_eq!(
        cmp("1:0.2.0~pr48.git10.gabc", "1:0.2.0+git10.gabc"),
        Ordering::Less
    );
}

#[test]
fn revisions_are_compared_after_the_upstream_version() {
    assert_eq!(cmp("1.0-1", "1.0-2"), Ordering::Less);
    assert_eq!(cmp("1.0-10", "1.0-9"), Ordering::Greater);
    assert_eq!(cmp("1.0", "1.0-0"), Ordering::Equal);
}

#[test]
fn letters_sort_before_other_punctuation() {
    assert_eq!(cmp("1.0a", "1.0+"), Ordering::Less);
}

#[test]
fn leading_zeros_do_not_change_a_numeric_run() {
    assert_eq!(cmp("1.007", "1.7"), Ordering::Equal);
}

#[test]
fn malformed_versions_are_errors_not_equality() {
    assert!(compare("", "1.0").is_err());
    assert!(compare("1.0", "not a version").is_err());
    assert!(compare("x:1.0", "1.0").is_err());
    assert!(!is_valid("1.0 "));
    assert!(!is_valid("-1"));
    assert!(is_valid("1:0.2.0+git1.gabc-1"));
}

#[test]
fn an_upstream_version_must_start_with_a_digit() {
    assert!(!is_valid("v1.0"));
    assert!(is_valid("1.0"));
}
