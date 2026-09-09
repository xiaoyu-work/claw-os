from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import time

import pytest

from claw_os_sdk import kernel
from claw_os_sdk.mcp import CallCancelled


def executable(tmp_path: Path, body: str) -> str:
    path = tmp_path / "cos-fixture"
    path.write_text("#!/usr/bin/python3\n" + body, encoding="utf-8")
    path.chmod(0o700)
    return str(path)


def until() -> int:
    return time.time_ns() // 1_000_000 + 5000


def test_fixed_binary_stdin_and_authenticated_environment_are_preserved(tmp_path, monkeypatch):
    binary = executable(tmp_path, """
import json, os, sys
print(json.dumps({"wire_version":1,"ok":True,"data":{
    "args":sys.argv[1:],"input":sys.stdin.read(),"session":os.environ["COS_SESSION"]
}}))
""")
    monkeypatch.setenv("PATH", "/not-the-kernel")
    monkeypatch.setenv("COS_SESSION", "broker-bound-session")
    before = dict(os.environ)
    value = kernel.call_json_with_stdin_binary(
        binary, ["__notifications", "request", "--request-stdin"], "界".encode(),
        deadline_unix_ms=until(),
    )
    assert value == {
        "args": ["--wire=1", "__notifications", "request", "--request-stdin"],
        "input": "界", "session": "broker-bound-session",
    }
    assert dict(os.environ) == before


@pytest.mark.parametrize("response,status,error", [
    ({"wire_version": 1, "ok": False, "code": "denied", "error": "Not granted"}, 1, kernel.KernelDenied),
    ({"wire_version": 1, "ok": True, "data": {}}, 1, kernel.KernelUnavailable),
    ({"wire_version": 2, "ok": True, "data": {}}, 0, kernel.KernelUnavailable),
    ({"ok": True}, 0, kernel.KernelUnavailable),
])
def test_existing_wire_decoder_classifies_failures(tmp_path, response, status, error):
    binary = executable(tmp_path, f"import sys\nprint({json.dumps(response)!r})\nsys.exit({status})\n")
    with pytest.raises(error) as caught:
        kernel.call_json_with_stdin_binary(binary, [], b"x" * 100_000, deadline_unix_ms=until())
    if error is kernel.KernelDenied:
        assert caught.value.payload == response


@pytest.mark.parametrize("cancel", [False, True])
def test_deadline_and_cancellation_kill_and_reap_a_started_child(tmp_path, monkeypatch, cancel):
    binary = executable(tmp_path, "import time\ntime.sleep(30)\n")
    processes = []
    real_popen = subprocess.Popen

    def start(*args, **kwargs):
        process = real_popen(*args, **kwargs)
        processes.append(process)
        return process

    def check():
        if processes:
            raise CallCancelled("cancelled")

    monkeypatch.setattr(kernel.subprocess, "Popen", start)
    with pytest.raises(CallCancelled if cancel else kernel.KernelUnavailable):
        kernel.call_json_with_stdin_binary(
            binary, [], b"{}", deadline_unix_ms=time.time_ns() // 1_000_000 + 150,
            check_cancelled=check if cancel else None,
        )
    assert len(processes) == 1
    assert processes[0].returncode is not None
    with pytest.raises(ChildProcessError):
        os.waitpid(processes[0].pid, os.WNOHANG)


@pytest.mark.parametrize("change", [
    {"binary": "cos"}, {"args": "request"}, {"data": "not bytes"},
    {"data": b"x" * (kernel.MAX_INPUT_BYTES + 1)}, {"deadline_unix_ms": True},
])
def test_invalid_transport_arguments_do_not_launch(monkeypatch, change):
    def forbidden(*args, **kwargs):
        pytest.fail("invalid input launched a process")

    monkeypatch.setattr(kernel.subprocess, "Popen", forbidden)
    arguments = dict(binary="/usr/local/bin/cos", args=[], data=b"{}", deadline_unix_ms=until())
    arguments.update(change)
    with pytest.raises(ValueError):
        kernel.call_json_with_stdin_binary(**arguments)


def test_expired_or_cancelled_request_never_launches(monkeypatch):
    def forbidden(*args, **kwargs):
        pytest.fail("expired request launched a process")

    def cancelled():
        raise CallCancelled("cancelled before launch")

    monkeypatch.setattr(kernel.subprocess, "Popen", forbidden)
    with pytest.raises(kernel.KernelUnavailable):
        kernel.call_json_with_stdin_binary("/usr/local/bin/cos", [], b"{}", deadline_unix_ms=1)
    with pytest.raises(CallCancelled):
        kernel.call_json_with_stdin_binary(
            "/usr/local/bin/cos", [], b"{}", deadline_unix_ms=until(), check_cancelled=cancelled,
        )
