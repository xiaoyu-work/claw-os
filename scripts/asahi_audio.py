"""Package model-matched Asahi DSP presets and speaker protection for Aurora."""

import os
import re
import subprocess
from pathlib import Path

from surface_laptop7 import extract, patch, replace, stage_file, write_file


MODELS = ("274", "293", "313", "314", "316", "375", "413", "414", "415", "416", "473", "474", "475", "493")
M3_MODELS = ("504", "514", "516", "575", "613", "615")
NEO_INPUTS = ("asahi-neo-guard", "asahi-neo-audio", "asahi-neo-ucm")
DAEMON = "usr/lib/cos/asahi/speakersafetyd"
NEO_DAEMON = "usr/lib/cos/asahi/speakerguardd"
THERMAL = "usr/share/cos/asahi/speaker-protection"
ASSETS = Path(__file__).resolve().parents[1] / "rootfs/features/kernel-platform/apple"


def build_guard(tree, work):
    subprocess.run(["cargo", "build", "--locked", "--release", "--jobs", "4"],
                   cwd=tree, env=dict(os.environ, CARGO_HOME=str(work / "speaker-cargo"),
                                      CARGO_TARGET_DIR=str(tree / "target")), check=True)


def m3_speaker_protection(source, work, stage, doc):
    tree = work / "asahi-speakersafetyd"
    ucm = work / "asahi-ucm"
    recipes = "pkgbuilds/speakersafetyd/"
    fix = "0001-j504-Write-the-full-speaker-names-in-conf.patch"
    extract(source["asahi-speakersafetyd"], tree)
    extract(source["asahi-ucm"], ucm)
    extract(source["amd64-hardware-recipes"], work, prefixes=(recipes,))
    patch(tree, work / recipes / fix)
    build_guard(tree, work)
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


def neo_audio(source, work, stage, doc, release):
    guard, dsp, ucm = (work / key for key in NEO_INPUTS)
    for key, tree in zip(NEO_INPUTS, (guard, dsp, ucm), strict=True):
        extract(source[key], tree)
        stage_file(tree / ("LICENSE.asahi" if tree == ucm else "LICENSE"), doc / f"{key}.LICENSE")
    build_guard(guard, work)
    stage_file(guard / "target/release/speakerguardd", stage / NEO_DAEMON, 0o755)
    stage_file(guard / "conf/apple/j700.conf", stage / THERMAL / "apple/j700.conf")
    stage_file(source["asahi-neo-audio"], doc / "neo-audio-source.tar.gz")

    for file in ("graph.json", "mic.json", "48.wav"):
        stage_file(dsp / "firs/j700" / file, stage / "usr/share/asahi-audio/j700" / file)
    (stage / "usr/share/cos/asahi/speakers/j700.conf").symlink_to("/usr/share/asahi-audio/j700/graph.json")
    for file in ("macaudio/MacBook Neo J700.conf", "t8140-aop-audio/MacBook Neo J700 AOP Audio.conf",
                 "t8140-aop-audio/2chan/HiFi.conf"):
        path = "ucm2/conf.d/" + file
        stage_file(ucm / path, stage / "usr/share/alsa" / path)

    wireplumber = stage / "usr/share/wireplumber"
    stage_file(dsp / "conf/asahi-unplugged-routes.lua", wireplumber / "scripts/device/asahi-unplugged-routes.lua")
    stage_file(dsp / "conf/asahi-restore-volume.lua", wireplumber / "scripts/node/asahi-restore-volume.lua")
    stage_file(ASSETS / "99-claw-asahi-neo.conf", wireplumber / "wireplumber.conf.d/99-claw-asahi-neo.conf")
    write_file(stage / "usr/lib/udev/rules.d/96-claw-asahi-neo-speakers.rules", """SUBSYSTEM=="sound", DRIVERS=="snd-soc-macaudio", GOTO="claw_asahi_neo_audio"
GOTO="claw_asahi_neo_audio_end"
LABEL="claw_asahi_neo_audio"
KERNEL=="pcmC*D2c", ATTRS{id}=="AppleJ700", ENV{ACP_IGNORE}="1", TAG+="systemd", ENV{SYSTEMD_WANTS}+="claw-asahi-neo-speaker-guard.service"
LABEL="claw_asahi_neo_audio_end"
""")
    write_file(stage / "usr/lib/systemd/system/claw-asahi-neo-speaker-guard.service", f"""[Unit]
Description=Protect MacBook Neo speakers
After=claw-asahi-audio.service
StartLimitIntervalSec=60
StartLimitBurst=5

[Service]
ExecCondition=/usr/lib/cos/asahi/claw-asahi-audio check {release} speakerguardd
ExecStart=/{NEO_DAEMON} -c /{THERMAL} -C AppleJ700 --wait-for-card 30
DynamicUser=yes
SupplementaryGroups=audio
RuntimeDirectory=speakerguardd
RuntimeDirectoryMode=0700
RuntimeDirectoryPreserve=yes
UMask=0077
Restart=on-failure
RestartSec=1
DevicePolicy=closed
DeviceAllow=char-alsa rw
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
""")


def asahi_audio_package(profile, source, provenance, work, out, package):
    key = profile["asahi_audio"]
    release = profile["release"]
    tree = work / "asahi-audio-source"
    stage = work / "asahi-audio"
    name = "claw-os-asahi-audio"
    doc = stage / "usr/share/doc" / name
    extended = "asahi-speakersafetyd" in source
    models = (*MODELS, *M3_MODELS) if extended else MODELS
    pins = (key, "asahi-speakersafetyd", "asahi-ucm", "amd64-hardware-recipes", *NEO_INPUTS) if extended else (key,)
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
        neo_audio(source, work, stage, doc, release)
        override += f"""
[Service]
ExecCondition=/usr/lib/cos/asahi/claw-asahi-audio check {release} speakersafetyd
ExecStart=
ExecStart=/{DAEMON} -c /{THERMAL}/ -m 7
"""
    else:
        (stage / DAEMON).symlink_to("/usr/bin/speakersafetyd")
        calibration = stage / THERMAL / "apple"
        calibration.mkdir(parents=True)
        for model in MODELS:
            (calibration / f"j{model}.conf").symlink_to(f"/usr/share/speakersafetyd/apple/j{model}.conf")
    write_file(stage / "usr/lib/systemd/system/speakersafetyd.service.d/10-claw-asahi.conf", override)
    version = "1-3+native2" if extended else "1-3+base1"
    runtime = ", libasound2t64, libgcc-s1, libc6 (>= 2.34)" if extended else ""
    wp = "0.5.12" if extended else "0.5.8"
    package(stage, out, name, version, "arm64", f"linux-image-{release.lower()} (>= {release.lower()}+clawos1), "
            "alsa-ucm-conf-asahi (>= 8-2+deb13u1), speakersafetyd (>= 1.1.2-4), pipewire (>= 1.4.2), "
            f"wireplumber (>= {wp}), lsp-plugins-lv2 (>= 1.2.21), bankstown-lv2 (>= 1.1.0), "
            f"triforce-lv2 (>= 0.3.2), kmod, systemd, init-system-helpers{runtime}",
            "Mac model-matched audio DSP and protected speaker activation", {k: provenance[k] for k in pins}, None)
