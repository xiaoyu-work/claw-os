import ast
import inspect
import json
import os
import pathlib
import re
from unittest import mock

import pytest

from test_support import load_local_module


APP_DIR = pathlib.Path(__file__).parent
MANIFEST_PATH = APP_DIR / "app.json"
SERVER_PATH = APP_DIR / "server.py"
COS_BIN = "/usr/local/bin/cos"
APP_ID = "com.example.Editor"
TOOL_NAMES = [
    "launcher.list",
    "launcher.find",
    "launcher.open",
    "launcher.recent",
    "launcher.is-running",
]

main = load_local_module(
    APP_DIR / "main.py",
    "claw_test_launcher_main",
    clear_modules=("_shared",),
)


def _server_bindings() -> dict[str, ast.FunctionDef]:
    bindings: dict[str, ast.FunctionDef] = {}
    tree = ast.parse(SERVER_PATH.read_text(encoding="utf-8"))
    for node in tree.body:
        if not isinstance(node, ast.FunctionDef):
            continue
        for decorator in node.decorator_list:
            if (
                isinstance(decorator, ast.Call)
                and isinstance(decorator.func, ast.Attribute)
                and isinstance(decorator.func.value, ast.Name)
                and decorator.func.value.id == "app"
                and decorator.func.attr == "tool"
                and len(decorator.args) == 1
                and isinstance(decorator.args[0], ast.Constant)
                and isinstance(decorator.args[0].value, str)
            ):
                bindings[decorator.args[0].value] = node
    return bindings


def _argument_contract(
    function: ast.FunctionDef,
) -> tuple[list[str], dict[str, object], dict[str, str]]:
    names = [argument.arg for argument in function.args.args]
    default_names = names[len(names) - len(function.args.defaults) :]
    defaults = {
        name: ast.literal_eval(default)
        for name, default in zip(
            default_names,
            function.args.defaults,
            strict=True,
        )
    }
    annotations = {
        argument.arg: ast.unparse(argument.annotation)
        for argument in function.args.args
        if argument.annotation is not None
    }
    return names, defaults, annotations


def _read_need(value: str) -> dict[str, object]:
    return {
        "verb": "fs.read",
        "scope": {
            "kind": "fixed",
            "scope": {"kind": "path", "value": value},
        },
        "why": {
            "en": {
                "/usr/share/applications/**": "Read `.desktop` entries shipped by system packages.",
                "/usr/local/share/applications/**": "Read `.desktop` entries installed locally as admin.",
                "~/.local/share/applications/**": "Read `.desktop` entries installed in your home directory.",
            }[value]
        },
    }


def _entry(**values: object) -> dict[str, object]:
    entry: dict[str, object] = {
        "app_id": APP_ID,
        "name": "Editor",
        "generic_name": "",
        "comment": "",
        "keywords": "",
        "exec_binary": "editor",
        "path": "/usr/share/applications/com.example.Editor.desktop",
    }
    entry.update(values)
    return entry


def test_manifest_and_handlers_are_mcp_only_and_aligned() -> None:
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    assert "operations" not in manifest
    assert "dependencies" not in manifest

    tools = manifest["mcp"]["tools"]
    assert [tool["name"] for tool in tools] == TOOL_NAMES
    tool_map = {tool["name"]: tool for tool in tools}
    assert [tool["summary"]["en"] for tool in tools] == [
        "Enumerate every `.desktop` entry installed under the standard XDG application directories. Returns AppID, localized name, comment, categories, keywords, and icon.",
        "Fuzzy-search installed apps by localized name, generic name, comment, or keywords. Returns the top matches ranked by relevance.",
        "Launch an installed graphical app. `app_id` is the AppID — the `.desktop` filename without extension (e.g. `com.clawos.Files`, `org.mozilla.firefox`). Repeat `--uri` for absolute non-file URIs and `--path` for exact local files the app should open.",
        "Show desktop apps the agent has launched recently, newest first. Useful for re-opening the last app the user worked with.",
        "Look for live processes whose executable matches the AppID's `Exec=` binary. Returns the list of matching PIDs (empty if the app is not running).",
    ]
    assert tool_map["launcher.list"]["args"] == [
        {
            "name": "include_no_display",
            "kind": "bool",
            "required": False,
            "binding": "flag",
            "default": False,
        },
        {
            "name": "include_hidden",
            "kind": "bool",
            "required": False,
            "binding": "flag",
            "default": False,
        },
    ]
    assert tool_map["launcher.find"]["args"] == [
        {
            "name": "query",
            "kind": "text",
            "required": True,
            "binding": "positional",
        },
        {
            "name": "limit",
            "kind": "integer",
            "required": False,
            "binding": "flag",
            "default": 10,
        },
    ]
    assert tool_map["launcher.open"]["args"] == [
        {
            "name": "app_id",
            "kind": "name",
            "required": True,
            "binding": "positional",
        },
        {
            "name": "uri",
            "kind": "text",
            "required": False,
            "binding": "flag",
            "repeatable": True,
        },
        {
            "name": "path",
            "kind": "path",
            "required": False,
            "binding": "flag",
            "repeatable": True,
        },
    ]
    assert tool_map["launcher.recent"]["args"] == [
        {
            "name": "limit",
            "kind": "integer",
            "required": False,
            "binding": "flag",
            "default": 20,
        }
    ]
    assert tool_map["launcher.is-running"]["args"] == [
        {
            "name": "app_id",
            "kind": "name",
            "required": True,
            "binding": "positional",
        }
    ]

    reads = [
        _read_need("/usr/share/applications/**"),
        _read_need("/usr/local/share/applications/**"),
        _read_need("~/.local/share/applications/**"),
    ]
    assert tool_map["launcher.list"]["needs"] == reads
    assert tool_map["launcher.find"]["needs"] == reads
    assert tool_map["launcher.open"]["needs"] == [
        {
            "verb": "desktop.launch",
            "scope": {"kind": "from-arg", "arg": "app_id"},
            "why": {
                "en": "Open the graphical application you asked to launch."
            },
        },
        {
            "verb": "fs.read",
            "scope": {"kind": "from-arg", "arg": "path"},
            "when": {"kind": "arg-present", "arg": "path"},
            "why": {
                "en": "Allow the launched application to read each exact local file you asked it to open."
            },
        },
        *reads,
    ]
    assert tool_map["launcher.recent"].get("needs", []) == []
    assert tool_map["launcher.is-running"]["needs"] == [
        {
            "verb": "proc.observe",
            "scope": {"kind": "wild"},
            "why": {
                "en": "Scan the process list for the app you asked about."
            },
        },
        *reads,
    ]

    server_source = SERVER_PATH.read_text(encoding="utf-8")
    assert "serve_manifest_operations" not in server_source
    assert server_source.count("App.from_manifest()") == 1
    bindings = _server_bindings()
    assert list(bindings) == TOOL_NAMES
    assert _argument_contract(bindings["launcher.list"]) == (
        ["include_no_display", "include_hidden"],
        {"include_no_display": False, "include_hidden": False},
        {"include_no_display": "bool", "include_hidden": "bool"},
    )
    assert _argument_contract(bindings["launcher.find"]) == (
        ["query", "limit"],
        {"limit": 10},
        {"query": "str", "limit": "int"},
    )
    assert _argument_contract(bindings["launcher.open"]) == (
        ["app_id", "uri", "path"],
        {"uri": None, "path": None},
        {
            "app_id": "str",
            "uri": "list[str] | None",
            "path": "list[str] | None",
        },
    )
    assert _argument_contract(bindings["launcher.recent"]) == (
        ["limit"],
        {"limit": 20},
        {"limit": "int"},
    )
    assert _argument_contract(bindings["launcher.is-running"]) == (
        ["app_id"],
        {},
        {"app_id": "str"},
    )
    assert bindings["launcher.is-running"].name == "is_running"

    implementations = {
        "launcher.list": main.list_apps,
        "launcher.find": main.find,
        "launcher.open": main.open_app,
        "launcher.recent": main.recent,
        "launcher.is-running": main.is_running,
    }
    for tool_name, implementation in implementations.items():
        arguments = tool_map[tool_name].get("args", [])
        expected_names = [argument["name"] for argument in arguments]
        expected_defaults = {
            argument["name"]: argument.get("default")
            for argument in arguments
            if not argument.get("required", False)
        }
        signature = inspect.signature(implementation)
        assert list(signature.parameters) == expected_names
        assert {
            name: parameter.default
            for name, parameter in signature.parameters.items()
            if parameter.default is not inspect.Signature.empty
        } == expected_defaults
        assert all(
            parameter.annotation is not inspect.Signature.empty
            for parameter in signature.parameters.values()
        )
        assert signature.return_annotation is not inspect.Signature.empty

    main_source = (APP_DIR / "main.py").read_text(encoding="utf-8")
    assert "canonical_argv" not in main_source
    assert "argparse" not in main_source
    assert "def run(" not in main_source
    assert "def cmd_" not in main_source
    assert "_spawn_detached" not in main_source
    assert "_expand_exec_line" not in main_source
    assert "gtk-launch" not in main_source
    assert "gio launch" not in main_source


def test_desktop_file_parsing_and_locale_resolution(tmp_path: pathlib.Path) -> None:
    path = tmp_path / "editor.desktop"
    path.write_text(
        "[Desktop Entry]\n"
        "Type=Application\n"
        "Name=Editor\n"
        "Name[zh_CN]=编辑器\n"
        "Exec=/usr/bin/editor %F\n"
        "[Desktop Action extra]\n"
        "Name=Ignored\n",
        encoding="utf-8",
    )
    entries = main._parse_desktop_file(str(path))
    assert entries is not None
    assert entries["Name"] == "Editor"
    assert main._localized(entries, "Name", ["zh_CN", "zh"]) == "编辑器"
    assert main._exec_binary(entries) == "editor"


def test_non_application_and_missing_desktop_entries_are_skipped(
    tmp_path: pathlib.Path,
) -> None:
    path = tmp_path / "link.desktop"
    path.write_text(
        "[Desktop Entry]\nType=Link\nName=Docs\nURL=https://example.test\n",
        encoding="utf-8",
    )
    assert main._parse_desktop_file(str(path)) is None
    assert main._parse_desktop_file(str(tmp_path / "missing.desktop")) is None


def test_visibility_and_fuzzy_scoring_are_preserved() -> None:
    assert not main._passes_visibility({"Hidden": "true"}, set(), False, False)
    assert main._passes_visibility({"Hidden": "true"}, set(), True, True)
    assert not main._passes_visibility({"NoDisplay": "true"}, set(), False, False)
    assert main._passes_visibility(
        {"OnlyShowIn": "COSMIC;GNOME;"},
        {"COSMIC"},
        False,
        False,
    )
    assert not main._passes_visibility(
        {"NotShowIn": "GNOME;"},
        {"GNOME"},
        False,
        False,
    )
    exact = _entry(name="Files")
    partial = _entry(name="File Manager")
    assert main._score("Files", exact) > main._score("Files", partial)
    assert main._score("manager", _entry(keywords="file;manager;")) > 0
    assert main._score("calculator", _entry(name="Firefox")) == 0


def test_user_desktop_entry_shadows_system_entry(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    system = tmp_path / "system"
    user = tmp_path / "user"
    (system / "applications").mkdir(parents=True)
    (user / "applications").mkdir(parents=True)
    (system / "applications" / f"{APP_ID}.desktop").write_text(
        "[Desktop Entry]\nName=System Editor\nExec=system-editor\n",
        encoding="utf-8",
    )
    (user / "applications" / f"{APP_ID}.desktop").write_text(
        "[Desktop Entry]\nName=User Editor\nExec=user-editor\n",
        encoding="utf-8",
    )
    monkeypatch.setenv("XDG_DATA_HOME", str(user))
    monkeypatch.setenv("XDG_DATA_DIRS", str(system))
    monkeypatch.setenv("XDG_CURRENT_DESKTOP", "COSMIC")
    monkeypatch.setenv("LANG", "C")
    apps = main._scan_apps(gate=False)
    assert apps[APP_ID]["name"] == "User Editor"
    assert apps[APP_ID]["exec_binary"] == "user-editor"


def test_owner_home_and_cosmic_default_drive_user_app_discovery(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    owner_home = tmp_path / "owner"
    applications = owner_home / ".local" / "share" / "applications"
    applications.mkdir(parents=True)
    (applications / f"{APP_ID}.desktop").write_text(
        "[Desktop Entry]\n"
        "Name=Cosmic Editor\n"
        "Exec=cosmic-editor\n"
        "OnlyShowIn=COSMIC;\n",
        encoding="utf-8",
    )
    monkeypatch.delenv("XDG_DATA_HOME", raising=False)
    monkeypatch.delenv("XDG_CURRENT_DESKTOP", raising=False)
    monkeypatch.setenv("COS_OWNER_HOME", str(owner_home))
    monkeypatch.setenv("XDG_DATA_DIRS", str(tmp_path / "system"))
    monkeypatch.setenv("LANG", "C")

    apps = main._scan_apps(gate=False)

    assert apps[APP_ID]["name"] == "Cosmic Editor"
    assert apps[APP_ID]["exec_binary"] == "cosmic-editor"


@pytest.mark.parametrize(
    ("call", "message"),
    [
        (lambda: main.list_apps(1, False), "include_no_display must be a boolean"),
        (lambda: main.list_apps(False, "false"), "include_hidden must be a boolean"),
        (lambda: main.find(["editor"]), "query must be non-empty text"),
        (lambda: main.find(" \n"), "query must be non-empty text"),
        (lambda: main.find("editor", True), "limit must be an integer"),
        (lambda: main.recent(False), "limit must be an integer"),
        (lambda: main.open_app("*"), "app_id must be an exact desktop AppID"),
        (lambda: main.is_running("-editor"), "app_id must be an exact desktop AppID"),
        (lambda: main.open_app(APP_ID, "https://example.test"), "uri must be a list"),
        (lambda: main.open_app(APP_ID, None, "file.txt"), "path must be a list"),
        (
            lambda: main.open_app(APP_ID, [""]),
            "uri values must be non-empty text",
        ),
        (
            lambda: main.open_app(APP_ID, ["bad\nvalue"]),
            "uri values must be non-empty text",
        ),
        (
            lambda: main.open_app(APP_ID, ["x" * (main.MAX_URI_BYTES + 1)]),
            "uri values must be non-empty text",
        ),
        (
            lambda: main.open_app(
                APP_ID,
                ["https://example.test"] * (main.MAX_URI_COUNT + 1),
            ),
            "uri and path accept at most",
        ),
        (
            lambda: main.open_app(APP_ID, ["file:///tmp/report.txt"]),
            "file URIs are not accepted; use path",
        ),
        (
            lambda: main.open_app(APP_ID, ["not-a-uri"]),
            "uri values must be absolute URIs",
        ),
        (
            lambda: main.open_app(APP_ID, None, ["relative.txt"]),
            "path values must be absolute",
        ),
    ],
)
def test_invalid_inputs_are_rejected_before_policy(call, message: str) -> None:
    with mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main, "_scan_apps"
    ) as scan, mock.patch.object(main.subprocess, "run") as run:
        with pytest.raises(ValueError, match=re.escape(message)):
            call()
    require.assert_not_called()
    scan.assert_not_called()
    run.assert_not_called()


def test_unsafe_local_files_are_rejected_before_policy(
    tmp_path: pathlib.Path,
) -> None:
    regular = tmp_path / "report.txt"
    regular.write_text("report", encoding="utf-8")
    symlink = tmp_path / "report-link.txt"
    symlink.symlink_to(regular)
    missing = tmp_path / "missing.txt"
    noncanonical = tmp_path / "nested" / ".." / "report.txt"

    for value, message in [
        (str(missing), "path values must name existing regular files"),
        (str(tmp_path), "path values must name existing regular files"),
        (str(symlink), "path values must not contain symbolic links"),
        (str(noncanonical), "path values must already be canonical"),
    ]:
        with mock.patch.object(main.policy, "require") as require:
            with pytest.raises(ValueError, match=message):
                main.open_app(APP_ID, path=[value])
        require.assert_not_called()


def test_list_preserves_hidden_implies_no_display() -> None:
    with mock.patch.object(main, "_scan_apps", return_value={}) as scan:
        assert main.list_apps(include_hidden=True) == {"count": 0, "apps": []}
    scan.assert_called_once_with(include_hidden=True, include_no_display=True)


def test_find_accepts_one_query_without_legacy_joining() -> None:
    with mock.patch.object(main, "_scan_apps", return_value={APP_ID: _entry()}):
        result = main.find("example editor", 10)
    assert result["query"] == "example editor"
    assert result["count"] == 1


def test_open_uses_exact_policy_and_only_brokered_app_id_and_uris(
    tmp_path: pathlib.Path,
) -> None:
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps(
            {
                "launched": True,
                "app_id": APP_ID,
                "launcher": "/usr/bin/gtk4-launch",
            }
        ),
        stderr="",
    )
    entry = _entry()
    local_file = tmp_path / "report.txt"
    local_file.write_text("report", encoding="utf-8")
    canonical_path = str(local_file.resolve())
    file_uri = local_file.resolve().as_uri()
    events: list[str] = []

    def require(verb: str, **kwargs: object) -> None:
        events.append(f"policy:{verb}")
        if verb == "desktop.launch":
            assert kwargs == {"name": APP_ID}
        else:
            assert verb == "fs.read"
            assert kwargs == {"path": canonical_path}

    def find_entry(_app_id: str) -> dict[str, object]:
        events.append("entry")
        return entry

    def run(*_args: object, **_kwargs: object) -> object:
        events.append("broker")
        return completed

    def append(record: dict[str, object]) -> None:
        events.append("recent")
        assert record["extras"] == ["https://example.test/report", file_uri]

    with mock.patch.dict(os.environ, {"COS_BIN": COS_BIN}), mock.patch.object(
        main.policy, "require", side_effect=require
    ) as require_mock, mock.patch.object(
        main, "_find_entry", side_effect=find_entry
    ), mock.patch.object(
        main.subprocess, "run", side_effect=run
    ) as run_mock, mock.patch.object(
        main, "_append_recent", side_effect=append
    ):
        result = main.open_app(
            APP_ID,
            ["https://example.test/report"],
            [canonical_path],
        )

    assert events == ["entry", "policy:fs.read", "policy:desktop.launch", "broker", "recent"]
    assert require_mock.call_args_list == [
        mock.call("fs.read", path=canonical_path),
        mock.call("desktop.launch", name=APP_ID),
    ]
    assert run_mock.call_args.args[0] == [
        COS_BIN,
        "__desktop",
        "launch",
        "--app-id",
        APP_ID,
        "--uri",
        "https://example.test/report",
        "--uri",
        file_uri,
    ]
    assert entry["path"] not in run_mock.call_args.args[0]
    assert entry["exec_binary"] not in run_mock.call_args.args[0]
    assert run_mock.call_args.kwargs["stdin"] is main.subprocess.DEVNULL
    assert result["launched"] is True
    assert result["launcher"] == "/usr/bin/gtk4-launch"
    assert result["name"] == "Editor"


def test_open_records_nothing_when_broker_fails() -> None:
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"error": "desktop unavailable"}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": COS_BIN}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(main, "_find_entry", return_value=_entry()), mock.patch.object(
        main.subprocess, "run", return_value=completed
    ), mock.patch.object(main, "_append_recent") as append:
        with pytest.raises(RuntimeError, match="desktop unavailable"):
            main.open_app(APP_ID)
    append.assert_not_called()


def test_recent_write_failure_after_launch_is_not_hidden() -> None:
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps(
            {
                "launched": True,
                "app_id": APP_ID,
                "launcher": "/usr/bin/gtk4-launch",
            }
        ),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": COS_BIN}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(main, "_find_entry", return_value=_entry()), mock.patch.object(
        main.subprocess, "run", return_value=completed
    ), mock.patch.object(
        main, "_append_recent", side_effect=OSError("read-only state")
    ):
        with pytest.raises(OSError, match="read-only state"):
            main.open_app(APP_ID)


@pytest.mark.parametrize(
    ("returncode", "stdout", "stderr", "message"),
    [
        (0, "{", "", "Launcher broker returned invalid JSON"),
        (0, "[]", "", "Launcher broker returned a non-object result"),
        (0, "", "", "Launcher broker returned no JSON result"),
        (
            0,
            json.dumps({"error": None}),
            "",
            "Launcher broker returned an invalid error payload",
        ),
        (
            0,
            json.dumps({"error": "launch denied"}),
            "",
            "launch denied",
        ),
        (
            7,
            json.dumps(
                {
                    "launched": True,
                    "app_id": APP_ID,
                    "launcher": "/usr/bin/gtk4-launch",
                }
            ),
            "",
            "Launcher broker exited 7",
        ),
        (
            0,
            json.dumps({"launched": True, "app_id": APP_ID}),
            "",
            "Launcher broker returned an invalid launch result",
        ),
        (
            0,
            json.dumps(
                {
                    "launched": False,
                    "app_id": APP_ID,
                    "launcher": "/usr/bin/gtk4-launch",
                }
            ),
            "",
            "Launcher broker did not confirm the launch",
        ),
        (
            0,
            json.dumps(
                {
                    "launched": True,
                    "app_id": "com.example.Other",
                    "launcher": "/usr/bin/gtk4-launch",
                }
            ),
            "",
            "Launcher broker returned the wrong app_id",
        ),
    ],
)
def test_broker_payload_failures_raise(
    returncode: int,
    stdout: str,
    stderr: str,
    message: str,
) -> None:
    completed = mock.Mock(returncode=returncode, stdout=stdout, stderr=stderr)
    with mock.patch.dict(os.environ, {"COS_BIN": COS_BIN}), mock.patch.object(
        main.subprocess, "run", return_value=completed
    ):
        with pytest.raises(RuntimeError, match=re.escape(message)):
            main._broker_launch(APP_ID, [])


@pytest.mark.parametrize(
    ("failure", "exception_type", "message"),
    [
        (
            FileNotFoundError("gone"),
            FileNotFoundError,
            "Launcher broker executable not found",
        ),
        (
            PermissionError("denied"),
            PermissionError,
            "permission denied launching Launcher broker",
        ),
        (
            main.subprocess.TimeoutExpired(["cos"], main.BROKER_TIMEOUT_SECS),
            TimeoutError,
            "Launcher broker exceeded",
        ),
    ],
)
def test_broker_execution_failures_raise(
    failure: Exception,
    exception_type: type[Exception],
    message: str,
) -> None:
    with mock.patch.dict(os.environ, {"COS_BIN": COS_BIN}), mock.patch.object(
        main.subprocess, "run", side_effect=failure
    ):
        with pytest.raises(exception_type, match=message):
            main._broker_launch(APP_ID, [])


def test_missing_broker_binary_raises() -> None:
    with mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(
        main.shutil, "which", return_value=None
    ):
        with pytest.raises(FileNotFoundError, match="Launcher broker unavailable"):
            main._broker_launch(APP_ID, [])


@pytest.fixture
def recent_state(
    tmp_path: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> pathlib.Path:
    launcher_dir = tmp_path / "launcher"
    recent_path = launcher_dir / "recent.jsonl"
    monkeypatch.setattr(main, "LAUNCHER_DIR", str(launcher_dir))
    monkeypatch.setattr(main, "RECENT_PATH", str(recent_path))
    return recent_path


def test_recent_state_is_locked_atomic_and_deduplicated(
    recent_state: pathlib.Path,
) -> None:
    with mock.patch.object(
        main,
        "atomic_write_bytes",
        wraps=main.atomic_write_bytes,
    ) as atomic_write:
        main._append_recent({"ts": "t1", "app_id": "a", "name": "A"})
        main._append_recent({"ts": "t2", "app_id": "b", "name": "B"})
        main._append_recent({"ts": "t3", "app_id": "a", "name": "A"})
    assert atomic_write.call_count == 3
    assert recent_state.is_file()
    recent = main._read_recent(10)
    assert [record["app_id"] for record in recent] == ["a", "b"]
    assert recent[0]["last_launched_at"] == "t3"
    assert recent[0]["count"] == 2
    assert main._read_recent(0) == []


def test_missing_recent_state_is_empty(recent_state: pathlib.Path) -> None:
    assert not recent_state.exists()
    assert main._read_recent(10) == []


@pytest.mark.parametrize(
    "content",
    [
        "not json\n",
        "[]\n",
        json.dumps({"ts": "t1", "name": "Missing AppID"}) + "\n",
        json.dumps({"ts": 1, "app_id": "a", "name": "A"}) + "\n",
    ],
)
def test_corrupt_recent_state_fails_explicitly(
    recent_state: pathlib.Path,
    content: str,
) -> None:
    recent_state.parent.mkdir(parents=True, exist_ok=True)
    recent_state.write_text(content, encoding="utf-8")
    with pytest.raises(ValueError, match="recent state is corrupt"):
        main._read_recent(10)
    with pytest.raises(ValueError, match="recent state is corrupt"):
        main._append_recent({"ts": "t2", "app_id": "b", "name": "B"})


def test_is_running_uses_exact_process_capability() -> None:
    with mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main, "_find_entry", return_value=_entry(exec_binary="editor")
    ), mock.patch.object(main, "_pids_matching", return_value=[10, 20]) as pids:
        result = main.is_running(APP_ID)
    require.assert_called_once_with("proc.observe", wild=True)
    pids.assert_called_once_with("editor")
    assert result == {
        "app_id": APP_ID,
        "exec_binary": "editor",
        "running": True,
        "pids": [10, 20],
    }
