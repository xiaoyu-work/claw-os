#!/usr/bin/env python3
"""Build the Mail UI protocol adapter shipped alongside its native host."""

import argparse
import json
from pathlib import Path
import zipfile


EXTENSION_ID = "claw-mail-ai@claw.os"


def build_extension(source: Path, destination: Path) -> None:
    manifest = json.loads((source / "manifest.json").read_text(encoding="utf-8"))
    if manifest["browser_specific_settings"]["gecko"]["id"] != EXTENSION_ID:
        raise ValueError("Mail extension identity does not match the native host")

    assets = []
    for path in sorted(source.rglob("*")):
        relative = path.relative_to(source)
        if any(
            part.startswith(".") or part in ("__pycache__", "node_modules", "tests", "test")
            for part in relative.parts
        ) or path.name.startswith("test_") or path.suffix in (".pyc", ".swp", ".bak"):
            continue
        if path.is_symlink():
            raise ValueError(f"Mail extension asset is a symlink: {relative}")
        if path.is_dir():
            continue
        if not path.is_file():
            raise ValueError(f"Mail extension asset is not a regular file: {relative}")
        assets.append((relative.as_posix(), path))

    destination.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(destination, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for name, path in assets:
            # Fixed ZIP metadata makes the XPI independent of checkout mtimes,
            # host ownership and Windows/Linux filesystem permissions.
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            info.create_system = 3
            info.external_attr = 0o100644 << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, path.read_bytes())
    destination.chmod(0o644)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    build_extension(args.source, args.destination)
