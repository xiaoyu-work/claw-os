"""Comprehensive tests for the cos CLI tool.

Invokes cos as a subprocess (like a real caller would) and validates JSON output.
All filesystem tests use a temporary directory. Network tests are mocked.
"""

import json
import os
import platform
import subprocess
import sys
import textwrap

import pytest

COS = os.path.join(
    os.path.dirname(__file__),
    "..",
    "rootfs",
    "overlay",
    "usr",
    "local",
    "bin",
    "cos",
)
COS = os.path.abspath(COS)


def run_cos(*args, stdin_data=None, cwd=None):
    """Run the cos CLI and return (parsed_json, returncode)."""
    result = subprocess.run(
        [sys.executable, COS, *args],
        capture_output=True,
        text=True,
        input=stdin_data,
        cwd=cwd,
        timeout=30,
    )
    try:
        data = json.loads(result.stdout)
    except json.JSONDecodeError:
        data = None
    return data, result.returncode


# ---------------------------------------------------------------------------
# Usage / unknown command
# ---------------------------------------------------------------------------

class TestUsage:
    def test_no_args_shows_usage(self):
        data, rc = run_cos()
        assert rc == 0
        assert data["name"] == "cos"
        assert "version" in data
        assert isinstance(data["commands"], list)
        assert len(data["commands"]) == 12

    def test_one_arg_shows_usage(self):
        data, rc = run_cos("fs")
        assert rc == 0
        assert "commands" in data

    def test_unknown_command(self):
        data, rc = run_cos("foo", "bar")
        assert rc != 0
        assert "error" in data


# ---------------------------------------------------------------------------
# fs ls
# ---------------------------------------------------------------------------

class TestFsLs:
    def test_ls_current_dir(self, tmp_path):
        (tmp_path / "a.txt").write_text("a")
        (tmp_path / "b.txt").write_text("b")
        data, rc = run_cos("fs", "ls", str(tmp_path))
        assert rc == 0
        assert data["path"] == str(tmp_path)
        assert "a.txt" in data["files"]
        assert "b.txt" in data["files"]

    def test_ls_empty_dir(self, tmp_path):
        data, rc = run_cos("fs", "ls", str(tmp_path))
        assert rc == 0
        assert data["files"] == []

    def test_ls_default_cwd(self, tmp_path):
        (tmp_path / "hello").write_text("x")
        data, rc = run_cos("fs", "ls", cwd=str(tmp_path))
        assert rc == 0
        assert "hello" in data["files"]

    def test_ls_nonexistent(self, tmp_path):
        data, rc = run_cos("fs", "ls", str(tmp_path / "nope"))
        assert rc != 0
        assert "error" in data

    def test_ls_sorted(self, tmp_path):
        for name in ["c", "a", "b"]:
            (tmp_path / name).write_text("")
        data, rc = run_cos("fs", "ls", str(tmp_path))
        assert data["files"] == ["a", "b", "c"]


# ---------------------------------------------------------------------------
# fs pwd
# ---------------------------------------------------------------------------

class TestFsPwd:
    def test_pwd(self, tmp_path):
        data, rc = run_cos("fs", "pwd", cwd=str(tmp_path))
        assert rc == 0
        # Resolve symlinks for macOS /tmp -> /private/tmp etc.
        assert os.path.realpath(data["cwd"]) == os.path.realpath(str(tmp_path))

    def test_pwd_json_format(self):
        data, rc = run_cos("fs", "pwd")
        assert rc == 0
        assert "cwd" in data
        assert isinstance(data["cwd"], str)


# ---------------------------------------------------------------------------
# fs read
# ---------------------------------------------------------------------------

class TestFsRead:
    def test_read_file(self, tmp_path):
        f = tmp_path / "test.txt"
        f.write_text("hello world")
        data, rc = run_cos("fs", "read", str(f))
        assert rc == 0
        assert data["content"] == "hello world"
        assert data["path"] == str(f)

    def test_read_empty_file(self, tmp_path):
        f = tmp_path / "empty.txt"
        f.write_text("")
        data, rc = run_cos("fs", "read", str(f))
        assert rc == 0
        assert data["content"] == ""

    def test_read_missing_file(self, tmp_path):
        data, rc = run_cos("fs", "read", str(tmp_path / "nope.txt"))
        assert rc != 0
        assert "error" in data

    def test_read_no_args(self):
        data, rc = run_cos("fs", "read")
        assert rc != 0
        assert "error" in data
        assert "missing argument" in data["error"]

    def test_read_multiline(self, tmp_path):
        f = tmp_path / "multi.txt"
        f.write_text("line1\nline2\nline3\n")
        data, rc = run_cos("fs", "read", str(f))
        assert rc == 0
        assert data["content"] == "line1\nline2\nline3\n"


# ---------------------------------------------------------------------------
# fs write
# ---------------------------------------------------------------------------

class TestFsWrite:
    def test_write_file(self, tmp_path):
        target = tmp_path / "out.txt"
        data, rc = run_cos("fs", "write", str(target), stdin_data="hello")
        assert rc == 0
        assert data["bytes"] == 5
        assert data["path"] == str(target)
        assert target.read_text() == "hello"

    def test_write_creates_parent_dirs(self, tmp_path):
        target = tmp_path / "a" / "b" / "c.txt"
        data, rc = run_cos("fs", "write", str(target), stdin_data="nested")
        assert rc == 0
        assert target.read_text() == "nested"

    def test_write_empty(self, tmp_path):
        target = tmp_path / "empty.txt"
        data, rc = run_cos("fs", "write", str(target), stdin_data="")
        assert rc == 0
        assert data["bytes"] == 0
        assert target.read_text() == ""

    def test_write_no_args(self):
        data, rc = run_cos("fs", "write", stdin_data="x")
        assert rc != 0
        assert "error" in data

    def test_write_overwrites(self, tmp_path):
        target = tmp_path / "over.txt"
        target.write_text("old")
        data, rc = run_cos("fs", "write", str(target), stdin_data="new")
        assert rc == 0
        assert target.read_text() == "new"


# ---------------------------------------------------------------------------
# fs stat
# ---------------------------------------------------------------------------

class TestFsStat:
    def test_stat_file(self, tmp_path):
        f = tmp_path / "f.txt"
        f.write_text("1234567890")
        data, rc = run_cos("fs", "stat", str(f))
        assert rc == 0
        assert data["size"] == 10
        assert data["is_file"] is True
        assert data["is_dir"] is False
        assert data["path"] == str(f)
        assert "mode" in data
        assert "uid" in data
        assert "gid" in data

    def test_stat_dir(self, tmp_path):
        data, rc = run_cos("fs", "stat", str(tmp_path))
        assert rc == 0
        assert data["is_dir"] is True
        assert data["is_file"] is False

    def test_stat_missing(self, tmp_path):
        data, rc = run_cos("fs", "stat", str(tmp_path / "nope"))
        assert rc != 0
        assert "error" in data

    def test_stat_no_args(self):
        data, rc = run_cos("fs", "stat")
        assert rc != 0
        assert "missing argument" in data["error"]


# ---------------------------------------------------------------------------
# fs rm
# ---------------------------------------------------------------------------

class TestFsRm:
    def test_rm_file(self, tmp_path):
        f = tmp_path / "bye.txt"
        f.write_text("gone")
        data, rc = run_cos("fs", "rm", str(f))
        assert rc == 0
        assert data["removed"] == str(f)
        assert not f.exists()

    def test_rm_directory(self, tmp_path):
        d = tmp_path / "subdir"
        d.mkdir()
        (d / "inner.txt").write_text("x")
        data, rc = run_cos("fs", "rm", str(d))
        assert rc == 0
        assert not d.exists()

    def test_rm_missing(self, tmp_path):
        data, rc = run_cos("fs", "rm", str(tmp_path / "nope"))
        assert rc != 0
        assert "error" in data

    def test_rm_no_args(self):
        data, rc = run_cos("fs", "rm")
        assert rc != 0
        assert "missing argument" in data["error"]


# ---------------------------------------------------------------------------
# fs mkdir
# ---------------------------------------------------------------------------

class TestFsMkdir:
    def test_mkdir(self, tmp_path):
        target = tmp_path / "newdir"
        data, rc = run_cos("fs", "mkdir", str(target))
        assert rc == 0
        assert data["created"] == str(target)
        assert target.is_dir()

    def test_mkdir_nested(self, tmp_path):
        target = tmp_path / "a" / "b" / "c"
        data, rc = run_cos("fs", "mkdir", str(target))
        assert rc == 0
        assert target.is_dir()

    def test_mkdir_existing(self, tmp_path):
        """exist_ok=True means this should succeed."""
        data, rc = run_cos("fs", "mkdir", str(tmp_path))
        assert rc == 0

    def test_mkdir_no_args(self):
        data, rc = run_cos("fs", "mkdir")
        assert rc != 0
        assert "missing argument" in data["error"]


# ---------------------------------------------------------------------------
# exec run
# ---------------------------------------------------------------------------

class TestExecRun:
    def test_run_echo(self):
        data, rc = run_cos("exec", "run", "echo", "hello")
        assert rc == 0
        assert data["exit_code"] == 0
        assert data["stdout"].strip() == "hello"
        assert data["command"] == ["echo", "hello"]

    def test_run_exit_code(self):
        data, rc = run_cos("exec", "run", "false")
        assert rc == 0  # cos itself exits 0; the inner command's code is in JSON
        assert data["exit_code"] != 0

    def test_run_stderr(self):
        data, rc = run_cos("exec", "run", "sh", "-c", "echo err >&2")
        assert rc == 0
        assert "err" in data["stderr"]

    def test_run_not_found(self):
        data, rc = run_cos("exec", "run", "nonexistent_command_xyz_12345")
        assert rc != 0
        assert "error" in data
        assert "not found" in data["error"]

    def test_run_no_args(self):
        data, rc = run_cos("exec", "run")
        assert rc != 0
        assert "error" in data


# ---------------------------------------------------------------------------
# exec which
# ---------------------------------------------------------------------------

class TestExecWhich:
    def test_which_python(self):
        # python3 should always be available if we're running tests
        data, rc = run_cos("exec", "which", "python3")
        assert rc == 0
        assert data["command"] == "python3"
        assert os.path.isabs(data["path"])

    def test_which_not_found(self):
        data, rc = run_cos("exec", "which", "nonexistent_cmd_xyz_99999")
        assert rc != 0
        assert "error" in data
        assert "not found" in data["error"]

    def test_which_no_args(self):
        data, rc = run_cos("exec", "which")
        assert rc != 0
        assert "missing argument" in data["error"]


# ---------------------------------------------------------------------------
# net fetch (mocked)
# ---------------------------------------------------------------------------

class TestNetFetch:
    """Test net fetch by mocking urllib so no network access is needed.

    Since cos runs as a subprocess, we can't mock its imports directly.
    Instead we create a wrapper script that patches urllib before running cos.
    """

    def _run_fetch_with_mock(self, tmp_path, url, mock_status=200, mock_body="ok",
                              raise_http_error=False, raise_generic=False):
        """Create a wrapper that patches urllib and runs the cos fetch logic."""
        wrapper = tmp_path / "fetch_wrapper.py"
        wrapper.write_text(textwrap.dedent(f"""\
            import sys
            import os
            import json
            import unittest.mock

            # Add cos to importable path by running its main
            sys.argv = ["cos", "net", "fetch", "{url}"]

            cos_path = {COS!r}

            # We need to mock urllib.request.urlopen before cos uses it
            import urllib.request
            import urllib.error

            mock_resp = unittest.mock.MagicMock()
            mock_resp.status = {mock_status}
            mock_resp.read.return_value = {mock_body!r}.encode("utf-8")
            mock_resp.__enter__ = lambda s: s
            mock_resp.__exit__ = lambda s, *a: None

            raise_http = {raise_http_error!r}
            raise_generic = {raise_generic!r}

            def fake_urlopen(req, timeout=None):
                if raise_http:
                    raise urllib.error.HTTPError(
                        "{url}", 404, "Not Found", {{}}, None
                    )
                if raise_generic:
                    raise ConnectionError("connection refused")
                return mock_resp

            with unittest.mock.patch("urllib.request.urlopen", side_effect=fake_urlopen):
                # exec the cos script
                with open(cos_path) as f:
                    code = f.read()
                exec(compile(code, cos_path, "exec"), {{"__name__": "__main__"}})
        """))
        result = subprocess.run(
            [sys.executable, str(wrapper)],
            capture_output=True,
            text=True,
            timeout=15,
        )
        try:
            data = json.loads(result.stdout)
        except json.JSONDecodeError:
            data = None
        return data, result.returncode

    def test_fetch_success(self, tmp_path):
        data, rc = self._run_fetch_with_mock(
            tmp_path, "http://example.com", mock_status=200, mock_body="hello"
        )
        assert rc == 0
        assert data["url"] == "http://example.com"
        assert data["status"] == 200
        assert data["body"] == "hello"

    def test_fetch_http_error(self, tmp_path):
        data, rc = self._run_fetch_with_mock(
            tmp_path, "http://example.com/404", raise_http_error=True
        )
        assert rc != 0
        assert data["status"] == 404

    def test_fetch_connection_error(self, tmp_path):
        data, rc = self._run_fetch_with_mock(
            tmp_path, "http://example.com", raise_generic=True
        )
        assert rc != 0
        assert "error" in data
        assert "fetch failed" in data["error"]

    def test_fetch_no_args(self):
        data, rc = run_cos("net", "fetch")
        assert rc != 0
        assert "missing argument" in data["error"]


# ---------------------------------------------------------------------------
# sys info
# ---------------------------------------------------------------------------

class TestSysInfo:
    def test_info_fields(self):
        data, rc = run_cos("sys", "info")
        assert rc == 0
        assert data["name"] == "claw-os"
        assert "version" in data
        assert "platform" in data
        assert "arch" in data
        assert "python" in data
        assert "hostname" in data
        assert "pid" in data
        assert "uid" in data

    def test_info_types(self):
        data, _ = run_cos("sys", "info")
        assert isinstance(data["version"], str)
        assert isinstance(data["pid"], int)
        assert isinstance(data["uid"], int)
        assert isinstance(data["platform"], str)


# ---------------------------------------------------------------------------
# sys env
# ---------------------------------------------------------------------------

class TestSysEnv:
    def test_env_returns_dict(self):
        data, rc = run_cos("sys", "env")
        assert rc == 0
        assert isinstance(data["env"], dict)

    def test_env_contains_path(self):
        data, _ = run_cos("sys", "env")
        assert "PATH" in data["env"]

    def test_env_custom_var(self):
        """Verify a custom env var is visible."""
        result = subprocess.run(
            [sys.executable, COS, "sys", "env"],
            capture_output=True,
            text=True,
            env={**os.environ, "COS_TEST_VAR": "12345"},
            timeout=15,
        )
        data = json.loads(result.stdout)
        assert data["env"]["COS_TEST_VAR"] == "12345"
