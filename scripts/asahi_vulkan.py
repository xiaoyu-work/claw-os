"""Package the signed Aurora Vulkan driver without replacing Debian Mesa."""

import shutil
import subprocess
from pathlib import PurePosixPath

from surface_laptop7 import extract, stage_file, write_file


INPUTS = ("asahi-vulkan", "asahi-vulkan-source", "asahi-display-info")
DRIVER = PurePosixPath("/usr/lib/aarch64-linux-gnu/libvulkan_asahi.so")


def build_asahi_vulkan(profile, source, provenance, work, out, package, unpack):
    name = "claw-os-asahi-vulkan"
    stage = work / name
    upstream = work / "asahi-vulkan"
    member = "usr/lib/libvulkan_asahi.so"
    unpack(source["asahi-vulkan"], upstream, lambda path: path == member)
    stage_file(upstream / member, stage / DRIVER.relative_to("/"))
    extract(source["asahi-vulkan-source"], upstream, prefixes=("docs/license.rst", "licenses/"))
    doc = stage / "usr/share/doc" / name
    stage_file(upstream / "docs/license.rst", doc / "copyright")
    shutil.copytree(upstream / "licenses", doc / "licenses")

    display = source["asahi-display-info"]
    fields = subprocess.check_output(
        ["dpkg-deb", "-f", display, "Package", "Version", "Architecture"], text=True
    ).splitlines()
    if fields != ["Package: libdisplay-info3", "Version: 0.3.0-1", "Architecture: arm64"]:
        raise ValueError("the Vulkan display-info dependency does not match the pinned Debian ARM64 package")
    stage_file(display, out / "libdisplay-info3_0.3.0-1_arm64.deb")

    # Debian's ICD already loads this SONAME. Keep the diverted copy out of
    # ldconfig's scan so it cannot compete with the installed driver.
    stock = DRIVER.with_name(f".{DRIVER.name}.distrib")
    divert = f"dpkg-divert --package {name} --rename --divert {stock}"
    write_file(stage / "DEBIAN/preinst", f"""#!/bin/sh
set -e
case "$1" in
    install|upgrade) {divert} --add {DRIVER} ;;
esac
""", 0o755)
    release = profile["release"].lower()
    package(stage, out, name, "26.3.0~devel20261107+clawos1", "arm64",
            f"linux-image-{release} (= {release}+clawos2), "
            "mesa-vulkan-drivers (>= 26.1.6-1~bpo13+1), libvulkan1, libdisplay-info3 (= 0.3.0-1), "
            "libc6 (>= 2.38), libstdc++6 (>= 12), libgcc-s1, libdrm2 (>= 2.4.124), libexpat1, libudev1, "
            "libwayland-client0 (>= 1.26.0), libx11-xcb1, libxcb1, libxcb-dri3-0 (>= 1.17), "
            "libxcb-present0 (>= 1.17), libxcb-xfixes0, libxcb-sync1, libxcb-randr0, "
            "libxcb-shm0, libxcb-keysyms1, libxshmfence1, zlib1g, libzstd1",
            "Aurora Honeykrisp Vulkan for the matching Apple Silicon kernel",
            {key: provenance[key] for key in INPUTS}, None,
            f"""case "$1" in
    remove|abort-install) {divert} --remove {DRIVER} ;;
esac
""")
