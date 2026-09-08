"""Mail feature packaging must not replace the shared authenticated App."""

import os
from pathlib import Path
import subprocess

import pytest


FEATURE = Path(__file__).resolve().parent


def _write(path, content, mode=0o644):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    path.chmod(mode)


def _fixture(tmp_path):
    rootfs = tmp_path / "rootfs"
    project = tmp_path / "project"
    extension = project / "extensions" / "claw-mail-ai"
    _write(extension / "manifest.json", '{"name": "fixture"}')
    _write(extension / "README.md", "Mail fixture")
    for name in ("app.json", "main.py", "server.py", "native_host.py"):
        _write(rootfs / "usr/lib/cos/apps/mail-ai" / name, f"installed {name}")
        _write(project / "apps/mail-ai" / name, f"source must not replace {name}")
    for package in ("claw_os_sdk", "cos_runtime"):
        _write(rootfs / "usr/lib/cos/python" / package / "__init__.py", "installed")
    _write(rootfs / "usr/lib/cos/claw-mail-ai-host", "#!/bin/sh\nexit 0\n", 0o755)
    _write(rootfs / "usr/lib/thunderbird/distribution/extensions/claw-mail-ai@claw.os.xpi",
           "installed XPI")
    env = {
        **os.environ,
        "ROOTFS": str(rootfs),
        "PROJECT_DIR": str(project),
        "SCRIPT_DIR": str(FEATURE.parent.parent),
        "PATH": "/usr/bin:/bin",
    }
    return rootfs, env


def _package_state(rootfs):
    return {
        str(path.relative_to(rootfs)): (path.read_bytes(), path.stat().st_mode)
        for path in (rootfs / "usr/lib").rglob("*")
        if path.is_file()
    }


def test_feature_preserves_the_canonical_package_and_sdk(tmp_path):
    rootfs, env = _fixture(tmp_path)
    before = _package_state(rootfs)
    result = subprocess.run(
        ["bash", str(FEATURE / "install.sh")],
        env=env, text=True, capture_output=True, timeout=30,
    )
    assert result.returncode == 0, result.stderr
    assert _package_state(rootfs) == before
    assert not (rootfs / "usr/lib/cos/mail-ai").exists()
    assert (rootfs / "etc/thunderbird/native-messaging-hosts/os.claw.mail_ai.json").is_file()


@pytest.mark.parametrize("missing", ["app.json", "main.py", "server.py", "native_host.py"])
def test_feature_refuses_incomplete_installed_mail_package(tmp_path, missing):
    rootfs, env = _fixture(tmp_path)
    (rootfs / "usr/lib/cos/apps/mail-ai" / missing).unlink()
    result = subprocess.run(
        ["bash", str(FEATURE / "install.sh")],
        env=env, text=True, capture_output=True, timeout=30,
    )
    assert result.returncode != 0
    assert f"Mail package is incomplete: {rootfs}/usr/lib/cos/apps/mail-ai/{missing}" in result.stderr
    assert not (rootfs / "usr/lib/cos/mail-ai").exists()
    assert not (rootfs / "etc/thunderbird").exists()


def test_feature_refuses_missing_package_owned_extension(tmp_path):
    rootfs, env = _fixture(tmp_path)
    (rootfs / "usr/lib/thunderbird/distribution/extensions/claw-mail-ai@claw.os.xpi").unlink()
    result = subprocess.run(
        ["bash", str(FEATURE / "install.sh")],
        env=env, text=True, capture_output=True, timeout=30,
    )
    assert result.returncode != 0
    assert "Mail extension is missing" in result.stderr
    assert not (rootfs / "etc/thunderbird").exists()
