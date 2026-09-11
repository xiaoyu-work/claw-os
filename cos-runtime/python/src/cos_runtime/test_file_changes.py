import base64
import hashlib
import json
import subprocess
import sys
from unittest import mock

import pytest

from cos_runtime import file_changes
from cos_runtime.policy import PolicyUnavailable


PATH = "/home/owner/document.txt"
CONTENT = b"private-proposal-\x00\xff\n"


def state(content=CONTENT):
    return {
        "sha256": "sha256:" + hashlib.sha256(content).hexdigest(),
        "size": len(content),
        "device": 1,
        "inode": 2,
        "mode": 0o100600,
        "modified_ns": -1,
        "changed_ns": 1,
    }


def result(path=PATH, content=CONTENT, changed=True):
    return {
        "path": path,
        "bytes": len(content),
        "sha256": "sha256:" + hashlib.sha256(content).hexdigest(),
        "changed": changed,
    }


@pytest.fixture
def transport():
    with mock.patch.object(file_changes, "_cos_binary", return_value="/usr/local/bin/cos"):
        with mock.patch.object(file_changes.subprocess, "run") as run:
            run.return_value = subprocess.CompletedProcess([], 0, json.dumps(result()), "")
            yield run


def test_file_replace_uses_only_fixed_bridge_args_and_json_stdin(transport):
    assert file_changes.replace_file(PATH, state(), CONTENT) == result()
    args, kwargs = transport.call_args
    assert args == (["/usr/local/bin/cos", "__file", "replace"],)
    payload = json.loads(kwargs["input"])
    assert set(payload) == {"path", "expected", "content_base64"}
    assert payload["expected"] == state()
    assert base64.b64decode(payload["content_base64"]) == CONTENT
    assert "env" not in kwargs
    assert "shell" not in kwargs
    assert kwargs["timeout"] == 60
    assert kwargs["encoding"] == "utf-8"
    assert "private-proposal" not in repr(args)


def test_file_replace_sends_explicit_null_and_full_binary_content(transport):
    content = bytes(range(256)) * 256
    transport.return_value.stdout = json.dumps(result(content=content))
    file_changes.replace_file(PATH, None, content)
    payload = json.loads(transport.call_args.kwargs["input"])
    assert "expected" in payload and payload["expected"] is None
    assert base64.b64decode(payload["content_base64"]) == content
    assert len(transport.call_args.kwargs["input"].encode()) <= 128 * 1024


@pytest.mark.parametrize("path", [
    "", "relative", "/", "/srv/../secret", "/srv/./file", "/srv//file",
    "/srv/file/", "/srv/*.txt", "/srv/[ab]", "/srv/x>y", "/srv/x|y",
    "/srv/x\n", "/srv/x\x00", "/srv/$HOME", "/" + "a" * 4096, 1,
    "/srv/\ud800",
])
def test_file_replace_rejects_nonliteral_or_unbounded_paths_before_transport(transport, path):
    with pytest.raises(file_changes.FileChangeError) as error:
        file_changes.replace_file(path, None, CONTENT)
    assert error.value.indeterminate is False
    transport.assert_not_called()


@pytest.mark.parametrize("content", ["not bytes", bytearray(b"a"), b"x" * 65537])
def test_file_replace_rejects_invalid_content_before_transport(transport, content):
    with pytest.raises(file_changes.FileChangeError) as error:
        file_changes.replace_file(PATH, None, content)
    assert error.value.code == "invalid_request"
    assert error.value.indeterminate is False
    transport.assert_not_called()


@pytest.mark.parametrize("key,value", [
    ("size", 65537), ("size", -1), ("size", True), ("mode", 1 << 32),
    ("inode", 1 << 64), ("device", -1), ("modified_ns", 1 << 63),
    ("changed_ns", -(1 << 63) - 1), ("changed_ns", 1.5),
    ("sha256", "A" * 64), ("sha256", "sha256:" + "A" * 64),
])
def test_file_replace_checks_fingerprint_types_and_bounds(transport, key, value):
    expected = state()
    expected[key] = value
    with pytest.raises(file_changes.FileChangeError) as error:
        file_changes.replace_file(PATH, expected, CONTENT)
    assert error.value.indeterminate is False
    transport.assert_not_called()


def test_file_replace_fingerprint_is_closed_without_namespace_uid_gid(transport):
    for expected in [{}, [], {**state(), "uid": 0}, {**state(), "gid": 0}]:
        with pytest.raises(file_changes.FileChangeError):
            file_changes.replace_file(PATH, expected, CONTENT)
    transport.assert_not_called()


@pytest.mark.parametrize("failure", [
    FileNotFoundError("cos missing"),
    OSError("transport lost"),
    subprocess.TimeoutExpired(["cos", "__file", "replace"], 60),
    UnicodeError("invalid output"),
])
def test_file_replace_transport_failure_is_uncertain_and_never_retried(transport, failure, tmp_path):
    path = tmp_path / "document"
    path.write_bytes(b"original")
    transport.side_effect = failure
    with pytest.raises(file_changes.FileChangeError) as error:
        file_changes.replace_file(str(path), state(b"original"), CONTENT)
    assert error.value.indeterminate is True
    assert path.read_bytes() == b"original"
    transport.assert_called_once()


@pytest.mark.parametrize("code", ["indeterminate", "not_authorized", "execution_failed", "unavailable"])
def test_file_replace_does_not_infer_no_effect_from_a_flattened_error(transport, code):
    transport.return_value = subprocess.CompletedProcess(
        [], 1, json.dumps({"error": "definitely no effects", "code": code, "indeterminate": False}), "",
    )
    with pytest.raises(file_changes.FileChangeError) as error:
        file_changes.replace_file(PATH, None, CONTENT)
    assert error.value.code == code
    assert error.value.indeterminate is True
    assert str(error.value) == "definitely no effects"
    transport.assert_called_once()


@pytest.mark.parametrize("output", [
    "", "not json", "null", "[]", "{}", "x" * (128 * 1024 + 1),
    "[" * 2000 + "0" + "]" * 2000,
])
def test_file_replace_malformed_or_oversized_response_stays_uncertain(transport, output):
    transport.return_value.stdout = output
    with pytest.raises(file_changes.FileChangeError) as error:
        file_changes.replace_file(PATH, None, CONTENT)
    assert error.value.indeterminate is True
    transport.assert_called_once()


@pytest.mark.parametrize("field,value", [
    ("path", "/other/path"), ("bytes", True), ("bytes", 10000),
    ("bytes", float(len(CONTENT))), ("sha256", "sha256:" + "0" * 64),
    ("changed", "true"), ("extra", "undeclared"),
])
def test_file_replace_response_is_closed_and_matches_the_request(transport, field, value):
    invalid = result()
    invalid[field] = value
    transport.return_value.stdout = json.dumps(invalid)
    with pytest.raises(file_changes.FileChangeError) as error:
        file_changes.replace_file(PATH, None, CONTENT)
    assert error.value.indeterminate is True


def test_file_replace_unchanged_is_valid_only_for_the_same_existing_content(transport):
    transport.return_value.stdout = json.dumps(result(changed=False))
    assert file_changes.replace_file(PATH, state(), CONTENT)["changed"] is False
    for expected in [None, state(b"different"), {**state(), "size": len(CONTENT) + 1}]:
        with pytest.raises(file_changes.FileChangeError):
            file_changes.replace_file(PATH, expected, CONTENT)


def test_file_replace_discovery_failure_never_invokes_or_writes(transport):
    with mock.patch.object(file_changes, "_cos_binary", side_effect=PolicyUnavailable("missing")):
        with pytest.raises(file_changes.FileChangeError) as error:
            file_changes.replace_file(PATH, None, CONTENT)
    assert error.value.indeterminate is False
    transport.assert_not_called()


def test_file_replace_real_subprocess_uses_stdin_without_a_direct_write_fallback(tmp_path, monkeypatch):
    binary = tmp_path / "cos"
    binary.write_text(
        f"#!{sys.executable}\n"
        "import base64, hashlib, json, os, sys\n"
        "assert sys.argv[1:] == ['__file', 'replace']\n"
        "assert os.environ.get('COS_SESSION') == 'transport-test-session'\n"
        "raw = sys.stdin.buffer.read(128 * 1024 + 1)\n"
        "assert len(raw) <= 128 * 1024\n"
        "body = json.loads(raw)\n"
        "assert set(body) == {'path', 'expected', 'content_base64'}\n"
        "data = base64.b64decode(body['content_base64'], validate=True)\n"
        "assert 'private-proposal' not in repr(sys.argv)\n"
        "assert 'private-proposal' not in repr(dict(os.environ))\n"
        "print(json.dumps({'path': body['path'], 'bytes': len(data), "
        "'sha256': 'sha256:' + hashlib.sha256(data).hexdigest(), 'changed': True}))\n",
        encoding="utf-8",
    )
    binary.chmod(0o700)
    monkeypatch.setenv("COS_BIN", str(binary))
    monkeypatch.setenv("COS_SESSION", "transport-test-session")
    target = tmp_path / "document"
    assert file_changes.replace_file(str(target), None, CONTENT) == result(path=str(target))
    assert not target.exists(), "the helper must not write around a broker transport"
    binary.write_text(f"#!{sys.executable}\nimport sys\nsys.exit(7)\n", encoding="utf-8")
    with pytest.raises(file_changes.FileChangeError):
        file_changes.replace_file(str(target), None, CONTENT)
    assert not target.exists()
