"""Build model-specific driver packages for Surface Laptop 7."""

import hashlib
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


def kernel_modules(profile, source, fixes, work, stage, kbuild):
    release = profile["release"]
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


def touchpad(archive, patches, work, stage, *, recipes=None):
    tree = work / "iptsd"
    extract(archive, tree)
    for series_patch in sorted(patches.glob("*.patch")):
        patch(tree, series_patch)
    if recipes:
        rule = tree / "etc/udev/50-iptsd.rules.in"
        replace(rule, 'ACTION=="add"', 'ACTION!="remove"')
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
    if recipes:
        stage_file(recipes / "surface-laptop-7-15.conf", stage / "usr/share/iptsd/surface-laptop-7-15.conf")
        stage_file(recipes / "iptsd-romulus15-boot", stage / "usr/lib/iptsd/iptsd-romulus15-boot", 0o755)
        for action, board, target in (("boot", "romulus15", "multi-user"), ("resume", "romulus", "suspend")):
            unit = f"iptsd-{board}-{action}.service"
            systemd = stage / "usr/lib/systemd/system"
            stage_file(recipes / f"iptsd-romulus15-{action}.service", systemd / unit)
            if action == "resume":
                replace(systemd / unit, 'Description=Restart iptsd after resume on the Surface Laptop 7 15"',
                        "Description=Restart iptsd after resume on the Surface Laptop 7")
                replace(systemd / unit, "ConditionFirmware=device-tree-compatible(microsoft,romulus15)",
                        "ConditionFirmware=|device-tree-compatible(microsoft,romulus13)\n"
                        "ConditionFirmware=|device-tree-compatible(microsoft,romulus13-spi)\n"
                        "ConditionFirmware=|device-tree-compatible(microsoft,romulus15)")
            wants = systemd / f"{target}.target.wants"
            wants.mkdir(parents=True, exist_ok=True)
            (wants / unit).symlink_to(f"../{unit}")


def snapdragon_packages(release, source, provenance, work, out, package, unpack):
    name = "claw-os-surface-snapdragon-radio"
    stage = work / "romulus-radio"
    unpack(source["surface-snapdragon-addresses"], stage, lambda path: path.startswith("usr/"))
    wifi = stage / "usr/lib/surface-snapdragon-addresses/surface-wifi-address"
    replace(wifi, """# The rule matches any ath12k interface; act only on Microsoft machines. The
# address variable is checked below, unless an override makes it unnecessary.
vendor_path="${SURFACE_WIFI_DMI:-/sys/class/dmi/id}/sys_vendor"
if [[ ! -r "$vendor_path" || "$(<"$vendor_path")" != 'Microsoft Corporation' ]]; then""",
            """compatible=/sys/firmware/devicetree/base/compatible
if [[ ! -r $compatible ]] || ! tr '\\\\0' '\\\\n' <"$compatible" |
    grep -Eq '^microsoft,romulus(13|15)$'; then""")
    bt = stage / "usr/lib/systemd/system/surface-bluetooth-address@.service"
    compatible = ("ConditionFirmware=|device-tree-compatible(microsoft,romulus13)\n"
                  "ConditionFirmware=|device-tree-compatible(microsoft,romulus13-spi)\n"
                  "ConditionFirmware=|device-tree-compatible(microsoft,romulus15)")
    replace(bt, 'ConditionFirmware=smbios-field(sys_vendor = "Microsoft Corporation")', compatible)
    package(stage, out, name, "1-2+clawos2", "arm64", "bash, bluez, iproute2, systemd, udev",
            "Wi-Fi and Bluetooth addresses for Snapdragon Surface Laptop 7",
            {"surface-snapdragon-addresses": provenance["surface-snapdragon-addresses"]}, None)
    if "surface-snapdragon-iptsd" not in source:
        return
    if subprocess.check_output(["dpkg", "--print-architecture"], text=True).strip() != "arm64":
        raise ValueError("build the Snapdragon touchpad package in an arm64 Debian 13 environment")
    fixes = work / "romulus-fixes"
    extract(source["surface-snapdragon-fixes"], fixes, prefixes=("pkgbuilds/iptsd/", "pkgbuilds/alsa-ucm-conf/"))
    stage = work / "romulus-touchpad"
    recipes = fixes / "pkgbuilds/iptsd"
    touchpad(source["surface-snapdragon-iptsd"], recipes, work, stage, recipes=recipes)
    name = "claw-os-surface-snapdragon-touchpad"
    stage_file(work / "iptsd/LICENSE", stage / "usr/share/doc" / name / "copyright")
    romulus_fastrpc(source["surface-snapdragon-fastrpc"], work, stage, name)
    # The OV02C10 black level is 64 in its 10-bit mode; SoftISP uses 16-bit tuning.
    write_file(stage / "usr/share/libcamera/ipa/simple/ov02c10.yaml", """version: 1
algorithms:
  - BlackLevel:
      blackLevel: 4096
  - Awb:
  - Adjust:
      gamma: 2.4
  - Agc:
""")
    write_file(stage / "usr/share/wireplumber/wireplumber.conf.d/60-claw-romulus-ir.conf", """monitor.libcamera.rules = [
  {
    matches = [{ api.libcamera.path = "/base/soc@0/cci@ac15000/i2c-bus@0/camera@10" }]
    actions = { update-props = { device.disabled = true } }
  }
]
""")
    package(stage, out, name, "3.1.0-5+clawos4", "arm64",
            f"linux-image-{release}, libc6, libgcc-s1, libstdc++6, libbsd0, libyaml-0-2, systemd, udev, curl, msitools",
            "Input, camera and FastRPC integration for Snapdragon Surface Laptop 7",
            {name: spec for name, spec in provenance.items() if name.startswith("surface-snapdragon-")}, None)
    romulus_audio(fixes / "pkgbuilds/alsa-ucm-conf", work, out)


def romulus_fastrpc(archive, work, stage, package_name):
    tree = work / "fastrpc"
    extract(archive, tree)
    replace(tree / "src/Makefile.am", ';/usr/lib/rfsa/adsp;/usr/lib/dsp;',
            ';/usr/lib/dsp/cdsp;/usr/lib/rfsa/adsp;/usr/lib/dsp;')
    subprocess.run(["autoreconf", "-fi"], cwd=tree, check=True)
    libdir = "/usr/lib/aarch64-linux-gnu"
    subprocess.run([tree / "configure", "--prefix=/usr", f"--libdir={libdir}"], cwd=tree, check=True)
    subprocess.run(["make", "-C", tree / "src", f"-j{os.cpu_count()}", "libcdsprpc.la"], check=True)
    subprocess.run(["make", "-C", tree / "test", "fastrpc-healthcheck"], check=True)
    dest = stage / libdir.lstrip("/")
    dest.mkdir(parents=True, exist_ok=True)
    for path in (tree / "src/.libs").glob("libcdsprpc.so*"):
        shutil.copy2(path, dest / path.name, follow_symlinks=False)
    if not (dest / "libcdsprpc.so").is_file():
        raise ValueError("libcdsprpc.so: missing FastRPC build output")
    stage_file(tree / "test/fastrpc-healthcheck", stage / "usr/bin/fastrpc-healthcheck", 0o755)
    stage_file(tree / "LICENSE.txt", stage / "usr/share/doc" / package_name / "fastrpc.LICENSE")
    write_file(stage / "usr/lib/udev/rules.d/70-claw-romulus-fastrpc.rules",
               'SUBSYSTEM=="misc", KERNEL=="fastrpc-cdsp", TAG+="uaccess"\n'
               'SUBSYSTEM=="dma_heap", KERNEL=="system", TAG+="uaccess"\n')
    write_file(stage / "DEBIAN/triggers", "activate-noawait ldconfig\n")


def romulus_audio(recipes, work, out):
    subprocess.run(["apt-get", "download", "alsa-ucm-conf/trixie-backports"], cwd=work, check=True)
    archive, = work.glob("alsa-ucm-conf_*.deb")
    _, version, _ = archive.name.split("_")
    version += "+clawos1"
    stage = work / "romulus-audio"
    subprocess.run(["dpkg-deb", "--raw-extract", archive, stage], check=True)
    ucm = stage / "usr/share/alsa/ucm2/Qualcomm/x1e80100"
    profiles = ("MICROSOFT-Surface-Laptop-7.conf", "SurfaceLaptop7-HiFi.conf")
    for profile in profiles:
        stage_file(recipes / profile, ucm / profile)
    dispatcher = ucm / "x1e80100.conf"
    replace(dispatcher, "|(Microsoft Corporation.*Surface.*Microsoft Surface Laptop, 7th Edition)", "")
    with dispatcher.open("a") as stream:
        stream.write("\n" + (recipes / "x1e80100-microsoft-surface-laptop-7.conf").read_text())
    control = stage / "DEBIAN/control"
    replace(control, "\nVersion: " + version.removesuffix("+clawos1") + "\n", f"\nVersion: {version}\n")
    checksums = []
    for path in sorted(stage.glob("usr/**/*")):
        if path.is_file() and not path.is_symlink():
            checksums.append(f"{hashlib.md5(path.read_bytes(), usedforsecurity=False).hexdigest()}  {path.relative_to(stage)}")
    write_file(stage / "DEBIAN/md5sums", "\n".join(checksums) + "\n")
    subprocess.run(["dpkg-deb", "--root-owner-group", "--build", stage,
                    out / f"alsa-ucm-conf_{version}_all.deb"], check=True)


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


def build_surface(profile, source, provenance, work, out, package, *, debian=False):
    release = profile["release"]
    os_release = Path("/etc/os-release").read_text().splitlines()
    arch = subprocess.check_output(["dpkg", "--print-architecture"], text=True).strip()
    if arch != profile["arch"] or "ID=debian" not in os_release or "VERSION_CODENAME=trixie" not in os_release:
        raise ValueError("build the Intel Surface Laptop 7 packages on amd64 Debian 13 (trixie)")
    name = "claw-os-surface-laptop-7"
    if debian:
        from platform_kernels import digest

        version = subprocess.check_output(
            ["dpkg-query", "-W", "-f=${Version}", f"linux-image-{release}"], text=True
        ).strip()
        series = release.split("+", 1)[0].rsplit(".", 1)[0]
        source_name = f"linux-source-{series}"
        subprocess.run(["apt-get", "download", f"{source_name}={version}"], cwd=work, check=True)
        archive, = work.glob(f"{source_name}_*.deb")
        subprocess.run(["dpkg-deb", "-x", archive, work / "debian-source"], check=True)
        provenance[source_name] = {"version": version, "sha256": digest(archive),
                                   "source": "https://tracker.debian.org/pkg/linux"}
        source["surface-linux-source"] = work / "debian-source/usr/src" / f"{source_name}.tar.xz"
        kbuild = Path("/lib/modules") / release / "build"
    else:
        version = subprocess.check_output(["dpkg-deb", "-f", source["surface-image"], "Version"], text=True).strip()
        headers = work / "headers"
        subprocess.run(["dpkg-deb", "-x", source["surface-headers"], headers], check=True)
        kbuild = headers / "usr/src" / f"linux-headers-{release}"
    fixes = work / "surface-fixes"
    stage = work / "surface"
    extract(source["surface-fixes"], fixes)
    kernel_modules(profile, source, fixes, work, stage, kbuild)
    touchpad(source["surface-iptsd"], fixes / "iptsd/patches", work, stage)
    configs(fixes, stage)
    camera_packages(source, fixes, work, out)
    stage_file(fixes / "LICENSE", stage / "usr/share/doc" / name / "copyright")
    if debian:
        write_file(stage / "etc/default/grub.d/60-claw-os-surface-laptop-7.cfg",
                   f'GRUB_TOP_LEVEL="/boot/vmlinuz-{release}"\n')
    refresh = f"depmod -a '{release}'\nupdate-initramfs -u -k '{release}'\n"
    package(stage, out, name, version + "+clawos1", profile["arch"],
            f"linux-image-{release} (= {version}), kmod, initramfs-tools, libc6, libgcc-s1, libstdc++6",
            "Claw OS drivers for Surface Laptop 7 for Business (Intel)", provenance,
            '[ "$1" = configure ] || exit 0\n' + refresh,
            f"""[ "$1" = remove ] && [ -d '/lib/modules/{release}' ] || exit 0\n""" + refresh)
