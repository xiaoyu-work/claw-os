#!/usr/bin/env python3
"""Build the pinned native compositor dependencies into a private Linux cache."""

import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parent
GIT = ["git", "-c", "core.autocrlf=false", "-c", "core.eol=lf"]


def run(arguments, *, environment=None, capture=False):
    return subprocess.run(
        [str(argument) for argument in arguments],
        check=True,
        env=environment,
        text=True,
        stdout=subprocess.PIPE if capture else None,
    )


def checkout(source, cache):
    destination = cache / "sources" / f"{source['name']}-{source['revision']}"
    if not destination.exists():
        destination.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="source-", dir=destination.parent,
        ) as temporary:
            candidate = Path(temporary) / "checkout"
            run([*GIT, "init", "--quiet", candidate])
            run([
                *GIT, "-C", candidate, "fetch", "--quiet", "--depth=1", "--no-tags",
                source["repository"], source["revision"],
            ])
            run([
                *GIT, "-C", candidate, "-c", "advice.detachedHead=false",
                "checkout", "--quiet", "--detach", "FETCH_HEAD",
            ])
            candidate.rename(destination)
    if destination.is_symlink():
        raise ValueError(f"Native source checkout is a symlink: {destination}")
    actual = run(
        [*GIT, "-C", destination, "rev-parse", "HEAD"], capture=True,
    ).stdout.strip()
    if actual != source["revision"]:
        raise ValueError(f"Native source revision changed: {destination}")
    run([*GIT, "-C", destination, "diff", "--exit-code", "--quiet"])
    run([*GIT, "-C", destination, "diff", "--cached", "--exit-code", "--quiet"])
    return destination


def build(source, cache, prefix, environment, jobs):
    if "archive" in source:
        name = f"{source['name']}-{source['version']}"
        downloads = cache / "downloads"
        downloads.mkdir(exist_ok=True)
        archive = downloads / source["sha256"]
        if not archive.exists():
            with tempfile.TemporaryDirectory(dir=downloads) as temporary:
                download = Path(temporary) / "download"
                run(["curl", "--proto", "=https", "--proto-redir", "=https", "-fLsS",
                     "--output", download, source["archive"]])
                with download.open("rb") as stream:
                    if hashlib.file_digest(stream, "sha256").hexdigest() != source["sha256"]:
                        raise ValueError(f"{name} source digest mismatch")
                download.rename(archive)
        with archive.open("rb") as stream:
            if hashlib.file_digest(stream, "sha256").hexdigest() != source["sha256"]:
                raise ValueError(f"{name} cached source digest mismatch")
        directory = cache / "sources" / name
        if not directory.exists():
            directory.parent.mkdir(exist_ok=True)
            with tempfile.TemporaryDirectory(dir=directory.parent) as temporary:
                unpacked = Path(temporary) / "source"
                unpacked.mkdir()
                run(["tar", "-xf", archive, "-C", unpacked, "--strip-components=1", "--no-same-owner"])
                unpacked.rename(directory)
    else:
        directory = checkout(source, cache)
        name = f"{source['name']}-{source['revision']}"
    output = cache / "build" / name
    if source["build"] == "lua":
        run(["make", "-C", directory / "src", "-j", jobs, "linux", "MYCFLAGS=-fPIC"],
            environment=environment)
        run(["make", "-C", directory, "install", f"INSTALL_TOP={prefix}"], environment=environment)
        pc = prefix / "lib/pkgconfig"
        pc.mkdir(parents=True, exist_ok=True)
        (pc / "lua5.5.pc").write_text(
            f"prefix={prefix}\nName: Lua\nDescription: Lua embedded language\n"
            f"Version: {source['version']}\nLibs: -L${{prefix}}/lib -llua -lm -ldl\n"
            "Cflags: -I${prefix}/include\n"
        )
    elif source["build"] == "cmake":
        # FindPkgConfig asks for explicit system -L paths even when a pinned
        # .pc file supplied newer headers. Use the normal pkg-config filtering.
        run([
            "cmake", "--fresh", "-S", directory, "-B", output, "-G", "Ninja",
            "-DCMAKE_BUILD_TYPE=Release",
            "-DPKG_CONFIG_EXECUTABLE=/usr/bin/env",
            "-DPKG_CONFIG_ARGN=-u;PKG_CONFIG_ALLOW_SYSTEM_LIBS;/usr/bin/pkg-config",
            f"-DCMAKE_INSTALL_PREFIX={prefix}",
            "-DCMAKE_INSTALL_LIBDIR=lib",
            "-DCMAKE_BUILD_WITH_INSTALL_RPATH=ON",
            "-DCMAKE_INSTALL_RPATH=$ORIGIN;$ORIGIN/../lib",
            "-DCMAKE_INSTALL_RPATH_USE_LINK_PATH=OFF",
            "-DBUILD_TESTING=OFF",
        ], environment=environment)
        run([
            "cmake", "--build", output, "--parallel", jobs, "--target", "install",
        ], environment=environment)
    else:
        command = ["meson", "setup"]
        if (output / "meson-private/coredata.dat").is_file():
            command.extend(["--reconfigure", "--clearcache"])
        extra = (["-Dudev-dir=" + str(prefix / "lib/udev")]
                 if source["name"] == "libinput" else [])
        run([
            *command, f"--prefix={prefix}", "--libdir=lib",
            "--buildtype=release", "-Dtests=false",
            *extra, *source.get("meson_args", []), output, directory,
        ], environment=environment)
        run(["meson", "install", "-C", output], environment=environment)
    if "pkg_config" in source:
        actual = run(
            ["pkg-config", "--modversion", source["pkg_config"]],
            environment=environment, capture=True,
        ).stdout.strip()
        if actual != source["version"]:
            raise ValueError(f"{source['name']} version is {actual}, not {source['version']}")
    print(f"Built {name}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=Path, required=True)
    parser.add_argument("--jobs", type=int, default=min(os.cpu_count() or 1, 4))
    options = parser.parse_args()
    if sys.platform != "linux" or options.jobs < 1:
        parser.error("Use Linux/WSL with a positive build job count")
    for tool in ("git", "cmake", "ninja", "meson", "pkg-config", "curl", "tar", "make"):
        if shutil.which(tool) is None:
            parser.error(f"Required native build tool is missing: {tool}")
    cache = options.cache.resolve()
    repository = ROOT.parents[1]
    if cache in (Path("/"), Path.home(), repository) or cache.is_relative_to(repository):
        parser.error("Use an explicit external Linux build cache, not a source or system root")
    cache.mkdir(parents=True, exist_ok=True)
    for child in ("prefix", "sources", "build", "downloads", ".build.lock"):
        if (cache / child).is_symlink():
            parser.error(f"Native build cache entry is a symlink: {child}")
    filesystem = run(["stat", "-f", "-c", "%T", cache], capture=True).stdout.strip()
    if filesystem in ("9p", "drvfs"):
        parser.error("Native source/build caches must use the Linux filesystem")
    sources = json.loads((ROOT / "dependencies.lock.json").read_text())["sources"]
    names = set()
    for source in sources:
        archive = source.get("archive")
        valid_source = (
            archive.startswith("https://")
            and bool(re.fullmatch(r"[0-9a-f]{64}", source.get("sha256", "")))
            if archive is not None
            else bool(re.fullmatch(r"[0-9a-f]{40}", source.get("revision", "")))
            and source["repository"].startswith("https://github.com/")
            and source["repository"].endswith(".git")
        )
        if (
            not re.fullmatch(r"[a-z][a-z0-9-]*", source["name"])
            or source["name"] in names
            or not re.fullmatch(r"[0-9]+(?:\.[0-9]+)+", source["version"])
            or not valid_source
            or source["build"] not in ("cmake", "meson", "lua")
        ):
            raise ValueError("Invalid pinned native dependency")
        names.add(source["name"])
    prefix = cache / "prefix"
    environment = dict(os.environ)
    environment["PATH"] = str(prefix / "bin") + os.pathsep + environment.get("PATH", "")
    environment["CMAKE_PREFIX_PATH"] = str(prefix)
    environment["PKG_CONFIG_PATH"] = os.pathsep.join([
        str(prefix / "lib/pkgconfig"), str(prefix / "share/pkgconfig"),
    ])
    with (cache / ".build.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        for source in sources:
            build(source, cache, prefix, environment, options.jobs)
    print(f"Native dependency prefix: {prefix}")


if __name__ == "__main__":
    main()
