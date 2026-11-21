"""Build matching NVIDIA ARM64 modules and stage the original vendor runtime."""

import os
import re
import subprocess

from surface_laptop7 import extract, patch, stage_file


LIBRARIES = (
    "EGL_nvidia", "GLESv1_CM_nvidia", "GLESv2_nvidia", "GLX_nvidia", "cuda", "nvcuvid",
    "nvidia-allocator", "nvidia-cfg", "nvidia-eglcore", "nvidia-encode", "nvidia-fbc", "nvidia-fmdrv",
    "nvidia-glcore", "nvidia-glsi", "nvidia-glvkspirv", "nvidia-gpucomp", "nvidia-imex", "nvidia-ml",
    "nvidia-ngx", "nvidia-nvvm", "nvidia-opticalflow", "nvidia-present", "nvidia-ptxjitcompiler",
    "nvidia-rmapi-tegra", "nvidia-rtcore", "nvidia-sandboxutils", "nvidia-tileiras", "nvidia-tls",
)


def build_nvidia_modules(archive, version, release, tree, work, stage):
    driver = work / "nvidia-source"
    extract(archive, driver)
    recipe = work / "pkgbuilds/linux-omarchy-aarch64"
    fixes = sorted(recipe.glob("nvidia-*.patch"))
    if not fixes:
        raise ValueError("the pinned ARM recipe is missing its NVIDIA driver fixes")
    for path in fixes:
        patch(driver, path)

    cross = os.environ.get("CROSS_COMPILE", "")
    env = {key: value for key, value in os.environ.items() if key not in {"CFLAGS", "CXXFLAGS", "LDFLAGS"}}
    subprocess.run(["make", "-C", driver, "-j8", f"SYSSRC={tree}", f"SYSOUT={tree}", "ARCH=arm64",
                    "TARGET_ARCH=aarch64", f"CC={cross}gcc", f"CXX={cross}g++", f"LD={cross}ld", f"AR={cross}ar",
                    f"OBJCOPY={cross}objcopy", "IGNORE_PREEMPT_RT_PRESENCE=1", "modules"], env=env, check=True)
    destination = stage / f"usr/lib/modules/{release}/extramodules"
    for name in ("nvidia", "nvidia-drm", "nvidia-modeset", "nvidia-uvm", "nvidia-peermem"):
        module = destination / f"{name}.ko"
        stage_file(driver / "kernel-open" / module.name, module)
        kernel = subprocess.check_output(["modinfo", "-F", "vermagic", module], text=True).strip().partition(" ")[0]
        if kernel != release:
            raise ValueError(f"{name}: kernel ABI {kernel}, expected {release}")
        if name == "nvidia":
            driver_version = subprocess.check_output(["modinfo", "-F", "version", module], text=True).strip()
            if driver_version != version:
                raise ValueError(f"{name}: NVIDIA version {driver_version}, expected {version}")
        subprocess.run([f"{cross}strip", "--strip-debug", module], check=True)
        subprocess.run([tree / "scripts/sign-file", "sha512", tree / "certs/signing_key.pem",
                        tree / "certs/signing_key.x509", module], check=True)
        subprocess.run(["zstd", "-19", "--rm", "-q", module], check=True)
    stage_file(driver / "COPYING", stage / "usr/share/doc/claw-os-nvidia-platform/copyright")


def stage_nvidia_userspace(archive, version, work, stage):
    driver = work / "nvidia-driver"
    subprocess.run(["sh", archive, "--extract-only", "--target", driver], cwd=work, check=True)
    libraries = [f"lib{name}.so.{version}" for name in LIBRARIES]
    libraries += ["libnvidia-api.so.1", "libnvidia-nvvm70.so.4"]
    libdir = stage / "usr/lib"
    for name in libraries:
        target = libdir / name
        mode = 0o644 if name == f"libnvidia-nvvm.so.{version}" else 0o755
        stage_file(driver / name, target, mode)
        dynamic = subprocess.check_output(["readelf", "-d", target], text=True)
        match = re.search(r"\(SONAME\)[^\n]*\[([^/\]\n]+)\]", dynamic)
        if match:
            soname = match[1]
            base = soname.split(".so", 1)[0] + ".so"
            if soname != name:
                (libdir / soname).symlink_to(name)
            if base != soname:
                (libdir / base).symlink_to(soname)

    firmware = stage / f"usr/lib/firmware/nvidia/{version}"
    for blob in sorted((driver / "firmware").glob("*.bin")):
        stage_file(blob, firmware / blob.name)
    if not (firmware / "gsp_ga10x.bin").is_file():
        raise ValueError(f"NVIDIA {version}: missing Ada and Ampere GSP firmware")
    for path in (
        "usr/share/glvnd/egl_vendor.d/10_nvidia.json",
        "usr/share/vulkan/icd.d/nvidia_icd.json",
        "usr/share/vulkan/implicit_layer.d/nvidia_layers.json",
        "usr/share/licenses/nvidia-utils/LICENSE",
    ):
        dest = stage / path
        stage_file(driver / dest.name, dest)
    stage_file(driver / "nvidia-smi", stage / "usr/bin/nvidia-smi", 0o755)
