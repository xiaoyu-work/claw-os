#!/usr/bin/env python3
"""Verify public App delivery through an actually installed Agent on disposable CI."""

import argparse
import errno
import json
import os
from pathlib import Path
import pty
import secrets
import shutil
import signal
import subprocess
import threading
import time
import urllib.error
import urllib.request


ORIGIN = "http://127.0.0.1:38082"
OWNER = "claw-store-acceptance"
UID = 20001


def request(path, token=None, body=None, bootstrap=False, timeout=60):
    headers = {"Origin": ORIGIN}
    if token:
        headers["Authorization"] = f"{'Bootstrap' if bootstrap else 'Bearer'} {token}"
    if body is not None:
        headers["Content-Type"] = "application/json"
    message = urllib.request.Request(
        ORIGIN + path,
        data=None if body is None else json.dumps(body).encode(),
        headers=headers,
    )
    try:
        with urllib.request.urlopen(message, timeout=timeout) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        raise RuntimeError(
            f"{path}: HTTP {error.code}: {error.read(16384).decode(errors='replace')}"
        ) from error


def confirm(review, allowed_apps):
    actions = {
        "install": "confirm_install",
        "activation": "confirm_activation",
        "update": "confirm_update",
    }
    assert review["status"] == "pending", review
    assert review["subject"]["app_id"] in allowed_apps, review
    action = actions[review["kind"]]
    assert action in review["actions"], review
    decision = {
        "id": review["id"],
        "revision": int(review["revision"]),
        "action": action,
        "choices": [],
    }
    # This disposable CI Root controller confirms only the new synthetic owner.
    # The installed helper still checks the exact displayed revision at Root.
    result = subprocess.run(
        ["/usr/local/bin/claw-approval-helper", "--system-review-json"],
        input=json.dumps(decision), text=True, capture_output=True, check=True,
        env={"PATH": "/usr/local/bin:/usr/bin:/bin", "PKEXEC_UID": str(UID)},
        timeout=30,
    )
    assert json.loads(result.stdout)["status"] == "confirmed", result.stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("node", type=Path)
    args = parser.parse_args()
    assert os.geteuid() == 0
    assert os.environ.get("GITHUB_ACTIONS") == "true"
    assert os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted"
    os.umask(0o077)
    subprocess.run(
        ["useradd", "--create-home", "--uid", str(UID), "--shell", "/bin/sh", OWNER],
        check=True,
    )
    home = Path("/home") / OWNER
    owner_env = {
        "HOME": str(home), "USER": OWNER, "LOGNAME": OWNER,
        "PATH": "/usr/local/bin:/usr/bin:/bin", "LANG": "C.UTF-8",
    }
    gid = home.stat().st_gid

    def owner_identity():
        os.setgroups([])
        os.setgid(gid)
        os.setuid(UID)

    bootstrap = secrets.token_hex(32)
    master, slave = pty.openpty()
    web = subprocess.Popen(
        ["/usr/local/bin/cos", "agent", "serve", "--bind", "127.0.0.1",
         "--port", "38082", "--token", bootstrap],
        cwd=home, env=owner_env, stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL, stderr=slave, preexec_fn=owner_identity,
        start_new_session=True,
    )
    os.close(slave)

    def drain():
        try:
            while os.read(master, 65536):
                pass
        except OSError as error:
            if error.errno != errno.EIO:
                raise

    # Preserve ordinary TTY bootstrap without recording the printed bootstrap URL.
    reader = threading.Thread(target=drain, daemon=True)
    reader.start()
    try:
        deadline = time.monotonic() + 20
        while True:
            assert web.poll() is None, "installed Agent Web exited before readiness"
            try:
                with urllib.request.urlopen(ORIGIN, timeout=1):
                    break
            except urllib.error.URLError:
                if time.monotonic() >= deadline:
                    raise RuntimeError("installed Agent Web did not become ready")
                time.sleep(0.1)
        token = request(
            "/api/auth/token", bootstrap, {"ttl_seconds": 3600}, bootstrap=True,
        )["access_token"]
        meta = request("/api/meta", token)
        assert meta["owner_uid"] == UID, meta
        assert meta["version"] == os.environ["COS_PACKAGE_VERSION"], meta
        assert not any(app["id"] == "kv" for app in request("/api/apps", token)["apps"])
        store = request("/api/app-store", token)
        catalog = store["catalog"]
        assert catalog["repository"] == "xiaoyu-work/clawos-app", catalog["repository"]
        candidates = [
            entry for entry in catalog["apps"] if entry["app_id"] == "kv"
            and entry["architecture"] in ("all", store["architecture"])
        ]
        assert len(candidates) == 1, "official catalog must select one compatible KV"
        selected = candidates[0]
        package = next(
            item for item in catalog["packages"]
            if item["sha256"] == selected["delivery"]["package_sha256"]
        )
        prepared = request(
            "/api/apps/kv/install", token, {"sha256": selected["artifact"]["sha256"]},
            timeout=900,
        )
        assert prepared["delivery"] == "apt" and prepared["status"] == "review_required"
        assert prepared["reviews"], "official APT plan omitted its App reviews"
        for review in prepared["reviews"]:
            displayed = request(f"/api/app-reviews/{review['id']}", token)["review"]
            assert displayed["revision"] == review["revision"]
            confirm(displayed, set(package["apps"]))
        installed = request(
            f"/api/app-installs/{prepared['request_id']}/commit", token, {}, timeout=900,
        )
        assert installed["status"] == "installed", installed
        status = subprocess.check_output(
            ["dpkg-query", "-W", "-f=${db:Status-Status}\\t${Version}", package["package"]],
            text=True,
        )
        assert status.split("\t") == ["installed", package["version"]], status
        inventory = request("/api/apps", token)
        assert any(app["id"] == "kv" and app["web_ready"] for app in inventory["apps"])
        opened = request("/api/apps/kv/open", token, {})
        if opened["status"] == "review_required":
            confirm(opened["review"], {"kv"})
            opened = request("/api/apps/kv/open", token, {})
        assert opened["status"] == "ready", opened
        view = opened["view_id"]
        value = f"Official signed App release {selected['release_version']} without a model"
        for tool, arguments in [
            ("kv.set", {"key": "web-proof", "value": value}),
            ("kv.get", {"key": "web-proof"}),
        ]:
            result = request(
                "/api/apps/kv/call", token,
                {"view_id": view, "tool": tool, "arguments": arguments},
            )["result"]
            assert result.get("isError") is not True, result
            if tool == "kv.get":
                assert result["content"][0]["text"] == value, result
        token_file = home / "browser.token"
        token_file.write_text(token)
        os.chown(token_file, UID, gid)
        driver = home / "app-store.browser.mjs"
        shutil.copyfile(
            Path(__file__).resolve().parents[2]
            / "core/src/agent/web/ui/test/app-store.browser.mjs",
            driver,
        )
        os.chown(driver, UID, gid)
        try:
            subprocess.run(
                [str(args.node), "--experimental-websocket", str(driver)],
                cwd=home, preexec_fn=owner_identity, check=True, timeout=120,
                env={
                    **owner_env, "COS_APP_APT_PROCESS": "1",
                    "APP_STORE_INSTALLED_ORIGIN": ORIGIN,
                    "APP_STORE_INSTALLED_TOKEN_FILE": str(token_file),
                    "APP_STORE_INSTALLED_VALUE": value,
                },
            )
        finally:
            token_file.unlink()
        request(f"/api/app-views/{view}/close", token, {})
        print(json.dumps({
            "status": "verified", "catalog_generation": catalog["generation"],
            "app_release": selected["release_version"], "package": package["package"],
            "version": package["version"], "architecture": store["architecture"],
            "delivery": "official signed APT, protected review, installed Web and Root MCP",
        }))
    finally:
        if web.poll() is None:
            web.send_signal(signal.SIGINT)
        try:
            status = web.wait(timeout=15)
            assert status == 0, f"installed Agent Web exited with status {status}"
        except subprocess.TimeoutExpired:
            web.kill()
            web.wait(timeout=5)
            raise RuntimeError("installed Agent Web did not stop after SIGINT")
        finally:
            reader.join(timeout=2)
            os.close(master)


if __name__ == "__main__":
    main()
