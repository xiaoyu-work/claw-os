"""Build local Debian platform packages from pinned kernel and boot artifacts."""

import argparse
from collections import Counter
import gzip
import hashlib
import json
import lzma
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request


ROOT = Path(__file__).resolve().parents[1]
PIN = ROOT / "packaging/platform-kernels.json"


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def fetch(url, cache, stage, expected=None):
    name = url.rsplit("/", 1)[-1]
    cached = cache / name
    result = stage / name
    if cached.is_file():
        shutil.copyfile(cached, result)
    else:
        with urllib.request.urlopen(url, timeout=90) as response, result.open("wb") as target:
            shutil.copyfileobj(response, target)
    if expected and digest(result) != expected:
        raise ValueError(f"{name}: SHA-256 does not match the repository pin")
    return result


def inputs(lock, names, cache, stage):
    keyring = stage / "omarchy-keyring.gpg"
    if any("file" in lock["inputs"][name] for name in names):
        key = fetch(lock["key_url"], cache, stage, lock["key_sha256"])
        subprocess.run(["gpg", "--batch", "--dearmor", "-o", keyring, key], check=True)
        if not (cache / key.name).exists():
            shutil.copyfile(key, cache / key.name)
    for name in names:
        spec = lock["inputs"][name]
        archive = "file" in spec
        url = spec.get("archive", lock["archive"]) + spec["file"] if archive else spec["url"]
        path = fetch(url, cache, stage, spec["sha256"])
        if archive:
            signature = fetch(url + ".sig", cache, stage)
            verified = subprocess.check_output(
                ["gpgv", "--status-fd", "1", "--keyring", keyring, signature, path],
                text=True,
            )
            signers = [line.split()[-1] for line in verified.splitlines() if line.startswith("[GNUPG:] VALIDSIG ")]
            if signers != [lock["fingerprint"]]:
                raise ValueError(f"{path.name}: unexpected archive signer")
        for item in (path, signature) if archive else (path,):
            if not (cache / item.name).exists():
                shutil.copyfile(item, cache / item.name)
        yield name, path


def unpack(archive, target, include):
    with subprocess.Popen(["zstd", "-qdc", archive], stdout=subprocess.PIPE) as stream:
        try:
            with tarfile.open(fileobj=stream.stdout, mode="r|") as members:
                for member in members:
                    name = PurePosixPath(member.name).as_posix()
                    if include(name):
                        members.extract(member, target, filter="data")
            stream.stdout.read()
        finally:
            stream.stdout.close()
        if stream.wait() != 0:
            raise ValueError(f"unable to decompress {archive.name}")


def copy_required(source, target):
    if not source.is_file() or source.stat().st_size == 0:
        raise ValueError(f"missing required platform file: {source}")
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, target)
    target.chmod(0o644)


def package(stage, out, name, version, arch, depends, description, provenance, postinst, postrm=None, *, replaces=None):
    metadata = stage / "DEBIAN"
    metadata.mkdir(exist_ok=True)
    transition = f"Breaks: {replaces}\nReplaces: {replaces}\n" if replaces else ""
    (metadata / "control").write_text(
        f"Package: {name}\nVersion: {version}\nArchitecture: {arch}\n"
        f"Maintainer: Claw OS <noreply@github.com>\nSection: kernel\nPriority: optional\n"
        f"{transition}Depends: {depends}\nDescription: {description}\n",
        encoding="utf-8",
    )
    notices = stage / "usr/share/doc" / name
    notices.mkdir(parents=True, exist_ok=True)
    (notices / "platform-sources.json").write_text(json.dumps(provenance, indent=2) + "\n", encoding="utf-8")
    for filename, script in (("postinst", postinst), ("postrm", postrm)):
        if script:
            path = metadata / filename
            path.write_text("#!/bin/sh\nset -e\n" + script, encoding="utf-8")
            path.chmod(0o755)
    result = out / f"{name}_{version}_{arch}.deb"
    subprocess.run(["dpkg-deb", "--root-owner-group", "-Zxz", "--build", stage, result], check=True)


def kernel_hooks(release, phase):
    return f"""\
export DEB_MAINT_PARAMS="$*" INITRD=Yes
for directory in /etc/kernel/{phase}.d /usr/share/kernel/{phase}.d; do
    if [ -d "$directory" ]; then
        run-parts --arg='{release}' --arg='/boot/vmlinuz-{release}' "$directory"
    fi
done
"""


def patch_series(recipes):
    recipe = (recipes / "PKGBUILD").read_text().split("\nsource=(\n", 1)[1].split("\n)\n", 1)[0]
    series = re.findall(r"^  (\d[^\s{}]+\.patch)(?:\{,\.sig\})?$", recipe, flags=re.MULTILINE)
    if not series or set(series) != {file.name for file in recipes.glob("[0-9]*.patch")}:
        raise ValueError(f"{recipes.name}: patch series does not match the pinned recipe")
    return [recipes / name for name in series]


def prepare_arm_source(source, work):
    from surface_laptop7 import extract, patch

    tree = work / "linux"
    extract(source["arm-kernel-source"], tree)
    extract(source["arm-kernel-recipes"], work,
            prefixes=("pkgbuilds/linux-omarchy-aarch64/", "pkgbuilds/linux-omarchy-dtbs/"))
    for path in patch_series(work / "pkgbuilds/linux-omarchy-aarch64"):
        patch(tree, path)
    return tree


def build_qualcomm_kernel(release, tree, source, work, stage):
    from surface_laptop7 import patch

    patch(tree, source["surface-snapdragon-posture-fix"])
    patch(tree, ROOT / "packaging/surface-romulus-camss.patch")
    config = tree / ".config"
    copy_required(work / "pkgbuilds/linux-omarchy-aarch64/config.aarch64", config)
    subprocess.run([tree / "scripts/config", "--file", config, "--set-str", "LOCALVERSION",
                    "-" + release.split("-", 1)[1], "--enable", "HIBERNATION",
                    "--enable", "DEBUG_INFO_COMPRESSED_ZSTD"], check=True)
    make = ["make", "-C", tree]
    subprocess.run([*make, "olddefconfig"], check=True)
    config = tree / ".config"
    actual_release = subprocess.check_output([*make, "-s", "kernelrelease"], text=True).strip()
    if actual_release != release or "CONFIG_HIBERNATION=y" not in config.read_text().splitlines():
        raise ValueError("Qualcomm kernel release or hibernation config does not match the installer")
    subprocess.run([*make, f"-j{len(os.sched_getaffinity(0))}", "Image", "modules"], check=True)
    subprocess.run([*make, f"INSTALL_MOD_PATH={stage / 'usr'}", "INSTALL_MOD_STRIP=1", "modules_install"], check=True)
    (stage / f"usr/lib/modules/{release}/build").unlink(missing_ok=True)
    copy_required(tree / "arch/arm64/boot/Image", stage / f"boot/vmlinuz-{release}")
    copy_required(config, stage / f"boot/config-{release}")


def build_qualcomm_dtbs(tree, work, boards):
    from surface_laptop7 import patch

    dts_dir = tree / "arch/arm64/boot/dts/qcom"
    original = set(dts_dir.glob("*.dts"))
    recipe = (work / "pkgbuilds/linux-omarchy-aarch64/PKGBUILD").read_text()
    targets = re.search(r"local dts=\(arch/arm64/boot/dts/qcom/\{([a-z0-9,-]+)\}\*\.dts\)", recipe)
    if not targets:
        raise ValueError("the pinned kernel recipe does not select Qualcomm laptop trees")
    laptop_prefixes = tuple(targets[1].split(","))
    # The board recipe starts with the laptop DTS files shipped in the kernel headers.
    selected = {path for path in original if path.name.startswith(laptop_prefixes)}
    for path in patch_series(work / "pkgbuilds/linux-omarchy-dtbs"):
        # A board patch may have landed in the pinned kernel series already.
        applied = subprocess.run(["patch", "--force", "--reverse", "--fuzz=0", "--dry-run", "-p1", "-i", path],
                                 cwd=tree, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if applied.returncode != 0:
            patch(tree, path)
    boards.mkdir(parents=True)
    prefixes = tree / "scripts/dtc/include-prefixes"
    preprocessed = work / "board.pre.dts"
    current = set(dts_dir.glob("*.dts"))
    for dts in sorted((selected & current) | (current - original)):
        subprocess.run(["gcc", "-E", "-nostdinc", "-undef", "-D__DTS__", "-x", "assembler-with-cpp",
                        "-I", prefixes, "-o", preprocessed, dts], check=True)
        subprocess.run(["dtc", "-q", "-@", "-O", "dtb", "-b", "0", "-i", dts.parent, "-i", prefixes,
                        "-o", boards / dts.with_suffix(".dtb").name, preprocessed], check=True)


def kernel_package(profile, source, provenance, work, out, *, board_tools=False):
    release = profile["release"]
    name = f"linux-image-{release.lower()}"
    version = release.lower()
    stage = work / "kernel"
    prefix = f"usr/lib/modules/{release}"
    module_dir = stage / prefix
    tree = prepare_arm_source(source, work) if profile.get("dtbs") else None
    if board_tools:
        build_qualcomm_kernel(release, tree, source, work, stage)
        patch = ROOT / "packaging/surface-romulus-camss.patch"
        provenance[patch.stem] = {
            "sha256": digest(patch),
            "source": f"https://github.com/xiaoyu-work/claw-os/blob/main/packaging/{patch.name}",
        }
    else:
        unpack(
            source[profile["kernel"]],
            stage,
            lambda p: p.startswith((f"{prefix}/kernel/", f"{prefix}/dtbs/"))
            or p in {f"{prefix}/{name}" for name in ("vmlinuz", "modules.builtin", "modules.builtin.modinfo", "modules.order")},
        )
        copy_required(module_dir / "vmlinuz", stage / f"boot/vmlinuz-{release}")
        (module_dir / "vmlinuz").unlink()
        image = (stage / f"boot/vmlinuz-{release}").read_bytes()
        start = image.find(b"IKCFG_ST\x1f\x8b\x08")
        if start >= 0:
            end = image.find(b"IKCFG_ED", start)
            if end > start:
                (stage / f"boot/config-{release}").write_bytes(gzip.decompress(image[start + 8:end]))
    if not (module_dir / "modules.builtin").is_file() or not (module_dir / "kernel").is_dir():
        raise ValueError(f"incomplete modules for {release}")
    if tree is not None:
        boards = module_dir / "dtb/qcom"
        build_qualcomm_dtbs(tree, work, boards)
        if "romulus" in profile:
            romulus_trees(boards, work, provenance, board_tools)
            version += "+clawos4" if board_tools else "+clawos3"
        else:
            version += "+clawos1"
        write_qualcomm_boards(boards)
    if "boot" in profile:
        config = stage / "usr/lib/modprobe.d/90-claw-apple-speakers.conf"
        config.parent.mkdir(parents=True)
        loader = f"/usr/lib/cos/asahi/claw-asahi-audio load '{release}'" if "asahi_audio" in profile else "/bin/false"
        # Keep alias autoload blocked; the install hook admits only calibrated boards.
        config.write_text(f"blacklist snd_soc_macaudio\ninstall snd_soc_macaudio {loader}\n", encoding="utf-8")
        # This Aurora release can write shared Secure Enclave state. Never probe it before owner consent.
        config.with_name("90-claw-apple-touch-id.conf").write_text(
            "blacklist apple_sep\ninstall apple_sep /usr/lib/cos/asahi/claw-asahi-touch-id load\n", encoding="utf-8")
        if "asahi_audio" in profile:
            version += "+clawos2"
    package(
        stage, out, name, version, profile["arch"], "kmod, initramfs-tools, linux-base",
        f"Claw OS platform Linux kernel ({release})", provenance,
        f"""[ "$1" = configure ] || exit 0\ndepmod -a '{release}'\n""" + kernel_hooks(release, "postinst"),
        """case "$1" in remove|purge) ;; *) exit 0 ;; esac\n""" + kernel_hooks(release, "postrm"),
    )


def romulus_trees(boards, work, provenance, include_ir):
    references = {
        "touchpad": "https://github.com/ItsLucas/surface-laptop-7-ubuntu-kernel/tree/25c934e1aab32d578b3fb2b0d52dd3d56221a9bc",
        "touch-i2c": "https://lkml.org/lkml/2026/9/30/518",
        "touch-spi": "https://github.com/ItsLucas/surface-laptop-7-ubuntu-kernel/tree/25c934e1aab32d578b3fb2b0d52dd3d56221a9bc",
        "camera": "https://github.com/bryce-hoehn/linux-surface-laptop-7/blob/af765428493e13b84f06c23bdea4cedd3e58cf72/patches/outgoing/ov02c10/0004-OV02C10-camera-device-tree.patch",
        "iris": "https://github.com/omacom/omarchy-pkgs/blob/b0ff9ed3b6b2b8a237a0266541b670e7faec7883/pkgbuilds/linux-omarchy-dtbs/0030-arm64-dts-qcom-romulus15-enable-the-Iris-video-codec.patch",
    }
    if include_ir:
        references["ir"] = "https://github.com/qBitnaut/omarchy-dragon-sl7/blob/de20e9ba631c44fa9ca218bd94fd5888a08d427c/pkgs/linux-sl7/patches/0044-arm64-dts-qcom-x1e80100-romulus13-Add-the-IR-camera.patch"
    overlays = {}
    for name, reference in references.items():
        file = "surface-romulus-ir.dtso" if name == "ir" else f"surface-romulus13-{name}.dtso"
        source = ROOT / "packaging" / file
        overlays[name] = work / f"{name}.dtbo"
        subprocess.run(["dtc", "-@", "-I", "dts", "-O", "dtb", "-o", overlays[name], source], check=True)
        provenance[Path(file).stem] = {
            "sha256": digest(source),
            "source": f"https://github.com/xiaoyu-work/claw-os/blob/main/packaging/{file}",
            "board_reference": reference,
        }

    ir = [overlays["ir"]] if include_ir else []
    original = boards / "x1e80100-microsoft-romulus13.dtb"
    for transport in ("spi", "i2c"):
        filename = original.name if transport == "i2c" else original.stem + "-spi.dtb"
        patched = work / filename
        subprocess.run(["fdtoverlay", "-i", original, "-o", patched, overlays["touchpad"],
                        overlays["camera"], overlays["iris"], overlays[f"touch-{transport}"], *ir], check=True)
        patched.replace(boards / filename)

    if include_ir:
        romulus15 = boards / "x1e80100-microsoft-romulus15.dtb"
        patched = work / romulus15.name
        subprocess.run(["fdtoverlay", "-i", romulus15, "-o", patched, *ir], check=True)
        patched.replace(romulus15)

    # These USB2 PHYs lose state during sleep. DWC3 fully reinitializes the
    # host controller at resume if it is not itself a wake source.
    for model in ("romulus13", "romulus13-spi", "romulus15"):
        tree = boards / f"x1e80100-microsoft-{model}.dtb"
        usb = subprocess.check_output(["fdtget", tree, "/__symbols__", "usb_mp"], text=True).strip()
        subprocess.run(["fdtput", "-d", tree, usb, "wakeup-source"], check=True)


def write_qualcomm_boards(directory):
    rows = []
    for tree in sorted(directory.glob("*.dtb")):
        if tree.name.endswith(("-crd.dtb", "-qcp.dtb", "-el2.dtb")):
            continue
        compatible = subprocess.check_output(["fdtget", tree, "/", "compatible"], text=True).split()[0]
        model = subprocess.check_output(["fdtget", tree, "/", "model"], text=True).strip()
        if not re.fullmatch(r"[a-z0-9,-]+", compatible) or not re.fullmatch(r"[A-Za-z0-9 .,+()_/-]+", model):
            raise ValueError(f"{tree.name}: unsupported model or compatible string")
        rows.append((tree.name, compatible, model))
    if not rows:
        raise ValueError("the platform kernel is missing Qualcomm board descriptions")
    labels = Counter(row[2] for row in rows)
    with (directory / "claw-boards.list").open("w", encoding="utf-8") as boards:
        for tree, compatible, model in rows:
            if labels[model] > 1:
                model += f" ({tree.removesuffix('.dtb')})"
            boards.write(f"{tree}|{compatible}|{model}\n")


def nvidia_package(profile, source, provenance, work, out):
    from nvidia_platform import stage_nvidia_userspace

    release = profile["release"]
    version = "615.78.08"
    stage = work / "nvidia"
    prefix = f"usr/lib/modules/{release}/extramodules/"
    unpack(source["nvidia-modules"], stage, lambda p: p.startswith(prefix) and p.endswith(".ko.zst"))
    module = stage / (prefix + "nvidia.ko.zst")
    if not module.is_file():
        raise ValueError(f"missing matching NVIDIA driver module: {module.name}")
    driver = subprocess.check_output(["modinfo", "-F", "version", module], text=True).strip()
    kernel = subprocess.check_output(["modinfo", "-F", "vermagic", module], text=True).split()[0]
    if driver != version or kernel != release:
        raise ValueError(f"NVIDIA {driver} for {kernel} does not match the pinned driver and kernel")
    stage_nvidia_userspace(source["nvidia-userspace"], version, work, stage)
    options = stage / "usr/lib/modprobe.d/90-claw-nvidia.conf"
    options.parent.mkdir(parents=True)
    options.write_text("options nvidia_drm modeset=1 fbdev=1\nsoftdep nvidia post: nvidia_drm\n", encoding="utf-8")
    package(
        stage, out, "claw-os-nvidia-platform", version + "+clawos1", profile["arch"],
        f"linux-image-{release.lower()}, libglvnd0, libegl1, libgl1, libgbm1, libx11-6, libxext6, libvulkan1, "
        "libnvidia-egl-gbm1, libnvidia-egl-wayland1",
        "Matched NVIDIA platform kernel modules and driver libraries", provenance,
        f"""[ "$1" = configure ] || exit 0\ndepmod -a '{release}'\nldconfig\nupdate-initramfs -u -k '{release}'\n""",
    )


def apple_boot(profile, source, work, out):
    from asahi_boot import build_uboot

    directory = work / "apple-boot"
    m1n1 = "usr/lib/asahi-boot/m1n1.bin"
    neo_stage1 = "usr/share/m1n1-aurora/stage1/m1n1-raw.bin"
    unpack(source["m1n1"], directory, lambda p: p in (m1n1, neo_stage1) or p.startswith("usr/share/licenses/m1n1-aurora/"))
    uboot, license_path = build_uboot(source, work)
    copy_required(license_path, directory / "usr/share/licenses/uboot-aurora/LICENSE")
    dtbs = work / f"kernel/usr/lib/modules/{profile['release']}/dtbs"
    trees = sorted(path for path in dtbs.glob("t*-j*.dtb") if path.is_file())
    if not trees or not any(path.name.endswith("-j700.dtb") for path in trees):
        raise ValueError("the Aurora kernel is missing its Apple bootstrap device trees")
    for suffix, bundle in (
        ("", [p for p in trees if not p.name.endswith("-j700.dtb")]),
        ("-j700", [p for p in trees if p.name.endswith("-j700.dtb")]),
    ):
        with (out / f"boot{suffix}.bin").open("wb") as boot:
            boot.write((directory / m1n1).read_bytes())
            for tree in bundle:
                boot.write(tree.read_bytes())
            boot.write(gzip.compress(uboot.read_bytes(), mtime=0))
    copy_required(directory / neo_stage1, out / "stage1-j700.bin")
    shutil.copytree(directory / "usr/share/licenses", out / "boot-licenses")


def proart_p14_firmware(profile, source, provenance, work, out):
    key = profile["speaker_firmware"]
    license_key = "n1x-proart-p14-license"
    stage = work / "proart-p14-audio"
    name = "claw-os-proart-p14-audio"
    prefix = "cs35l56-b0-dsp1-misc-104333a1"
    # ASUS appends the driver as a 7-Zip archive; do not run the Windows installer.
    carrier = source[key].read_bytes()
    offset = carrier.find(b"7z\xbc\xaf\x27\x1c")
    if offset < 0:
        raise ValueError(f"{source[key].name}: missing embedded driver archive")
    archive = work / "proart-p14.7z"
    archive.write_bytes(carrier[offset:])
    members = {
        ".wmfw": "XU_Ext/fw/35L56/3.13.4/dflt/b0_dflt_35l56_3.13.4.wmfw",
        "-l1u0.bin": "XU_Ext/tn/35L56/33A1/dflt/b0_dflt_33a1_lw_l1u0.bin",
        "-l1u1.bin": "XU_Ext/tn/35L56/33A1/dflt/b0_dflt_33a1_rw_l1u1.bin",
    }
    target = stage / "usr/lib/firmware/updates/cirrus"
    target.mkdir(parents=True)
    for suffix, member in members.items():
        original = f"{prefix}-spkid0{suffix}"
        payload = subprocess.check_output(["7z", "x", "-so", archive, member])
        if not payload:
            raise ValueError(f"{original}: empty ProArt P14 firmware")
        # Keep the original payload and prefer uncompressed native firmware once it is available.
        (target / f"{original}.xz").write_bytes(lzma.compress(payload, check=lzma.CHECK_CRC32))
        (target / f"{prefix}-spkid1{suffix}.xz").symlink_to(f"{original}.xz")
    copy_required(source[license_key], stage / "usr/share/doc" / name / "copyright")
    package(stage, out, name, "1.0.678.52824-1+clawos1", profile["arch"], "firmware-cirrus",
            "ASUS ProArt P14 H7407BA Cirrus amplifier firmware and tuning",
            {name: provenance[name] for name in (key, license_key)}, None)


def build(lock, profile, cache, output, debian_release=None, board_tools=False, apple_tools=False):
    if debian_release:
        profile = dict(profile, release=debian_release)
        keys = [name for name in profile["surface"] if name != "surface-linux-source"]
    elif board_tools:
        keys = [*profile["romulus"], "qualcomm-audioreach", "qualcomm-firmware-index", "qualcomm-firmware-wudrivers",
                "qualcomm-firmware-update-ca", "denali-hexagonrpc", "denali-libssc", "denali-iio-sensor-proxy"]
    else:
        keys = list(profile.get("native", [])) or [profile["kernel"]]
        keys += profile.get("surface", []) + profile.get("t2", [])
    if profile.get("dtbs"):
        keys += ["arm-kernel-source", "arm-kernel-recipes"]
    keys += profile.get("gpu", []) + profile.get("boot", [])
    if "boot" in profile:
        keys.append("arm-kernel-recipes")
    if profile.get("asahi_vulkan"):
        from asahi_vulkan import INPUTS as vulkan_inputs

        keys += vulkan_inputs
    if "asahi_audio" in profile:
        keys.append(profile["asahi_audio"])
    if "speaker_firmware" in profile:
        keys += [profile["speaker_firmware"], "n1x-proart-p14-license"]
    if apple_tools:
        from asahi_audio import NEO_INPUTS

        keys += ["asahi-libfprint", "asahi-speakersafetyd", "asahi-ucm", "amd64-hardware-recipes",
                 *NEO_INPUTS]
    if "romulus" in profile and not board_tools:
        keys.append("surface-snapdragon-fixes")
    output.mkdir(parents=True, exist_ok=True)
    cache.mkdir(parents=True, exist_ok=True)
    provenance = {name: lock["inputs"][name] for name in keys}
    with tempfile.TemporaryDirectory(prefix="platform-", dir=output) as temp:
        work = Path(temp)
        out = work / "out"
        out.mkdir()
        source = dict(inputs(lock, keys, cache, work))
        if debian_release:
            from surface_laptop7 import build_surface

            build_surface(profile, source, provenance, work, out, package, debian=True)
        elif "native" in profile:
            for kind, key in zip(("image", "headers"), profile["native"], strict=True):
                artifact = source[key]
                arch = subprocess.check_output(["dpkg-deb", "-f", artifact, "Architecture"], text=True).strip()
                name = subprocess.check_output(["dpkg-deb", "-f", artifact, "Package"], text=True).strip()
                if arch != profile["arch"] or name != f"linux-{kind}-{profile['release']}":
                    raise ValueError(f"{artifact.name}: package does not match {profile['arch']} {profile['release']}")
                shutil.copyfile(artifact, out / artifact.name)
            if "surface" in profile:
                from surface_laptop7 import build_surface

                build_surface(profile, source, provenance, work, out, package)
            if "t2" in profile:
                from t2_support import t2_package

                t2_package(profile["release"], source, provenance, work, out, package)
        else:
            kernel_package(profile, source, provenance, work, out, board_tools=board_tools)
            if "romulus" in profile:
                from surface_laptop7 import snapdragon_packages

                snapdragon_packages(profile["release"], source, provenance, work, out, package)
            if board_tools:
                from qualcomm_audio import audio_packages, laptop8_audio_package
                from qualcomm_firmware import package_firmware_index
                from surface_pro11 import pro11_package, pro11_sensors_package

                audio_packages(source, provenance, work, out, package)
                laptop8_audio_package(profile["release"], source, provenance, work, out, package)
                pro11_package(profile["release"], source, provenance, work, out, package)
                pro11_sensors_package(profile["release"], source, provenance, work, out, package)
                package_firmware_index(source, provenance, work, out, package)
            if "gpu" in profile:
                nvidia_package(profile, source, provenance, work, out)
            if "speaker_firmware" in profile:
                proart_p14_firmware(profile, source, provenance, work, out)
            if "boot" in profile:
                apple_boot(profile, source, work, out)
            if profile.get("asahi_vulkan"):
                from asahi_vulkan import build_asahi_vulkan

                build_asahi_vulkan(profile, source, provenance, work, out, package, unpack)
            if "asahi_audio" in profile:
                from asahi_audio import asahi_audio_package

                asahi_audio_package(profile, source, provenance, work, out, package)
            if apple_tools:
                from asahi_touch_id import asahi_touch_id_package

                asahi_touch_id_package(profile, source, provenance, work, out, package)
        (out / "platform-sources.json").write_text(json.dumps(provenance, indent=2) + "\n", encoding="utf-8")
        for artifact in sorted(out.iterdir()):
            target = output / artifact.name
            if artifact.is_dir() and target.is_dir():
                shutil.copytree(artifact, target, dirs_exist_ok=True)
            else:
                artifact.replace(target)
            print(target)


def main():
    os.umask(0o022)
    lock = json.loads(PIN.read_text(encoding="utf-8"))
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("platform", choices=lock["platforms"])
    parser.add_argument("--describe", action="store_true", help="print architecture, kernel release and optional boot arguments")
    parser.add_argument("--cache", type=Path, default=ROOT / "build/platform-kernels/cache")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--debian-kernel", metavar="RELEASE",
                        help="build the Intel driver pack against an installed Debian kernel and its headers")
    parser.add_argument("--board-tools", action="store_true", help="build the native Snapdragon installer kernel and board tools")
    parser.add_argument("--apple-tools", action="store_true", help="include native Apple Silicon speaker protection and Touch ID")
    args = parser.parse_args()
    if args.debian_kernel and (args.platform != "x86-surface-laptop-7-intel" or args.describe
                               or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+\+deb13(?:\+[0-9]+)?-amd64", args.debian_kernel)):
        parser.error("--debian-kernel requires the Intel Surface profile and a Debian 13 amd64 kernel release")
    if args.board_tools and args.platform != "aarch64-qualcomm":
        parser.error("--board-tools requires the aarch64-qualcomm profile")
    if args.apple_tools and args.platform != "aarch64-apple":
        parser.error("--apple-tools requires the aarch64-apple profile")
    profile = lock["platforms"][args.platform]
    if args.board_tools:
        profile = dict(profile, release=profile["installer_release"])
    if args.describe:
        print(profile["arch"], profile["release"], profile.get("boot_args", ""))
    else:
        if (args.board_tools or args.apple_tools) and subprocess.check_output(
            ["dpkg", "--print-architecture"], text=True,
        ).strip() != "arm64":
            parser.error("native board tools require an arm64 build environment")
        name = args.debian_kernel or args.platform
        if args.board_tools:
            name += "-installer"
        output = args.output or ROOT / "build/platform-kernels" / name
        build(lock, profile, args.cache, output, args.debian_kernel, args.board_tools, args.apple_tools)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, tarfile.TarError, subprocess.SubprocessError) as error:
        raise SystemExit(f"platform kernel: {error}") from error
