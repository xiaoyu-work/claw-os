"""Package model-matched Asahi DSP presets and speaker protection for Aurora."""

import os
import re
import subprocess
from pathlib import Path

from surface_laptop7 import extract, patch, replace, stage_file, write_file


MODELS = ("274", "293", "313", "314", "316", "375", "413", "414", "415", "416", "473", "474", "475", "493")
M3_MODELS = ("504", "514", "516", "575", "613", "615")
DAEMON = "usr/lib/cos/asahi/speakersafetyd"
THERMAL = "usr/share/cos/asahi/speaker-protection"
ASSETS = Path(__file__).resolve().parents[1] / "rootfs/features/kernel-platform/apple"


def m3_speaker_protection(source, work, stage, doc):
    tree = work / "asahi-speakersafetyd"
    ucm = work / "asahi-ucm"
    recipes = "pkgbuilds/speakersafetyd/"
    fix = "0001-j504-Write-the-full-speaker-names-in-conf.patch"
    extract(source["asahi-speakersafetyd"], tree)
    extract(source["asahi-ucm"], ucm)
    extract(source["amd64-hardware-recipes"], work, prefixes=(recipes,))
    patch(tree, work / recipes / fix)
    subprocess.run(["cargo", "build", "--locked", "--release", "--jobs", "4"],
                   cwd=tree, env=dict(os.environ, CARGO_HOME=str(work / "speaker-cargo"),
                                      CARGO_TARGET_DIR=str(tree / "target")), check=True)
    stage_file(tree / "target/release/speakersafetyd", stage / DAEMON, 0o755)
    for model in (*MODELS, *M3_MODELS):
        config = f"apple/j{model}.conf"
        stage_file(tree / "conf" / config, stage / THERMAL / config)
    for model in M3_MODELS:
        profiles = ucm / "ucm2/conf.d"
        card, = (profiles / "macaudio").glob(f"* J{model}.conf")
        for path in (card, *(profiles / "aop_audio").glob(f"* J{model} High-Power Audio Interface.conf")):
            stage_file(path, stage / "usr/share/alsa/ucm2/conf.d" / path.relative_to(profiles))
    ids = "|".join(f"AppleJ{model}" for model in M3_MODELS)
    write_file(stage / "usr/lib/udev/rules.d/96-claw-asahi-m3-speakers.rules", f"""SUBSYSTEM=="sound", DRIVERS=="snd-soc-macaudio", GOTO="claw_asahi_m3_audio"
GOTO="claw_asahi_m3_audio_end"
LABEL="claw_asahi_m3_audio"
KERNEL=="pcmC*D2c", ATTRS{{id}}=="{ids}", TAG+="systemd", ENV{{SYSTEMD_WANTS}}+="speakersafetyd.service"
LABEL="claw_asahi_m3_audio_end"
""")
    stage_file(tree / "LICENSE", doc / "speakersafetyd.LICENSE")
    stage_file(ucm / "LICENSE.asahi", doc / "alsa-ucm.LICENSE")
    stage_file(source["asahi-speakersafetyd"], doc / "speakersafetyd-source.tar.gz")
    stage_file(work / recipes / fix, doc / fix)


def asahi_audio_package(profile, source, provenance, work, out, package):
    key = profile["asahi_audio"]
    release = profile["release"]
    tree = work / "asahi-audio-source"
    stage = work / "asahi-audio"
    name = "claw-os-asahi-audio"
    doc = stage / "usr/share/doc" / name
    extended = "asahi-speakersafetyd" in source
    models = (*MODELS, *M3_MODELS) if extended else MODELS
    pins = (key, "asahi-speakersafetyd", "asahi-ucm", "amd64-hardware-recipes") if extended else (key,)
    extract(source[key], tree)
    subprocess.run(["make", "-C", tree, f"DESTDIR={stage}", "install"], check=True)
    conf = stage / "usr/share/wireplumber/wireplumber.conf.d/99-asahi.conf"
    replace(conf, "        node.software-dsp = required", "        pw.node-factory.adapter = required\n"
            "        node.software-dsp = required")
    graphs = dict(re.findall(r'api.alsa.path = "hw:AppleJ([0-9]{3}),1"[^}]*\}[\s\S]+?'
                             r'filter-path = "(/usr/share/asahi-audio/[^"]+)"', conf.read_text()))
    for model in models:
        preset = graphs[model]
        if not (stage / preset.lstrip("/")).is_file():
            raise ValueError(f"missing Asahi DSP preset for J{model}")
        model_path = stage / f"usr/share/cos/asahi/speakers/j{model}.conf"
        model_path.parent.mkdir(parents=True, exist_ok=True)
        model_path.symlink_to(preset)

    stage_file(tree / "LICENSE", doc / "copyright")
    stage_file(tree / "LICENCE.MIT", doc / "asahi-audio.MIT")
    stage_file(source[key], doc / "asahi-audio-source.tar.gz")
    stage_file(ASSETS / "claw-asahi-audio", stage / "usr/lib/cos/asahi/claw-asahi-audio", 0o755)
    write_file(stage / "usr/lib/systemd/system/claw-asahi-audio.service", f"""[Unit]
Description=Load protected Asahi audio on a supported Mac
ConditionKernelVersion={release}
After=systemd-udev-trigger.service

[Service]
Type=oneshot
ExecCondition=/usr/lib/cos/asahi/claw-asahi-audio check {release}
ExecStart=/usr/sbin/modprobe snd_soc_macaudio
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
""")
    override = "[Unit]\nConditionPathExistsGlob=/sys/class/sound/pcmC*D2c\n"
    if extended:
        m3_speaker_protection(source, work, stage, doc)
        override += f"\n[Service]\nExecStart=\nExecStart=/{DAEMON} -c /{THERMAL}/ -m 7\n"
    else:
        (stage / DAEMON).symlink_to("/usr/bin/speakersafetyd")
        calibration = stage / THERMAL / "apple"
        calibration.mkdir(parents=True)
        for model in MODELS:
            (calibration / f"j{model}.conf").symlink_to(f"/usr/share/speakersafetyd/apple/j{model}.conf")
    write_file(stage / "usr/lib/systemd/system/speakersafetyd.service.d/10-claw-asahi.conf", override)
    version = "1-3+native1" if extended else "1-3"
    runtime = ", libasound2t64, libgcc-s1, libc6 (>= 2.34)" if extended else ""
    package(stage, out, name, version, "arm64", f"linux-image-{release.lower()} (>= {release.lower()}+clawos1), "
            "alsa-ucm-conf-asahi (>= 8-2+deb13u1), speakersafetyd (>= 1.1.2-4), pipewire (>= 1.4.2), "
            "wireplumber (>= 0.5.8), lsp-plugins-lv2 (>= 1.2.21), bankstown-lv2 (>= 1.1.0), "
            f"triforce-lv2 (>= 0.3.2), kmod, systemd, init-system-helpers{runtime}",
            "Mac model-matched audio DSP and protected speaker activation", {k: provenance[k] for k in pins}, None)
