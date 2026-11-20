"""Build local Debian platform packages from pinned kernel and boot artifacts."""

import argparse
from collections import Counter
import gzip
import hashlib
import json
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


def prepare_qualcomm_kernel(tree, recipes, release, posture):
    from surface_laptop7 import patch

    recipe = (recipes / "PKGBUILD").read_text().split("\nsource=(\n", 1)[1].split("\n)\n", 1)[0]
    series = re.findall(r"^  (\d[^\s{}]+\.patch)(?:\{,\.sig\})?$", recipe, flags=re.MULTILINE)
    if not series or set(series) != {file.name for file in recipes.glob("[0-9]*.patch")}:
        raise ValueError("the vendor kernel patch series does not match the pinned recipe")
    for name in series:
        patch(tree, recipes / name)
    patch(tree, posture)
    patch(tree, ROOT / "packaging/surface-romulus-camss.patch")
    config = tree / ".config"
    copy_required(recipes / "config.aarch64", config)
    subprocess.run([tree / "scripts/config", "--file", config, "--set-str", "LOCALVERSION",
                    "-" + release.split("-", 1)[1], "--enable", "HIBERNATION",
                    "--enable", "DEBUG_INFO_COMPRESSED_ZSTD"], check=True)


def build_qualcomm_kernel(release, source, work, stage):
    from surface_laptop7 import extract

    tree = work / "linux"
    recipe = Path("pkgbuilds/linux-omarchy-aarch64")
    extract(source["omarchy-linux-source"], tree)
    extract(source["surface-snapdragon-kernel-fixes"], work, prefixes=(f"{recipe}/",))
    prepare_qualcomm_kernel(tree, work / recipe, release, source["surface-snapdragon-posture-fix"])
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


def kernel_package(profile, source, provenance, work, out):
    release = profile["release"]
    name = f"linux-image-{release.lower()}"
    version = release.lower()
    stage = work / "kernel"
    prefix = f"usr/lib/modules/{release}"
    module_dir = stage / prefix
    source_build = "omarchy-linux-source" in source
    if source_build:
        build_qualcomm_kernel(release, source, work, stage)
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
    if "dtbs" in profile:
        directory = "boot/dtbs/linux-omarchy/qcom"
        unpack(source[profile["dtbs"]], work / "dtbs", lambda p: p.startswith(directory + "/") and p.endswith(".dtb"))
        (module_dir / "dtb").mkdir()
        boards = module_dir / "dtb/qcom"
        shutil.move(work / "dtbs" / directory, boards)
        if "romulus" in profile:
            romulus_trees(boards, work, provenance, source_build)
            version += "+clawos3" if source_build else "+clawos2"
        write_qualcomm_boards(boards)
    if "boot" in profile:
        # Do not expose internal speakers without the matching speaker protection stack.
        config = stage / "usr/lib/modprobe.d/90-claw-apple-speakers.conf"
        config.parent.mkdir(parents=True)
        config.write_text("blacklist snd_soc_macaudio\ninstall snd_soc_macaudio /bin/false\n", encoding="utf-8")
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
    release = profile["release"]
    version = "615.78.08"
    stage = work / "nvidia"
    prefix = f"usr/lib/modules/{release}/extramodules/"
    unpack(source["nvidia-modules"], stage, lambda p: p.startswith(prefix) and p.endswith(".ko.zst"))
    libraries = re.compile(r"usr/lib/(?:libnvidia-[^/]+|libcuda|libnvcuvid|lib(?:EGL|GLX|GLESv[12](?:_CM)?)_nvidia)\.so(?:\.[0-9.]+)?$")
    data = (
        f"usr/lib/firmware/nvidia/{version}/",
        "usr/share/glvnd/egl_vendor.d/10_nvidia.json",
        "usr/share/vulkan/icd.d/nvidia_icd.json",
        "usr/share/vulkan/implicit_layer.d/nvidia_layers.json",
        "usr/share/licenses/nvidia-utils/LICENSE",
        "usr/bin/nvidia-smi",
    )
    unpack(source["nvidia-userspace"], stage, lambda p: bool(libraries.fullmatch(p)) or p.startswith(data))
    for path in (prefix + "nvidia.ko.zst", "usr/bin/nvidia-smi", f"usr/lib/libcuda.so.{version}",
                 f"usr/lib/firmware/nvidia/{version}/gsp_ga10x.bin", data[-2]):
        if not (stage / path).is_file():
            raise ValueError(f"missing matching NVIDIA driver file: {path}")
    module = stage / (prefix + "nvidia.ko.zst")
    driver = subprocess.check_output(["modinfo", "-F", "version", module], text=True).strip()
    kernel = subprocess.check_output(["modinfo", "-F", "vermagic", module], text=True).split()[0]
    if driver != version or kernel != release:
        raise ValueError(f"NVIDIA {driver} for {kernel} does not match the pinned driver and kernel")
    options = stage / "usr/lib/modprobe.d/90-claw-nvidia.conf"
    options.parent.mkdir(parents=True)
    options.write_text("options nvidia_drm modeset=1 fbdev=1\nsoftdep nvidia post: nvidia_drm\n", encoding="utf-8")
    package(
        stage, out, "claw-os-nvidia-platform", version, profile["arch"],
        f"linux-image-{release.lower()}, libglvnd0, libegl1, libgl1, libgbm1, libx11-6, libxext6, libvulkan1, "
        "libnvidia-egl-gbm1, libnvidia-egl-wayland1",
        "Matched NVIDIA platform kernel modules and driver libraries", provenance,
        f"""[ "$1" = configure ] || exit 0\ndepmod -a '{release}'\nldconfig\nupdate-initramfs -u -k '{release}'\n""",
    )


def apple_boot(profile, source, work, out):
    directory = work / "apple-boot"
    m1n1 = "usr/lib/asahi-boot/m1n1.bin"
    uboot = "usr/lib/asahi-boot/u-boot-nodtb.bin"
    neo = "usr/lib/asahi-boot-j700/u-boot-nodtb.bin"
    unpack(source["m1n1"], directory, lambda p: p == m1n1 or p.startswith("usr/share/licenses/m1n1-aurora/"))
    unpack(source["uboot"], directory, lambda p: p in (uboot, neo) or p.startswith("usr/share/licenses/uboot-aurora/"))
    dtbs = work / f"kernel/usr/lib/modules/{profile['release']}/dtbs"
    trees = sorted(path for path in dtbs.glob("t*-j*.dtb") if path.is_file())
    if not trees or not any(path.name.endswith("-j700.dtb") for path in trees):
        raise ValueError("the Aurora kernel is missing its Apple bootstrap device trees")
    for suffix, uboot_path, bundle in (
        ("", uboot, [p for p in trees if not p.name.endswith("-j700.dtb")]),
        ("-j700", neo, [p for p in trees if p.name.endswith("-j700.dtb")]),
    ):
        with (out / f"boot{suffix}.bin").open("wb") as boot:
            boot.write((directory / m1n1).read_bytes())
            for tree in bundle:
                boot.write(tree.read_bytes())
            boot.write(gzip.compress((directory / uboot_path).read_bytes(), mtime=0))
    shutil.copytree(directory / "usr/share/licenses", out / "boot-licenses")


def build(lock, profile, cache, output, debian_release=None, board_tools=False):
    if debian_release:
        profile = dict(profile, release=debian_release)
        keys = [name for name in profile["surface"] if name != "surface-linux-source"]
    elif board_tools:
        keys = [*profile["romulus"], "qualcomm-audioreach", "qualcomm-firmware-index", "qualcomm-firmware-wudrivers",
                "qualcomm-firmware-update-ca"]
    else:
        keys = list(profile.get("native", [])) or [profile["kernel"]]
        keys += profile.get("surface", []) + profile.get("t2", [])
    if "dtbs" in profile:
        keys.append(profile["dtbs"])
    keys += profile.get("gpu", []) + profile.get("boot", [])
    if "romulus" in profile and not board_tools:
        keys.append("surface-snapdragon-addresses")
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
            kernel_package(profile, source, provenance, work, out)
            if "romulus" in profile:
                from surface_laptop7 import snapdragon_packages

                snapdragon_packages(profile["release"], source, provenance, work, out, package, unpack)
            if board_tools:
                from qualcomm_audio import audio_packages, laptop8_audio_package
                from qualcomm_firmware import package_firmware_index
                from surface_pro11 import pro11_package

                audio_packages(source, provenance, work, out, package)
                laptop8_audio_package(profile["release"], source, provenance, work, out, package)
                pro11_package(profile["release"], source, provenance, work, out, package)
                package_firmware_index(source, provenance, work, out, package)
            if "gpu" in profile:
                nvidia_package(profile, source, provenance, work, out)
            if "boot" in profile:
                apple_boot(profile, source, work, out)
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
    args = parser.parse_args()
    if args.debian_kernel and (args.platform != "x86-surface-laptop-7-intel" or args.describe
                               or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+\+deb13(?:\+[0-9]+)?-amd64", args.debian_kernel)):
        parser.error("--debian-kernel requires the Intel Surface profile and a Debian 13 amd64 kernel release")
    if args.board_tools and args.platform != "aarch64-qualcomm":
        parser.error("--board-tools requires the aarch64-qualcomm profile")
    profile = lock["platforms"][args.platform]
    if args.board_tools:
        profile = dict(profile, release=profile["installer_release"])
    if args.describe:
        print(profile["arch"], profile["release"], profile.get("boot_args", ""))
    else:
        if args.board_tools and subprocess.check_output(["dpkg", "--print-architecture"], text=True).strip() != "arm64":
            parser.error("--board-tools requires a native arm64 build environment")
        name = args.debian_kernel or args.platform
        if args.board_tools:
            name += "-installer"
        output = args.output or ROOT / "build/platform-kernels" / name
        build(lock, profile, args.cache, output, args.debian_kernel, args.board_tools)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, tarfile.TarError, subprocess.SubprocessError) as error:
        raise SystemExit(f"platform kernel: {error}") from error
