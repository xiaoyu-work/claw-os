"""Build the native Debian userspace support package for Apple T2 Macs."""

from pathlib import Path
import subprocess

from surface_laptop7 import extract, stage_file, write_file


ASSETS = Path(__file__).resolve().parents[1] / "packaging/t2"


def t2_package(release, source, provenance, work, out, package):
    fan = work / "t2-fan"
    audio = work / "t2-audio"
    stage = work / "t2-support"
    doc = stage / "usr/share/doc/claw-os-t2-support"
    extract(source["t2-fan"], fan)
    subprocess.run(["cargo", "build", "--locked", "--release", "--target-dir", fan / "target"], cwd=fan, check=True)
    stage_file(fan / "target/release/t2fanrd", stage / "usr/bin/t2fanrd", 0o755)
    stage_file(fan / "LICENCE", doc / "t2fanrd.LICENSE")

    extract(source["t2-audio"], audio, prefixes=("LICENSE", "modules/t2bce_audio-alsa-ucm-conf/ucm2/"))
    profiles = audio / "modules/t2bce_audio-alsa-ucm-conf/ucm2"
    for channels in (2, 4, 6):
        for path in (f"AppleT2/HiFi-x{channels}.conf", f"conf.d/AppleT2x{channels}/AppleT2x{channels}.conf"):
            stage_file(profiles / path, stage / "usr/share/alsa/ucm2" / path)
    stage_file(audio / "LICENSE", doc / "ucm.LICENSE")

    converter = source["t2-firmware-converter"].read_text()
    _, body = converter.split("python3 - \"$@\" <<'EOF'\n", 1)
    python, _ = body.split("\nEOF\n}", 1)
    write_file(stage / "usr/lib/cos/hw/t2-firmware-convert.py", python + "\n")
    stage_file(source["t2-firmware-license"], doc / "firmware-converter.LICENSE")
    stage_file(ASSETS / "claw-import-t2-firmware", stage / "usr/sbin/claw-import-t2-firmware", 0o755)
    stage_file(ASSETS / "claw-t2-fan.service", stage / "usr/lib/systemd/system/claw-t2-fan.service")
    write_file(stage / "usr/lib/modules-load.d/60-claw-t2.conf", "t2bce_vhci\nhci_bcm4377\n")
    package(stage, out, "claw-os-t2-support", "1-1", "amd64",
            f"linux-image-{release}, alsa-ucm-conf, python3, libc6, libgcc-s1, kmod, mount, util-linux, systemd, initramfs-tools",
            "Audio profiles, fan service and local wireless firmware import for Apple T2 Macs",
            {name: spec for name, spec in provenance.items() if name.startswith("t2-")}, None)
