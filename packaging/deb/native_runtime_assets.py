"""Stage OS-owned shared native UI/runtime assets without Desktop."""

import argparse
import os
from pathlib import Path
import re
import shutil
import struct


ROOT = Path(__file__).resolve().parents[2]


def stage(destination, launcher_binary=None):
    destination = Path(destination).resolve()
    if destination == Path(destination.anchor):
        raise ValueError("Native runtime assets require an explicit package staging root")
    destination.mkdir(parents=True, exist_ok=True)

    def directory(relative):
        current = destination
        for component in Path(relative).parts:
            current = current / component
            if current.is_symlink():
                raise ValueError("Native runtime staging parents cannot be symlinks")
            if not current.exists():
                current.mkdir(mode=0o755)
                current.chmod(0o755)
        return current

    theme = destination / "usr/share/icons/Cosmic"
    if theme.exists() or theme.is_symlink():
        raise ValueError("Shared native icon theme is already staged")
    source = ROOT / "desktop/icons"
    directory("usr/share/icons/Cosmic")
    for group in ("freedesktop", "extra"):
        shutil.copytree(source / group / "scalable", theme / "scalable",
                        symlinks=True, dirs_exist_ok=True)
    shutil.copy2(source / "index.theme", theme / "index.theme")
    documentation = directory("usr/share/doc/claw-os-agent/cosmic-icons")
    for name in ("README.md", "LICENSE", "COPYING"):
        shutil.copy2(source / name, documentation / name)
    shutil.copy2(ROOT / "desktop/PROVENANCE.md", documentation / "PROVENANCE.md")
    if launcher_binary is not None:
        launcher_binary = Path(launcher_binary).resolve(strict=True)
        if not launcher_binary.is_file():
            raise ValueError("Launcher runtime requires a compiled regular file")
        with launcher_binary.open("rb") as stream:
            header = stream.read(64)
        if len(header) != 64 or header[:6] != b"\x7fELF\x02\x01":
            raise ValueError("Launcher runtime requires the actual compiled Linux ELF")
        if int.from_bytes(header[18:20], "little") not in (62, 183):
            raise ValueError("Unsupported Launcher runtime architecture")
        kind = struct.unpack_from("<H", header, 16)[0]
        offset = struct.unpack_from("<Q", header, 32)[0]
        width, count = struct.unpack_from("<HH", header, 54)
        size = launcher_binary.stat().st_size
        if kind not in (2, 3) or width != 56 or not 1 <= count <= 1024 \
                or offset < 64 or offset + width * count > size:
            raise ValueError("Launcher ELF has no bounded executable program headers")
        with launcher_binary.open("rb") as stream:
            stream.seek(offset)
            headers = stream.read(width * count)
        segments = [struct.unpack_from("<IIQQQQQQ", headers, index * width)
                    for index in range(count)]
        if not any(segment[0] == 1 and segment[1] & 1 and segment[5] > 0
                   and segment[2] + segment[5] <= size for segment in segments):
            raise ValueError("Launcher ELF has no executable load segment")
        program = destination / "usr/bin/pop-launcher"
        runtime = destination / "usr/lib/pop-launcher"
        if any(path.exists() or path.is_symlink() for path in (program, runtime)):
            raise ValueError("Shared Launcher runtime is already staged")
        directory("usr/bin")
        shutil.copy2(launcher_binary, program)
        program.chmod(0o755)
        backend = ROOT / "desktop/launcher-backend"
        declaration = re.search(r"^plugins := '([a-z_ ]+)'$", (backend / "justfile").read_text(),
                                re.MULTILINE)
        if declaration is None:
            raise ValueError("Original Launcher plugin declaration is unavailable")
        for plugin in declaration.group(1).split():
            target = directory(Path("usr/lib/pop-launcher/plugins") / plugin)
            manifests = list((backend / "plugins/src" / plugin).glob("*.ron"))
            if not manifests:
                raise ValueError(f"Original Launcher plugin manifest is missing: {plugin}")
            for manifest in manifests:
                shutil.copy2(manifest, target / manifest.name)
                (target / manifest.name).chmod(0o644)
            (target / plugin.replace("_", "-")).symlink_to(os.path.relpath(program, target))
        shutil.copytree(backend / "scripts", runtime / "scripts", symlinks=True)
        licenses = directory("usr/share/doc/claw-os-agent/pop-launcher")
        for name in ("README.md", "LICENSE"):
            shutil.copy2(backend / name, licenses / name)
        shutil.copy2(backend / "bin/Cargo.toml", licenses / "binary-Cargo.toml")
        shutil.copy2(ROOT / "desktop/comp/LICENSE", licenses / "GPL-3")
        shutil.copy2(backend / "debian/copyright", licenses / "copyright")
        shutil.copy2(ROOT / "desktop/PROVENANCE.md", licenses / "PROVENANCE.md")
        for path in (runtime, licenses):
            for child in path.rglob("*"):
                if child.is_dir() and not child.is_symlink():
                    child.chmod(0o755)
    for path in (theme, documentation):
        for child in path.rglob("*"):
            if child.is_dir() and not child.is_symlink():
                child.chmod(0o755)
    return theme


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--launcher-binary", type=Path)
    args = parser.parse_args()
    print(stage(args.root, args.launcher_binary))
