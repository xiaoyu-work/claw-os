"""Controlled native-binutils transformation of a staging copy, never Cargo output."""

from dataclasses import dataclass
from pathlib import Path
import re
import shutil
import subprocess

from artifacts import BuildError, debug_sections, validate_elf, validate_staging_recipe


STRIP_TARGETS = {
    "x86_64-unknown-linux-gnu": ("x86_64-linux-gnu-strip", "elf64-x86-64"),
    "aarch64-unknown-linux-gnu": ("aarch64-linux-gnu-strip", "elf64-littleaarch64"),
}


@dataclass(frozen=True)
class StripTool:
    program: Path
    name: str
    version: str
    target: str


def select_strip_tool(target):
    if target not in STRIP_TARGETS:
        raise BuildError(f"Unsupported strip target: {target}")
    name, elf_format = STRIP_TARGETS[target]
    executable = shutil.which(name)
    if executable is None:
        raise BuildError(f"Required strip tool is unavailable: {name}; use the matching native runner/binutils")
    version = subprocess.run(
        [executable, "--version"], capture_output=True, text=True, check=True,
    ).stdout.splitlines()
    if not version or not version[0].startswith("GNU strip "):
        raise BuildError(f"Expected GNU target binutils, not this strip tool: {name}")
    info = subprocess.run(
        [executable, "--info"], capture_output=True, text=True, check=True,
    ).stdout
    if not re.search(r"(?m)^" + re.escape(elf_format) + r"\s*$", info):
        raise BuildError(f"Wrong-target strip tool: {name} does not support {elf_format}")
    return StripTool(Path(executable), name, version[0], target)


def strip_staged_binary(binary, target, recipe, tool):
    validate_staging_recipe(recipe)
    if tool.target != target:
        raise BuildError(f"Wrong-target strip tool: {tool.target}, expected {target}")
    validate_elf(binary, target)
    subprocess.run([str(tool.program), "--strip-debug", str(binary)], check=True)
    validate_elf(binary, target)
    if debug_sections(binary):
        raise BuildError("Strip tool left debug sections in the staged executable")
    return {"name": tool.name, "version": tool.version, "target": tool.target}
