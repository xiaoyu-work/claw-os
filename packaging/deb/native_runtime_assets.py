"""Stage OS-owned shared native UI data without the Desktop package."""

import argparse
from pathlib import Path
import shutil


ROOT = Path(__file__).resolve().parents[2]


def stage(destination):
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
    for path in (theme, documentation):
        for child in path.rglob("*"):
            if child.is_dir() and not child.is_symlink():
                child.chmod(0o755)
    return theme


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    print(stage(parser.parse_args().root))
