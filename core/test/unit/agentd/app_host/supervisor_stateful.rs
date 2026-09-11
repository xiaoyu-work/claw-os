use super::*;

pub(super) const APP_SOURCE: &str = r#"
import json
import os
from pathlib import Path
import socket
import threading
import time
from claw_os_sdk.serve import App
from cos_runtime import file_changes, policy

app = App()
data = Path(os.environ["COS_DATA_DIR"])
initializations = data / "initializations"
initializations.write_text(str(int(initializations.read_text()) + 1 if initializations.exists() else 1))
count = 0
first_path = None

def publish(name, value):
    stage = data / (name + ".new")
    stage.write_text(json.dumps(value))
    os.replace(stage, data / name)

def probe_idle():
    deadline = time.monotonic() + 60
    while not (data / "between-calls").exists():
        if time.monotonic() > deadline:
            return
        time.sleep(0.02)
    decision = policy.check("fs.write", path=first_path)
    try:
        file_changes.replace_file(first_path, None, b"must not execute")
    except file_changes.FileChangeError as error:
        denied = str(error)
    else:
        raise AssertionError("idle App mutated a host file")
    publish("at-rest.json", {"decision":decision["decision"], "replacement_denied":denied})

threading.Thread(target=probe_idle, daemon=True).start()

@app.tool(
    "host.replace", summary="Replace one explicitly authorized file",
    args={
        "path":{"type":"string"}, "other":{"type":"string"},
        "expected":{"type":"string"}, "body":{"type":"string"},
    },
    required=["path", "other", "expected", "body"],
)
def replace(path, other, expected, body):
    global count, first_path
    policy.require("fs.read", path=path)
    policy.require("fs.write", path=path)
    assert policy.check("fs.write", path=other)["decision"] == "deny"
    assert not Path(path).exists() and not Path(other).exists()
    try:
        socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    except OSError:
        network_denied = True
    else:
        raise AssertionError("persistent App acquired direct networking")
    replacement = file_changes.replace_file(path, json.loads(expected), body.encode())
    count += 1
    if count == 1:
        first_path = path
    (data / "counter").write_text(str(count))
    facts = {
        "invocations":count, "replacement":replacement,
        "session_id":os.environ["COS_SESSION"],
        "app_pid":os.getpid(), "app_uid":os.getuid(), "app_gid":os.getgid(),
        "mount_namespace":os.readlink("/proc/self/ns/mnt"),
        "network_denied":network_denied,
        "host_target_visible":Path(path).exists(),
    }
    publish("ready-" + str(count) + ".json", facts)
    deadline = time.monotonic() + 30
    while not (data / ("release-" + str(count))).exists():
        if time.monotonic() > deadline:
            raise RuntimeError("root never inspected the active call")
        time.sleep(0.02)
    return facts

app.serve()
"#;

pub(super) fn manifest() -> Value {
    json!({
        "id":APP_ID,"version":"0.1.0","name":"Controlled stateful App","runtime":"python",
        "operations":{},
        "session":{"entry":"server.py","transport":"stdio","tools":[{
            "name":support::SESSION_TOOL,"summary":"Replace one explicitly authorized file",
            "args":[
                {"name":"path","kind":"path","required":true},
                {"name":"other","kind":"path","required":true},
                {"name":"expected","kind":"text","required":true},
                {"name":"body","kind":"text","required":true},
            ],
            "needs":[
                {"verb":"fs.read","scope":{"kind":"from-arg","arg":"path"},"why":"read baseline"},
                {"verb":"fs.write","scope":{"kind":"from-arg","arg":"path"},"why":"replace target"},
            ],
        }]},
    })
}

pub(super) fn file_state(path: &Path) -> Value {
    let bytes = std::fs::read(path).unwrap();
    let metadata = std::fs::metadata(path).unwrap();
    json!({
        "sha256":format!("sha256:{}", crate::crypto::sha256_hex(&bytes)),
        "size":metadata.len(),"device":metadata.dev(),"inode":metadata.ino(),
        "mode":metadata.mode(),
        "modified_ns":metadata.mtime() * 1_000_000_000 + metadata.mtime_nsec(),
        "changed_ns":metadata.ctime() * 1_000_000_000 + metadata.ctime_nsec(),
    })
}

async fn wait_for(path: &Path) {
    tokio::time::timeout(Duration::from_secs(40), async {
        while !path.is_file() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("missing App process checkpoint {}", path.display()));
}

async fn read_checkpoint(path: &Path) -> Value {
    wait_for(path).await;
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

pub(super) async fn inspect(
    context: &ProcessContext,
    parent: &str,
    package: &PackageRef,
    worker_pid: u32,
) -> (String, ProcessIdentity, Value) {
    let mut calls = Vec::new();
    let mut first_process = None;
    let mut first_session = None;
    let mut at_rest = Value::Null;
    for (index, (path, other)) in [
        (context.input(), context.second_input()),
        (context.second_input(), context.input()),
    ]
    .into_iter()
    .enumerate()
    {
        let number = index + 1;
        let facts = read_checkpoint(&context.app_data().join(format!("ready-{number}.json"))).await;
        let id = facts["session_id"].as_str().unwrap();
        let instance = crate::provenance::runtime::instance_for(context.uid, id)
            .unwrap()
            .unwrap();
        assert_eq!(instance.class, InstanceClass::App);
        assert_eq!(instance.package.as_ref(), Some(package));
        let process = instance.process.unwrap();
        assert!(process.still_matches());
        assert_ne!(process.pid, worker_pid);
        if let Some(first) = &first_process {
            assert_eq!(first, &process);
            assert_eq!(first_session.as_deref(), Some(id));
        } else {
            first_process = Some(process.clone());
            first_session = Some(id.to_string());
        }
        let row = owner_row(context.uid, &context.home(), id).await.unwrap();
        assert_eq!(row.parent.as_deref(), Some(parent));
        assert_eq!(row.pid, process.pid);
        assert_eq!(row.app_id.as_deref(), Some(APP_ID));
        let base = row.caps.unwrap();
        assert_eq!(base.len(), 1);
        assert!(base.covers(&Cap::new(Verb::AGENT_INVOKE, Scope::name(APP_ID))));
        let transient = row.transient_caps.unwrap();
        assert_eq!(transient.len(), 2);
        assert!(transient.covers(&Cap::new(
            Verb::FS_WRITE,
            Scope::path(path.to_string_lossy())
        )));
        assert!(!transient.covers(&Cap::new(
            Verb::FS_WRITE,
            Scope::path(other.to_string_lossy())
        )));
        let grant = crate::clawd::authority::authority()
            .resolve_session(
                id,
                &crate::clawd::authority::Presentation::new(
                    context.uid,
                    process.pid,
                    process.start_time_ticks,
                    crate::clawd::authority::Audience::SystemService,
                    "test.stateful",
                ),
            )
            .unwrap();
        assert_eq!(grant.caps.len(), 3);
        assert!(grant.expires_in <= Duration::from_secs(75));
        assert_eq!(facts["invocations"], number);
        assert_eq!(facts["app_uid"], context.uid);
        assert_eq!(facts["app_gid"], context.gid);
        assert_eq!(facts["network_denied"], true);
        assert_eq!(facts["host_target_visible"], false);
        assert_ne!(
            facts["mount_namespace"].as_str(),
            Some(context.mount_namespace.as_str())
        );
        write_public(
            &context.app_data().join(format!("release-{number}")),
            b"continue",
        );
        if index == 0 {
            wait_for(&context.app_data().join("between-calls")).await;
            let row = owner_row(context.uid, &context.home(), id).await.unwrap();
            assert!(row.transient_caps.is_none());
            let grant = crate::clawd::authority::authority()
                .resolve_session(
                    id,
                    &crate::clawd::authority::Presentation::new(
                        context.uid,
                        process.pid,
                        process.start_time_ticks,
                        crate::clawd::authority::Audience::SystemService,
                        "test.stateful.idle",
                    ),
                )
                .unwrap();
            assert_eq!(grant.caps.len(), 1);
            at_rest = read_checkpoint(&context.app_data().join("at-rest.json")).await;
            assert_eq!(at_rest["decision"], "deny");
            assert!(!at_rest["replacement_denied"].as_str().unwrap().is_empty());
            assert_eq!(
                std::fs::read_to_string(context.input()).unwrap(),
                "session call 1\n"
            );
            write_public(&context.app_data().join("continue"), b"continue");
        }
        calls.push(facts);
    }
    (
        first_session.unwrap(),
        first_process.unwrap(),
        json!({"calls":calls,"at_rest":at_rest}),
    )
}

pub(super) fn assert_finished(
    context: &ProcessContext,
    result: &Value,
    receipts: &[crate::activities::ActivityReceipt],
) {
    assert_eq!(
        std::fs::read_to_string(context.app_data().join("initializations")).unwrap(),
        "1"
    );
    for (index, path) in [context.input(), context.second_input()]
        .into_iter()
        .enumerate()
    {
        let body = format!("session call {}\n", index + 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        assert_eq!(std::fs::metadata(&path).unwrap().uid(), 0);
        let receipt = receipts
            .iter()
            .find(|receipt| result["receipt_ids"][index].as_str() == Some(receipt.id.as_str()))
            .unwrap();
        let raw = result["output"]["raw_outputs"][index].as_str().unwrap();
        let summary = receipt.report.result.as_ref().unwrap();
        assert_eq!(summary.bytes, raw.len() as u64);
        assert_eq!(
            summary.sha256,
            format!("sha256:{}", crate::crypto::sha256_hex(raw.as_bytes()))
        );
        assert_eq!(receipt.source, ReceiptSource::CallerReported);
        assert_eq!(receipt.report.outcome, ReceiptOutcome::Returned);
        assert_eq!(
            receipt.report.operation,
            crate::activities::ReceiptReport::session_operation(support::SESSION_TOOL)
        );
        assert!(receipt.declaration.as_ref().unwrap().effects.is_empty());
        assert!(receipt.declaration_error.is_none());
        let replacement = &result["output"]["calls"][index]["replacement"];
        assert_eq!(replacement["changed"], true);
        assert_eq!(replacement["bytes"], body.len());
        assert_eq!(
            replacement["sha256"],
            format!("sha256:{}", crate::crypto::sha256_hex(body.as_bytes()))
        );
    }
}
