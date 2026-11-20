"""Build matched Snapdragon ALSA configuration and AudioReach topology packages."""

import hashlib
import subprocess

from surface_laptop7 import extract, replace, stage_file, write_file


TOPOLOGIES = (
    ("X1P42100-ACER-Aspire-16-AI", "X1E80100-Romulus",
     "aa303397750f883ecaeed874d7547da658500596247676a6d405bf1ec43290b5"),
    ("X1P42100-ASUS-ProArt-PZ13", "X1P42100-Microsoft-Surface-Pro-12in",
     "89b731f3f98fc2b84699bca39a56e390925a44a26d5aea80382cf617e00c08d8"),
)


def audio_packages(source, provenance, work, out, package):
    profile_dir = "pkgbuilds/alsa-ucm-conf/"
    romulus = work / "romulus-ucm"
    boards = work / "qualcomm-ucm"
    extract(source["surface-snapdragon-fixes"], romulus, prefixes=(profile_dir,))
    extract(source["surface-snapdragon-kernel-fixes"], boards, prefixes=(profile_dir,))
    romulus, boards = romulus / profile_dir, boards / profile_dir

    subprocess.run(["apt-get", "download", "alsa-ucm-conf/trixie-backports", "firmware-qcom-soc/trixie-backports"],
                   cwd=work, check=True)
    ucm_deb, = work.glob("alsa-ucm-conf_*.deb")
    _, base_version, _ = ucm_deb.name.split("_")
    version = f"{base_version}+clawos2"
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
    with dispatcher.open("a") as stream:
        stream.write("\n" + (romulus / "x1e80100-microsoft-surface-laptop-7.conf").read_text())
        for path in ("x1e80100-acer-aspire-16-ai.conf", "x1e80100-asus-proart-pz13.conf"):
            stream.write("\n" + (boards / path).read_text())
    for model in ("UX3480QA", "UX3480QAB"):
        alias = ucm2 / "conf.d/x1e80100" / f"ASUSTeKCOMPUTERINC.-Zenbook14{model}-1.0-{model}.conf"
        alias.symlink_to("../../Qualcomm/x1e80100/LENOVO-T14s.conf")
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
    for model, stock, expected in TOPOLOGIES:
        topology = firmware / "usr/lib/firmware/qcom/x1e80100" / f"{stock}-tplg.bin"
        if hashlib.sha256(topology.read_bytes()).hexdigest() != expected:
            raise ValueError(f"{stock}: topology has changed from the qualified board profile")
        stage_file(topology, stage / "usr/lib/firmware/updates/qcom/x1e80100" / f"{model}-tplg.bin")
    stage_file(firmware / "usr/share/doc/firmware-qcom-soc/copyright",
               stage / "usr/share/doc" / name / "firmware-qcom-soc.copyright")
    package(stage, out, name, "1-1", "all", f"alsa-ucm-conf (>= {version})",
            "Matched AudioReach topologies for supported Acer and ASUS Snapdragon models",
            {key: provenance[key] for key in ("surface-snapdragon-fixes", "surface-snapdragon-kernel-fixes")}, None)
