"""Verify the Claw payload and hand new installations to the pinned Asahi engine."""

import json
import os
from pathlib import Path
import subprocess
import sys
import zipfile

import asahi_engine as engine


PACKAGE = "claw-os.zip"
PAYLOAD = Path("../os") / PACKAGE
EFI = "esp/EFI/BOOT/BOOTAA64.EFI"
STAGE2 = "esp/m1n1/boot.bin"
LAYOUT = {"root.img", EFI, STAGE2, "esp/m1n1/boot-j700.bin"}
DIRECTORIES = {"esp/", "esp/EFI/", "esp/EFI/BOOT/", "esp/m1n1/"}
ESP_BYTES = 500 * 1024**2


class ClawInstaller(engine.InstallerMain):
    def check_cur_os(self):
        if self.cur_os:
            owners = self.cur_os.admin_users or ()
            if owners == ["_mbsetupuser"] or self.sysinfo.login_user not in owners:
                # The stock installer would refresh the current macOS Preboot volume here.
                raise ValueError("sign in as a recognized macOS volume owner before installing")
        return super().check_cur_os()

    def choice(self, prompt, options, default=None):
        if prompt == "Action":
            options = {key: value for key, value in options.items() if key in {"f", "r", "q"}}
            default = next(key for key in ("f", "r", "q") if key in options)
        return super().choice(prompt, options, default)

    def step2(self, report=False):
        # The payload is on disk now; release the private copy before shutdown.
        self.osins.pkg.close()
        PAYLOAD.unlink()
        return super().step2(report)


def template_for(payload, mac):
    device = engine.DEVICES.get(mac.device_class)
    if not device or device.expert_only or mac.chip_id not in engine.CHIP_MIN_VER:
        raise ValueError(f"{mac.device_class}: not supported by this Asahi enrollment engine")
    if mac.boot_mode != "macOS" or not mac.login_user or mac.login_user == "unknown":
        raise ValueError("start in macOS with a user logged in at the local console")
    if engine.split_ver(mac.macos_ver) < engine.split_ver(engine.MIN_MACOS_VERSION):
        raise ValueError(f"upgrade macOS to at least {engine.MIN_MACOS_VERSION} before installing")

    with zipfile.ZipFile(payload) as package:
        members = package.infolist()
        names = {member.orig_filename for member in members}
        if len(names) != len(members) or not LAYOUT <= names <= LAYOUT | DIRECTORIES:
            raise ValueError("unexpected files in the Claw OS installer payload")
        if any(not package.getinfo(name).file_size for name in LAYOUT):
            raise ValueError("the Claw OS installer payload contains an empty file")
        size = package.getinfo("root.img").file_size
        with package.open("root.img") as image:
            header = image.read(2048)[1024:]
        extents = int.from_bytes(header[96:100], "little") & 0x40
        if size % 4096 or header[56:58] != b"\x53\xef" or not extents:
            raise ValueError("the Linux root image is not an aligned ext4 filesystem")
        with package.open(EFI) as loader:
            header = loader.read(4096)
        pe = int.from_bytes(header[60:64], "little")
        if header[:2] != b"MZ" or header[pe:pe + 6] != b"PE\0\0\x64\xaa":
            raise ValueError("the EFI loader is not for ARM64")
        board = mac.device_class.removesuffix("ap")
        compatible = f"apple,{board}\0apple,t{mac.chip_id:x}\0".encode("ascii")
        if compatible not in package.read(STAGE2):
            raise ValueError(f"the supplied stage-two image does not match {mac.device_class}")

    return {
        "name": "Claw OS",
        "default_os_name": "Claw OS",
        "package": PACKAGE,
        "boot_object": "m1n1.bin",
        "next_object": "m1n1/boot.bin",
        "supported_fw": ["13.5", "14.8.3"],
        "partitions": [
            {"type": "EFI", "name": "EFI", "size": f"{ESP_BYTES}B", "format": "fat",
             "source": "esp", "copy_firmware": True, "copy_installer_data": True},
            {"type": "Linux", "name": "Root", "size": f"{size}B", "image": "root.img", "expand": True},
        ],
    }


def run(mode):
    if sys.platform != "darwin" or os.geteuid() != 0:
        raise ValueError("use install-macos.sh from Apple Silicon macOS")
    mac = engine.system.SystemInfo()
    template = template_for(PAYLOAD, mac)
    metadata = {"os_list": [template]}
    Path("installer_data.json").write_text(json.dumps(metadata), encoding="utf-8")
    installer = ClawInstaller(Path("version.tag").read_text().strip())
    installer.sysinfo = mac
    installer.device = engine.DEVICES[mac.device_class]
    installer.choose_ipsw(template["supported_fw"])
    size = engine.STUB_SIZE + engine.osinstall.OSInstaller(None, metadata, template).min_recommended_size
    print(f"{mac.device_class}: Claw OS, dedicated APFS boot stub and ESP, unencrypted Linux root")
    print(f"Recommended free space: {engine.ssize(size)}. Asahi checks the actual partition layout before installation.")
    if mode == "--plan":
        print("Read-only preflight: no partitions or boot policies changed.")
        return
    print("Back up macOS before proceeding. The installer may shrink its APFS container.")
    if input("Type INSTALL to open the Asahi installation menu: ") != "INSTALL":
        raise ValueError("installation cancelled; no partitions changed")
    # Asahi collects this file with its install data; keep credentials and local paths out of it.
    Path("installer.log").touch()
    installer.main()


if __name__ == "__main__":
    try:
        if sys.argv[1:] not in (["--plan"], ["--install"]):
            raise ValueError("choose --plan or --install")
        run(sys.argv[1])
    except (OSError, ValueError, zipfile.BadZipFile) as error:
        raise SystemExit(f"Claw OS: {error}") from None
    except subprocess.CalledProcessError:
        raise SystemExit("Claw OS: an Asahi system operation failed") from None
    except (KeyboardInterrupt, EOFError):
        raise SystemExit("Claw OS: interrupted") from None
