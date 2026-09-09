"""Build the versioned development SDK consumed by independently released Apps."""

import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import posixpath
import re
import subprocess
import tarfile
import tempfile


ROOT = Path(__file__).resolve().parents[1]
SOURCE_ROOTS = ("claw-os-sdk", "cos-runtime", "desktop/toolkit", "desktop/launcher-backend")
EXPORT_NAMES = {
    "python-sdk", "python-runtime", "rust-sdk", "rust-runtime",
    "ui-toolkit", "launcher-client",
}
SCHEMA = "claw.app-platform/v1"
SEMVER = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")


def source_path(path):
    return any(path == root or path.startswith(root + "/") for root in SOURCE_ROOTS)


def read_contract(root):
    contract = json.loads((root / "packaging/app-platform.json").read_text(encoding="utf-8"))
    if set(contract) != {"schema", "version", "runtime_abi", "exports"}:
        raise ValueError("Unexpected App platform contract fields")
    if contract["schema"] != SCHEMA or not SEMVER.fullmatch(contract["version"]):
        raise ValueError("Invalid App platform schema or release version")
    if type(contract["runtime_abi"]) is not int or contract["runtime_abi"] < 1:
        raise ValueError("App platform runtime ABI must be a positive integer")
    exports = contract["exports"]
    if set(exports) != EXPORT_NAMES:
        raise ValueError("App platform must declare exactly the supported library exports")
    for path in exports.values():
        validate_path(path)
        if not source_path(path):
            raise ValueError(f"App platform export is not an SDK library: {path}")
    return contract


def validate_path(path):
    if not isinstance(path, str) or not path or "\\" in path:
        raise ValueError(f"Invalid platform path: {path!r}")
    parsed = PurePosixPath(path)
    if parsed.is_absolute() or str(parsed) != path or ".." in parsed.parts:
        raise ValueError(f"Noncanonical platform path: {path}")
    if any(ord(char) < 32 or ord(char) == 127 for char in path):
        raise ValueError("Control character in platform path")


def git(root, *args):
    return subprocess.check_output(
        ["git", "-C", str(root), *args], stderr=subprocess.PIPE
    )


def tracked_payload(root, archive):
    payload = []
    seen = set()
    with tarfile.open(archive, "r:") as source:
        for member in source:
            path = member.name.rstrip("/")
            validate_path(path)
            if path in seen:
                raise ValueError(f"Duplicate platform path: {path}")
            seen.add(path)
            if not source_path(path):
                if member.isdir() and any(base.startswith(path + "/") for base in SOURCE_ROOTS):
                    continue
                raise ValueError(f"Private OS source in platform artifact: {path}")
            item = {"path": path, "mode": member.mode & 0o777}
            if not member.issym() and item["mode"] & 0o022:
                raise ValueError(f"Writable platform library: {path}")
            if member.isdir():
                item.update(kind="directory", size=0)
                data = None
            elif member.issym():
                target = member.linkname
                if not target or "\\" in target or PurePosixPath(target).is_absolute():
                    raise ValueError(f"Unsafe platform symlink: {path}")
                resolved = posixpath.normpath(posixpath.join(posixpath.dirname(path), target))
                if not source_path(resolved):
                    raise ValueError(f"Platform symlink escapes the SDK exports: {path}")
                item.update(kind="symlink", size=0, target=target)
                data = None
            elif member.isfile():
                stream = source.extractfile(member)
                if stream is None:
                    raise ValueError(f"Unreadable platform source: {path}")
                data = stream.read()
                if data.startswith(b"version https://git-lfs.github.com/spec/v1\n"):
                    pointer = data.decode("ascii").splitlines()
                    oid = next((line[11:] for line in pointer if line.startswith("oid sha256:")), "")
                    size = next((line[5:] for line in pointer if line.startswith("size ")), "")
                    data = (root / path).read_bytes()
                    if hashlib.sha256(data).hexdigest() != oid or str(len(data)) != size:
                        raise ValueError(f"Fetch the committed Git LFS object before publishing: {path}")
                item.update(kind="file", size=len(data), sha256=hashlib.sha256(data).hexdigest())
            else:
                raise ValueError(f"Unsupported platform node: {path}")
            payload.append((item, data))
    return sorted(payload, key=lambda entry: entry[0]["path"])


def write_archive(path, manifest, payload, timestamp):
    data = (json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\n").encode()
    with path.open("xb") as output:
        with gzip.GzipFile(filename="", mode="wb", fileobj=output, mtime=timestamp) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as archive:
                metadata = tarfile.TarInfo("platform.json")
                metadata.mode = 0o644
                metadata.size = len(data)
                metadata.mtime = timestamp
                archive.addfile(metadata, io.BytesIO(data))
                for item, body in payload:
                    member = tarfile.TarInfo(item["path"])
                    member.mode = item["mode"]
                    member.mtime = timestamp
                    member.size = item["size"]
                    if item["kind"] == "directory":
                        member.type = tarfile.DIRTYPE
                    elif item["kind"] == "symlink":
                        member.type = tarfile.SYMTYPE
                        member.linkname = item["target"]
                    archive.addfile(member, io.BytesIO(body) if body is not None else None)


def build(output, version, root=ROOT):
    root = Path(root)
    output = Path(output)
    contract = read_contract(root)
    if version != contract["version"]:
        raise ValueError("Release version must match packaging/app-platform.json")
    committed_contract = json.loads(git(root, "show", "HEAD:packaging/app-platform.json"))
    if committed_contract != contract:
        raise ValueError("Commit the platform contract before publishing")
    revision = git(root, "rev-parse", "HEAD").decode().strip()
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise ValueError("Platform publication requires an immutable source revision")
    if git(root, "diff", "--name-only", "HEAD", "--", *SOURCE_ROOTS).strip():
        raise ValueError("Commit the SDK libraries before publishing")
    timestamp = int(git(root, "show", "-s", "--format=%ct", "HEAD").strip())
    output.mkdir(parents=True, exist_ok=True)
    name = f"claw-os-app-platform-{version}.tar.gz"
    destination = output / name
    checksums = output / "SHA256SUMS"
    if destination.exists() or checksums.exists():
        raise FileExistsError("Platform release output already exists; versions are immutable")
    with tempfile.TemporaryDirectory(prefix=".app-platform-", dir=output) as temporary:
        temporary = Path(temporary)
        source_archive = temporary / "source.tar"
        subprocess.run(
            ["git", "-C", str(root), "-c", "tar.umask=0022", "archive", "--format=tar",
             f"--output={source_archive}", revision, "--", *SOURCE_ROOTS],
            check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        payload = tracked_payload(root, source_archive)
        directories = {item["path"] for item, _ in payload if item["kind"] == "directory"}
        for name, path in contract["exports"].items():
            if path not in directories:
                raise ValueError(f"Missing platform export {name}: {path}")
        manifest = {**contract, "source_revision": revision, "files": [item for item, _ in payload]}
        candidate = temporary / destination.name
        write_archive(candidate, manifest, payload, timestamp)
        with candidate.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        checksum_file = temporary / "SHA256SUMS"
        checksum_file.write_text(f"{digest}  {destination.name}\n", encoding="ascii")
        os.link(candidate, destination)
        os.link(checksum_file, checksums)
    return destination


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--output", type=Path, default=ROOT / "build/app-platform-release")
    args = parser.parse_args()
    try:
        print(build(args.output, args.version))
    except (OSError, ValueError, subprocess.CalledProcessError, tarfile.TarError) as error:
        parser.exit(1, f"error: {error}\n")


if __name__ == "__main__":
    main()
