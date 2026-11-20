"""Build Aurora Linux with the Apple driver configuration and qualified toolchain."""

import os
from pathlib import Path
import subprocess
import sys

from asahi_boot import rust_toolchain
from surface_laptop7 import extract, stage_file


def build_asahi_kernel(release, source, work, stage):
    tree = work / "aurora"
    build = work / "aurora-build"
    recipes = "pkgbuilds/linux-aurora"
    extract(source["aurora-kernel"], tree)
    extract(source["arm-kernel-recipes"], work, prefixes=tuple(
        f"{recipes}/{name}" for name in ("config", "validate-config.py", "bindgen-kernel.py")
    ))
    recipe = work / recipes
    wrapper = recipe / "bindgen-kernel.py"
    wrapper.chmod(0o755)
    libclang = "/usr/lib/llvm-18/lib"
    clang = "/usr/lib/llvm-18/bin/"

    tools = work / "aurora-bindgen"
    env = rust_toolchain(work, "1.99.0", "--component", "rustfmt", llvm=18)
    rustfmt = Path(env["PATH"].split(os.pathsep)[0]) / "rustfmt"
    env.update(LIBCLANG_PATH=libclang, RUSTFMT=str(rustfmt))
    subprocess.run([sys.executable, tree / "tools/aurora-bindgen/build.py", "--output-dir", tools, "--jobs", "2"],
                   env=env, check=True)
    env = rust_toolchain(work, "1.85.0", "--component", "rust-src", llvm=18)
    env.update(LIBCLANG_PATH=libclang, RUSTFMT=str(rustfmt), KBUILD_BUILD_HOST="clawos", KBUILD_BUILD_USER="clawos",
               AURORA_BINDGEN_REAL=str(tools / "rust-bindgen-0.73.2/target/release/bindgen"))
    make = ["make", "-C", tree, f"O={build}", "ARCH=arm64", f"LLVM={clang}", f"BINDGEN={wrapper}", "LOCALVERSION="]

    config = build / ".config"
    fragment = tree / "arch/arm64/configs/asahi.config"
    stage_file(recipe / "config", config)
    subprocess.run([tree / "scripts/kconfig/merge_config.sh", "-m", "-O", build, config, fragment],
                   cwd=tree, env=env, check=True)
    subprocess.run([tree / "scripts/config", "--file", config, "--set-str", "LOCALVERSION",
                    "-" + release.split("-", 1)[1], "--disable", "LOCALVERSION_AUTO"], check=True)
    subprocess.run([*make, "olddefconfig"], env=env, check=True)
    subprocess.run([sys.executable, recipe / "validate-config.py", fragment, config], check=True)
    actual_release = subprocess.check_output([*make, "-s", "kernelrelease"], env=env, text=True).strip()
    required = {"CONFIG_RUST=y", "CONFIG_DRM_ASAHI=m", "CONFIG_DRM_APPLE=m",
                "CONFIG_APPLE_SEP=m", "CONFIG_SND_SOC_APPLE_MACAUDIO=m"}
    if actual_release != release or not required.issubset(config.read_text().splitlines()):
        raise ValueError(f"{release}: unexpected Apple kernel release or driver configuration")

    subprocess.run([*make, "-j8", "Image", "modules", "dtbs"], env=env, check=True)
    stage_asahi_kernel(release, tree, build, stage, make, env)


def stage_asahi_kernel(release, tree, build, stage, make, env):
    subprocess.run([*make, f"INSTALL_MOD_PATH={stage / 'usr'}", "INSTALL_MOD_STRIP=1", "modules_install"],
                   env=env, check=True)
    modules = stage / f"usr/lib/modules/{release}"
    (modules / "build").unlink(missing_ok=True)
    for dtb in sorted((build / "arch/arm64/boot/dts/apple").glob("*.dtb")):
        stage_file(dtb, modules / "dtbs" / dtb.name)
    stage_file(build / "arch/arm64/boot/Image", stage / f"boot/vmlinuz-{release}")
    stage_file(build / ".config", stage / f"boot/config-{release}")
    stage_file(tree / "COPYING", stage / "usr/share/doc" / f"linux-image-{release}" / "copyright")
