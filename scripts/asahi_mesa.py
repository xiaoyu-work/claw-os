"""Build Aurora Vulkan and OpenGL against the Debian Apple Silicon image."""

import os
import shutil
import subprocess
from pathlib import PurePosixPath

from surface_laptop7 import extract, stage_file, write_file


INPUTS = ("asahi-vulkan-source",)
LIB = PurePosixPath("/usr/lib/aarch64-linux-gnu")
DRIVER = LIB / "libvulkan_asahi.so"
GL_DRIVERS = PurePosixPath("/usr/lib/cos/asahi/mesa")
GL_LOADER_FILES = ("libEGL_mesa.so.0.0.0", "libGLX_mesa.so.0.0.0", "libgbm.so.1.0.0")
MESON_OPTIONS = [
    "--prefix=/usr", "--libdir=lib/aarch64-linux-gnu", "--buildtype=release", "--wrap-mode=nodownload",
    "-Dplatforms=x11,wayland", "-Ddisplay-info=enabled", "-Dexpat=enabled", "-Dzstd=enabled",
    "-Dlibunwind=disabled", "-Dvalgrind=disabled", "-Dbuild-tests=false",
]
VULKAN_OPTIONS = [
    "-Dgallium-drivers=", "-Dvulkan-drivers=asahi", "-Degl=disabled", "-Dgbm=disabled",
    "-Dglx=disabled", "-Dgles1=disabled", "-Dgles2=disabled",
]
PRECOMPILED_SHADERS = ["-Dspirv-tools=disabled", "-Dmesa-clc=system", "-Dprecomp-compiler=system"]


def build_shader_tools(upstream, work, env):
    tools = work / "mesa-tools"
    shader_tools = {
        "mesa_clc": "src/compiler/clc/mesa_clc",
        "vtn_bindgen2": "src/compiler/spirv/vtn_bindgen2",
        "asahi_clc": "src/asahi/clc/asahi_clc",
    }
    subprocess.run(["meson", "setup", tools, upstream, *MESON_OPTIONS, *VULKAN_OPTIONS, "-Dllvm=enabled"],
                   env=env, check=True)
    subprocess.run(["ninja", "-C", tools, "-j8", *shader_tools.values()], env=env, check=True)
    native = work / "mesa-tools.ini"
    native.write_text("[binaries]\n" + "".join(f"{name} = '{tools / path}'\n" for name, path in shader_tools.items()))
    return native


def build_vulkan_driver(upstream, work, native, env):
    build = work / "mesa-vulkan"
    # The OpenCL/LLVM tools compile shaders at build time, not on the installed system.
    subprocess.run(["meson", "setup", build, upstream, *MESON_OPTIONS, *VULKAN_OPTIONS, "--native-file", native,
                    "-Dllvm=disabled", *PRECOMPILED_SHADERS], env=env, check=True)
    driver = "src/asahi/vulkan/libvulkan_asahi.so"
    subprocess.run(["ninja", "-C", build, "-j8", driver], env=env, check=True)
    return build / driver


def build_opengl_drivers(upstream, work, native, env):
    build = work / "mesa-opengl"
    install = work / "mesa-opengl-install"
    subprocess.run(["meson", "setup", build, upstream, *MESON_OPTIONS, "--native-file", native,
                    "-Dgallium-drivers=asahi,llvmpipe,softpipe,zink", "-Dvulkan-drivers=",
                    "-Degl=enabled", "-Dgbm=enabled", "-Dglx=dri", "-Dglvnd=enabled", "-Dgles1=enabled",
                    "-Dgles2=enabled", "-Dlmsensors=enabled", "-Dgallium-va=disabled",
                    "-Dllvm=enabled", "-Dshared-llvm=enabled", *PRECOMPILED_SHADERS,
                    f"-Ddri-drivers-path={GL_DRIVERS / 'dri'}", f"-Dgbm-backends-path={GL_DRIVERS / 'gbm'}"],
                   env=env, check=True)
    subprocess.run(["ninja", "-C", build, "-j8"], env=env, check=True)
    subprocess.run(["meson", "install", "-C", build, "--no-rebuild", "--destdir", install], env=env, check=True)
    return install


def stage_license(upstream, stage, name):
    doc = stage / "usr/share/doc" / name
    stage_file(upstream / "docs/license.rst", doc / "copyright")
    shutil.copytree(upstream / "licenses", doc / "licenses")


def package_honeykrisp(profile, upstream, driver, provenance, work, out, package):
    name = "claw-os-asahi-vulkan"
    stage = work / name
    stage_file(driver, stage / DRIVER.relative_to("/"))
    stage_license(upstream, stage, name)

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


def package_opengl(profile, upstream, install, provenance, work, out, package):
    name = "claw-os-asahi-opengl"
    stage = work / name
    lib = LIB.relative_to("/")
    gl = GL_DRIVERS.relative_to("/")
    source_lib = install / lib
    gallium, = source_lib.glob("libgallium-*.so")
    for member in (*GL_LOADER_FILES, gallium.name):
        stage_file(source_lib / member, stage / lib / member)
    shutil.copytree(install / gl / "dri", stage / gl / "dri", symlinks=True)
    stage_file(source_lib / "gbm/dri_gbm.so", stage / gl / "gbm/dri_gbm.so")
    for path in ("asahi_dri.so", "swrast_dri.so", "zink_dri.so"):
        if not (stage / gl / "dri" / path).is_file():
            raise ValueError(f"{path}: missing Aurora OpenGL driver")
    stage_license(upstream, stage, name)

    # Keep the stock SONAME links and preserve Debian's original ELF files for upgrades and rollback.
    paths = " ".join(str(LIB / member) for member in GL_LOADER_FILES)
    write_file(stage / "DEBIAN/preinst", f"""#!/bin/sh
set -e
case "$1" in
    install|upgrade)
        for path in {paths}; do
            dpkg-divert --package {name} --rename --divert "${{path%/*}}/.${{path##*/}}.distrib" --add "$path"
        done
        ;;
esac
""", 0o755)
    write_file(stage / "DEBIAN/triggers", "activate-noawait ldconfig\n")
    release = profile["release"].lower()
    package(stage, out, name, "26.3.0~devel20261107+clawos1", "arm64",
            f"linux-image-{release} (= {release}+clawos2), libegl-mesa0 (>= 26.1.6-1~bpo13+1), "
            "libglx-mesa0 (>= 26.1.6-1~bpo13+1), libgbm1 (>= 26.1.6-1~bpo13+1), "
            "libllvm19, libvulkan1, libc6, libstdc++6, libgcc-s1, libdrm2, libexpat1, libsensors5, "
            "libwayland-client0 (>= 1.26.0), libx11-6, libx11-xcb1, libxext6, libxxf86vm1, libxcb1, "
            "libxcb-dri3-0, libxcb-glx0, libxcb-present0, libxcb-randr0, libxcb-shm0, libxcb-sync1, "
            "libxcb-xfixes0, libxshmfence1, zlib1g, libzstd1",
            "Aurora OpenGL, EGL and GBM for Apple Silicon with software rendering",
            {"asahi-vulkan-source": provenance["asahi-vulkan-source"]}, None,
            f"""case "$1" in
    remove|abort-install)
        for path in {paths}; do
            [ "$(dpkg-divert --listpackage "$path")" = {name} ] || continue
            dpkg-divert --package {name} --rename --divert "${{path%/*}}/.${{path##*/}}.distrib" --remove "$path"
        done
        ldconfig
        ;;
esac
""")


def build_asahi_mesa(profile, source, provenance, work, out, package, *, opengl):
    upstream = work / "mesa"
    extract(source["asahi-vulkan-source"], upstream)
    env = dict(os.environ, PATH=f"/usr/lib/llvm-19/bin:{os.environ['PATH']}")
    native = build_shader_tools(upstream, work, env)
    driver = build_vulkan_driver(upstream, work, native, env)
    package_honeykrisp(profile, upstream, driver, provenance, work, out, package)
    if opengl:
        install = build_opengl_drivers(upstream, work, native, env)
        package_opengl(profile, upstream, install, provenance, work, out, package)
