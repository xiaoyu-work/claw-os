"""Stage the pinned NVIDIA ARM64 user-space driver without running its installer."""

import re
import subprocess

from surface_laptop7 import stage_file


LIBRARIES = (
    "EGL_nvidia", "GLESv1_CM_nvidia", "GLESv2_nvidia", "GLX_nvidia", "cuda", "nvcuvid",
    "nvidia-allocator", "nvidia-cfg", "nvidia-eglcore", "nvidia-encode", "nvidia-fbc", "nvidia-fmdrv",
    "nvidia-glcore", "nvidia-glsi", "nvidia-glvkspirv", "nvidia-gpucomp", "nvidia-imex", "nvidia-ml",
    "nvidia-ngx", "nvidia-nvvm", "nvidia-opticalflow", "nvidia-present", "nvidia-ptxjitcompiler",
    "nvidia-rmapi-tegra", "nvidia-rtcore", "nvidia-sandboxutils", "nvidia-tileiras", "nvidia-tls",
)


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
