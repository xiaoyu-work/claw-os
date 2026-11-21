"""Build model-specific input, camera, audio and resume support for Surface Pro 11."""

import os
import shutil
import subprocess
from textwrap import dedent

from qualcomm_audio import compile_topology
from surface_laptop7 import extract, patch, replace, stage_file, write_file


def pro11_audio(profile, work, stage, source, name):
    audioreach = work / "denali-audioreach"
    extract(source["qualcomm-audioreach"], audioreach)
    model = "X1E80100-Microsoft-Surface-Pro-11"
    topology = work / f"{model}-tplg.bin"
    compile_topology(audioreach / f"{model}.m4", audioreach, topology,
                     "b192044f01e97eabe7f6df9ae0d394086badb3c30124573210bc781093da8c03")
    stage_file(topology, stage / "usr/lib/firmware/updates/qcom/x1e80100" / topology.name)
    stage_file(audioreach / "LICENSE.BSD-3-Clause", stage / "usr/share/doc" / name / "audioreach.LICENSE")

    ucm2 = stage / "usr/share/alsa/ucm2"
    config = "MICROSOFT-Surface-Pro-11.conf"
    stage_file(profile / config, ucm2 / "Qualcomm/surface-pro-11" / config)
    replace(ucm2 / "Qualcomm/surface-pro-11" / config,
            'Include.wsam-init.File "/codecs/qcom-lpass/wsa-macro/init.conf"',
            'Include.wsam-init.File "/Qualcomm/surface-pro-11/WSAInit.conf"')
    write_file(ucm2 / "Qualcomm/surface-pro-11/WSAInit.conf", """BootSequence [
	cset "name='WSA WSA_COMP1 Switch' 0"
	cset "name='WSA WSA_COMP2 Switch' 0"
]
""")
    hifi = (profile / "Surface11-HiFi.conf").read_text()
    # Keep both panel variants within the standard WSA range even if an OLED kernel permits higher gain.
    for control, upstream, limit in (("WSA WSA_RX0 Digital Volume", 84, 81),
                                     ("WSA WSA_RX1 Digital Volume", 84, 81),
                                     ("SpkrLeft PA Volume", 12, 6), ("SpkrRight PA Volume", 12, 6)):
        original = f"cset \"name='{control}' {upstream}\""
        if hifi.count(original) != 2:
            raise ValueError(f"{model}: unexpected UCM levels for {control}")
        hifi = hifi.replace(original, f"cset \"name='{control}' {limit}\"")
    start = hifi.index("\t# Begin muted at the daily-use ceiling.")
    end = hifi.index("\t# The backend connects last.", start)
    hifi = hifi[:start] + "\t# Begin muted at -3 dB digital and 0 dB PA gain.\n" + hifi[end:]
    write_file(ucm2 / "Qualcomm/surface-pro-11/Surface11-HiFi.conf", hifi)
    aliases = ucm2 / "conf.d/x1e80100"
    aliases.mkdir(parents=True)
    for alias in (model, "MicrosoftCorporation-MicrosoftSurfacePro11thEdition-124I00117T000M0000000F0"):
        (aliases / f"{alias}.conf").symlink_to(f"../../Qualcomm/surface-pro-11/{config}")

    # Retain the pinned multiband limiter, but attach it to the real speaker node instead of starting it unconditionally.
    _, header, settings = (profile / "50-surface-pro-11-speakers.conf").read_text().partition("    args = {\n")
    graph, footer, tail = settings.rpartition("\n    }\n  }\n]")
    if not header or not footer or tail.strip():
        raise ValueError(f"{model}: unexpected speaker filter format")
    filter_path = "usr/share/cos/qualcomm/denali-speakers.conf"
    write_file(stage / filter_path, "{\n" + dedent(graph) + "\n}\n")
    write_file(stage / "usr/share/wireplumber/wireplumber.conf.d/51-surface-pro-11-speakers.conf", f"""wireplumber.profiles = {{
  main = {{
    pw.node-factory.adapter = required
    node.software-dsp = required
  }}
}}

monitor.alsa.rules = [
  {{
    matches = [ {{ node.name = "alsa_output.platform-sound.HiFi__Speaker__sink" }} ]
    actions = {{ update-props = {{ media.class = "Audio/Sink/Internal" }} }}
  }}
]

node.software-dsp.rules = [
  {{
    matches = [ {{ node.name = "alsa_output.platform-sound.HiFi__Speaker__sink" }} ]
    actions = {{
      create-filter = {{ filter-path = "/{filter_path}" hide-parent = true }}
    }}
  }}
]
""")


def pro11_package(release, source, provenance, work, out, package):
    key = "arm-kernel-recipes"
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
    pro11_audio(profile, work, stage, source, name)
    package(stage, out, name, "1-3", "arm64",
            f"linux-image-{release}, claw-os-surface-snapdragon-iptsd, bash, bluez, libcamera-ipa, "
            "libspa-0.2-libcamera, python3, sudo, systemd, udev, util-linux, "
            "alsa-ucm-conf (>= 1.2.16.1-1~bpo13+1), pipewire (>= 1.4.2), wireplumber (>= 0.5.8), "
            "lsp-plugins-lv2 (>= 1.2.21)",
            "Input, cameras, audio and resume integration for Snapdragon Surface Pro 11",
            {key: provenance[key], "qualcomm-audioreach": provenance["qualcomm-audioreach"]}, None)


def pro11_sensors_package(release, source, provenance, work, out, package):
    recipe = "arm-kernel-recipes"
    directory = "pkgbuilds/microsoft-surface-pro-11-sensors/"
    recipes = work / "denali-sensors-recipes"
    extract(source[recipe], recipes, prefixes=(directory, "LICENSE"))
    stage = work / "denali-sensors"
    name = "claw-os-surface-pro-11-sensors"
    lib = stage / "usr/lib/surface-pro-11-sensors"
    docs = stage / "usr/share/doc" / name
    stage_file(recipes / "LICENSE", docs / "omarchy-pkgs.LICENSE")
    recipes /= directory
    pins = {recipe: provenance[recipe]}
    env = os.environ.copy()

    for project, fix, license_file in (
        ("hexagonrpc", "0001-apps-std-support-writable-sensor-registry.patch", "COPYING"),
        ("libssc", "0001-light-use-surface-color-sensor.patch", "LICENSE"),
        ("iio-sensor-proxy", None, "COPYING"),
    ):
        key = f"denali-{project}"
        pins[key] = provenance[key]
        tree, build, install = (work / f"denali-{project}{suffix}" for suffix in ("", "-build", "-install"))
        extract(source[key], tree)
        if fix:
            patch(tree, recipes / fix)
        options = ["-Dssc-support=enabled"] if project == "iio-sensor-proxy" else []
        subprocess.run(["meson", "setup", build, tree, "--prefix=/usr", "--libdir=lib",
                        "--libexecdir=libexec", "--buildtype=release", *options], env=env, check=True)
        subprocess.run(["meson", "compile", "-C", build], check=True)
        subprocess.run(["meson", "install", "-C", build, "--destdir", install, "--no-rebuild"], check=True)
        install /= "usr"
        stage_file(tree / license_file, docs / f"{project}.LICENSE")
        if project == "libssc":
            pkgconfig = install / "lib/pkgconfig"
            replace(pkgconfig / "libssc.pc", "prefix=/usr\n", f"prefix={install}\n")
            env["PKG_CONFIG_PATH"] = str(pkgconfig)
        if project != "iio-sensor-proxy":
            library = "libssc" if project == "libssc" else "libhexagonrpc"
            lib.mkdir(parents=True, exist_ok=True)
            for path in (install / "lib").glob(f"{library}.so.*"):
                shutil.copy2(path, lib / path.name, follow_symlinks=False)
        if project != "libssc":
            binary = "bin/hexagonrpcd" if project == "hexagonrpc" else "libexec/iio-sensor-proxy"
            stage_file(install / binary, lib / binary.split("/")[-1], 0o755)

    unit = stage / "usr/lib/systemd/system/surface-pro-11-sensors.service"
    stage_file(recipes / unit.name, unit)
    replace(unit, "Documentation=file:///usr/share/doc/microsoft-surface-pro-11-sensors/README.package.md\n", "")
    stage_file(recipes / "surface-pro-11-sensors-sleep", stage / "usr/lib/systemd/system-sleep/surface-pro-11-sensors", 0o755)
    write_file(stage / "usr/lib/systemd/system/iio-sensor-proxy.service.d/10-surface-pro-11-sensors.conf", """[Unit]
After=surface-pro-11-sensors.service

[Service]
ExecStart=
ExecStart=/usr/lib/surface-pro-11-sensors/iio-sensor-proxy
Environment=LD_LIBRARY_PATH=/usr/lib/surface-pro-11-sensors
RestrictAddressFamilies=AF_QIPCRTR
""")
    write_file(stage / "usr/lib/udev/rules.d/81-surface-pro-11-sensors.rules",
               'SUBSYSTEM=="misc", KERNEL=="fastrpc-adsp", TEST=="/var/lib/surface-pro-11-sensors/root/sensors/sns_reg.conf", '
               'ENV{IIO_SENSOR_PROXY_TYPE}+="ssc-light", TAG+="systemd", '
               'ENV{SYSTEMD_WANTS}+="surface-pro-11-sensors.service iio-sensor-proxy.service"\n')
    package(stage, out, name, "1-2", "arm64", f"linux-image-{release}, iio-sensor-proxy, libc6, libglib2.0-0t64, "
            "libgudev-1.0-0, libpolkit-gobject-1-0, libqmi-glib5, libprotobuf-c1, systemd, udev",
            "Qualcomm ambient light sensor service for Snapdragon Surface Pro 11", pins, None)
