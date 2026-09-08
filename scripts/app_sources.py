"""Resolve pinned application sources for OS package assembly, never at runtime."""

import argparse
import json
from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]


def read_lock():
    lock = json.loads((ROOT / "packaging" / "apps.lock.json").read_text())
    if lock["version"] != 1 or not re.fullmatch(r"[0-9a-f]{40}", lock["revision"]):
        raise ValueError("App sources require a version-1 lock with a full Git revision")
    for field in ("products", "apps"):
        values = lock[field]
        if not isinstance(values, list) or not values or any(
            not isinstance(value, str) or not re.fullmatch(r"[a-z][a-z0-9-]*", value)
            for value in values
        ) or len(set(values)) != len(values):
            raise ValueError(f"Invalid or duplicate {field} in App source lock")
    return lock


def prepare_sources(lock):
    revision = lock["revision"]
    destination = ROOT / "build" / "app-sources" / revision
    if not destination.exists():
        destination.mkdir(parents=True)
        git = ["git", "-C", str(destination)]
        subprocess.run([*git, "init", "--quiet"], check=True)
        subprocess.run([*git, "remote", "add", "origin", lock["repository"]], check=True)
        subprocess.run([*git, "fetch", "--quiet", "--depth=1", "origin", revision], check=True)
        subprocess.run([*git, "checkout", "--quiet", "--detach", revision], check=True)
    git = ["git", "-C", str(destination)]
    actual = subprocess.check_output([*git, "rev-parse", "HEAD"], text=True).strip()
    if actual != revision:
        raise RuntimeError("App source cache does not match the locked revision")
    dirty = subprocess.check_output(
        [*git, "status", "--porcelain", "--untracked-files=normal"], text=True
    )
    if dirty:
        raise RuntimeError(f"App source cache is modified: {destination}")
    return destination


def stage_products(destination):
    lock = read_lock()
    source = prepare_sources(lock)
    installed = []
    for product in lock["products"]:
        result = subprocess.check_output(
            [sys.executable, str(source / "tools" / "stage.py"),
             product, "--root", str(destination.resolve())],
            cwd=source, text=True,
        )
        installed.extend(json.loads(result))
    if sorted(installed) != sorted(lock["apps"]):
        raise RuntimeError("Staged App identities do not match the OS source lock")
    return installed


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stage", type=Path)
    args = parser.parse_args()
    if args.stage:
        print(len(stage_products(args.stage)))
    else:
        print(prepare_sources(read_lock()))
