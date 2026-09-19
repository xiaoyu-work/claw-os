use super::*;

#[test]
fn accepts_upstream_envelopes_and_correlates_typed_ids() {
    for (wire, expected) in [
        (
            r#"{"id":"init","method":"initialize","params":{}}"#,
            RequestId::String("init".into()),
        ),
        (
            r#"{"jsonrpc":"2.0","id":17,"method":"model/list"}"#,
            RequestId::Integer(17),
        ),
    ] {
        match decode(wire).unwrap() {
            Incoming::Request { id, .. } => assert_eq!(id, expected),
            _ => panic!("not a request"),
        }
    }
    match decode(r#"{"id":-5,"result":{"ok":true}}"#).unwrap() {
        Incoming::Response {
            id,
            result: Ok(value),
        } => {
            assert_eq!(id, RequestId::Integer(-5));
            assert_eq!(value["ok"], true);
        }
        _ => panic!("not a response"),
    }
}

#[test]
fn refuses_ambiguous_or_unbounded_protocol_shapes() {
    for wire in [
        r#"[]"#,
        r#"{"id":null,"method":"x"}"#,
        r#"{"id":1.1,"method":"x"}"#,
        r#"{"id":1,"method":"x","result":{}}"#,
        r#"{"id":1,"result":{},"error":{}}"#,
        r#"{"jsonrpc":"1.0","id":1,"method":"x"}"#,
        r#"{"id":1,"method":"x","trace":{"traceparent":7}}"#,
        r#"{"id":1,"method":"x","extra":true}"#,
    ] {
        assert_eq!(decode(wire).unwrap_err().1.code, -32600, "{wire}");
    }
    assert_eq!(decode("{").unwrap_err().1.code, -32700);
    let long = json!({ "id": "x".repeat(257), "method": "initialize" }).to_string();
    assert_eq!(decode(&long).unwrap_err().1.code, -32600);
}

#[test]
fn notifications_and_server_responses_are_distinct() {
    assert!(matches!(
        decode(r#"{"method":"initialized"}"#).unwrap(),
        Incoming::Notification { .. }
    ));
    let error = error_response(
        Some(&RequestId::Integer(3)),
        RpcError::unsupported("review/start"),
    );
    assert_eq!(error["id"], 3);
    assert!(error.get("result").is_none());
    assert_eq!(error["error"]["code"], -32601);
}

#[test]
fn rejects_unknown_parameters_and_noninteger_limits() {
    assert!(Params::new(json!({ "unknown": null }), &[]).is_err());
    for limit in [json!(0), json!(101), json!("4"), json!(1.1), json!(-1)] {
        let params = Params::new(json!({ "limit": limit }), &["limit"]).unwrap();
        assert!(params.limit(25).is_err());
    }
}
