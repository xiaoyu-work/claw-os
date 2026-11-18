#!/usr/bin/env python3
"""Extract the pinned Debian-target GCC compiler without installing it."""

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import subprocess
import tarfile
import tempfile
import urllib.request


ROOT = Path(__file__).resolve().parent


class HttpsRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, url):
        if not url.startswith("https://"):
            raise ValueError("The compiler registry must use HTTPS")
        return super().redirect_request(request, response, code, message, headers, url)


HTTP = urllib.request.build_opener(HttpsRedirect())


def fetch(url, destination, sha256, limit, token=None):
    request = urllib.request.Request(url)
    if token:
        request.add_unredirected_header("Authorization", "Bearer " + token)
    digest = hashlib.sha256()
    size = 0
    with HTTP.open(request, timeout=60) as response, destination.open("xb") as target:
        while chunk := response.read(1024 * 1024):
            size += len(chunk)
            if size > limit:
                raise ValueError("Compiler download exceeds its pinned size")
            digest.update(chunk)
            target.write(chunk)
    if digest.hexdigest() != sha256:
        raise ValueError("Compiler input does not match the pinned digest")
    return size


def prepare(work):
    pin = json.loads((ROOT / "toolchain.lock.json").read_bytes())
    arch = subprocess.check_output(["dpkg", "--print-architecture"], text=True).strip()
    layer = pin["compiler_layers"][arch]
    directory = work / f"gcc-{pin['version']}-{arch}-{layer['sha256'][:12]}"
    prefix = directory / "usr/local"
    if not directory.exists():
        with tempfile.TemporaryDirectory(prefix="gcc-download-", dir=work) as temporary:
            temporary = Path(temporary)
            token_url = "https://auth.docker.io/token?service=registry.docker.io&scope=repository:library/gcc:pull"
            with HTTP.open(token_url, timeout=30) as reply:
                token = json.loads(reply.read(64 * 1024))["token"]
            archive = temporary / "compiler.tar.gz"
            url = f"https://registry-1.docker.io/v2/library/gcc/blobs/sha256:{layer['sha256']}"
            if fetch(url, archive, layer["sha256"], layer["size"], token) != layer["size"]:
                raise ValueError("Compiler layer is incomplete")
            staged = temporary / "staged"
            staged.mkdir()
            with tarfile.open(archive, "r:gz") as source:
                source.extractall(staged, members=(
                    member for member in source
                    if PurePosixPath(member.name).parts[:2] == ("usr", "local")
                ), filter="data")
            notice_dir = staged / "usr/local/claw-notices"
            notice_dir.mkdir()
            version = pin["version"]
            origin = f"https://raw.githubusercontent.com/gcc-mirror/gcc/releases/gcc-{version}"
            for name, digest in pin["notices"].items():
                fetch(f"{origin}/{name}", notice_dir / name, digest, 64 * 1024)
            staged.rename(directory)
    actual = subprocess.check_output([prefix / "bin/g++", "-dumpfullversion"], text=True).strip()
    if actual != pin["version"]:
        raise ValueError("Private compiler version mismatch")
    return prefix


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("work", type=Path)
    options = parser.parse_args()
    print(prepare(options.work.resolve()))


if __name__ == "__main__":
    main()
