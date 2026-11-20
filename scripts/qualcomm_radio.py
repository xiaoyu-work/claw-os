"""Build the Debian Qualcomm radio board-data helper from pinned sources."""

import argparse
import ast
import json
from pathlib import Path
import tempfile

from platform_kernels import PIN, inputs, package
from surface_laptop7 import extract, replace, stage_file, write_file


SOURCE = "qualcomm-radio-recipes"


def build(archive, provenance, work, output):
    recipes = "pkgbuilds/ath12k-board-fallback/"
    tree = work / "radio-source"
    extract(archive, tree, prefixes=("LICENSE", recipes))
    stage = work / "radio"
    name = "claw-os-qualcomm-radio"
    tool = stage / "usr/sbin/ath12k-board-fallback"
    stage_file(tree / recipes / tool.name, tool, 0o755)
    replace(tool, "#!/usr/bin/env python3", "#!/usr/bin/python3")
    for key, path in (
        ("SYSFS", "/sys/bus/pci/devices"), ("FIRMWARE", "/usr/lib/firmware"),
        ("CARDS", "/usr/share/ath12k-board-fallback/cards"), ("STATE", "/var/lib/ath12k-board-fallback/installed"),
    ):
        replace(tool, f'{key} = os.environ.get("ATH12K_BOARD_{key}", "{path}")', f'{key} = "{path}"')
    replace(tool, "    if args not in ([], [\"--check\"], [\"--remove\"]):",
            "    if args not in ([], [\"--check\"], [\"--remove\"], [\"--boot\"]):")
    replace(tool, "    check = args == [\"--check\"]", """    check = args == ["--check"]
    if not check and os.geteuid() != 0:
        sys.exit("ath12k-board-fallback: applying board data requires root")""")
    replace(tool, "    here = machine_cards()", r"""    here = machine_cards()
    if not here and not state:
        return
    try:
        with open("/sys/firmware/devicetree/base/compatible", "rb") as stream:
            compatible = set(stream.read().split(b"\0"))
    except FileNotFoundError:
        compatible = set()
    surface_reference = compatible.intersection({b"microsoft,denali-lcd", b"microsoft,denali-oled",
                                                 b"microsoft,surface-laptop-8-13p8", b"microsoft,surface-laptop-8-15"})
    if surface_reference:
        card_family["17cb:1107", "17cb:1107"] = "surface-reference"
        families["surface-reference"] = ["bus=pci,vendor=17cb,device=1107,subsystem-vendor=17cb,"
                                         "subsystem-device=3378,qmi-chip-id=2,qmi-board-id=255"]""")
    replace(tool, "            if lists_card(records, chip, card):", """            if surface_reference and (chip, card) == ("17cb:1107", "17cb:1107"):
                native = "bus=pci,vendor=17cb,device=1107,subsystem-vendor=17cb,subsystem-device=1107,qmi-chip-id=2,qmi-board-id=255"
                if native in records:
                    continue
            elif lists_card(records, chip, card):""")
    replace(tool, "        needed = {}\n        for card in cards:", "        needed = {}\n        unknown = False\n        for card in cards:")
    replace(tool, '            if family is None:\n                log(',
            '            if family is None:\n                unknown = True\n                log(')
    replace(tool, """        if len(set(needed.values())) > 1:
            log(f"cards of different families share {folder}; installing nothing")
            continue""", """        if unknown or len(set(needed.values())) > 1:
            remove_copy(rel, state, check, "the connected cards do not share a known fallback record")
            continue""")

    replace(tool, "def main():", """def reprobe(state):
    for device in sorted(os.listdir(SYSFS)):
        base = os.path.join(SYSFS, device)
        try:
            with open(os.path.join(base, "vendor")) as stream:
                vendor = stream.read().strip().removeprefix("0x")
            with open(os.path.join(base, "device")) as stream:
                chip = CHIPS.get(vendor + ":" + stream.read().strip().removeprefix("0x"))
            if not chip or f"{chip}/board.bin" not in state:
                continue
            driver = os.path.join("/sys/bus/pci/drivers", chip.split("/")[0] + "_pci")
            bound = os.path.join(base, "driver")
            network = os.path.join(base, "net")
            if os.path.isdir(network) and os.listdir(network):
                continue
            if os.path.islink(bound):
                if os.path.realpath(bound) != driver:
                    continue
                with open(os.path.join(driver, "unbind"), "w") as stream:
                    stream.write(device)
            with open(os.path.join(driver, "bind"), "w") as stream:
                stream.write(device)
        except OSError as error:
            log(f"{device}: could not reprobe radio: {error}")


def main():""")
    replace(tool, "    write_state(state, check)\n\n\nif",
            '    write_state(state, check)\n    if args == ["--boot"]:\n        reprobe(state)\n\n\nif')
    ast.parse(tool.read_text(), filename=str(tool))
    stage_file(tree / recipes / "cards", stage / "usr/share/ath12k-board-fallback/cards")
    stage_file(tree / "LICENSE", stage / "usr/share/doc" / name / "copyright")
    write_file(stage / "usr/lib/systemd/system/claw-radio-board-data.service", """\
[Unit]
Description=Prepare matched Qualcomm Wi-Fi board data
After=systemd-udev-trigger.service
Before=NetworkManager.service
Wants=systemd-udev-trigger.service

[Service]
Type=oneshot
ExecStart=/usr/sbin/ath12k-board-fallback --boot
NoNewPrivileges=yes

[Install]
WantedBy=multi-user.target
""")
    package(stage, output, name, "1-1", "all", "python3, firmware-atheros, zstd, systemd",
            "Matched board data for Qualcomm PCI Wi-Fi adapters", provenance, None)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    lock = json.loads(PIN.read_text())
    args.cache.mkdir(parents=True, exist_ok=True)
    args.output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        source = dict(inputs(lock, (SOURCE,), args.cache, work))
        build(source[SOURCE], {SOURCE: lock["inputs"][SOURCE]}, work, args.output)


if __name__ == "__main__":
    main()
