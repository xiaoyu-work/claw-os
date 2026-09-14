"""The pinned frontend artifact/attribution contract consumed by packaging."""

import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import struct


SUPPORTED_TARGETS = {
    "x86_64-unknown-linux-gnu": 62,
    "aarch64-unknown-linux-gnu": 183,
}
MARKER_VERSION = 2
ARTIFACT_KIND = "claw-agent-tui"
STAGING_RECIPE = {"strip": "debug", "tool": "gnu-target-binutils"}


class BuildError(Exception):
    pass


def relative_path(value):
    if not isinstance(value, str):
        raise BuildError("Expected a repository-relative path string")
    path = PurePosixPath(value)
    if not path.parts or path.is_absolute() or ".." in path.parts or "\\" in value:
        raise BuildError(f"Expected a repository-relative path, got {value!r}")
    return Path(*path.parts)


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def pin_sha256(pin):
    canonical = json.dumps(pin, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(canonical).hexdigest()


def validate_staging_recipe(recipe):
    if recipe != STAGING_RECIPE:
        raise BuildError("Unsupported staging recipe; require target GNU binutils --strip-debug")


def require_same_compilation(previous_pin, current_pin):
    previous = {key: value for key, value in previous_pin.items() if key != "staging"}
    current = {key: value for key, value in current_pin.items() if key != "staging"}
    if previous != current:
        raise BuildError("Cannot restage a different source pin, patch set or Cargo recipe; rebuild it")


def validate_patch_entries(pin):
    if not isinstance(pin["patches"], list):
        raise BuildError("Patches must be an ordered list")
    seen = set()
    for entry in pin["patches"]:
        if not isinstance(entry, dict) or set(entry) != {"path", "sha256", "description"}:
            raise BuildError("Patch entries require path, sha256 and description")
        path = relative_path(entry["path"])
        if path.parts[0] != "patches" or path.suffix != ".patch":
            raise BuildError("Maintained patches must be .patch files below terminal/patches")
        if entry["path"] in seen:
            raise BuildError(f"Duplicate patch: {entry['path']}")
        seen.add(entry["path"])
        if not isinstance(entry["sha256"], str) or not re.fullmatch(r"[0-9a-f]{64}", entry["sha256"]):
            raise BuildError(f"Invalid patch SHA-256: {entry['path']}")
        if not isinstance(entry["description"], str) or not entry["description"].strip():
            raise BuildError(f"Patch needs a change description: {entry['path']}")


def read_patch_inputs(pin, directory):
    validate_patch_entries(pin)
    patches = []
    for entry in pin["patches"]:
        path = _bundle_path(directory, entry["path"])
        with path.open("rb") as stream:
            data = stream.read(8 * 1024 * 1024 + 1)
        if len(data) > 8 * 1024 * 1024 or hashlib.sha256(data).hexdigest() != entry["sha256"]:
            raise BuildError(f"Patch digest mismatch: {entry['path']}")
        patches.append((entry, data))
    return patches


def modifications_bytes(pin):
    lines = [
        "Claw OS downstream modifications to OpenAI Codex",
        f"Upstream: {pin['upstream']['repository']}",
        f"Unmodified upstream revision: {pin['upstream']['revision']}",
        "Original LICENSE and NOTICE are preserved alongside this record.",
        "",
    ]
    for entry in pin["patches"]:
        lines.extend([entry["path"], f"SHA-256: {entry['sha256']}", entry["description"], ""])
    return ("\n".join(lines) + "\n").encode("utf-8")


def validate_elf(binary, target):
    if target not in SUPPORTED_TARGETS:
        raise BuildError(f"Unsupported TUI target: {target}")
    if binary.is_symlink() or not binary.is_file():
        raise BuildError(f"Missing or non-regular TUI executable: {binary}")
    with binary.open("rb") as stream:
        header = stream.read(64)
    if (
        len(header) != 64
        or header[:7] != b"\x7fELF\x02\x01\x01"
        or header[7] not in (0, 3)
        or struct.unpack_from("<H", header, 16)[0] not in (2, 3)
        or struct.unpack_from("<I", header, 20)[0] != 1
        or struct.unpack_from("<Q", header, 24)[0] == 0
        or struct.unpack_from("<H", header, 52)[0] != 64
    ):
        raise BuildError(f"TUI artifact is not a Linux ELF64 executable: {binary}")
    machine = struct.unpack_from("<H", header, 18)[0]
    if machine != SUPPORTED_TARGETS[target]:
        raise BuildError(f"Wrong TUI architecture: ELF machine {machine}, expected {target}")


def debug_sections(binary):
    """Return non-allocated debugging sections removed by GNU --strip-debug."""
    with binary.open("rb") as stream:
        header = stream.read(64)
        offset = struct.unpack_from("<Q", header, 40)[0]
        entry_size, count, names_index = struct.unpack_from("<HHH", header, 58)
        if offset == 0 and count == 0:
            return []
        file_size = binary.stat().st_size
        if entry_size < 64 or entry_size > 256 or offset + entry_size > file_size:
            raise BuildError("Invalid ELF section table")
        stream.seek(offset)
        first = stream.read(entry_size)
        if count == 0:
            count = struct.unpack_from("<Q", first, 32)[0]
        if names_index == 65535:
            names_index = struct.unpack_from("<I", first, 40)[0]
        if (
            count * entry_size > 8 * 1024 * 1024
            or offset + count * entry_size > file_size
            or not 0 < names_index < count
        ):
            raise BuildError("Invalid ELF section-name table")
        stream.seek(offset)
        sections = stream.read(count * entry_size)
        names_header = names_index * entry_size
        names_offset, names_size = struct.unpack_from("<QQ", sections, names_header + 24)
        if names_size > 8 * 1024 * 1024 or names_offset + names_size > file_size:
            raise BuildError("Oversized or invalid ELF section names")
        stream.seek(names_offset)
        names = stream.read(names_size)
    found = []
    for index in range(count):
        start = struct.unpack_from("<I", sections, index * entry_size)[0]
        end = names.find(b"\0", start)
        if start >= len(names) or end < 0:
            raise BuildError("Invalid ELF section name")
        name = names[start:end]
        flags = struct.unpack_from("<Q", sections, index * entry_size + 8)[0]
        if name.startswith((b".debug", b".zdebug")) and not flags & 2:
            found.append(name.decode("ascii", errors="replace"))
    return found


def artifact_receipt(pin, build_info, binary, assets):
    return {
        **build_info,
        "schema_version": MARKER_VERSION,
        "artifact_kind": ARTIFACT_KIND,
        "upstream_revision": pin["upstream"]["revision"],
        "source_pin_sha256": pin_sha256(pin),
        "cargo_recipe": pin["cargo"],
        "staging_recipe": pin.get("staging"),
        "binary_sha256": sha256(binary),
        "license_sha256": {
            name: sha256(assets / relative_path(name))
            for name in pin["runtime"]["license_files"]
        },
    }


def _bundle_path(root, relative, *, directory=False):
    path = root
    for part in relative_path(relative).parts:
        path /= part
        if path.is_symlink():
            raise BuildError(f"Artifact bundles must not contain symlinked paths: {path}")
    if not (path.is_dir() if directory else path.is_file()):
        raise BuildError(f"Missing artifact bundle {'directory' if directory else 'file'}: {path}")
    return path


def _json_object(path):
    with path.open("rb") as stream:
        data = stream.read(128 * 1024 + 1)
    if len(data) > 128 * 1024:
        raise BuildError(f"Oversized artifact metadata: {path}")
    try:
        value = json.loads(data)
    except (json.JSONDecodeError, UnicodeDecodeError) as error:
        raise BuildError(f"Invalid artifact metadata: {path}") from error
    if not isinstance(value, dict):
        raise BuildError(f"Artifact metadata must be a JSON object: {path}")
    return value


def read_staged_pin(root, assets_relative):
    return _json_object(_bundle_path(root, f"{assets_relative}/source.json"))


def verify_artifact(root, pin, target):
    assets_relative = pin["runtime"]["assets_directory"]
    assets = _bundle_path(root, assets_relative, directory=True)
    marker = _bundle_path(root, f"{assets_relative}/build-info.json")
    receipt = _json_object(marker)
    if (
        receipt.get("schema_version") != MARKER_VERSION
        or receipt.get("artifact_kind") != ARTIFACT_KIND
    ):
        raise BuildError("Missing or obsolete TUI provenance marker; rebuild the pinned frontend")
    staged_pin = read_staged_pin(root, assets_relative)
    expected_pin_hash = pin_sha256(pin)
    if (
        receipt.get("upstream_revision") != pin["upstream"]["revision"]
        or receipt.get("source_pin_sha256") != expected_pin_hash
        or pin_sha256(staged_pin) != expected_pin_hash
        or receipt.get("cargo_recipe") != pin["cargo"]
        or receipt.get("profile") != pin["cargo"]["profile"]
        or receipt.get("default_features") != pin["cargo"]["default_features"]
        or receipt.get("features") != pin["cargo"]["features"]
        or receipt.get("patches") != pin["patches"]
        or receipt.get("staging_recipe") != pin.get("staging")
    ):
        raise BuildError("Stale TUI source pin or build recipe; rebuild the pinned frontend")
    if receipt.get("target") != target:
        raise BuildError(f"Wrong TUI target: {receipt.get('target')!r}, expected {target}")
    binary = _bundle_path(root, pin["runtime"]["binary"])
    validate_elf(binary, target)
    if receipt.get("binary_sha256") != sha256(binary):
        raise BuildError("TUI executable digest does not match its provenance marker")
    if pin.get("staging") is not None:
        validate_staging_recipe(pin["staging"])
        tool = receipt.get("strip_tool")
        if (
            not isinstance(tool, dict)
            or tool.get("target") != target
            or tool.get("name") != target.replace("-unknown", "") + "-strip"
            or not isinstance(tool.get("version"), str)
            or not tool["version"].startswith("GNU strip ")
            or not isinstance(receipt.get("staging_input_sha256"), str)
            or not re.fullmatch(r"[0-9a-f]{64}", receipt["staging_input_sha256"])
        ):
            raise BuildError("Missing or invalid controlled-strip provenance")
        if debug_sections(binary):
            raise BuildError("Staged TUI still contains debug sections")
    license_hashes = receipt.get("license_sha256")
    if not isinstance(license_hashes, dict) or set(license_hashes) != set(pin["runtime"]["license_files"]):
        raise BuildError("TUI attribution manifest is incomplete")
    for name, expected in license_hashes.items():
        path = _bundle_path(root, f"{assets_relative}/{name}")
        actual = sha256(path)
        if actual != expected or (name in pin["checksums"] and actual != pin["checksums"][name]):
            raise BuildError(f"TUI attribution digest mismatch: {name}")
    if pin["patches"]:
        read_patch_inputs(pin, assets)
        modifications = _bundle_path(root, f"{assets_relative}/MODIFICATIONS")
        if modifications.read_bytes() != modifications_bytes(pin):
            raise BuildError("TUI downstream modification notice does not match the source pin")
    return {
        **receipt,
        "binary": str(binary),
        "assets_directory": str(assets),
        "provenance": str(marker),
    }
