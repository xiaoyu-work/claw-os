use super::*;
use std::os::unix::fs::PermissionsExt;

fn fixture(script: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("agent");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    (root, path)
}

#[tokio::test]
async fn authentication_agent_notifies_then_is_reaped() {
    let (_root, path) = fixture(
        "#!/usr/bin/python3\nimport os, signal, sys, time\nassert '--fallback' not in sys.argv\nassert '--notify-fd=1' in sys.argv\nsignal.signal(signal.SIGTERM, lambda *_: sys.exit(0))\nos.close(1)\ntime.sleep(30)\n",
    );
    let mut agent = start_agent(&path).await.unwrap();
    assert!(agent.try_wait().unwrap().is_none());
    stop_agent(&mut agent).await.unwrap();
    assert!(agent.try_wait().unwrap().is_some());
}

#[tokio::test]
async fn authentication_agent_rejects_invalid_notification() {
    let (_root, path) =
        fixture("#!/usr/bin/python3\nimport os, time\nos.write(1, b'not-ready')\ntime.sleep(30)\n");
    assert!(start_agent(&path)
        .await
        .unwrap_err()
        .contains("invalid readiness signal"));
}
