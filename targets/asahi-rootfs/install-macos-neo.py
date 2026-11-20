"""Use the pinned Neo restore support with Claw OS's Asahi installer."""

import os
from pathlib import Path
import plistlib
from types import SimpleNamespace
from uuid import UUID

import asahi_engine as engine
import omarchy_apple_aea as restore
import omarchy_asahi as asahi


BOARD = "j700ap"
FIRMWARE = "26.6.2"
BUILD = "25G83"
NEXT = "m1n1/boot.bin"


def select_board(mac):
    if (mac.device_class, mac.chip_id, mac.board_id, mac.product_type) != (BOARD, 0x8140, 0x64, "Mac17,5"):
        raise ValueError("the MacBook Neo restore environment does not match this board")
    engine.DEVICES[BOARD] = engine.Device("26.6", False)
    engine.CHIP_MIN_VER[mac.chip_id] = "26.6"
    engine.IPSW_VERSIONS.append(engine.IPSW(
        FIRMWARE, "26.0", "mBoot-18000.161.10", "25.7.83.0.0,0", False, ["j613ap", "j615ap", BOARD],
        "https://updates.cdn-apple.com/2026SummerFCS/fullrestores/140-75212/"
        "A2A24B94-1FC1-45A3-93F7-C51B02AF1F4D/UniversalMac_26.6.2_25G83_Restore.ipsw",
    ))
    # macOS 26 recovery and its two personalized boot objects need the larger stub.
    engine.STUB_SIZE = engine.align_down(engine.psize("6GB"), engine.PART_ALIGN)


def stage_one(package, name, partuuid=asahi.PLACEHOLDER_UUID):
    if name not in package.namelist():
        raise ValueError("the installer package is missing the MacBook Neo stage-one image")
    return asahi.fill_aurora_stage1(package.read(name), partuuid, NEXT, 0)


def check_restore(mac, ipsw):
    restore.decoder()
    if ipsw.version != FIRMWARE:
        raise ValueError("the MacBook Neo requires the pinned Apple 25G83 restore")
    stub = engine.stub.StubInstaller(mac, None, None)
    try:
        stub.load_ipsw(ipsw)
        with stub.open("BuildManifest.plist") as source:
            manifest = plistlib.load(source)
        if (manifest.get("ProductBuildVersion"), manifest.get("ProductVersion")) != (BUILD, FIRMWARE):
            raise ValueError("the Apple restore does not match the pinned MacBook Neo build")
        for identity in manifest["BuildIdentities"]:
            info = identity["Info"]
            if (info["DeviceClass"], info["Variant"], info["RestoreBehavior"],
                    identity["ApChipID"], identity["ApBoardID"]) == (
                    BOARD, "macOS Customer", "Erase", "0x8140", "0x64"):
                return
        raise ValueError("the Apple restore does not contain the MacBook Neo boot identity")
    finally:
        if stub.pkg is not None:
            stub.pkg.close()
        if stub.ucache is not None:
            stub.ucache.close_connection()


class NeoOSInstaller(engine.osinstall.OSInstaller):
    def install(self, stub):
        identity = UUID(self.efi_part.uuid)
        if not identity.int:
            raise ValueError("the new EFI partition has no identity")
        boot = stage_one(self.pkg, "stage1-j700.bin", str(identity))
        super().install(stub)
        esp = self.dutil.mount(self.efi_part.name)
        members = [member.filename for member in self.pkg.infolist()
                   if member.filename.startswith("esp/") and not member.is_dir()]
        asahi.install_esp_board_layout(BOARD, members, esp)
        Path(stub.boot_obj_path).write_bytes(boot)


class FirmwareCommands:
    def __init__(self, process):
        self.process = process
        self.recovery_mounted = False

    def run(self, command, *args, **kwargs):
        # The stock diagnostic archive copies the whole macOS FactoryData directory.
        if command[:3] == ["tar", "czf", "all_firmware.tar.gz"]:
            return self.process.CompletedProcess(command, 0)
        result = self.process.run(command, *args, **kwargs)
        if command[0] == "hdiutil" and "recovery" in command and result.returncode == 0:
            self.recovery_mounted = command[1] == "attach"
        return result

    def close(self):
        if self.recovery_mounted:
            self.process.run(["hdiutil", "detach", "-quiet", "recovery"])


class NeoStub(asahi.stub.StubInstaller):
    def __init__(self, sysinfo, dutil, osinfo, radios):
        super().__init__(sysinfo, dutil, osinfo)
        self.radios = radios

    def load_identity(self):
        result = super().load_identity()
        if self.manifest.get("ProductBuildVersion") != BUILD:
            raise ValueError("the MacBook Neo boot identity is not from Apple build 25G83")
        self.bootcaches["bless2"].setdefault("RestoreBundlePath", asahi.RESTORE_BUNDLE_PATH)
        return result

    def install_files(self, current_os):
        super().install_files(current_os)
        asahi._use_decrypted_images(self, current_os)
        asahi._write_step2(self)

    def collect_firmware(self, package):
        process = asahi.stub.subprocess
        commands = FirmwareCommands(process)
        try:
            asahi.stub.subprocess = commands
            with asahi._newer_trackpad_keys(), asahi._neo_peripheral_collectors(self):
                super().collect_firmware(package)
        finally:
            asahi.stub.subprocess = process
            commands.close()
        self.copy_idata.remove(("all_firmware.tar.gz", "all_firmware.tar.gz"))
        asahi._validate_neo_firmware_package(package)
        package.add_files(self.radios.items())


def prepare_install(mac):
    # Collect and validate the radio data before Asahi offers to resize any partition.
    radios = {}
    asahi._collect_neo_radios(SimpleNamespace(sysinfo=mac), SimpleNamespace(add_file=radios.__setitem__), required=True)
    os.environ["OMARCHY_MACHINE_OWNER"] = mac.login_user
    engine.osinstall = SimpleNamespace(OSInstaller=NeoOSInstaller)
    engine.stub = SimpleNamespace(StubInstaller=lambda sysinfo, disk, osinfo: NeoStub(sysinfo, disk, osinfo, radios))


def check_startup_disk(disk, group_uuid):
    try:
        asahi.verify_default_boot(disk, group_uuid)
    except asahi.AsahiAdapterError:
        raise ValueError("the new Claw OS volume was not confirmed as the startup disk") from None
