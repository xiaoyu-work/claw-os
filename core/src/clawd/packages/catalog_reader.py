import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import stat
import sys

import gi

gi.require_version("AppStream", "1.0")
from gi.repository import AppStream


ROOTS = tuple(
    Path(prefix) / catalog
    for prefix in ("/usr/share", "/var/lib", "/var/cache")
    for catalog in ("swcatalog", "app-info")
)
SOURCE_ROOTS = (*ROOTS, Path("/var/lib/apt/lists"))
MAX_FILES = 128
MAX_FILE = 16 * 1024 * 1024
MAX_SOURCE = 64 * 1024 * 1024
MAX_XML = 32 * 1024 * 1024
MAX_COMPONENTS = 100_000
PAGE_ITEMS = 100
PAGE_BYTES = 512 * 1024


class CatalogChanged(Exception):
    pass


def _stamp(info):
    return (
        info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns,
        info.st_ctime_ns, info.st_mode, info.st_uid,
    )


def _directory(path):
    for parent in (path, *path.parents):
        info = parent.stat(follow_symlinks=False)
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022:
            raise ValueError("unprotected catalog directory")


def _sources():
    paths = set()
    for root in ROOTS:
        for kind in ("xml", "xmls", "yaml"):
            directory = root / kind
            if not directory.exists():
                continue
            directory = directory.resolve(strict=True)
            if not any(directory.is_relative_to(base) for base in SOURCE_ROOTS):
                raise ValueError("catalog directory is outside standard metadata roots")
            _directory(directory)
            for entry in directory.iterdir():
                if not entry.name.endswith((".xml", ".xml.gz", ".yml", ".yml.gz", ".yaml", ".yaml.gz")):
                    continue
                if entry.lstat().st_uid != 0:
                    raise ValueError("unprotected catalog source")
                path = entry.resolve(strict=True)
                if not any(path.is_relative_to(base) for base in SOURCE_ROOTS):
                    raise ValueError("catalog source is outside standard metadata roots")
                _directory(path.parent)
                paths.add(path)
                if len(paths) > MAX_FILES:
                    raise ValueError("catalog source count exceeds its bound")
    return sorted(paths)


def snapshot():
    records = []
    digest = hashlib.sha256(b"claw.package-catalog/v1\0")
    source_bytes = 0
    for path in _sources():
        fd = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(fd, "rb") as handle:
            before = handle.fileno()
            info = os.fstat(before)
            if not stat.S_ISREG(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022:
                raise ValueError("unprotected catalog file")
            data = handle.read(MAX_FILE + 1)
            if _stamp(os.fstat(before)) != _stamp(info):
                raise CatalogChanged()
        source_bytes += len(data)
        if len(data) > MAX_FILE or source_bytes > MAX_SOURCE:
            raise ValueError("catalog source bytes exceed their bound")
        key = os.fsencode(path)
        digest.update(key + b"\0" + hashlib.sha256(data).digest())
        records.append({
            "path": path, "stamp": _stamp(info), "data": data,
            "source": hashlib.sha256(key).hexdigest(),
        })
    return digest.hexdigest(), records


def page(revision, records, cursor):
    offset = 0
    if cursor:
        expected, position = cursor.split("-", 1)
        if expected != revision:
            raise CatalogChanged()
        offset = int(position)
        if offset < 0 or offset > MAX_COMPONENTS:
            raise ValueError("invalid catalog position")
    items = []
    total = 0
    emitted_bytes = 0
    closed = False
    for record in records:
        data = record["data"]
        if data.startswith(b"\x1f\x8b"):
            with gzip.GzipFile(fileobj=io.BytesIO(data)) as compressed:
                data = compressed.read(MAX_XML + 1)
        if len(data) > MAX_XML:
            raise ValueError("catalog metadata exceeds its bound")
        metadata = AppStream.Metadata.new()
        metadata.set_format_style(AppStream.FormatStyle.CATALOG)
        metadata.set_locale("ALL")
        kind = AppStream.FormatKind.XML if record["path"].name.endswith((".xml", ".xml.gz")) else AppStream.FormatKind.YAML
        if not metadata.parse_data(data.decode("utf-8"), len(data), kind):
            raise ValueError("invalid catalog metadata")
        for component in metadata.get_components().as_array():
            if total >= MAX_COMPONENTS:
                raise ValueError("catalog component count exceeds its bound")
            if total >= offset and not closed:
                exported = AppStream.Metadata.new()
                exported.set_format_style(AppStream.FormatStyle.CATALOG)
                exported.set_locale("ALL")
                origin = metadata.get_origin()
                if origin:
                    exported.set_origin(origin)
                exported.add_component(component)
                xml = exported.components_to_catalog(AppStream.FormatKind.XML)
                item = {"source": record["source"], "xml": xml}
                size = len(json.dumps(item, ensure_ascii=True, separators=(",", ":")).encode("ascii"))
                if size > PAGE_BYTES:
                    raise ValueError("catalog component exceeds the page bound")
                if len(items) == PAGE_ITEMS or emitted_bytes + size > PAGE_BYTES:
                    closed = True
                else:
                    emitted_bytes += size
                    items.append(item)
            total += 1
    if offset > total:
        raise ValueError("catalog position is outside its snapshot")
    return {
        "revision": revision, "configured": bool(records), "items": items,
        "count": len(items), "total": total,
        "next": f"{revision}-{offset + len(items)}" if offset + len(items) < total else None,
    }


def main(cursor):
    revision, records = snapshot()
    result = page(revision, records, cursor)
    if _sources() != [record["path"] for record in records]:
        raise CatalogChanged()
    for record in records:
        if _stamp(record["path"].stat(follow_symlinks=False)) != record["stamp"]:
            raise CatalogChanged()
    return result


if __name__ == "__main__":
    try:
        print(json.dumps(main(sys.argv[1]), ensure_ascii=True, separators=(",", ":")))
    except CatalogChanged:
        raise SystemExit(2)
    except Exception:
        print("The standard system package catalog could not be read", file=sys.stderr)
        raise SystemExit(1)
