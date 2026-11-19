"""Embed the immutable signed App payload into claw-os-desktop."""

import argparse
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import tarfile
import tempfile

from app_packages import DESKTOP_SHELL_ROOT, ROOT, run, sha256, work_root


APP = "desktop-shell"
ENTRY = "bin/claw-desktop-shell"
SHELL_QML = "resources/usr/share/claw-desktop-shell/shell.qml"
BUILD_FORMAT = "claw.desktop-shell-build/v1"
MAX_BYTES = 264 * 1024 * 1024
MAX_FILES = 20_001
PUBLISHER = ROOT / "packaging/deb/claw-os-agent/trust/publishers.d/clawos-app.json"
RELEASE = "https://github.com/xiaoyu-work/clawos-app/releases/download"
VERIFIER = "https://github.com/xiaoyu-work/claw-os/releases/download/sdk-v0.2.1"
VERIFIER_SHA256 = {
    "amd64": "df30cf7d57fe3dc81532e2c5599ce61de91ad3a625d1305b2f848640f5ed2a9b",
    "arm64": "613a592f1c03602853b15d4316d425bfa17c60fba1a3a757e4c43535e3d0be70",
}


def download(url, limit, destination=None):
    output = ["--output", destination] if destination else []
    return run([
        "curl", "--proto", "=https", "--proto-redir", "=https", "--fail", "--silent", "--show-error",
        "--location", "--max-time", "600", "--max-filesize", str(limit), *output, url,
    ]).stdout


def inputs(architecture):
    return work_root() / f"desktop-shell-{architecture}"


def host_architecture():
    return run(["dpkg", "--print-architecture"]).stdout.decode().strip()


def verifier(architecture):
    return work_root() / f"cos-provenance-linux-{architecture}"


def verify(root, architecture, pin):
    host = host_architecture()
    tool = verifier(host)
    if not tool.is_file() or sha256(tool) != VERIFIER_SHA256[host]:
        raise ValueError("Missing or changed public Desktop Shell verifier")
    publisher = json.loads(PUBLISHER.read_text())["keys"][0]["key_id"]
    with tempfile.TemporaryDirectory(prefix="claw-desktop-shell-trust-") as home:
        trust = Path(home) / ".config/cos/trust/publishers.d"
        trust.mkdir(mode=0o700, parents=True)
        shutil.copyfile(PUBLISHER, trust / PUBLISHER.name)
        result = subprocess.run(
            [tool, "provenance", "verify", "--kind", "app", "--id", APP, "--path", root],
            env={"HOME": home, "PATH": os.defpath, "LC_ALL": "C"},
            check=False, capture_output=True,
        )
    if result.returncode:
        raise ValueError("Desktop Shell failed public App provenance verification")
    report = json.loads(result.stdout)
    package = report["package"]
    if report["verified"] is not True or report["manifest_path"] != "app.json" or (
        report["entrypoints"] != [ENTRY] or package["trust"] != "publisher"
        or package["publisher_key_id"] != publisher
    ):
        raise ValueError("Desktop Shell requires its existing App publisher and GUI entrypoint")
    envelope = root / ".provenance.json"
    if json.loads(envelope.read_text())["package"]["resources"] != [SHELL_QML]:
        raise ValueError("Desktop Shell requires signed private resources")
    if pin.get("envelope_sha256") not in (None, sha256(envelope)):
        raise ValueError("Desktop Shell differs from the frozen release input")
    manifest = json.loads((root / "app.json").read_text())
    if manifest["id"] != APP or manifest["version"] != package["version"] or (
        manifest["runtime"] != "binary" or manifest["entry"] != ENTRY
        or manifest["desktop"]["exec"] != "--gui"
    ):
        raise ValueError("Signed Desktop Shell manifest does not match the GUI launch contract")
    with (root / ENTRY).open("rb") as stream:
        header = stream.read(20)
    if header[:7] != b"\x7fELF\x02\x01\x01" or int.from_bytes(header[18:20], "little") != {
        "amd64": 62, "arm64": 183,
    }[architecture]:
        raise ValueError("Desktop Shell does not match the target architecture")
    build = json.loads((root / "build.json").read_text())
    if (build["format"], build["architecture"], build["release_version"], build["source_revision"]) != (
        BUILD_FORMAT, architecture, pin["release_version"], pin["source_revision"],
    ):
        raise ValueError("Signed Desktop Shell does not match the selected release and source revision")
    depends = build["depends"]
    if not isinstance(depends, list) or not depends or any(
        not isinstance(item, str) or not re.fullmatch(r"[a-z0-9][a-z0-9.+-]*(?: \(>= [A-Za-z0-9.+:~-]+\))?", item)
        for item in depends
    ):
        raise ValueError("Invalid signed Desktop Shell Debian runtime dependencies")
    return ", ".join(depends)


def extract(archive, root, checksum):
    if not archive.is_file() or archive.stat().st_size > MAX_BYTES or sha256(archive) != checksum:
        raise ValueError("Missing or changed Desktop Shell archive")
    root.mkdir(mode=0o755)
    nodes = set()
    size = 0
    with tarfile.open(archive, mode="r|gz") as payload:
        for member in payload:
            path = PurePosixPath(member.name)
            size += member.size
            if (
                path.is_absolute() or path.as_posix() != member.name or not path.parts or ".." in path.parts
                or path in nodes or len(nodes) >= MAX_FILES or size > MAX_BYTES or member.size < 0
                or member.uid or member.gid or member.mode & 0o7022
                or (not member.isdir() and not member.isfile()) or member.issparse()
                or set(member.pax_headers) - {"path"}
            ):
                raise ValueError(f"Invalid Desktop Shell archive entry: {member.name}")
            payload.extract(member, root, filter="data")
            (root / path).chmod(member.mode)
            nodes.add(path)


def prepare(architecture, version):
    if not re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", version):
        raise ValueError("Select an approved MAJOR.MINOR.PATCH Desktop Shell release")
    tag = f"desktop-shell-v{version}"
    asset = f"desktop-shell-linux-{architecture}.tar.gz"
    release = json.loads(download(f"https://api.github.com/repos/xiaoyu-work/clawos-app/releases/tags/{tag}", 2**20))
    if release["immutable"] is not True or release["draft"] or release["tag_name"] != tag:
        raise ValueError("Desktop Shell release must be published and immutable")
    expected = [entry["digest"] for entry in release["assets"] if entry["name"] == asset]
    revision = release["target_commitish"]
    if len(expected) != 1 or not re.fullmatch(r"sha256:[0-9a-f]{64}", expected[0] or "") or (
        not re.fullmatch(r"[0-9a-f]{40}", revision)
    ):
        raise ValueError("Desktop Shell release requires its exact source revision and archive SHA256")
    pin = {"release_version": version, "source_revision": revision, "archive_sha256": expected[0][7:]}
    directory = inputs(architecture)
    directory.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="prepare-", dir=directory) as scratch:
        scratch = Path(scratch)
        for arch in {architecture, host_architecture()}:
            tool = verifier(arch)
            if tool.is_file() and sha256(tool) == VERIFIER_SHA256[arch]:
                continue
            candidate = scratch / tool.name
            download(f"{VERIFIER}/{tool.name}", 128 * 1024 * 1024, candidate)
            if sha256(candidate) != VERIFIER_SHA256[arch]:
                raise ValueError("Public Desktop Shell verifier SHA256 mismatch")
            candidate.chmod(0o755)
            candidate.replace(tool)
        archive = scratch / "payload.tar.gz"
        download(f"{RELEASE}/{tag}/{asset}", MAX_BYTES, archive)
        root = scratch / APP
        extract(archive, root, pin["archive_sha256"])
        verify(root, architecture, pin)
        pin["envelope_sha256"] = sha256(root / ".provenance.json")
        (scratch / "selection.json").write_text(json.dumps(pin, sort_keys=True) + "\n")
        archive.replace(directory / archive.name)
        (scratch / "selection.json").replace(directory / "selection.json")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["prepare", "stage", "check"])
    parser.add_argument("--architecture", choices=["amd64", "arm64"], required=True)
    parser.add_argument("--version")
    parser.add_argument("--stage", type=Path)
    args = parser.parse_args()
    if args.command == "prepare":
        if not args.version:
            parser.error("prepare requires --version")
        prepare(args.architecture, args.version)
        return
    if not args.stage:
        parser.error("stage and check require --stage")
    directory = inputs(args.architecture)
    selection = directory / "selection.json"
    if not selection.is_file():
        raise ValueError("Approved Desktop Shell inputs are required; run scripts/desktop_shell.py prepare")
    pin = json.loads(selection.read_text())
    if not re.fullmatch(r"[0-9a-f]{64}", pin["envelope_sha256"]):
        raise ValueError("Missing frozen Desktop Shell envelope digest")
    root = args.stage / DESKTOP_SHELL_ROOT
    if args.command == "stage":
        root.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix=".shell-", dir=root.parent) as scratch:
            candidate = Path(scratch) / APP
            extract(directory / "payload.tar.gz", candidate, pin["archive_sha256"])
            verify(candidate, args.architecture, pin)
            if root.exists() or root.is_symlink():
                raise ValueError("Desktop Shell staging path already exists")
            candidate.rename(root)
    else:
        print(verify(root, args.architecture, pin))


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        raise SystemExit(f"error: Desktop Shell input failed: {error.stderr.decode(errors='replace').strip()}")
    except (ValueError, KeyError, OSError, tarfile.TarError) as error:
        raise SystemExit(f"error: Desktop Shell input failed: {error}")
