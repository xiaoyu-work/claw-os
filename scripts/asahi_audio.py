"""Package the Asahi DSP presets for Aurora models with Debian speaker protection."""

import re
import subprocess
from pathlib import Path

from surface_laptop7 import extract, replace, stage_file, write_file


MODELS = ("274", "293", "313", "314", "316", "375", "413", "414", "415", "416", "473", "474", "475", "493")
ASSETS = Path(__file__).resolve().parents[1] / "rootfs/features/kernel-platform/apple"


def asahi_audio_package(profile, source, provenance, work, out, package):
    key = profile["asahi_audio"]
    release = profile["release"]
    tree = work / "asahi-audio-source"
    stage = work / "asahi-audio"
    name = "claw-os-asahi-audio"
    extract(source[key], tree)
    subprocess.run(["make", "-C", tree, f"DESTDIR={stage}", "install"], check=True)
    conf = stage / "usr/share/wireplumber/wireplumber.conf.d/99-asahi.conf"
    replace(conf, "        node.software-dsp = required", "        pw.node-factory.adapter = required\n"
            "        node.software-dsp = required")
    graphs = dict(re.findall(r'api.alsa.path = "hw:AppleJ([0-9]{3}),1"[^}]*\}[\s\S]+?'
                             r'filter-path = "(/usr/share/asahi-audio/[^"]+)"', conf.read_text()))
    for model in MODELS:
        preset = graphs[model]
        if not (stage / preset.lstrip("/")).is_file():
            raise ValueError(f"missing Asahi DSP preset for J{model}")
        model_path = stage / f"usr/share/cos/asahi/speakers/j{model}.conf"
        model_path.parent.mkdir(parents=True, exist_ok=True)
        model_path.symlink_to(preset)

    doc = stage / "usr/share/doc" / name
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
    write_file(stage / "usr/lib/systemd/system/speakersafetyd.service.d/10-claw-asahi.conf",
               "[Unit]\nConditionPathExistsGlob=/sys/class/sound/pcmC*D2c\n")
    package(stage, out, name, "1-2", "arm64", f"linux-image-{release.lower()} (>= {release.lower()}+clawos1), "
            "alsa-ucm-conf-asahi (>= 8-2+deb13u1), speakersafetyd (>= 1.1.2-4), pipewire (>= 1.4.2), "
            "wireplumber (>= 0.5.8), lsp-plugins-lv2 (>= 1.2.21), bankstown-lv2 (>= 1.1.0), "
            "triforce-lv2 (>= 0.3.2), kmod, systemd, init-system-helpers",
            "Mac model-matched audio DSP and protected speaker activation", {key: provenance[key]}, None)
