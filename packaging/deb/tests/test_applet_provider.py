"""The desktop ABI is never advertised without a staged native provider."""

import json
import os
from pathlib import Path
from platform import machine
import shutil
import subprocess

import pytest


ROOT = Path(__file__).resolve().parents[3]
ARCH = {"x86_64": "amd64", "aarch64": "arm64"}[machine()]


def package_attempt(stage):
    epoch = json.loads((ROOT / "packaging/release-security/policy.json").read_text())["security_epoch"]
    return subprocess.run(
        ["bash", str(ROOT / "packaging/deb/build-desktop-deb.sh"), str(stage)],
        env={**os.environ, "ARCH": ARCH, "COS_PACKAGE_VERSION": f"{epoch}:0.0.0~applet-fixture"},
        capture_output=True, text=True, timeout=10,
    )


@pytest.mark.parametrize("kind", ["missing", "script", "nonexecutable", "symlink", "truncated", "wrong-architecture"])
def test_invalid_provider_is_rejected_before_packaging(tmp_path, kind):
    binary = tmp_path / "usr/libexec/claw-os-applet-provider"
    binary.parent.mkdir(parents=True)
    if kind not in ("missing", "symlink"):
        binary.write_bytes(b"#!/bin/sh\nexit 0\n")
        binary.chmod(0o755 if kind != "nonexecutable" else 0o644)
    if kind == "symlink":
        binary.symlink_to("/bin/true")
    elif kind == "truncated":
        binary.write_bytes(b"\x7fELF\x02\x01\x01")
    elif kind == "wrong-architecture":
        shutil.copyfile("/bin/true", binary)
        with binary.open("r+b") as output:
            output.seek(18)
            output.write(b"\xb7\x00" if ARCH == "amd64" else b"\x3e\x00")
    result = package_attempt(tmp_path)
    assert result.returncode != 0
    assert "Applet service" in result.stderr
    assert not (tmp_path / "DEBIAN").exists()


def test_native_provider_passes_abi_guard_without_bypassing_other_package_requirements(tmp_path):
    binary = tmp_path / "usr/libexec/claw-os-applet-provider"
    binary.parent.mkdir(parents=True)
    shutil.copy2(os.environ.get("CLAW_APPLET_TEST_BINARY", "/bin/true"), binary)
    result = package_attempt(tmp_path)
    assert result.returncode != 0
    assert "required desktop Agent binary missing" in result.stderr
    assert "Applet service" not in result.stderr
    assert not (tmp_path / "DEBIAN").exists()
