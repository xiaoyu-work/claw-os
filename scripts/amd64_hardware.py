"""Build offline AMD64 hardware utility packs from pinned sources."""

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

from platform_kernels import PIN, inputs, package, unpack
from surface_laptop7 import extract, replace, stage_file, write_file


QMK = "framework-qmk-hid"
RECIPES = "amd64-hardware-recipes"
LOOPBACK = "elgato-v4l2loopback"
RELAY = "elgato-v4l2-relayd"
RELAY_SOURCE = "elgato-v4l2-relayd-source"
LPMD = "intel-lpmd"
ROG = "asus-rog"
TUXEDO = "tuxedo-drivers"
CAMLINK_ASSETS = Path(__file__).resolve().parents[1] / "packaging/camlink"


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
    dest = out / "dell-xps-haptics"
    dest.mkdir(parents=True, exist_ok=True)
    upstream = work / "dell-source"
    prefix = "pkgbuilds/dell-xps-touchpad-haptics/"
    extract(source[RECIPES], upstream, prefixes=("LICENSE", prefix))
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
            {RECIPES: provenance[RECIPES]}, None)


def camlink(source, provenance, work, out):
    dest = out / "elgato-camlink-4k"
    dest.mkdir(parents=True, exist_ok=True)
    loopback = work / "loopback"
    module = "0.15.4"
    tree = loopback / "usr/src" / f"v4l2loopback-{module}"
    extract(source[LOOPBACK], tree)
    stage_file(tree / "COPYING", loopback / "usr/share/doc/v4l2loopback-dkms/copyright")
    write_file(loopback / "DEBIAN/prerm", f"""#!/bin/sh
set -e
case "$1" in
    remove|upgrade|deconfigure)
        if [ -n "$(dkms status -m v4l2loopback -v '{module}')" ]; then
            dkms remove -m v4l2loopback -v '{module}' --all
        fi
        ;;
esac
""", 0o755)
    version = f"{module}-1+clawos1"
    package(loopback, dest, "v4l2loopback-dkms", version, "all", "dkms (>= 3.0.11)",
            "V4L2 loopback module compatible with Debian Linux 7.2", {LOOPBACK: provenance[LOOPBACK]},
            f"""[ "$1" = configure ] || exit 0
/usr/lib/dkms/common.postinst v4l2loopback '{module}' /usr/share/v4l2loopback-dkms "" "${{2:-}}"
""")

    stage = work / "camlink"
    name = "claw-os-elgato-camlink"
    doc = stage / "usr/share/doc" / name
    unpack(source[RELAY], stage, lambda path: path == "usr/bin/v4l2-relayd")
    if not (stage / "usr/bin/v4l2-relayd").is_file():
        raise ValueError("the signed relay archive is missing v4l2-relayd")
    relay = work / "relay-source"
    extract(source[RELAY_SOURCE], relay, prefixes=("LICENSE",))
    stage_file(relay / "LICENSE", doc / "copyright")
    stage_file(source[RELAY_SOURCE], doc / "v4l2-relayd-upstream-0.2.0.tar.gz")
    recipe = "pkgbuilds/v4l2-relayd/"
    extract(source[RECIPES], relay, prefixes=(recipe,))
    for path in ("PKGBUILD", "0001-reset-output-on-idle.patch", "0002-escape-optional-splashsrc-expansion.patch"):
        stage_file(relay / recipe / path, doc / path)

    stage_file(CAMLINK_ASSETS / "71-claw-camlink.rules", stage / "usr/lib/udev/rules.d/71-claw-camlink.rules")
    for unit in ("claw-camlink-loopback.service", "claw-camlink-relay.service"):
        stage_file(CAMLINK_ASSETS / unit, stage / "usr/lib/systemd/system" / unit)
    package(stage, dest, name, "0.2.0-3+clawos1", "amd64",
            f"v4l2loopback-dkms (>= {version}), v4l2loopback-utils, gstreamer1.0-plugins-base, "
            "gstreamer1.0-plugins-good, libgstreamer1.0-0, libgstreamer-plugins-base1.0-0, libglib2.0-0t64, libc6, kmod, systemd, udev",
            "Fixed-aspect virtual camera for the Elgato Cam Link 4K",
            {key: provenance[key] for key in (RELAY, RELAY_SOURCE, RECIPES)},
            None)


def intel_power(source, provenance, work, out):
    dest = out / "intel-hybrid-power"
    dest.mkdir(parents=True, exist_ok=True)
    tree = work / LPMD
    stage = work / "lpmd-package"
    name = "claw-os-intel-lpmd"
    extract(source[LPMD], tree)
    replace(tree / "configure.ac", "GTK_DOC_CHECK([1.11],[--flavour no-tmpl])\n", "")
    replace(tree / "Makefile.am", 'install-data-hook:\n\tgzip -f -k "$(DESTDIR)$(mandir)/man8/intel_lpmd.8";\n\tmandb || true\n', "")
    subprocess.run(["autoreconf", "--install"], cwd=tree, check=True)
    subprocess.run([tree / "configure", "--prefix=/usr", "--sysconfdir=/etc", "--localstatedir=/var",
                    "--with-systemdsystemunitdir=/usr/lib/systemd/system", "--with-dbus-sys-dir=/usr/share/dbus-1/system.d",
                    "--disable-werror"], cwd=tree, check=True)
    subprocess.run(["make", f"-j{os.cpu_count()}"], cwd=tree, check=True)
    subprocess.run(["make", f"DESTDIR={stage}", "install"], cwd=tree, check=True)
    policy = stage / "usr/share/dbus-1/system.d/org.freedesktop.intel_lpmd.conf"
    replace(policy, """        <!-- Only allow members of the power group to communicate
             with the daemon -->
""", "")
    replace(policy, """        <policy group="power">
                <allow send_destination="org.freedesktop.intel_lpmd"/>
                <allow receive_sender="org.freedesktop.intel_lpmd"/>
        </policy>
""", "")
    doc = stage / "usr/share/doc" / name
    stage_file(tree / "COPYING", doc / "copyright")
    stage_file(source[LPMD], doc / "intel-lpmd-0.1.0.tar.gz")
    conf = stage / "etc/intel_lpmd"
    write_file(stage / "DEBIAN/conffiles", "".join(f"/etc/intel_lpmd/{path.name}\n" for path in sorted(conf.glob("*.xml"))))
    package(stage, dest, name, "0.1.0-1+clawos1", "amd64",
            "libc6, libglib2.0-0t64, libxml2, libnl-3-200, libnl-genl-3-200, libsystemd0, "
            "libupower-glib3, upower, dbus, systemd, init-system-helpers",
            "Intel low-power mode daemon for supported hybrid laptops", {LPMD: provenance[LPMD]},
            """[ "$1" = configure ] || exit 0
deb-systemd-helper enable intel_lpmd.service >/dev/null
""", """[ "$1" = purge ] || exit 0
deb-systemd-helper purge intel_lpmd.service >/dev/null
""")


def rog(source, provenance, work, out):
    dest = out / ROG
    dest.mkdir(parents=True, exist_ok=True)
    tree = work / "asusctl"
    stage = work / "rog-package"
    name = "claw-os-asus-rog"
    extract(source[ROG], tree)
    env = dict(os.environ, CARGO_TARGET_DIR=str(tree / "target"))
    subprocess.run(["cargo", "build", "--locked", "--release", "--jobs", "4", "-p", "asusctl",
                    "-p", "asusd", "-p", "asusd-user", "-p", "asus-shutdown"], env=env, cwd=tree, check=True)
    for binary in ("asusctl", "asusd", "asusd-user", "asus-shutdown"):
        stage_file(tree / "target/release" / binary, stage / "usr/bin" / binary, 0o755)
    for filename in ("asusd.service", "asus-shutdown.service"):
        unit = stage / "usr/lib/systemd/system" / filename
        text = (tree / "data" / filename).read_text()
        text = text.replace("[Unit]\n", '[Unit]\nConditionFirmware=smbios-field(sys_vendor = "ASUSTeK COMPUTER INC.")\n', 1)
        write_file(unit, text)
    rule = stage / "usr/lib/udev/rules.d/99-claw-asusd.rules"
    write_file(rule, 'ACTION=="add", DRIVER=="asus-nb-wmi", '
               'ATTR{[dmi/id]sys_vendor}=="ASUSTeK COMPUTER INC.", ATTR{[dmi/id]product_family}=="*ROG*", '
               'TAG+="systemd", ENV{SYSTEMD_WANTS}+="asusd.service asus-shutdown.service"\n')
    policy = stage / "usr/share/dbus-1/system.d/asusd.conf"
    stage_file(tree / "data/asusd.conf", policy)
    for group in ("adm", "users", "wheel"):
        replace(policy, f"""    <policy group="{group}">
        <allow send_destination="xyz.ljones.Asusd"/>
        <allow receive_sender="xyz.ljones.Asusd"/>
    </policy>
""", "")
    stage_file(tree / "rog-aura/data/aura_support.ron", stage / "usr/share/asusd/aura_support.ron")
    doc = stage / "usr/share/doc" / name
    stage_file(tree / "LICENSE", doc / "copyright")
    package(stage, dest, name, "6.5.0-1+clawos1", "amd64",
            "libc6, libgcc-s1, libusb-1.0-0, libudev1, dbus, systemd, udev",
            "ASUS ROG notebook control daemon, shutdown service and CLI", {ROG: provenance[ROG]}, None)


def tuxedo(source, provenance, work, out):
    dest = out / "tuxedo-keyboard"
    dest.mkdir(parents=True, exist_ok=True)
    upstream = work / TUXEDO
    stage = work / "tuxedo-package"
    name = "tuxedo-drivers-dkms"
    version = "4.25.0"
    extract(source[TUXEDO], upstream)
    modules = re.findall(r"'((?:DEST_MODULE_LOCATION|BUILT_MODULE_NAME|BUILT_MODULE_LOCATION)\[[0-9]+\]=\"[^\"\n]+\")'",
                         (upstream / "package.yml").read_text())
    if not any(line.endswith('="tuxedo_compatibility_check"') for line in modules):
        raise ValueError("the upstream TUXEDO DKMS module list is missing")
    target = stage / "usr/src" / f"{TUXEDO}-{version}"
    target.parent.mkdir(parents=True)
    shutil.copytree(upstream / "src", target)
    write_file(target / "dkms.conf",
               f'PACKAGE_NAME="{TUXEDO}"\nPACKAGE_VERSION="{version}"\nAUTOINSTALL="yes"\n'
               'MAKE[0]="make -C ${kernel_source_dir} M=${dkms_tree}/${PACKAGE_NAME}/${PACKAGE_VERSION}/build modules"\n'
               'CLEAN="true"\n' + "\n".join(modules) + "\n")
    for path in (upstream / "files/usr/lib/udev").rglob("*"):
        if path.is_file():
            stage_file(path, stage / path.relative_to(upstream / "files"))
    stage_file(upstream / "LICENSE", stage / "usr/share/doc" / name / "copyright")
    write_file(stage / "usr/lib/modprobe.d/60-claw-tuxedo-backlight.conf", "blacklist clevo_xsm_wmi\n")
    write_file(stage / "DEBIAN/prerm", f"""#!/bin/sh
set -e
case "$1" in
    remove|upgrade|deconfigure)
        if [ -n "$(dkms status -m '{TUXEDO}' -v '{version}')" ]; then
            dkms remove -m '{TUXEDO}' -v '{version}' --all
        fi
        ;;
esac
""", 0o755)
    package(stage, dest, name, f"{version}-1+clawos1", "amd64", "dkms (>= 3.0.11), udev",
            "Upstream compatibility-checked TUXEDO laptop modules", {TUXEDO: provenance[TUXEDO]},
            f"""[ "$1" = configure ] || exit 0
/usr/lib/dkms/common.postinst '{TUXEDO}' '{version}' /usr/share/{name} "" "${{2:-}}"
""")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    lock = json.loads(PIN.read_text())
    keys = (QMK, RECIPES, LOOPBACK, RELAY, RELAY_SOURCE, LPMD, ROG, TUXEDO)
    args.output.mkdir(parents=True, exist_ok=True)
    args.cache.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        source = dict(inputs(lock, keys, args.cache, work))
        framework(source, lock["inputs"], work, args.output)
        dell(source, lock["inputs"], work, args.output)
        camlink(source, lock["inputs"], work, args.output)
        intel_power(source, lock["inputs"], work, args.output)
        rog(source, lock["inputs"], work, args.output)
        tuxedo(source, lock["inputs"], work, args.output)


if __name__ == "__main__":
    main()
