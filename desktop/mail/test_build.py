"""Source-pair and build-layout contracts; no toolchain downloads in unit tests."""

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tomllib

import pytest


HERE = Path(__file__).parent
SPEC = importlib.util.spec_from_file_location("mail_build", HERE / "build.py")
build = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(build)


@pytest.fixture
def layout(tmp_path, monkeypatch):
    product = tmp_path / "desktop" / "mail"
    (product / "comm").mkdir(parents=True)
    work = tmp_path / "build" / "mail"
    gecko = work / "gecko"
    gecko.mkdir(parents=True)
    monkeypatch.setattr(build, "ROOT", tmp_path)
    monkeypatch.setattr(build, "PRODUCT", product)
    monkeypatch.setattr(build, "WORK", work)
    monkeypatch.setattr(build, "GECKO", gecko)
    monkeypatch.setattr(build.sys, "platform", "linux")
    calls = []
    monkeypatch.setattr(build, "run", lambda *args, **kwargs: calls.append((args, kwargs)))
    monkeypatch.setattr(build, "output", lambda *args, **kwargs: "pinned-revision")
    return product, gecko, calls


def test_release_pin_matches_the_imported_platform_contract():
    pin = json.loads((HERE / "upstream.json").read_text())
    source = (HERE / "comm" / ".gecko_rev.yml").read_text()
    assert f"GECKO_HEAD_REV: {pin['firefox']['mercurial_revision']}\n" in source
    assert f"GECKO_HEAD_REF: {pin['firefox']['tag']}\n" in source
    for component in ("thunderbird", "firefox"):
        assert len(pin[component]["revision"]) == 40
    assert "--enable-project=comm/mail" in (HERE / "mozconfig").read_text()


def test_native_workspaces_do_not_inherit_claw_rust_dependencies():
    workspace = tomllib.loads((HERE.parent.parent / "Cargo.toml").read_text())["workspace"]
    assert {"desktop/mail/comm", "build/mail/gecko"} <= set(workspace["exclude"])


def test_prepare_uses_live_product_source_not_another_copy(layout):
    product, gecko, calls = layout
    build.prepare({"revision": "pinned-revision"})
    assert (gecko / "comm").is_symlink()
    assert (gecko / "comm").resolve() == product / "comm"
    build.prepare({"revision": "pinned-revision"})
    assert len(calls) == 4


def test_prepare_refuses_mismatched_platform(layout):
    _, gecko, calls = layout
    with pytest.raises(RuntimeError, match="platform mismatch"):
        build.prepare({"revision": "different-revision"})
    assert not (gecko / "comm").exists()
    assert calls == []


def test_prepare_preserves_another_product_checkout(layout):
    _, gecko, _ = layout
    (gecko / "comm").mkdir()
    with pytest.raises(RuntimeError, match="refusing to overwrite"):
        build.prepare({"revision": "pinned-revision"})
    assert (gecko / "comm").is_dir()
    assert not (gecko / "comm").is_symlink()


def test_prepare_preserves_foreign_symlink(layout, tmp_path):
    _, gecko, _ = layout
    target = tmp_path / "other-source"
    target.mkdir()
    (gecko / "comm").symlink_to(target)
    with pytest.raises(RuntimeError, match="different Mail source"):
        build.prepare({"revision": "pinned-revision"})
    assert (gecko / "comm").resolve() == target


def test_dirty_platform_failure_is_not_hidden(layout, monkeypatch):
    _, gecko, _ = layout

    def dirty(*args, **kwargs):
        raise subprocess.CalledProcessError(1, args)

    monkeypatch.setattr(build, "run", dirty)
    with pytest.raises(subprocess.CalledProcessError):
        build.prepare({"revision": "pinned-revision"})
    assert not (gecko / "comm").exists()


def test_windows_mount_rejected_before_creating_output(layout, monkeypatch):
    monkeypatch.setattr(build, "ROOT", Path("/mnt/c/source"))
    with pytest.raises(RuntimeError, match="Linux filesystem"):
        build.prepare({"revision": "pinned-revision"})


def test_build_sets_source_provenance_and_passes_arguments(layout, monkeypatch):
    product, gecko, calls = layout
    (product / "upstream.json").write_text(json.dumps({
        "firefox": {"revision": "pinned-revision", "repository": "https://github.com/mozilla-firefox/firefox"},
    }))
    monkeypatch.setattr(sys, "argv", ["build.py", "build", "-j", "8"])
    build.main()
    command, kwargs = calls[-1]
    assert command == (sys.executable, str(gecko / "mach"), "build", "-j", "8")
    assert kwargs["cwd"] == gecko
    assert kwargs["env"]["COMM_HEAD_REV"] == "pinned-revision"
    assert kwargs["env"]["GECKO_HEAD_REV"] == "pinned-revision"
    assert kwargs["env"]["MOZCONFIG"] == str(product / "mozconfig")
    assert kwargs["env"]["MOZBUILD_STATE_PATH"] == str(gecko.parent / "state")
