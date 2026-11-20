"""Build model-specific input, camera and resume support for Surface Pro 11."""

from surface_laptop7 import extract, stage_file


def pro11_package(release, source, provenance, work, out, package):
    key = "surface-snapdragon-kernel-fixes"
    profile_dir = "pkgbuilds/microsoft-surface-pro-11-support/"
    recipes = work / "surface-pro-11-recipes"
    extract(source[key], recipes, prefixes=(profile_dir,))
    profile = recipes / profile_dir
    stage = work / "surface-pro-11"
    name = "claw-os-surface-pro-11-snapdragon"
    for file, dest in (
        ("LICENSE", f"usr/share/doc/{name}/copyright"),
        ("imx681.yaml", "usr/share/libcamera/ipa/simple/imx681.yaml"),
        ("50-surface-pro-11-cameras.conf",
         "usr/share/wireplumber/wireplumber.conf.d/50-surface-pro-11-cameras.conf"),
        ("50-surface-pro-11-flex-keyboard.quirks", "usr/share/libinput/50-surface-pro-11-flex-keyboard.quirks"),
        ("70-surface-pro-11-flex-keyboard.rules", "usr/lib/udev/rules.d/70-surface-pro-11-flex-keyboard.rules"),
    ):
        stage_file(profile / file, stage / dest)
    for hook in ("charge-limit", "iptsd", "lid-wake", "touchpad-mode"):
        stem = f"surface-pro-11-{hook}"
        stage_file(profile / f"{stem}-sleep", stage / "usr/lib/systemd/system-sleep" / stem, 0o755)
    for action in ("flex-autopair", "flex-reconnect"):
        stem = f"surface-pro-11-{action}"
        stage_file(profile / stem, stage / "usr/lib/surface-pro-11-support" / stem, 0o755)
        stage_file(profile / f"{stem}.service", stage / "usr/lib/systemd/system" / f"{stem}.service")
    for action in ("flex-pair", "pen-pair", "pen-forget"):
        stem = f"surface-pro-11-{action}"
        stage_file(profile / stem, stage / "usr/bin" / stem, 0o755)
    package(stage, out, name, "1-1", "arm64",
            f"linux-image-{release}, claw-os-surface-snapdragon-iptsd, bash, bluez, libcamera-ipa, "
            "libspa-0.2-libcamera, python3, sudo, systemd, udev, util-linux",
            "Input, cameras and resume integration for Snapdragon Surface Pro 11",
            {key: provenance[key]}, None)
