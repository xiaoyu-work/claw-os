"""Build offline AMD64 hardware utility packs from pinned sources."""

import argparse
import json
from pathlib import Path
import tempfile

from platform_kernels import PIN, inputs, package, unpack
from surface_laptop7 import extract, replace, stage_file, write_file


QMK = "framework-qmk-hid"
DELL = "dell-xps-haptics"


def framework(source, provenance, work, out):
    dest = out / "framework-laptop-16"
    dest.mkdir(parents=True, exist_ok=True)
    stage = work / "framework"
    name = "claw-os-framework-qmk-hid"
    unpack(source[QMK], stage, lambda path: path in {"usr/bin/qmk_hid", "usr/share/licenses/qmk-hid/LICENSE"})
    if not (stage / "usr/bin/qmk_hid").is_file():
        raise ValueError("the signed Framework archive is missing qmk_hid")
    stage_file(stage / "usr/share/licenses/qmk-hid/LICENSE", stage / "usr/share/doc" / name / "copyright")
    package(stage, dest, name, "0.1.13-2", "amd64", "libc6, libcap2, libgcc-s1, libudev1",
            "Framework Laptop 16 QMK HID utility", {QMK: provenance[QMK]}, None)


def dell(source, provenance, work, out):
    dest = out / DELL
    dest.mkdir(parents=True, exist_ok=True)
    upstream = work / "dell-source"
    prefix = "pkgbuilds/dell-xps-touchpad-haptics/"
    extract(source[DELL], upstream, prefixes=("LICENSE", prefix))
    stage = work / "dell"
    name = "claw-os-dell-xps-haptics"
    files = upstream / prefix
    utility = stage / "usr/bin/dell-xps-touchpad-haptics"
    daemon = stage / "usr/lib/cos/hw/dell-xps-touchpad-haptics-daemon"
    stage_file(files / utility.name, utility, 0o755)
    stage_file(files / daemon.name, daemon, 0o755)
    stage_file(upstream / "LICENSE", stage / "usr/share/doc" / name / "copyright")
    replace(utility, 'CONFIG_PATH="$HOME/.config/omarchy/dell-haptic.conf"', "CONFIG_PATH=/etc/claw-dell-haptics.conf")
    replace(utility, "  previous_level=$(level_for_intensity", """  if (( EUID != 0 )); then
    echo "Setting haptic intensity requires root." >&2
    exit 1
  fi

  previous_level=$(level_for_intensity""")
    replace(utility, """write_config() {
  local intensity=$1

  install -d -m 755 "$(dirname -- "$CONFIG_PATH")"
  printf '# Dell XPS touchpad haptic intensity.\\n# Presets: low=10, mid=50, high=100.\\nINTENSITY=%s\\n' "$intensity" >"$CONFIG_PATH"
}""", """write_config() (
  local intensity=$1 staged
  staged=$(mktemp /etc/.claw-dell-haptics.XXXXXX)
  trap 'rm -f -- "$staged"' EXIT
  printf 'INTENSITY=%s\\n' "$intensity" >"$staged"
  chmod 0644 "$staged"
  mv -T -- "$staged" "$CONFIG_PATH"
)""")

    replace(daemon, "#!/usr/bin/env python3", "#!/usr/bin/python3")
    replace(daemon, 'USER_HOME = Path(\n'
            '    os.environ.get("DELL_XPS_TOUCHPAD_HAPTICS_HOME", str(Path.home()))\n'
            ')\nCONFIG_PATH = USER_HOME / ".config/omarchy/dell-haptic.conf"',
            'CONFIG_PATH = Path("/etc/claw-dell-haptics.conf")\n'
            'TOUCHPAD = Path("/sys/bus/i2c/devices/i2c-VEN_06CB:00")')
    replace(daemon, "def find_hidraw():", """def is_touchpad(path):
    try:
        return Path(path).resolve().is_relative_to(TOUCHPAD.resolve(strict=True))
    except OSError:
        return False


def find_hidraw():""")
    replace(daemon, '            if f"0000{VENDOR}" in content:',
            '            if f"0000{VENDOR}" in content and is_touchpad(Path(path) / "device"):')
    replace(daemon, '            if VENDOR in name and "TOUCHPAD" in name:',
            '            if VENDOR in name and "TOUCHPAD" in name and is_touchpad(path):')
    replace(daemon, """def main():
    refresh_intensity(force=True)""", """def main():
    dmi = Path("/sys/class/dmi/id")
    models = " ".join(path.read_text().strip() for path in
                      (dmi / "product_name", dmi / "product_family") if path.is_file())
    if not TOUCHPAD.exists() or (dmi / "sys_vendor").read_text().strip() != "Dell Inc." or "XPS" not in models:
        return
    for parent in (Path("/sys/bus/pci/devices/0000:00:19.0"), Path("/sys/bus/platform/devices/i2c_designware.0")):
        if parent.exists() and TOUCHPAD.resolve().is_relative_to(parent.resolve()):
            control = parent / "power/control"
            if control.is_file():
                control.write_text("on")
    refresh_intensity(force=True)""")

    unit = stage / "usr/lib/systemd/system/dell-xps-touchpad-haptics.service"
    write_file(unit, """[Unit]
Description=Dell XPS Synaptics haptic touchpad
ConditionFirmware=smbios-field(sys_vendor = "Dell Inc.")
ConditionPathExists=/sys/bus/i2c/devices/i2c-VEN_06CB:00
After=systemd-udev-trigger.service

[Service]
ExecStart=/usr/lib/cos/hw/dell-xps-touchpad-haptics-daemon
Restart=on-failure
RestartSec=2
NoNewPrivileges=yes
ProtectHome=yes
ProtectSystem=strict

[Install]
WantedBy=multi-user.target
""")
    write_file(stage / "usr/lib/udev/rules.d/99-claw-dell-xps-haptics.rules",
               'ACTION=="add", SUBSYSTEM=="i2c", KERNEL=="i2c-VEN_06CB:00", '
               'ATTR{[dmi/id]sys_vendor}=="Dell Inc.", TAG+="systemd", '
               'ENV{SYSTEMD_WANTS}+="dell-xps-touchpad-haptics.service"\n')
    package(stage, dest, name, "1.0.0-5+clawos1", "amd64", "python3, bash, systemd, udev",
            "Model-matched Dell XPS Synaptics haptic touchpad support",
            {DELL: provenance[DELL]}, None)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    lock = json.loads(PIN.read_text())
    keys = (QMK, DELL)
    args.output.mkdir(parents=True, exist_ok=True)
    args.cache.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        source = dict(inputs(lock, keys, args.cache, work))
        framework(source, lock["inputs"], work, args.output)
        dell(source, lock["inputs"], work, args.output)


if __name__ == "__main__":
    main()
