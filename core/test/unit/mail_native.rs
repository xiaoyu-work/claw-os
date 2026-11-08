use super::*;

#[test]
fn ai_output_cannot_inject_mailbox_commands_or_complete_another_request() {
    let mut pending = HashSet::from(["ui-request".to_string()]);
    assert!(validate_ai_response(
        &json!({
            "id":"ui-request", "ok":true, "type":"mailbox.request",
            "verb":"send", "args":{},
        }),
        &mut pending
    )
    .is_err());
    assert!(pending.contains("ui-request"));
    assert!(validate_ai_response(
        &json!({
            "id":"mail-request", "ok":true, "result":{},
        }),
        &mut pending
    )
    .is_err());
    validate_ai_response(
        &json!({
            "id":"ui-request", "ok":true, "result":{"text":"safe AI reply"},
        }),
        &mut pending,
    )
    .unwrap();
    assert!(pending.is_empty());
}

#[test]
fn native_requests_cannot_smuggle_a_mailbox_type_into_the_ai_lane() {
    let request = json!({"id":"ui-request","verb":"summarize","args":{"body":"mail text"}});
    validate_request(&request).unwrap();
    let mut injected = request.clone();
    injected["type"] = json!("mailbox.request");
    assert!(validate_request(&injected).is_err());
}

#[test]
#[ignore = "requires root solely to represent the authenticated broker peer"]
fn root_private_native_gateway_preserves_ai_and_mailbox_lanes() {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let path = root.path().join("mail.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let (mut native, frontend) = UnixStream::pair().unwrap();
    let (mut ai, app) = UnixStream::pair().unwrap();
    let worker = std::thread::spawn(move || {
        let mut input = frontend.try_clone().unwrap();
        let mut output = frontend;
        let mut ai_input = app.try_clone().unwrap();
        let mut ai_output = app;
        serve_on(
            &listener,
            &mut input,
            &mut output,
            &mut ai_input,
            &mut ai_output,
        )
    });
    write_frame(
        &mut native,
        &json!({"type":"mailbox.ready","version":1}),
        MAX_HOST_FRAME,
    )
    .unwrap();
    write_frame(
        &mut native,
        &json!({
            "id":"ui-request","verb":"summarize","args":{"body":"synthetic mail"},
        }),
        MAX_HOST_FRAME,
    )
    .unwrap();
    let request = read_frame(&mut ai, MAX_AI_FRAME).unwrap().unwrap();
    assert_eq!(request["id"], "ui-request");
    let mut broker = UnixStream::connect(&path).unwrap();
    write_frame(
        &mut broker,
        &json!({
            "id":"mail-request","verb":"accounts","args":{},
        }),
        MAX_HOST_FRAME,
    )
    .unwrap();
    let request = read_frame(&mut native, MAX_HOST_FRAME).unwrap().unwrap();
    assert_eq!(request["type"], "mailbox.request");
    assert_eq!(request["id"], "mail-request");
    write_frame(
        &mut native,
        &json!({
            "type":"mailbox.response","id":"mail-request","ok":true,
            "result":{"accounts":[],"backend":"thunderbird"},
        }),
        MAX_MAIL_FRAME,
    )
    .unwrap();
    let response = read_frame(&mut broker, MAX_MAIL_FRAME).unwrap().unwrap();
    assert_eq!(response["dispatched"], true);
    assert_eq!(response["result"]["backend"], "thunderbird");
    write_frame(
        &mut ai,
        &json!({
            "id":"ui-request","ok":true,"result":{"summary":"AI-only response"},
        }),
        MAX_AI_FRAME,
    )
    .unwrap();
    let response = read_frame(&mut native, MAX_HOST_FRAME).unwrap().unwrap();
    assert_eq!(response["id"], "ui-request");
    assert!(response.get("type").is_none());
    drop(native);
    worker.join().unwrap().unwrap();
}
