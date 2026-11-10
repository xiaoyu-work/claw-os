"""Read authenticated APT index ownership for exact downloaded package bytes."""

import json
import re
import sys
from urllib.parse import urlsplit


def app_repository(uri):
    parsed = urlsplit(uri)
    return (
        parsed.scheme == "https"
        and parsed.hostname == "xiaoyu-work.github.io"
        and parsed.port in (None, 443)
        and parsed.path.rstrip("/") == "/clawos-app"
    )


def classify(packages, cache, records, sources):
    result = []
    for package in packages:
        if not isinstance(package, dict) or set(package) != {
            "package", "version", "architecture", "sha256",
        } or not all(isinstance(value, str) for value in package.values()):
            raise ValueError("invalid bounded APT package identity")
        if not re.fullmatch(r"[a-z0-9][a-z0-9+.-]{1,254}", package["package"]) or (
            not 0 < len(package["version"]) <= 255
            or package["architecture"] not in ("all", "amd64", "arm64")
            or not re.fullmatch(r"[0-9a-f]{64}", package["sha256"])
        ):
            raise ValueError("invalid APT package identity")
        key = package["package"]
        if package["architecture"] != "all":
            key += ":" + package["architecture"]
        trusted_app = trusted_os = False
        for version in cache[key].version_list:
            if version.ver_str != package["version"] or version.arch != package["architecture"]:
                continue
            for entry in version.file_list:
                index = sources.find_index(entry[0])
                if index is None or not index.is_trusted:
                    continue
                if not records.lookup(entry) or records.sha256_hash != package["sha256"]:
                    continue
                if app_repository(index.archive_uri("")):
                    trusted_app = True
                else:
                    trusted_os = True
        if not trusted_app and not trusted_os:
            raise ValueError("downloaded bytes have no matching authenticated APT index")
        result.append({**package, "app_source": trusted_app, "os_source": trusted_os})
    return result


def main():
    import apt.progress.base
    import apt_pkg

    data = sys.stdin.buffer.read(1024 * 1024 + 1)
    if len(data) > 1024 * 1024:
        raise ValueError("APT metadata request exceeds its byte limit")
    packages = json.loads(data)
    if not isinstance(packages, list) or not 0 < len(packages) <= 512:
        raise ValueError("APT metadata request exceeds its package limit")
    apt_pkg.init()
    cache = apt_pkg.Cache(apt.progress.base.OpProgress())
    records = apt_pkg.PackageRecords(cache)
    sources = apt_pkg.SourceList()
    sources.read_main_list()
    json.dump(classify(packages, cache, records, sources), sys.stdout, separators=(",", ":"))
    sys.stdout.write("\n")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, TypeError, ImportError, SystemError) as error:
        print(f"app-apt-metadata: {type(error).__name__}", file=sys.stderr)
        raise SystemExit(1)
