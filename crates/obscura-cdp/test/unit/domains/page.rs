use super::*;
use crate::dispatch::CdpContext;

#[tokio::test]
async fn get_layout_metrics_returns_chrome_default_viewport() {
    let mut ctx = CdpContext::new();
    let result = handle("getLayoutMetrics", &json!({}), &mut ctx, &None)
        .await
        .expect("getLayoutMetrics should succeed without a session");

    // CDP spec requires three top-level shapes; Playwright's screenshot
    // path reads contentSize.width/height to size the capture. Without
    // them the screenshot call panics with "cannot read property of
    // undefined".
    for key in [
        "layoutViewport",
        "visualViewport",
        "contentSize",
        "cssLayoutViewport",
        "cssVisualViewport",
        "cssContentSize",
    ] {
        assert!(result.get(key).is_some(), "missing key: {key}");
    }

    let layout = &result["layoutViewport"];
    assert_eq!(layout["clientWidth"].as_f64(), Some(1280.0));
    assert_eq!(layout["clientHeight"].as_f64(), Some(720.0));

    let visual = &result["visualViewport"];
    assert_eq!(visual["scale"].as_f64(), Some(1.0));
    assert_eq!(visual["clientWidth"].as_f64(), Some(1280.0));

    let content = &result["contentSize"];
    assert_eq!(content["width"].as_f64(), Some(1280.0));
    // Without a live page the content height falls back to the viewport.
    assert_eq!(content["height"].as_f64(), Some(720.0));
}

#[tokio::test]
async fn unknown_page_method_still_errors() {
    let mut ctx = CdpContext::new();
    let err = handle("notARealMethod", &json!({}), &mut ctx, &None)
        .await
        .expect_err("unknown methods must surface as errors");
    assert!(err.contains("Unknown Page method"));
}
