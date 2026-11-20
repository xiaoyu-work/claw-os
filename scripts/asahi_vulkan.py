"""Build Aurora Honeykrisp from source without replacing Debian Mesa."""

import os
import shutil
import subprocess
from pathlib import PurePosixPath

from surface_laptop7 import extract, stage_file, write_file


INPUTS = ("asahi-vulkan-source",)
DRIVER = PurePosixPath("/usr/lib/aarch64-linux-gnu/libvulkan_asahi.so")


def compile_honeykrisp(archive, work):
    upstream = work / "mesa"
    tools = work / "mesa-tools"
    build = work / "mesa-build"
    extract(archive, upstream)
    env = dict(os.environ, PATH=f"/usr/lib/llvm-19/bin:{os.environ['PATH']}")
    options = [
        "--prefix=/usr", "--libdir=lib/aarch64-linux-gnu", "--buildtype=release", "--wrap-mode=nodownload",
        "-Dplatforms=x11,wayland", "-Dgallium-drivers=", "-Dvulkan-drivers=asahi",
        "-Degl=disabled", "-Dgbm=disabled", "-Dglx=disabled", "-Dgles1=disabled", "-Dgles2=disabled",
        "-Ddisplay-info=enabled", "-Dexpat=enabled", "-Dzstd=enabled",
        "-Dlibunwind=disabled", "-Dvalgrind=disabled", "-Dbuild-tests=false",
    ]
    shader_tools = {
        "mesa_clc": "src/compiler/clc/mesa_clc",
        "vtn_bindgen2": "src/compiler/spirv/vtn_bindgen2",
        "asahi_clc": "src/asahi/clc/asahi_clc",
    }
    subprocess.run(["meson", "setup", tools, upstream, *options, "-Dllvm=enabled"], env=env, check=True)
    subprocess.run(["ninja", "-C", tools, "-j8", *shader_tools.values()], env=env, check=True)
    native = work / "mesa-tools.ini"
    native.write_text("[binaries]\n" + "".join(f"{name} = '{tools / path}'\n" for name, path in shader_tools.items()))
    # The OpenCL/LLVM tools compile shaders at build time, not on the installed system.
    subprocess.run(["meson", "setup", build, upstream, *options, "--native-file", native, "-Dllvm=disabled",
                    "-Dspirv-tools=disabled", "-Dmesa-clc=system", "-Dprecomp-compiler=system"], env=env, check=True)
    driver = "src/asahi/vulkan/libvulkan_asahi.so"
    subprocess.run(["ninja", "-C", build, "-j8", driver], env=env, check=True)
    return upstream, build / driver


def package_honeykrisp(profile, upstream, driver, provenance, work, out, package):
    name = "claw-os-asahi-vulkan"
    stage = work / name
    stage_file(driver, stage / DRIVER.relative_to("/"))
    doc = stage / "usr/share/doc" / name
    stage_file(upstream / "docs/license.rst", doc / "copyright")
    shutil.copytree(upstream / "licenses", doc / "licenses")

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
    package(stage, out, name, "26.3.0~devel20261107+clawos3", "arm64",
            f"linux-image-{release} (= {release}+clawos2), "
            "mesa-vulkan-drivers (>= 26.1.6-1~bpo13+1), libvulkan1, libdisplay-info2 (>= 0.2.0), "
            "libc6 (>= 2.38), libstdc++6 (>= 12), libgcc-s1, libdrm2 (>= 2.4.124), libexpat1, libudev1, "
            "libwayland-client0 (>= 1.26.0), libx11-xcb1, libxcb1, libxcb-dri3-0 (>= 1.17), "
            "libxcb-present0 (>= 1.17), libxcb-xfixes0, libxcb-sync1, libxcb-randr0, "
            "libxcb-shm0, libxcb-keysyms1, libxshmfence1, zlib1g, libzstd1",
            "Aurora Honeykrisp Vulkan for the matching Apple Silicon kernel",
            {"asahi-vulkan-source": provenance["asahi-vulkan-source"]}, None,
            f"""case "$1" in
    remove|abort-install) {divert} --remove {DRIVER} ;;
esac
""")


def build_asahi_vulkan(profile, source, provenance, work, out, package):
    upstream, driver = compile_honeykrisp(source["asahi-vulkan-source"], work)
    package_honeykrisp(profile, upstream, driver, provenance, work, out, package)
