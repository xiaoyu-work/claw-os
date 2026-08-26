use super::*;
use crate::types::CdpRequest;

fn req(method: &str) -> CdpRequest {
    CdpRequest {
        id: 1,
        method: method.into(),
        params: json!({}),
        session_id: None,
    }
}

#[tokio::test]
async fn audits_enable_returns_empty_success() {
    let mut ctx = CdpContext::new();
    let resp = dispatch(&req("Audits.enable"), &mut ctx).await;
    assert!(
        resp.error.is_none(),
        "Audits.enable should not error: {:?}",
        resp.error
    );
    assert_eq!(resp.result, Some(json!({})));
}

#[tokio::test]
async fn unknown_domain_still_errors() {
    let mut ctx = CdpContext::new();
    let resp = dispatch(&req("DefinitelyNotADomain.enable"), &mut ctx).await;
    let err = resp.error.expect("unknown domain must surface as error");
    assert_eq!(err.code, -32601);
    assert!(err.message.contains("Unknown domain"));
}
