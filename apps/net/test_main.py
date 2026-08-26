import io
import os
import pathlib
import stat
import urllib.error
from unittest import mock

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_net_main",
    clear_modules=("_shared",),
)


class _Response:
    def __init__(self, data, *, fail_after_reads=None, on_read=None):
        self._body = io.BytesIO(data)
        self._fail_after_reads = fail_after_reads
        self._on_read = on_read
        self.reads = 0
        self.exited = False

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        self.exited = True

    def read(self, size):
        if self._on_read is not None:
            self._on_read()
        if self._fail_after_reads is not None and self.reads >= self._fail_after_reads:
            raise urllib.error.URLError("connection reset")
        self.reads += 1
        return self._body.read(size)


def _run_download(response, destination):
    with mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main,
        "open_url",
        return_value=(response, "https://example.com/file", []),
    ):
        result = main.cmd_download(
            ["https://example.com/file", "--output", os.fspath(destination)]
        )
    require.assert_called_once_with("fs.write", path=os.path.realpath(destination))
    return result


def _assert_only_destination(directory, destination):
    assert set(directory.iterdir()) == {destination}


def test_network_open_failure_preserves_existing_destination(tmp_path):
    destination = tmp_path / "download.bin"
    destination.write_bytes(b"original")

    with mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main,
        "open_url",
        side_effect=urllib.error.URLError("offline"),
    ), mock.patch.object(main.tempfile, "mkstemp") as mkstemp:
        result = main.cmd_download(
            ["https://example.com/file", "--output", os.fspath(destination)]
        )

    require.assert_called_once_with("fs.write", path=os.path.realpath(destination))
    mkstemp.assert_not_called()
    assert result == {"error": "offline"}
    assert destination.read_bytes() == b"original"
    _assert_only_destination(tmp_path, destination)


def test_midstream_network_failure_removes_temporary_file(tmp_path):
    destination = tmp_path / "download.bin"
    destination.write_bytes(b"original")
    response = _Response(b"partial", fail_after_reads=1)

    result = _run_download(response, destination)

    assert result == {"error": "connection reset"}
    assert destination.read_bytes() == b"original"
    _assert_only_destination(tmp_path, destination)


def test_size_limit_is_hard_error_and_removes_temporary_file(tmp_path):
    destination = tmp_path / "download.bin"
    destination.write_bytes(b"original")
    response = _Response(b"12345")

    with mock.patch.object(main, "MAX_DOWNLOAD_BYTES", 4):
        result = _run_download(response, destination)

    assert result == {
        "error": "download exceeds size limit of 4 bytes",
        "limit": 4,
    }
    assert "path" not in result
    assert "truncated" not in result
    assert destination.read_bytes() == b"original"
    _assert_only_destination(tmp_path, destination)


def test_exact_size_limit_is_successful(tmp_path):
    destination = tmp_path / "download.bin"
    response = _Response(b"1234")

    with mock.patch.object(main, "MAX_DOWNLOAD_BYTES", 4):
        result = _run_download(response, destination)

    assert result["bytes"] == 4
    assert destination.read_bytes() == b"1234"
    _assert_only_destination(tmp_path, destination)


def test_fsync_failure_removes_temporary_file_without_replacing(tmp_path):
    destination = tmp_path / "download.bin"
    destination.write_bytes(b"original")
    response = _Response(b"replacement")

    with mock.patch.object(
        main.os, "fsync", side_effect=OSError("fsync failed")
    ), mock.patch.object(main.os, "replace") as replace:
        result = _run_download(response, destination)

    replace.assert_not_called()
    assert result == {"error": "fsync failed"}
    assert destination.read_bytes() == b"original"
    _assert_only_destination(tmp_path, destination)


def test_replace_failure_removes_temporary_file_and_preserves_destination(tmp_path):
    destination = tmp_path / "download.bin"
    destination.write_bytes(b"original")
    response = _Response(b"replacement")

    with mock.patch.object(
        main.os, "replace", side_effect=OSError("replace failed")
    ):
        result = _run_download(response, destination)

    assert result == {"error": "replace failed"}
    assert destination.read_bytes() == b"original"
    _assert_only_destination(tmp_path, destination)


def test_success_uses_private_same_directory_temp_and_replaces_after_fsync(tmp_path):
    destination = tmp_path / "download.bin"
    destination.write_bytes(b"original")
    observed = {}
    events = []

    def inspect_temp():
        if observed:
            return
        candidates = set(tmp_path.iterdir()) - {destination}
        assert len(candidates) == 1
        staged = candidates.pop()
        observed["path"] = staged
        observed["mode"] = stat.S_IMODE(staged.stat().st_mode)

    response = _Response(b"replacement", on_read=inspect_temp)
    real_fsync = main.os.fsync
    real_replace = main.os.replace

    def require(*_args, **_kwargs):
        events.append("fs.write")

    def open_url(*_args, **_kwargs):
        events.append("open_url")
        return response, "https://example.com/file", []

    def fsync(fd):
        events.append("fsync")
        return real_fsync(fd)

    def replace(source, target):
        events.append("replace")
        assert response.exited
        assert pathlib.Path(source) == observed["path"]
        return real_replace(source, target)

    with mock.patch.object(main.policy, "require", side_effect=require) as required, mock.patch.object(
        main, "open_url", side_effect=open_url
    ), mock.patch.object(main.os, "fsync", side_effect=fsync), mock.patch.object(
        main.os, "replace", side_effect=replace
    ):
        result = main.cmd_download(
            ["https://example.com/file", "--output", os.fspath(destination)]
        )

    required.assert_called_once_with("fs.write", path=os.path.realpath(destination))
    assert events[:2] == ["fs.write", "open_url"]
    assert events.index("fsync") < events.index("replace")
    assert observed["path"].parent == destination.parent
    assert observed["mode"] & 0o077 == 0
    assert result == {
        "url": "https://example.com/file",
        "path": os.path.realpath(destination),
        "bytes": len(b"replacement"),
    }
    assert destination.read_bytes() == b"replacement"
    _assert_only_destination(tmp_path, destination)
