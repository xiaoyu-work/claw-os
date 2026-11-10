"""Pure owner-path validation and lexical previews, never authority or mounts."""
from typing import Any

from .generated import validate_owner_path_need


def validate_owner_path(verb: str, path: str) -> None:
    validate_owner_path_need({"verb": verb, "scope": {"kind": "owner-path", "path": path}})
    if len(path.encode("utf-8")) > 4096 or any(len(part.encode("utf-8")) > 255 for part in path.split("/")):
        raise ValueError("owner-path exceeds its UTF-8 path or segment bound")


def resolve_owner_path(verb: str, path: str, owner_home: str) -> str:
    """Read-only lexical preview; Root independently supplies verified home."""
    validate_owner_path(verb, path)
    if not isinstance(owner_home, str) or not owner_home.startswith("/") or "*" in owner_home:
        raise ValueError("owner home must be absolute and contain no pattern")
    validate_owner_path("fs.read", owner_home[1:])
    resolved = f"{owner_home}/{path}"
    if len(resolved.encode("utf-8")) > 4096:
        raise ValueError("resolved owner path exceeds 4096 UTF-8 bytes")
    return resolved


def validate_owner_path_bindings(manifest: dict[str, Any]) -> None:
    entries = list((manifest.get("operations") or {}).values())
    entries.extend((manifest.get("mcp") or {}).get("tools", []))
    gui_entries = []
    desktop = manifest.get("desktop")
    if isinstance(desktop, dict):
        gui_entries = [desktop, *(desktop.get("entries") or {}).values()]
        entries.extend(gui_entries)
    for entry in entries:
        if not isinstance(entry, dict):
            continue
        _validate_owner_path_needs(entry.get("needs") or [])
    for gui in gui_entries:
        if isinstance(gui, dict) and "optional_needs" in gui:
            optional = gui["optional_needs"]
            if not isinstance(optional, list):
                raise ValueError("GUI optional_needs must be an array")
            _validate_owner_path_needs(optional)


def _validate_owner_path_needs(needs: list[Any]) -> None:
    for need in needs:
        if isinstance(need, dict) and isinstance(need.get("scope"), dict) and need["scope"].get("kind") == "owner-path":
            validate_owner_path_need(need)
            validate_owner_path(need["verb"], need["scope"]["path"])
