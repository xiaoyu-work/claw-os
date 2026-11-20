"""Build matched Snapdragon ALSA configuration and AudioReach topology packages."""

import hashlib
import subprocess

from surface_laptop7 import extract, replace, stage_file, write_file


TOPOLOGIES = (
    ("x1e80100", "X1P42100-ACER-Aspire-16-AI", "X1E80100-Romulus",
     "aa303397750f883ecaeed874d7547da658500596247676a6d405bf1ec43290b5"),
    ("x1e80100", "X1P42100-ASUS-ProArt-PZ13", "X1P42100-Microsoft-Surface-Pro-12in",
     "89b731f3f98fc2b84699bca39a56e390925a44a26d5aea80382cf617e00c08d8"),
    ("x1e80100", "X1E80100-HP-OMNIBOOK-3", "X1E80100-Romulus",
     "aa303397750f883ecaeed874d7547da658500596247676a6d405bf1ec43290b5"),
    ("glymur", "GLYMUR-ASUS-Zenbook-A16-UX3607OA", "GLYMUR-CRD",
     "ea851335b7e8e13977a597250f2c0f1b6434d68f2248ae85a332e158fd64db57"),
)


def audio_packages(source, provenance, work, out, package):
    profile_dir = "pkgbuilds/alsa-ucm-conf/"
    zenbook_dir = "pkgbuilds/asus-zenbook-a16-audio/"
    romulus = work / "romulus-ucm"
    recipes = work / "qualcomm-ucm"
    extract(source["surface-snapdragon-fixes"], romulus, prefixes=(profile_dir,))
    extract(source["arm-kernel-recipes"], recipes, prefixes=(profile_dir, zenbook_dir, "LICENSE"))
    romulus, boards, zenbook = romulus / profile_dir, recipes / profile_dir, recipes / zenbook_dir

    subprocess.run(["apt-get", "download", "alsa-ucm-conf/trixie-backports", "firmware-qcom-soc/trixie-backports"],
                   cwd=work, check=True)
    ucm_deb, = work.glob("alsa-ucm-conf_*.deb")
    _, base_version, _ = ucm_deb.name.split("_")
    version = f"{base_version}+clawos3"
    ucm_stage = work / "combined-ucm"
    subprocess.run(["dpkg-deb", "--raw-extract", ucm_deb, ucm_stage], check=True)
    ucm2 = ucm_stage / "usr/share/alsa/ucm2"
    profiles = ucm2 / "Qualcomm/x1e80100"
    for path in ("MICROSOFT-Surface-Laptop-7.conf", "SurfaceLaptop7-HiFi.conf"):
        stage_file(romulus / path, profiles / path)
    for path in ("ACER-Aspire-16-AI.conf", "Aspire-16-AI-HiFi.conf", "ASUS-ProArt-PZ13.conf", "ProArt-PZ13-HiFi.conf"):
        stage_file(boards / path, profiles / path)
    dispatcher = profiles / "x1e80100.conf"
    replace(dispatcher, "|(Microsoft Corporation.*Surface.*Microsoft Surface Laptop, 7th Edition)", "")
    replace(dispatcher, "|(HP.*Omnibook X.*)|", "|(HP.*Omnibook X.*)|(HP.*Omni[Bb]ook 3[ -].*)|")
    with dispatcher.open("a") as stream:
        stream.write("\n" + (romulus / "x1e80100-microsoft-surface-laptop-7.conf").read_text())
        for path in ("x1e80100-acer-aspire-16-ai.conf", "x1e80100-asus-proart-pz13.conf"):
            stream.write("\n" + (boards / path).read_text())
    for model in ("UX3480QA", "UX3480QAB"):
        alias = ucm2 / "conf.d/x1e80100" / f"ASUSTeKCOMPUTERINC.-Zenbook14{model}-1.0-{model}.conf"
        alias.symlink_to("../../Qualcomm/x1e80100/LENOVO-T14s.conf")
    stage_file(zenbook / "ucm-asus-zenbook-a16.conf",
               ucm2 / "conf.d/glymur/ASUSTeKCOMPUTERINC.-ZenbookA16UX3607OA-1.0-UX3607OA.conf")
    stage_file(recipes / "LICENSE", ucm_stage / "usr/share/doc/alsa-ucm-conf/omarchy-pkgs.LICENSE")
    replace(ucm_stage / "DEBIAN/control", f"\nVersion: {base_version}\n", f"\nVersion: {version}\n")
    checksums = []
    for path in sorted(ucm_stage.glob("usr/**/*")):
        if path.is_file() and not path.is_symlink():
            checksums.append(f"{hashlib.md5(path.read_bytes(), usedforsecurity=False).hexdigest()}  {path.relative_to(ucm_stage)}")
    write_file(ucm_stage / "DEBIAN/md5sums", "\n".join(checksums) + "\n")
    subprocess.run(["dpkg-deb", "--root-owner-group", "--build", ucm_stage,
                    out / f"alsa-ucm-conf_{version}_all.deb"], check=True)

    firmware_deb, = work.glob("firmware-qcom-soc_*.deb")
    firmware = work / "debian-qcom-firmware"
    subprocess.run(["dpkg-deb", "-x", firmware_deb, firmware], check=True)
    stage = work / "qualcomm-model-audio"
    name = "claw-os-qualcomm-model-audio"
    for family, model, stock, expected in TOPOLOGIES:
        topology = firmware / "usr/lib/firmware/qcom" / family / f"{stock}-tplg.bin"
        if hashlib.sha256(topology.read_bytes()).hexdigest() != expected:
            raise ValueError(f"{stock}: topology has changed from the qualified board profile")
        stage_file(topology, stage / "usr/lib/firmware/updates/qcom" / family / f"{model}-tplg.bin")
    stage_file(zenbook / "60-asus-zenbook-a16-soundwire.rules",
               stage / "usr/lib/udev/rules.d/60-asus-zenbook-a16-soundwire.rules")
    stage_file(zenbook / "60-asus-zenbook-a16-soft-volume.conf",
               stage / "usr/share/wireplumber/wireplumber.conf.d/60-asus-zenbook-a16-soft-volume.conf")
    stage_file(recipes / "LICENSE", stage / "usr/share/doc" / name / "omarchy-pkgs.LICENSE")
    stage_file(firmware / "usr/share/doc/firmware-qcom-soc/copyright",
               stage / "usr/share/doc" / name / "firmware-qcom-soc.copyright")
    package(stage, out, name, "1-2", "all", f"alsa-ucm-conf (>= {version})",
            "Matched AudioReach topologies for supported Acer, ASUS and HP Snapdragon models",
            {key: provenance[key] for key in ("surface-snapdragon-fixes", "arm-kernel-recipes")}, None)


def compile_topology(config, audioreach, topology, expected):
    conf = topology.with_suffix(".conf")
    with conf.open("wb") as stream:
        subprocess.run(["m4", "-I", audioreach, config], stdout=stream, check=True)
    subprocess.run(["alsatplg", "-c", conf, "-o", topology], check=True)
    if hashlib.sha256(topology.read_bytes()).hexdigest() != expected:
        raise ValueError(f"{config.stem}: topology differs from the qualified build")


def laptop8_audio_package(release, source, provenance, work, out, package):
    profile_dir = "pkgbuilds/microsoft-surface-laptop-8-support/"
    recipes = work / "laptop8-recipes"
    audioreach = work / "audioreach"
    extract(source["arm-kernel-recipes"], recipes, prefixes=(profile_dir,))
    extract(source["qualcomm-audioreach"], audioreach)
    profile = recipes / profile_dir
    stage = work / "laptop8-audio"
    model = "GLYMUR-Microsoft-Surface-Laptop-8"
    names = ("MicrosoftCorporation-SurfaceLaptop13.8in8thEdSnapdragon-124I00137T000M000000",
             "MicrosoftCorporation-SurfaceLaptop15in8thEdSnapdragon-124I00137T000M0000000F")
    topology = work / f"{model}-tplg.bin"
    compile_topology(profile / f"{model}.m4", audioreach, topology,
                     "4f81077a0d8bb7568bfcd490eeea27cf25ec08357eb0f9c5788413bdd79af28e")
    stage_file(topology, stage / "usr/lib/firmware/updates/qcom/glymur" / topology.name)
    ucm2 = stage / "usr/share/alsa/ucm2"
    for file in (f"{model}.conf", "HiFi.conf", "SpeakerSeq.conf", "WSAInit.conf"):
        stage_file(profile / file, ucm2 / "Qualcomm/surface-laptop8" / file)
    aliases = ucm2 / "conf.d/glymur"
    aliases.mkdir(parents=True)
    for name in names:
        (aliases / f"{name}.conf").symlink_to(f"../../Qualcomm/surface-laptop8/{model}.conf")
    rules = "60-surface-laptop8-usb-runtime-pm.rules"
    modalias = "|".join(f"dmi:*svnMicrosoftCorporation:pnSurfaceLaptop{size}in8thEdSnapdragon:*"
                        for size in ("13.8", "15"))
    write_file(stage / "usr/lib/udev/rules.d" / rules, (profile / rules).read_text().replace(
        'ATTR{power/control}="auto"', f'ATTR{{[dmi/id]modalias}}=="{modalias}", ATTR{{power/control}}="auto"'))
    stage_file(profile / "99-surface-laptop8.quirks", stage / "usr/share/libinput/99-surface-laptop8.quirks")
    name = "claw-os-surface-laptop-8-snapdragon"
    docs = stage / "usr/share/doc" / name
    stage_file(profile / "LICENSE", docs / "copyright")
    stage_file(audioreach / "LICENSE.BSD-3-Clause", docs / "audioreach.LICENSE")
    package(stage, out, name, "1-1", "arm64", f"linux-image-{release}, alsa-ucm-conf (>= 1.2.16.1-1~bpo13+1), udev",
            "Audio, input and USB suspend integration for Snapdragon Surface Laptop 8",
            {key: provenance[key] for key in ("qualcomm-audioreach", "arm-kernel-recipes")}, None)
