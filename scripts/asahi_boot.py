"""Build the Apple Silicon bootloaders from pinned source."""

import os
from pathlib import Path
import shutil
import subprocess

from surface_laptop7 import extract, patch


def rust_toolchain(work, version, *options):
    # Debian's rustup conflicts with the Rust packages used by the rest of the image.
    tools = work / "rust-tools"
    tools.mkdir()
    subprocess.run(["apt-get", "download", "rustup"], cwd=tools, check=True)
    archive, = tools.glob("rustup_*.deb")
    subprocess.run(["dpkg-deb", "-x", archive, tools], check=True)
    rustup = tools / "usr/bin/rustup"
    env = dict(os.environ, RUSTUP_HOME=str(work / "rustup-home"), CARGO_HOME=str(work / "cargo-home"),
               RUSTUP_DIST_SERVER="https://static.rust-lang.org")
    subprocess.run([rustup, "toolchain", "install", version, "--profile", "minimal", *options], env=env, check=True)
    cargo = Path(subprocess.check_output([rustup, "which", "--toolchain", version, "cargo"], env=env,
                                         text=True).strip())
    env["PATH"] = os.pathsep.join((str(cargo.parent), "/usr/lib/llvm-22/bin", env["PATH"]))
    return env


def build_m1n1(source, work):
    tree = work / "m1n1"
    recipe = "pkgbuilds/m1n1-aurora/"
    extract(source["m1n1"], tree)
    extract(source["arm-kernel-recipes"], work, prefixes=(recipe + "bootlogo_",))
    for size in (48, 128, 256):
        logo = f"bootlogo_{size}.bin"
        shutil.copyfile(work / recipe / logo, tree / "data" / logo)

    env = rust_toolchain(work, "1.89.0", "--target", "aarch64-unknown-none-softfloat")
    make = ["make", "-C", tree, "-j4", "EXTRA_CFLAGS=-Os", "RELEASE=1", "CHAINLOADING=1", "USE_CLANG=1",
            "CARGO_FLAGS=--locked --features chainload"]
    stage1 = work / "m1n1-stage1.bin"
    stage2 = tree / "build/m1n1.bin"
    subprocess.run(make, env=env, check=True)
    shutil.copyfile(stage2, stage1)
    subprocess.run([*make, "clean"], env=env, check=True)
    subprocess.run([*make, "ESP_STAGE2=1"], env=env, check=True)
    return stage1, stage2, tree


def build_uboot(source, work):
    tree = work / "u-boot"
    build = work / "u-boot-build"
    recipe = "pkgbuilds/uboot-aurora/"
    extract(source["uboot"], tree)
    extract(source["arm-kernel-recipes"], work, prefixes=(recipe + "quiet-console.patch",))
    patch(tree, work / recipe / "quiet-console.patch")
    make = ["make", "-C", tree, f"O={build}", "PYTHON3=/usr/bin/python3",
            f"CROSS_COMPILE={os.environ.get('CROSS_COMPILE', '')}"]
    subprocess.run([*make, "apple_m1_defconfig"], check=True)
    config = build / ".config"
    subprocess.run([tree / "scripts/config", "--file", config, "-e", "SILENT_CONSOLE",
                    "-e", "SILENT_CONSOLE_UPDATE_ON_SET", "-e", "SILENT_CONSOLE_UPDATE_ON_RELOC",
                    "-e", "SILENT_CONSOLE_UNTIL_ENV", "-e", "SILENT_U_BOOT_ONLY", "-d", "VIDEO_LOGO",
                    "--set-val", "BOOTDELAY", "0", "-e", "EFI_RT_VOLATILE_STORE"], cwd=tree, check=True)
    subprocess.run([*make, "olddefconfig"], check=True)
    if "CONFIG_EFI_RT_VOLATILE_STORE=y" not in config.read_text().splitlines():
        raise ValueError("the Apple U-Boot build requires EFI runtime variable storage")
    subprocess.run([*make, "-j8", "u-boot-nodtb.bin"], check=True)
    return build / "u-boot-nodtb.bin", tree / "Licenses/gpl-2.0.txt"
