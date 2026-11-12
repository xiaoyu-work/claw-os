"""Prepare one real, unsigned Editor fixture through the App-owned public CLI."""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import stat
import struct
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("app_sources", ROOT / "scripts/app_sources.py")
app_sources = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(app_sources)

APP = "cosmic-edit"
ENTRY = "bin/cosmic-edit"
RESOURCES = (
    "resources/usr/share/applications/com.clawos.Edit.desktop",
    "resources/usr/share/metainfo/com.clawos.Edit.metainfo.xml",
    "resources/usr/share/icons/hicolor/scalable/apps/com.clawos.Edit.svg",
)


def digest(path):
    with path.open("rb") as stream:
        return "sha256:" + hashlib.file_digest(stream, "sha256").hexdigest()


def inventory(root):
    """Use the existing signer's regular-file/directory inventory shape."""
    records = []
    for path in sorted(root.rglob("*")):
        info = path.lstat()
        if stat.S_ISDIR(info.st_mode):
            kind, size, checksum = "dir", 0, ""
        elif stat.S_ISREG(info.st_mode) and info.st_nlink == 1:
            kind, size, checksum = "file", info.st_size, digest(path)
        else:
            raise ValueError(f"Unsupported signed native payload node: {path}")
        records.append({
            "path": path.relative_to(root).as_posix(), "type": kind,
            "mode": stat.S_IMODE(info.st_mode), "size": size, "digest": checksum,
        })
    if not records or len(records) > 20_000:
        raise ValueError("Native fixture inventory exceeds the provenance contract")
    return records


def executable_elf(path):
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or not info.st_mode & 0o111:
        raise ValueError("Editor fixture must contain its real executable ELF")
    with path.open("rb") as stream:
        header = stream.read(64)
        if len(header) != 64 or header[:7] != b"\x7fELF\x02\x01\x01":
            raise ValueError("Editor fixture must contain its real executable ELF")
        _, kind, machine, version, entry, offset, _, _, size, phsize, count, _, _, _ = (
            struct.unpack("<16sHHIQQQIHHHHHH", header)
        )
        if kind not in (2, 3) or machine != 62 or version != 1 or not entry or (
            size != 64 or phsize != 56 or not 0 < count <= 4096
            or offset < 64 or offset + phsize * count > info.st_size
        ):
            raise ValueError("Editor acceptance requires a loadable amd64 ELF")
        stream.seek(offset)
        executable_entry = False
        for _ in range(count):
            segment = stream.read(phsize)
            kind, flags, offset, address, _, filesz, memsz, _ = struct.unpack(
                "<IIQQQQQQ", segment,
            )
            if kind == 1:
                if filesz > memsz or offset + filesz > info.st_size:
                    raise ValueError("Editor ELF has an invalid load segment")
                executable_entry |= bool(flags & 1 and address <= entry < address + memsz)
        if not executable_entry:
            raise ValueError("Editor ELF has no executable entry segment")


def payload_record(source, payload, lock):
    manifest_path = source / "products/editor/apps/cosmic-edit/app.json"
    original = manifest_path.read_bytes()
    if (payload / "app.json").read_bytes() != original:
        raise ValueError("Native fixture must preserve the exact product manifest")
    manifest = json.loads(original)
    if manifest.get("id") != APP or manifest.get("runtime") != "binary" or (
        manifest.get("entry") != ENTRY
        or manifest.get("mcp", {}).get("entry") != ENTRY
        or manifest.get("desktop", {}).get("entry", ENTRY) != ENTRY
        or manifest.get("desktop", {}).get("exec") != "--gui"
    ):
        raise ValueError("Editor fixture must use its normal signed GUI/MCP entry")
    if (payload / ".provenance.json").exists():
        raise ValueError("The private namespace, not preparation, signs this fixture")
    platform = json.loads((source / "platform.lock.json").read_text())
    if platform.get("version") != "0.2.1" or platform.get("runtime_abi") != 1 or (
        platform.get("url") != (
            "https://github.com/xiaoyu-work/claw-os/releases/download/"
            "app-platform-v0.2.1/claw-os-app-platform-0.2.1.tar.gz"
        )
    ):
        raise ValueError("Editor acceptance requires the published 0.2.1 platform pin")
    for path, original in (
        (payload / "LICENSE", source / "products/editor/native/LICENSE"),
        (payload / "licenses/claw-app-gui-argv/LICENSE", source / "LICENSE"),
    ):
        if path.read_bytes() != original.read_bytes():
            raise ValueError("Native fixture lost an original product/support license")
    executable_elf(payload / ENTRY)
    records = inventory(payload)
    files = {record["path"]: record for record in records if record["type"] == "file"}
    for resource in RESOURCES:
        if resource not in files or files[resource]["size"] == 0:
            raise ValueError(f"Missing original Editor installer resource: {resource}")
    return {
        "schema": "claw.private-native-editor/v1", "source_lock": lock,
        "platform_lock": platform, "entrypoints": [ENTRY],
        "resources": sorted(name for name in files if name.startswith("resources/")),
        "payload_inventory": records,
    }


def prepare(output, target):
    if sys.platform != "linux" or os.geteuid() == 0:
        raise ValueError("Prepare as an ordinary Linux build user, never as Root")
    output = output.resolve()
    if output == ROOT / "build" or not output.is_relative_to((ROOT / "build").resolve()):
        raise ValueError("Editor fixture output must be a new directory under build/")
    output.mkdir(parents=True, exist_ok=False)
    selected = app_sources.read_lock()
    source = app_sources.prepare_sources(selected)
    lock = app_sources.resolved_lock(selected, source)
    app_sources.locked_app_paths(lock, source)
    app_sources.write_lock(output / "app-sources.json", lock)
    platform_bytes = (source / "platform.lock.json").read_bytes()
    staged = source / "build" / f"native-editor-acceptance-{os.getpid()}" / APP
    subprocess.run(
        [sys.executable, "-B", str(source / "tools/native_build.py"),
         "editor", "build", "--release", "--target-dir", str(target.resolve()),
         "--app-root", str(staged)],
        check=True, cwd=source,
    )
    # Reuse the resolved lock: do not resolve a newer main after the build.
    if app_sources.prepare_sources(lock) != source or (
        (source / "platform.lock.json").read_bytes() != platform_bytes
    ):
        raise ValueError("Editor source/platform inputs changed during preparation")
    record = payload_record(source, staged, lock)
    payload = output / APP
    shutil.copytree(staged, payload, symlinks=True)
    if inventory(payload) != record["payload_inventory"]:
        raise ValueError("Editor payload bytes or modes changed during fixture staging")
    (output / "native-editor.json").write_text(
        json.dumps(record, sort_keys=True, indent=2) + "\n", encoding="utf-8",
    )
    print(json.dumps({
        "payload": str(payload), "receipt": str(output / "native-editor.json"),
        "app_revision": lock["revision"], "platform": record["platform_lock"],
        "purpose": "private-acceptance-only; unsigned; not installed or published",
    }, sort_keys=True))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--target-dir", type=Path, required=True)
    options = parser.parse_args()
    prepare(options.output, options.target_dir)


if __name__ == "__main__":
    main()
