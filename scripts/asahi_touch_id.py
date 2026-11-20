"""Build a private libfprint for the opt-in Aurora Touch ID service."""

import shutil
import subprocess
from pathlib import Path

from surface_laptop7 import extract, patch, stage_file, write_file


ASSETS = Path(__file__).resolve().parents[1] / "rootfs/features/kernel-platform/apple"


def asahi_touch_id_package(profile, source, provenance, work, out, package):
    key = "asahi-libfprint"
    recipes_key = "amd64-hardware-recipes"
    prefix = "pkgbuilds/libfprint-git/"
    tree = work / "touch-id-source"
    recipes = work / "touch-id-recipes"
    build = work / "touch-id-build"
    install = work / "touch-id-install"
    stage = work / "touch-id"
    name = "claw-os-asahi-touch-id"
    extract(source[key], tree)
    extract(source[recipes_key], recipes, prefixes=(prefix,))
    docs = stage / "usr/share/doc" / name
    for file in ("aurora-apple-sep.patch", "aurora-touchid-retry.patch", "aurora-touchid-lid-switch.patch"):
        fix = recipes / prefix / file
        patch(tree, fix)
        stage_file(fix, docs / file)
    subprocess.run(["meson", "setup", build, tree, "--prefix=/usr", "--libdir=lib", "--buildtype=release",
                    "-Ddrivers=all", "-Ddoc=false", "-Dintrospection=false", "-Dinstalled-tests=false"], check=True)
    subprocess.run(["meson", "compile", "-C", build], check=True)
    subprocess.run(["meson", "install", "-C", build, "--destdir", install, "--no-rebuild"], check=True)
    lib = stage / "usr/lib/cos/asahi/touch-id"
    lib.mkdir(parents=True)
    for file in (install / "usr/lib").glob("libfprint-2.so.*"):
        shutil.copy2(file, lib / file.name, follow_symlinks=False)
    if not (lib / "libfprint-2.so.2").is_file():
        raise ValueError("the private Touch ID library must retain the fprintd ABI")
    stage_file(tree / "COPYING", docs / "copyright")
    stage_file(source[key], docs / "libfprint-source.tar.gz")
    stage_file(ASSETS / "claw-asahi-touch-id", stage / "usr/lib/cos/asahi/claw-asahi-touch-id", 0o755)
    stage_file(ASSETS / "claw-asahi-fprintd", stage / "usr/lib/cos/asahi/claw-asahi-fprintd", 0o755)
    (stage / "usr/sbin").mkdir(parents=True)
    (stage / "usr/sbin/claw-asahi-touch-id").symlink_to("../lib/cos/asahi/claw-asahi-touch-id")
    release = profile["release"]
    write_file(stage / "usr/share/cos/asahi/touch-id-kernel", release + "\n")
    for file, target in (
        ("claw-asahi-touch-id.service", "usr/lib/systemd/system"),
        ("70-claw-asahi-fprintd.conf", "usr/lib/systemd/system/fprintd.service.d"),
        ("80-claw-asahi-touch-id.conf", "usr/share/cos/asahi"),
        ("70-claw-asahi-touch-id.rules", "usr/lib/udev/rules.d"),
    ):
        stage_file(ASSETS / file, stage / target / file)
    package(stage, out, name, "1-1", "arm64", f"linux-image-{release.lower()} (= {release.lower()}+clawos2), "
            "fprintd, libglib2.0-0t64, libgudev-1.0-0, libgusb2, libpixman-1-0, libssl3t64, "
            "libgcc-s1, libstdc++6, libc6, kmod, python3, systemd, udev",
            "Explicitly enabled Touch ID on matching Apple Silicon Macs", {k: provenance[k] for k in (key, recipes_key)}, None)
