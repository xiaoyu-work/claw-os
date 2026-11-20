"""Package the verified Qualcomm firmware index and Windows Update resolver."""

import json
from pathlib import PurePosixPath
import posixpath
import re
import shutil
import ssl
import tarfile
from urllib.parse import urlsplit

import yaml


INDEX = "qualcomm-firmware-index"
RESOLVER = "qualcomm-firmware-wudrivers"
UPDATE_CA = "qualcomm-firmware-update-ca"


def firmware_name(name):
    if not re.fullmatch(r"qcom(?:/[\w.-]+)+", name, re.ASCII) or ".." in name.split("/"):
        raise ValueError(f"invalid firmware path: {name}")
    return name


def member_path(member):
    for path in member.split("!/"):
        if (not path.isprintable() or path.startswith("/") or "\\" in path
                or any(part in ("", ".", "..") for part in path.split("/"))):
            raise ValueError(f"invalid OEM archive member: {member}")
    return member


def checksum(value):
    if not re.fullmatch(r"[a-f0-9]{64}", value):
        raise ValueError("invalid firmware SHA-256")
    return value


def catalog(document):
    archives = {}
    for spec in document["packages"]:
        name, url, size = (spec[key] for key in ("id", "url", "size"))
        parsed = urlsplit(url)
        if parsed.scheme == "wu":
            if not re.fullmatch(r"wu://[a-f0-9-]{36}/[0-9]+/[a-f0-9]{40}", url):
                raise ValueError(f"{name}: invalid Windows Update source")
            kind = "cab"
        elif parsed.scheme == "https" and parsed.hostname and not parsed.username and not parsed.password:
            kind = PurePosixPath(parsed.path).suffix.lstrip(".").lower()
            if kind == "exe" and spec["maker"] == "Lenovo":
                kind = "inno"
        else:
            raise ValueError(f"{name}: unsupported OEM source")
        if kind not in {"cab", "exe", "inno", "msi", "zip"} or not 0 < size < 2**31:
            raise ValueError(f"{name}: invalid OEM archive type or size")
        archives[name] = [url, size, checksum(spec["sha256"]), kind]

    files = {}
    aliases = {}
    for group in document["folders"]:
        folder = group["path"]
        for item in group.get("files", ()):
            name = firmware_name(f"{folder}/{item['name']}")
            members = {}
            for entry in item["from"]:
                archive = entry["package"]
                if archive not in archives:
                    raise ValueError(f"{name}: unknown OEM archive {archive}")
                members[archive] = member_path(entry["member"])
            if not members or not 0 < item["size"] < 2**31:
                raise ValueError(f"{name}: missing OEM source or size")
            files[name] = [item["size"], checksum(item["sha256"]), members]
        for link in group.get("links", ()):
            name = firmware_name(f"{folder}/{link['name']}")
            aliases[name] = firmware_name(posixpath.normpath(f"{folder}/{link['target']}"))
    for name in aliases:
        target = name
        seen = set()
        while target not in files:
            if target not in aliases or target in seen:
                raise ValueError(f"{name}: broken OEM firmware alias")
            seen.add(target)
            target = aliases[target]
        files[name] = files[target]
    return {"archives": archives, "files": files}


def package_firmware_index(source, provenance, work, out, package):
    stage = work / "qualcomm-firmware-index"
    index = stage / "usr/share/cos/qualcomm/firmware.json"
    index.parent.mkdir(parents=True)
    index.write_text(json.dumps(catalog(yaml.safe_load(source[INDEX].read_text())),
                                separators=(",", ":"), sort_keys=True) + "\n", encoding="utf-8")
    certificate = index.with_name("microsoft-update.pem")
    certificate.write_text(ssl.DER_cert_to_PEM_cert(source[UPDATE_CA].read_bytes()), encoding="ascii")
    name = "claw-os-qualcomm-firmware-index"
    license_file = stage / "usr/share/doc" / name / "wudrivers.LICENSE"
    license_file.parent.mkdir(parents=True)
    resolver = stage / "usr/lib/python3/dist-packages/wudrivers"
    resolver.mkdir(parents=True)
    with tarfile.open(source[RESOLVER]) as archive:
        for member in archive:
            parts = PurePosixPath(member.name).parts
            if len(parts) == 2 and parts[-1] == "LICENSE":
                dest = license_file
            elif len(parts) == 4 and parts[1:3] == ("src", "wudrivers") and parts[-1] in {"__init__.py", "wu.py"}:
                dest = resolver / parts[-1]
            else:
                continue
            if not member.isfile():
                raise ValueError(f"{member.name}: expected a regular resolver file")
            with archive.extractfile(member) as stream, dest.open("xb") as target:
                shutil.copyfileobj(stream, target)
    transport = resolver / "wu.py"
    unverified = "ssl._create_unverified_context()"
    text = transport.read_text()
    if text.count(unverified) != 1 or not (resolver / "__init__.py").is_file() or not license_file.is_file():
        raise ValueError("unexpected Windows Update resolver inputs")
    transport.write_text(text.replace(unverified,
                                      'ssl.create_default_context(cafile="/usr/share/cos/qualcomm/microsoft-update.pem")'),
                         encoding="utf-8")
    package(stage, out, name, "1-1", "all", "python3 (>= 3.11), curl, msitools, 7zip, innoextract",
            "Verified sources for model-specific Qualcomm OEM firmware",
            {key: provenance[key] for key in (INDEX, RESOLVER, UPDATE_CA)}, None)
