"""Build the Apple Silicon bootloaders from pinned source."""

import os
import subprocess

from surface_laptop7 import extract, patch


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
