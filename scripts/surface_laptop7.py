"""Build the additional drivers and desktop integration for the Intel Laptop 7."""

import os
from pathlib import Path
import shutil
import subprocess
import tarfile


MODULES = {
    "drivers/platform/surface": "surface_aggregator_registry",
    "drivers/media/i2c": "ov02c10 vd55g0",
    "drivers/media/pci/intel": "ipu-bridge",
    "drivers/staging/media/ipu7": "intel-ipu7-isys",
    "sound/soc/sdw_utils": "snd-soc-sdw-utils",
    "drivers/gpu/drm/display": "drm_display_helper",
}
CAMERA_PACKAGES = (
    "libcamera0.7", "libcamera-ipa", "libcamera-tools", "libcamera-v4l2", "gstreamer1.0-libcamera",
)


def extract(archive, dest, *, strip=True, prefixes=()):
    dest.mkdir(parents=True, exist_ok=True)
    with tarfile.open(archive, "r|*") as members:
        for member in members:
            path = member.name.partition("/")[2] if strip else member.name
            if path and (not prefixes or path.startswith(prefixes)):
                members.extract(member.replace(name=path), dest, filter="data")


def patch(tree, path):
    subprocess.run(["patch", "--batch", "--forward", "--fuzz=0", "-p1", "-i", path], cwd=tree, check=True)


def replace(path, old, new):
    text = path.read_text()
    if text.count(old) != 1:
        raise ValueError(f"{path.name}: expected one occurrence of {old!r}")
    path.write_text(text.replace(old, new))


def stage_file(source, dest, mode=0o644):
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, dest)
    dest.chmod(mode)


def write_file(dest, text, mode=0o644):
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_text(text)
    dest.chmod(mode)


def kernel_modules(profile, source, fixes, work, stage):
    release = profile["release"]
    headers = work / "headers"
    subprocess.run(["dpkg-deb", "-x", source["surface-headers"], headers], check=True)
    kbuild = headers / "usr/src" / f"linux-headers-{release}"
    tree = work / "linux"
    # Keep the source relatives used by the composite audio, IPU7 and DRM modules.
    extract(source["surface-linux-source"], tree, prefixes=(
        "drivers/platform/surface/", "drivers/media/i2c/", "drivers/media/pci/intel/",
        "drivers/staging/media/ipu7/", "sound/soc/", "drivers/gpu/drm/",
    ))
    subprocess.run(["python3", "-I", fixes / "kernel/port-patches.py", tree], check=True)
    patch(tree, source["surface-backlight"])

    audio = tree / "sound/soc/sdw_utils/soc_sdw_utils.c"
    replace(audio, "ghosts = asoc_sdw_count_ghosts(mach_params);",
            'ghosts = acpi_dev_present("MSHW0551", NULL, -1) ?\n'
            '\t\t asoc_sdw_count_ghosts(mach_params) : 0;')
    replace(audio, "#include <linux/device.h>", "#include <linux/acpi.h>\n#include <linux/device.h>")
    backlight = tree / "drivers/gpu/drm/display/drm_dp_helper.c"
    replace(backlight, "if (bl->aux_set && !bl->luminance_set) {",
            'if (acpi_dev_present("MSHW0551", NULL, -1) && bl->aux_set && !bl->luminance_set) {')
    replace(backlight, "#include <linux/backlight.h>", "#include <linux/acpi.h>\n#include <linux/backlight.h>")

    # Use the upstream IR sensor driver without the experimental flood-emitter changes.
    ir = tree / "drivers/media/i2c/vd55g0.c"
    stage_file(source["surface-ir-driver"], ir)
    stage_file(source["surface-ir-data"], ir.with_name("vd55g0_patches.h"))
    replace(ir, '"pwr2",', '"dovdd",')
    bridge = tree / "drivers/media/pci/intel/ipu-bridge.c"
    replace(bridge, 'IPU_SENSOR_CONFIG("OVTI02C1", 1, 400000000),',
            'IPU_SENSOR_CONFIG("OVTI02C1", 1, 400000000),\n'
            '\tIPU_SENSOR_CONFIG("SMO55F0", 1, 402000000),')

    write_file(tree / "Kbuild", "obj-m := " + " ".join(path + "/" for path in MODULES) + "\n")
    for path, modules in MODULES.items():
        directory = tree / path
        definitions = ""
        if path in ("drivers/staging/media/ipu7", "sound/soc/sdw_utils", "drivers/gpu/drm/display"):
            definitions = (directory / "Makefile").read_text()
        write_file(directory / "Kbuild", definitions + "\nobj-m := " +
                   " ".join(module + ".o" for module in modules.split()) + "\n")
    subprocess.run(["make", "-C", kbuild, f"M={tree}", f"-j{os.cpu_count()}", "modules"], check=True)
    destination = stage / "usr/lib/modules" / release / "updates/claw-surface"
    for path, modules in MODULES.items():
        for module in modules.split():
            filename = module + ".ko"
            stage_file(tree / path / filename, destination / filename)


def touchpad(source, fixes, work, stage):
    tree = work / "iptsd"
    extract(source["surface-iptsd"], tree)
    for series_patch in sorted((fixes / "iptsd/patches").glob("*.patch")):
        patch(tree, series_patch)
    build = tree / "build"
    subprocess.run([
        "meson", "setup", build, tree, "--prefix=/usr", "--buildtype=release",
        "-Ddebug_tools=calibrate,dump,perf", "-Dsample_config=false", "-Dwerror=false",
        "--force-fallback-for=fmt,spdlog,inih",
    ], check=True)
    subprocess.run(["meson", "compile", "-C", build], check=True)
    subprocess.run(["meson", "install", "-C", build, "--destdir", stage, "--skip-subprojects"], check=True)
    if not any(stage.rglob("iptsd@.service")) or not any(stage.rglob("50-iptsd.rules")):
        raise ValueError("iptsd was built without its systemd and udev integration")


def camera_packages(source, fixes, work, out):
    camera = work / "libcamera"
    extract(source["surface-libcamera"], camera)
    extract(source["surface-libcamera-debian"], camera, strip=False)
    patch(camera, fixes / "libcamera/libcamera-0.7.2-sl7.patch")
    stage_file(fixes / "libcamera/tuning/ov02c10.yaml", camera / "src/ipa/simple/data/ov02c10.yaml")
    replace(camera / "src/ipa/simple/data/meson.build", "    'imx471.yaml',\n",
            "    'imx471.yaml',\n    'ov02c10.yaml',\n")
    changelog = camera / "debian/changelog"
    version = "0.7.2-1+clawos1"
    write_file(changelog, f"""libcamera ({version}) unstable; urgency=medium

  * Build the Surface Laptop 7 IPU7 camera integration.

 -- Claw OS <noreply@github.com>  Wed, 18 Nov 2026 00:00:00 +0000

""" + changelog.read_text())
    subprocess.run(
        ["dpkg-buildpackage", "-B", "-us", "-uc", "-jauto"], cwd=camera, check=True,
        env=dict(os.environ, DEB_BUILD_OPTIONS="nocheck nodoc noautodbgsym", DEB_BUILD_PROFILES="nocheck nodoc"),
    )
    for name in CAMERA_PACKAGES:
        artifact = f"{name}_{version}_amd64.deb"
        shutil.move(work / artifact, out / artifact)


def configs(fixes, stage):
    source = fixes / "configs/etc"
    stage_file(source / "modprobe.d/surface-sam.conf", stage / "usr/lib/modprobe.d/60-claw-surface.conf")
    write_file(stage / "usr/share/initramfs-tools/modules.d/claw-surface",
               "pinctrl_intel_platform\nsurface_aggregator\nsurface_aggregator_registry\nsurface_hid\n")
    stage_file(source / "iptsd.d/99-surface-laptop-7-touchpad.conf",
               stage / "etc/iptsd.d/99-surface-laptop-7-touchpad.conf")
    stage_file(source / "libinput/local-overrides.quirks", stage / "usr/share/libinput/65-claw-surface.quirks")
    stage_file(source / "wireplumber/wireplumber.conf.d/50-surface-sl7-hide-ipu7-raw.conf",
               stage / "usr/share/wireplumber/wireplumber.conf.d/50-claw-surface.conf")
    model = "/sys/bus/acpi/devices/MSHW0551:00"
    wake = (source / "udev/rules.d/90-surface-sam-wakeup.rules").read_text()
    write_file(stage / "usr/lib/udev/rules.d/90-claw-surface-wakeup.rules",
               wake.replace('ACTION=="add", ', f'TEST=="{model}", ACTION=="add", '))
    for name in ("btintel-pcie.sh", "sl7-card-reader.sh"):
        script = (fixes / "configs/usr/lib/systemd/system-sleep" / name).read_text()
        shebang, body = script.split("\n", 1)
        write_file(stage / "usr/lib/systemd/system-sleep" / f"claw-{name}",
                   f"{shebang}\n[ -d {model} ] || exit 0\n{body}", mode=0o755)


def build_surface(profile, source, provenance, work, out, package):
    release = profile["release"]
    os_release = Path("/etc/os-release").read_text().splitlines()
    arch = subprocess.check_output(["dpkg", "--print-architecture"], text=True).strip()
    if arch != profile["arch"] or "ID=debian" not in os_release or "VERSION_CODENAME=trixie" not in os_release:
        raise ValueError("build the Intel Surface Laptop 7 packages on amd64 Debian 13 (trixie)")
    fixes = work / "surface-fixes"
    stage = work / "surface"
    extract(source["surface-fixes"], fixes)
    kernel_modules(profile, source, fixes, work, stage)
    touchpad(source, fixes, work, stage)
    configs(fixes, stage)
    camera_packages(source, fixes, work, out)
    name = "claw-os-surface-laptop-7"
    stage_file(fixes / "LICENSE", stage / "usr/share/doc" / name / "copyright")
    version = subprocess.check_output(["dpkg-deb", "-f", source["surface-image"], "Version"], text=True).strip()
    refresh = f"depmod -a '{release}'\nupdate-initramfs -u -k '{release}'\n"
    package(stage, out, name, version + "+clawos1", profile["arch"],
            f"linux-image-{release} (= {version}), kmod, initramfs-tools, libc6, libgcc-s1, libstdc++6",
            "Claw OS drivers for Surface Laptop 7 for Business (Intel)", provenance,
            '[ "$1" = configure ] || exit 0\n' + refresh,
            f"""[ "$1" = remove ] && [ -d '/lib/modules/{release}' ] || exit 0\n""" + refresh)
