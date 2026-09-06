"""Typed filesystem behavior, authority and manifest-bound MCP regressions."""

import base64
import json
import os
from pathlib import Path
import subprocess
import sys
from decimal import Decimal
from unittest import mock

import pytest

from claw_os_sdk.generated import encode_wire_json
from claw_os_sdk.mcp import MAX_LINE_BYTES
from test_support import authenticated_mcp_params, load_local_module


main = load_local_module(
    Path(__file__).with_name("main.py"),
    "claw_test_fs_main",
    clear_modules=("_shared",),
)
MANIFEST = json.loads(Path(__file__).with_name("app.json").read_text())
REAL_SNAPSHOT = main.snapshot.snapshot


@pytest.fixture(autouse=True)
def authority():
    with mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.snapshot, "snapshot"
    ) as snapshot, mock.patch.object(main.snapshot, "snapshot_pair") as pair:
        yield require, snapshot, pair


@pytest.fixture
def file(tmp_path):
    path = tmp_path / "file.txt"
    path.write_bytes(b"hello world")
    return str(path)


@pytest.fixture
def server(monkeypatch):
    monkeypatch.setenv("COS_APP_MANIFEST", str(Path(__file__).with_name("app.json")))
    with mock.patch.dict(sys.modules, {"main": main}):
        return load_local_module(
            Path(__file__).with_name("server.py"), "claw_test_fs_server"
        )


def call(server, name, arguments):
    return server.app._handle_request(
        "tools/call",
        authenticated_mcp_params({"name": f"fs.{name}", "arguments": arguments}),
        True,
    )


@pytest.mark.parametrize("reader", [main.read, main.read_bytes])
@pytest.mark.parametrize(
    ("offset", "limit", "expected", "truncated"),
    [(0, 20, b"hello world", False), (6, 20, b"world", False),
     (2, 4, b"llo ", True), (50, 4, b"", False)],
)
def test_bounded_reads(file, reader, offset, limit, expected, truncated):
    result = reader(file, offset=offset, limit=limit)
    content = (
        base64.b64decode(result["base64"])
        if reader == main.read_bytes else result["content"].encode()
    )
    assert content == expected
    assert result.get("truncated", False) is truncated
    if reader == main.read_bytes:
        assert result["offset"] == offset
        assert result["bytes_returned"] == len(expected)
        assert result["total_size"] == 11


@pytest.mark.parametrize("reader,cap", [
    (main.read, main.MAX_READ_BYTES),
    (main.read_bytes, main.MAX_READ_BYTES_BINARY),
])
@pytest.mark.parametrize("extra", [0, 500])
def test_read_hard_caps(tmp_path, reader, cap, extra):
    path = tmp_path / "large"
    path.write_bytes(b"x" * (cap + extra))
    result = reader(str(path), limit=cap + 50000)
    content = base64.b64decode(result["base64"]) if "base64" in result else result["content"]
    assert len(content) == cap
    assert result.get("truncated", False) is bool(extra)
    if extra:
        assert result["total_size"] == cap + extra


@pytest.mark.parametrize("data,expected", [
    (b"one\r\ntwo\rthree\nfour", ["one\n", "two\n", "three\n", "four"]),
    (b"a\nb\n", ["a\n", "b\n"]),
    (b"", []),
])
def test_line_ranges_count_entire_file(tmp_path, data, expected):
    path = tmp_path / "lines"
    path.write_bytes(data)
    result = main.read(str(path), start=2, end=3)
    assert result["content"] == "".join(expected[1:3])
    assert result["total_lines"] == len(expected)
    assert result["lines_returned"] == len(expected[1:3])
    assert result["start_line"] == 2
    assert result["end_line"] == 3
    assert main.read(str(path), start=2)["end_line"] == len(expected)


def test_line_cap_is_utf8_bytes_and_handles_huge_lines(tmp_path, monkeypatch):
    monkeypatch.setattr(main, "MAX_LINE_RANGE_BYTES", 7)
    path = tmp_path / "lines"
    path.write_text("é" * 100_000 + "\nlast\n", encoding="utf-8")
    result = main.read(str(path), start=1)
    assert result["content"] == "ééé"
    assert len(result["content"].encode()) <= 7
    assert result["truncated"]
    assert result["total_lines"] == 2
    assert result["lines_returned"] == 1


def test_line_range_after_huge_skipped_line(tmp_path):
    path = tmp_path / "lines"
    path.write_text("a" * 150_000 + "\nlast")
    result = main.read(str(path), start=2)
    assert result["content"] == "last"
    assert result["total_lines"] == 2
    assert result["lines_returned"] == 1


@pytest.mark.parametrize("reader", [main.read, main.read_bytes])
@pytest.mark.parametrize("kwargs", [
    {"offset": -1}, {"offset": True}, {"offset": "1"}, {"offset": 1.5},
    {"limit": 0}, {"limit": -1}, {"limit": False}, {"limit": "5"},
])
def test_invalid_slices_before_policy(file, reader, kwargs, authority):
    with pytest.raises(ValueError):
        reader(file, **kwargs)
    authority[0].assert_not_called()


@pytest.mark.parametrize("kwargs", [
    {"start": 0}, {"start": -1}, {"start": True}, {"start": "1"},
    {"end": 2}, {"start": 2, "end": 1}, {"start": 1, "end": False},
    {"start": 1, "offset": 1}, {"start": 1, "limit": 10},
])
def test_invalid_line_ranges_before_policy(file, kwargs, authority):
    with pytest.raises(ValueError):
        main.read(file, **kwargs)
    authority[0].assert_not_called()


@pytest.mark.parametrize("path", ["", None, 12, [], "a\0b"])
@pytest.mark.parametrize("handler", [main.ls, main.read, main.stat, main.rm, main.mkdir])
def test_invalid_paths_before_policy(path, handler, authority):
    with pytest.raises(ValueError):
        handler(path)
    authority[0].assert_not_called()


@pytest.mark.parametrize("handler", [main.read, main.read_bytes, main.rm, main.stat, main.ls])
def test_missing_paths_raise(tmp_path, handler):
    with pytest.raises(FileNotFoundError):
        handler(str(tmp_path / "missing"))


@pytest.mark.parametrize("handler", [main.read, main.read_bytes])
def test_directory_reads_fail(tmp_path, handler):
    with pytest.raises(ValueError, match="regular file"):
        handler(str(tmp_path))


def test_ls_exact_scope_and_sorted_entries(tmp_path, authority):
    (tmp_path / "z").mkdir()
    (tmp_path / "a").touch()
    (tmp_path / "linked").symlink_to(tmp_path / "z")
    result = main.ls(str(tmp_path))
    assert result == {"path": str(tmp_path), "files": [
        {"name": "a", "is_dir": False}, {"name": "linked", "is_dir": False},
        {"name": "z", "is_dir": True},
    ]}
    authority[0].assert_called_once_with("fs.read", path=str(tmp_path))


@pytest.mark.parametrize("writer", [main.write, main.write_bytes])
@pytest.mark.parametrize("data", [b"", b"--urgent\nhello", "é".encode(), bytes(range(256))])
def test_write_explicit_content_snapshot_and_missing_parents(tmp_path, writer, data, authority):
    if writer == main.write:
        content = data.decode("utf-8", errors="replace")
        data = content.encode()
    else:
        content = base64.b64encode(data).decode()
    path = str(tmp_path / "nested" / "out")
    result = writer(path, content, session_id="test-session")
    assert result == {"path": path, "bytes": len(data)}
    assert Path(path).read_bytes() == data
    authority[0].assert_called_once_with("fs.write", path=path)
    authority[1].assert_called_once_with(path, writer.__name__, session_id="test-session")


@pytest.mark.parametrize("writer", [main.write, main.write_bytes])
def test_missing_content_never_reads_protocol_stdin(file, writer, authority):
    stdin = mock.Mock()
    with mock.patch.object(sys, "stdin", stdin), pytest.raises(TypeError):
        writer(file)
    stdin.read.assert_not_called()
    authority[0].assert_not_called()
    assert Path(file).read_text() == "hello world"


@pytest.mark.parametrize("content", ["%%%", "Zg", "Zm9v!", "Zm9v\n", "é", None, b"a"])
def test_invalid_base64_before_policy(file, content, authority):
    with pytest.raises((ValueError, TypeError)):
        main.write_bytes(file, content)
    authority[0].assert_not_called()
    assert Path(file).read_text() == "hello world"


def test_write_failures_and_snapshot_failures_raise(file, authority):
    authority[1].side_effect = OSError("snapshot failed")
    with pytest.raises(OSError, match="snapshot failed"):
        main.write(file, "new", session_id="test-session")
    assert Path(file).read_text() == "hello world"
    authority[1].side_effect = None
    with mock.patch.object(main, "atomic_write_bytes", side_effect=OSError("disk full")):
        with pytest.raises(OSError, match="disk full"):
            main.write(file, "new")


def test_mkdir_rm_and_snapshots(tmp_path, authority):
    path = str(tmp_path / "directory")
    assert main.mkdir(path, session_id="test-session") == {"created": path}
    Path(path, "child").write_text("x")
    assert main.rm(path, session_id="test-session") == {"removed": path}
    assert not Path(path).exists()
    assert authority[1].call_args_list == [
        mock.call(path, "mkdir", session_id="test-session"),
        mock.call(path, "rm", session_id="test-session"),
    ]


@pytest.mark.parametrize("handler", [main.rename, main.move, main.copy])
def test_transfer_file(file, tmp_path, handler, authority):
    dst = str(tmp_path / "nested" / "new")
    result = handler(file, dst, session_id="test-session")
    assert result["from"] == file
    assert result["to"] == dst
    assert Path(dst).read_text() == "hello world"
    assert Path(file).exists() is (handler == main.copy)
    assert authority[0].call_args_list == [
        mock.call("fs.read" if handler == main.copy else "fs.delete", path=file),
        mock.call("fs.write", path=dst),
    ]
    if handler == main.copy:
        authority[1].assert_called_once_with(dst, "copy", session_id="test-session")
    else:
        authority[2].assert_called_once_with(file, dst, "rename", session_id="test-session")


@pytest.mark.parametrize("handler", [main.rename, main.move, main.copy])
def test_transfer_refuses_missing_source_or_existing_destination(file, tmp_path, handler):
    with pytest.raises(FileNotFoundError):
        handler(str(tmp_path / "missing"), str(tmp_path / "out"))
    dst = tmp_path / "existing"
    dst.write_text("preserve")
    with pytest.raises(FileExistsError):
        handler(file, str(dst))
    assert dst.read_text() == "preserve"
    assert Path(file).exists()


def test_copy_tree(tmp_path):
    src = tmp_path / "src"
    (src / "nested").mkdir(parents=True)
    (src / "nested" / "file").write_text("content")
    dst = tmp_path / "dst"
    assert main.copy(str(src), str(dst))["kind"] == "dir"
    assert (dst / "nested" / "file").read_text() == "content"


@pytest.mark.parametrize("handler", [main.copy, main.rename, main.move])
def test_transfer_descendant_rejected_before_policy(tmp_path, handler, authority):
    with pytest.raises(ValueError, match="inside the source"):
        handler(str(tmp_path), str(tmp_path / "sub"))
    authority[0].assert_not_called()


@pytest.mark.parametrize("target", ["outside", "inside", "dangling", "directory"])
def test_copy_tree_never_materializes_symlink_targets(tmp_path, target, authority):
    src = tmp_path / "src"
    src.mkdir()
    secret = tmp_path / "secret"
    secret.write_text("DO NOT COPY")
    inner = src / "file"
    inner.write_text("inside")
    destinations = {
        "outside": secret, "inside": inner, "dangling": tmp_path / "missing",
        "directory": tmp_path,
    }
    (src / "escape").symlink_to(destinations[target])
    dst = tmp_path / "dst"
    with pytest.raises(ValueError, match="symlinks"):
        main.copy(str(src), str(dst))
    assert not dst.exists()
    authority[1].assert_not_called()


def test_policy_receives_real_symlink_target(file, tmp_path, authority):
    link = tmp_path / "link"
    link.symlink_to(file)
    for reader in (main.read, main.read_bytes):
        reader(str(link))
        authority[0].assert_called_with("fs.read", path=file)
    main.read(str(link), start=1)
    authority[0].assert_called_with("fs.read", path=file)
    with pytest.raises(OSError):
        main._open_nofollow(str(link), os.O_RDONLY)


@pytest.mark.parametrize("kwargs", [{}, {"start": 1}])
def test_reads_refuse_leaf_symlink_swapped_after_policy(file, tmp_path, authority, kwargs):
    secret = tmp_path / "secret"
    secret.write_text("secret")
    def swap(*args, **kw):
        Path(file).unlink()
        Path(file).symlink_to(secret)
    authority[0].side_effect = swap
    with pytest.raises(OSError):
        main.read(file, **kwargs)


def test_copy_refuses_leaf_symlink_swapped_after_policy(file, tmp_path, authority):
    secret = tmp_path / "secret"
    secret.write_text("do not copy")
    original = main._copy_file
    def swap(src, dst):
        Path(src).unlink()
        Path(src).symlink_to(secret)
        return original(src, dst)
    dst = tmp_path / "out"
    with mock.patch.object(main, "_copy_file", side_effect=swap):
        with pytest.raises(OSError):
            main.copy(file, str(dst))
    assert not dst.exists()


def test_fifo_reads_and_tree_copies_do_not_block(tmp_path):
    path = tmp_path / "fifo"
    path.touch()
    fifo_stat = mock.Mock(st_mode=main.file_stat.S_IFIFO)
    with mock.patch.object(main.os, "fstat", return_value=fifo_stat), mock.patch.object(
        main, "_open_nofollow", wraps=main._open_nofollow
    ) as opened:
        for reader in (main.read, main.read_bytes):
            with pytest.raises(ValueError, match="regular file"):
                reader(str(path))
        assert all(call.args[1] & os.O_NONBLOCK for call in opened.call_args_list)
    src = tmp_path / "src"
    src.mkdir()
    (src / "fifo").touch()
    with mock.patch.object(main.os, "lstat", return_value=fifo_stat):
        with pytest.raises(ValueError, match="special files"):
            main._check_tree(str(src))


def test_tag_merge_stat_and_scopes(file, authority):
    parent = str(Path(file).parent)
    assert main.tag(file, ["blue", "blue"], session_id="test-session") == {"path": file, "tags": ["blue"]}
    assert authority[0].call_args_list == [
        mock.call("fs.meta", path=file), mock.call("fs.read", path=parent),
        mock.call("fs.write", path=parent),
    ]
    authority[1].assert_called_once_with(
        str(Path(parent, main.META_FILENAME)), "tag", session_id="test-session"
    )
    assert main.tag(file, ["green", "blue"])["tags"] == ["blue", "green"]
    authority[0].reset_mock()
    result = main.stat(file)
    assert result["tags"] == ["blue", "green"]
    assert result["is_file"] and not result["is_dir"]
    assert result["size"] == 11
    assert authority[0].call_args_list == [
        mock.call("fs.meta", path=file), mock.call("fs.read", path=parent),
    ]


@pytest.mark.parametrize("tags", [[], "blue", ["blue", 2], [""], None])
def test_invalid_tags_before_policy(file, tags, authority):
    with pytest.raises(ValueError):
        main.tag(file, tags)
    authority[0].assert_not_called()


@pytest.mark.parametrize("payload", ["{", "[]", '{"file.txt": []}', '{"file.txt": {"tags": "bad"}}'])
def test_corrupt_sidecars_are_not_silently_overwritten(file, payload):
    sidecar = Path(file).with_name(main.META_FILENAME)
    sidecar.write_text(payload)
    for action in (lambda: main.tag(file, ["new"]), lambda: main.stat(file)):
        with pytest.raises(ValueError):
            action()
    assert sidecar.read_text() == payload


def test_sidecar_symlinks_are_rejected(file, tmp_path):
    secret = tmp_path / "secret"
    secret.write_text('{"secret": {}}')
    Path(file).with_name(main.META_FILENAME).symlink_to(secret)
    with pytest.raises(ValueError, match="symlink"):
        main.tag(file, ["new"])
    with pytest.raises(ValueError, match="symlink"):
        main.stat(file)
    assert secret.read_text() == '{"secret": {}}'


def test_sidecar_permission_errors_raise(file):
    with mock.patch.object(main, "_open_file", side_effect=PermissionError("denied")):
        with pytest.raises(PermissionError):
            main.tag(file, ["new"])
        with pytest.raises(PermissionError):
            main.stat(file)


def test_stat_directory_and_missing_sidecar(file, tmp_path):
    assert "tags" not in main.stat(file)
    assert main.stat(str(tmp_path))["is_dir"]
    with pytest.raises(ValueError, match="regular file"):
        main.tag(str(tmp_path), ["new"])


def test_search_json_filename_and_option_shaped_query(tmp_path, authority):
    path = tmp_path / "--needle.txt"
    path.write_text("--needle content")
    record = {"type": "match", "data": {
        "path": {"text": str(path)}, "line_number": 1,
        "lines": {"text": "--needle content\n"},
    }}
    completed = subprocess.CompletedProcess([], 0, json.dumps(record), "")
    with mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        result = main.search("--needle", str(tmp_path))
    assert result["matches"] == [{"path": str(path), "line": 1, "text": "--needle content"}]
    assert run.call_args.args[0] == ["rg", "--json", "--color", "never", "--", "--needle", str(tmp_path)]
    assert run.call_args.kwargs["timeout"] == main.SEARCH_TIMEOUT
    assert run.call_args.kwargs["stdin"] == subprocess.DEVNULL
    authority[0].assert_called_once_with("fs.read", path=str(tmp_path))
    with mock.patch.object(main.subprocess, "run", return_value=subprocess.CompletedProcess([], 1, "", "")):
        assert main.search("--needle", str(path))["matches"][0]["line"] == 0


@pytest.mark.parametrize("error", [FileNotFoundError("rg"), subprocess.TimeoutExpired("rg", 30)])
def test_search_dependency_failures_raise(tmp_path, error):
    with mock.patch.object(main.subprocess, "run", side_effect=error):
        with pytest.raises(type(error)):
            main.search("needle", str(tmp_path))


def test_search_recurses_contents_and_filenames_without_following_links(tmp_path):
    nested = tmp_path / "nested"
    nested.mkdir()
    content = nested / "content.txt"
    content.write_text("first\nneedle in content\n")
    filename = nested / "needle.txt"
    filename.write_text("different text")
    (nested / "needle-link").symlink_to(content)
    result = main.search("needle", str(tmp_path))
    matches = {item["path"]: item for item in result["matches"]}
    assert matches[str(content)]["line"] == 2
    assert matches[str(content)]["text"] == "needle in content"
    assert matches[str(filename)]["line"] == 0
    assert matches[str(nested / "needle-link")]["line"] == 0


@pytest.mark.parametrize("name", [
    "write", "write_bytes", "rm", "mkdir", "rename", "move", "copy", "tag",
])
def test_denied_mutations_have_no_side_effects(file, tmp_path, name, authority):
    destination = str(tmp_path / "not-created" / "destination")
    args = {
        "write": (file, "changed"),
        "write_bytes": (file, "AAEC"),
        "rm": (file,),
        "mkdir": (destination,),
        "rename": (file, destination),
        "move": (file, destination),
        "copy": (file, destination),
        "tag": (file, ["blue"]),
    }
    checks = 3 if name == "tag" else 2 if name in ("rename", "move", "copy") else 1
    authority[0].side_effect = [None] * (checks - 1) + [PermissionError("denied")]
    with pytest.raises(PermissionError, match="denied"):
        getattr(main, name)(*args[name], session_id="test-session")
    assert Path(file).read_text() == "hello world"
    assert not Path(destination).parent.exists()
    assert not Path(file).with_name(main.META_FILENAME).exists()
    authority[1].assert_not_called()
    authority[2].assert_not_called()


@pytest.mark.parametrize("returncode,stdout", [(2, ""), (0, "not json")])
def test_search_bad_exit_or_output_raises(tmp_path, returncode, stdout):
    with mock.patch.object(main.subprocess, "run", return_value=subprocess.CompletedProcess(
        [], returncode, stdout, "invalid expression"
    )):
        with pytest.raises((RuntimeError, ValueError)):
            main.search("[", str(tmp_path))


@pytest.mark.parametrize("query", ["", None, 1, "x\0y"])
def test_search_invalid_query_before_policy(query, authority):
    with pytest.raises(ValueError):
        main.search(query)
    authority[0].assert_not_called()


def test_recent_filters_sorts_and_limits(tmp_path, monkeypatch, authority):
    monkeypatch.setattr(main, "WORKSPACE", str(tmp_path))
    for name, mtime in [("old", 10), ("new", 20), (".hidden", 30)]:
        path = tmp_path / name
        path.write_text(name)
        os.utime(path, (mtime, mtime))
    (tmp_path / ".hidden-dir").mkdir()
    (tmp_path / ".hidden-dir" / "hidden").touch()
    (tmp_path / "link").symlink_to(tmp_path / "new")
    assert [Path(item["path"]).name for item in main.recent()["files"]] == ["new", "old"]
    assert len(main.recent(1)["files"]) == 1
    assert main.recent(0) == {"files": []}
    authority[0].assert_called_with("fs.read", path=str(tmp_path))


@pytest.mark.parametrize("n", [-1, True, 1.5, "1"])
def test_recent_invalid_count_before_policy(n, authority):
    with pytest.raises(ValueError):
        main.recent(n)
    authority[0].assert_not_called()


def test_recent_walk_errors_raise(tmp_path, monkeypatch):
    monkeypatch.setattr(main, "WORKSPACE", str(tmp_path / "missing"))
    with pytest.raises(FileNotFoundError):
        main.recent()


def test_mcp_manifest_contract_and_defaults(server, tmp_path, monkeypatch):
    tools = MANIFEST["mcp"]["tools"]
    assert len(tools) == 14
    assert {tool["name"] for tool in tools} == {
        f"fs.{name}" for name in (
            "ls", "read", "write", "rm", "mkdir", "stat", "search", "tag",
            "recent", "rename", "move", "copy", "read_bytes", "write_bytes",
        )
    }
    assert MANIFEST["mcp"]["access"] == {"system_agent": True}
    assert "operations" not in MANIFEST
    assert not hasattr(main, "run")
    assert all("binding" in arg for tool in tools for arg in tool["args"])
    listed = server.app._handle_request("tools/list", {}, True)["tools"]
    assert {tool["name"] for tool in listed} == {tool["name"] for tool in tools}
    for tool in listed:
        assert "binding" not in json.dumps(tool["inputSchema"])
    monkeypatch.chdir(tmp_path)
    assert call(server, "ls", {})["structuredContent"] == {"path": str(tmp_path), "files": []}
    with mock.patch.object(main, "search", return_value={}) as search:
        call(server, "search", {"query": "needle"})
    search.assert_called_once_with("needle", "/workspace")
    with mock.patch.object(main, "recent", return_value={}) as recent:
        call(server, "recent", {})
    recent.assert_called_once_with(10)


@pytest.mark.parametrize("name,arguments", [
    ("write", {"path": "/workspace/file"}), ("write_bytes", {"path": "/workspace/file"}),
    ("write", {"path": "/workspace/file", "content": None}),
    ("read", {"path": "/workspace/file", "offset": True}),
    ("read", {"path": "/workspace/file", "unknown": 1}),
    ("tag", {"path": "/workspace/file", "tags": []}),
])
def test_mcp_argument_errors_before_policy(server, name, arguments, authority):
    assert call(server, name, arguments)["isError"]
    authority[0].assert_not_called()


def test_mcp_integer_wire_values_are_normalized(server, file):
    result = call(server, "read", {"path": file, "offset": Decimal("1"), "limit": Decimal("3")})
    assert result["structuredContent"]["content"] == "ell"


@pytest.mark.parametrize("name,kwargs,data", [
    ("read", {}, b"\xff" * (main.MAX_READ_BYTES + 1)),
    ("read", {"start": 1}, b"\0" * (main.MAX_LINE_RANGE_BYTES + 1)),
    ("read_bytes", {}, b"x" * (main.MAX_READ_BYTES_BINARY + 1)),
])
def test_maximum_reads_fit_mcp_and_host_frames(server, tmp_path, name, kwargs, data):
    path = tmp_path / "large"
    path.write_bytes(data)
    result = call(server, name, {"path": str(path), **kwargs})
    assert result["structuredContent"]["truncated"]
    frame = {"jsonrpc": "2.0", "id": 1, "result": result}
    assert len(encode_wire_json(frame).encode()) < MAX_LINE_BYTES
    # Core's CallToolResult projects content/isError into the Host envelope.
    host_result = {"content": result["content"], "isError": result["isError"]}
    assert len(encode_wire_json({"result": host_result}).encode()) < 8 * 1024 * 1024


def test_mcp_snapshots_use_each_authenticated_session(server, file, tmp_path, monkeypatch):
    data = tmp_path / "data"
    monkeypatch.setenv("COS_DATA_DIR", str(data))
    monkeypatch.setenv("COS_SESSION", "stale-service-session")
    monkeypatch.setenv("COS_SNAPSHOT", "1")
    monkeypatch.setattr(main.snapshot, "snapshot", REAL_SNAPSHOT)
    for session_id, before, after in [
        ("first-session", "hello world", "first"),
        ("second-session", "first", "second"),
    ]:
        params = authenticated_mcp_params({
            "name": "fs.write", "arguments": {"path": file, "content": after}
        })
        params["_meta"]["claw-os.dev/call-context"]["session_id"] = session_id
        result = server.app._handle_request("tools/call", params, True)
        assert not result.get("isError", False)
        entries = list(main.snapshot.iter_entries(session_id))
        assert len(entries) == 1
        assert entries[0]["path"] == file
        assert Path(entries[0]["_dir"], "blob").read_text() == before
    assert not (data / "trash" / "stale-service-session").exists()
    params["_meta"]["claw-os.dev/call-context"].pop("session_id")
    params["arguments"]["content"] = "no session"
    result = server.app._handle_request("tools/call", params, True)
    assert not result.get("isError", False)
    assert len(list(main.snapshot.iter_entries("second-session"))) == 1
    assert not (data / "trash" / "stale-service-session").exists()


def test_mcp_session_cannot_be_supplied_as_tool_argument(server, file, authority):
    result = call(server, "write", {"path": file, "content": "no", "session_id": "forged"})
    assert result["isError"]
    authority[0].assert_not_called()


def test_mcp_all_handlers_round_trip_and_failures(server, tmp_path, monkeypatch):
    monkeypatch.setattr(main, "WORKSPACE", str(tmp_path))
    directory = str(tmp_path / "work")
    path = str(Path(directory, "file"))
    assert call(server, "mkdir", {"path": directory})["structuredContent"]["created"] == directory
    assert call(server, "write", {"path": path, "content": "hello"})["structuredContent"]["bytes"] == 5
    assert call(server, "read", {"path": path})["structuredContent"]["content"] == "hello"
    assert call(server, "tag", {"path": path, "tags": ["blue"]})["structuredContent"]["tags"] == ["blue"]
    assert call(server, "stat", {"path": path})["structuredContent"]["tags"] == ["blue"]
    assert call(server, "ls", {"path": directory})["structuredContent"]["files"]
    assert call(server, "recent", {})["structuredContent"]["files"]
    encoded = base64.b64encode(bytes(range(256))).decode()
    assert call(server, "write_bytes", {"path": path, "content": encoded})["structuredContent"]["bytes"] == 256
    assert call(server, "read_bytes", {"path": path})["structuredContent"]["base64"] == encoded
    dst = str(Path(directory, "copy"))
    for name in ("copy", "move", "rename"):
        result = call(server, name, {"src": path, "dst": dst})
        assert result["structuredContent"]["to"] == dst
        path, dst = dst, dst + "-next"
    with mock.patch.object(main.subprocess, "run", return_value=subprocess.CompletedProcess([], 1, "", "")):
        assert "matches" in call(server, "search", {"query": "missing", "path": directory})["structuredContent"]
    assert call(server, "rm", {"path": directory})["structuredContent"]["removed"] == directory
    assert call(server, "read", {"path": path})["isError"]
    with mock.patch.object(main.policy, "require", side_effect=PermissionError("denied")):
        assert call(server, "write", {"path": path, "content": "no"})["isError"]
