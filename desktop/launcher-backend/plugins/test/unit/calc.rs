use crate::calc::App;

#[test]
fn extract_value() {
    assert_eq!("7.5", super::extract_value("7 + 1/2 = 7.5"));
    assert_eq!("7.5", super::extract_value("15/2 = 7 + 1/2 = 7.5"));
    assert_eq!("1.333333333", super::extract_value("1 + 1/3 ≈ 1.333333333"));
    assert_eq!(
        "1.333333333",
        super::extract_value("4/3 ≈ 1 + 1/3 ≈ 1.333333333")
    );
}

#[tokio::test]
async fn approximate_result_formatting() {
    let task = tokio::spawn(async {
        let mut app = App {
            decimal_comma: false,
            ..Default::default()
        };
        app.search("7 / 3").await;
        app.outcome.take()
    });

    if let Some(result) = task.await.unwrap() {
        assert_eq!("≈ 2.333333333", result);
    }
}
