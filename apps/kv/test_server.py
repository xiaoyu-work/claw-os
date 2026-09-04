"""Tests for the kv MCP service."""

import json
from pathlib import Path

import pytest

from test_support import load_local_module


APP_DIR = Path(__file__).parent


def _load_server(monkeypatch: pytest.MonkeyPatch, data_dir: Path):
    monkeypatch.setenv("COS_APP_MANIFEST", str(APP_DIR / "app.json"))
    monkeypatch.setenv("COS_DATA_DIR", str(data_dir))
    return load_local_module(
        APP_DIR / "server.py",
        "claw_test_kv_server",
        clear_modules=("_shared",),
    )


def test_list_defaults_to_all_keys(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    server = _load_server(monkeypatch, tmp_path)
    server.kv_set("zebra", "last")
    server.kv_set("alpha", "first")

    assert server.kv_list() == {
        "pattern": "*",
        "keys": ["alpha", "zebra"],
    }


def test_values_persist_across_cache_reload(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    server = _load_server(monkeypatch, tmp_path)
    server.kv_set("greeting", "hello")
    server._cache = None

    assert server.kv_get("greeting") == "hello"
    assert json.loads((tmp_path / "kv.json").read_text(encoding="utf-8")) == {
        "greeting": "hello",
    }


@pytest.mark.parametrize(
    "contents",
    [
        "{not json",
        '["not", "an", "object"]',
        '{"answer": 42}',
    ],
)
def test_invalid_persisted_store_fails_closed(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    contents: str,
) -> None:
    (tmp_path / "kv.json").write_text(contents, encoding="utf-8")
    server = _load_server(monkeypatch, tmp_path)

    with pytest.raises((json.JSONDecodeError, ValueError)):
        server.kv_dump()
