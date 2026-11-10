"""Consume the official signed App APT channel; never stage App source into OS packages."""

import argparse
from datetime import datetime, timedelta, timezone
from email.utils import parsedate_to_datetime
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import pwd
import re
import shutil
import subprocess
import tarfile


ROOT = Path(__file__).resolve().parents[1]
PACKAGE_ROOT_ENV = "CLAW_APP_PACKAGE_ROOT"
APP_NAME = re.compile(r"claw-(?:app-|apps-|cap-)[a-z0-9][a-z0-9+.-]*\Z")
APP_INTERFACE = re.compile(r"claw-app-(?:support|python-[a-z0-9][a-z0-9-]*)-v1\Z")


def run(arguments, **kwargs):
    private = {
        "APT_CONFIG", "GPG_PASSPHRASE", "CLAW_OS_APT_SIGNING_PRIVATE_KEY",
        "CLAW_APPS_APT_SIGNING_PRIVATE_KEY", "CLAW_APPS_APT_SIGNING_PASSPHRASE",
    }
    environment = {key: value for key, value in os.environ.items() if key not in private}
    return subprocess.run(
        [str(argument) for argument in arguments], env=environment,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True, **kwargs,
    )


def settings():
    value = json.loads((ROOT / "packaging/app-packages.json").read_text())
    if value["format"] != "claw.app-package-consumer/v1" or (
        value["url"] != "https://xiaoyu-work.github.io/clawos-app/"
        or value["suite"] != "trixie" or value["component"] != "main"
        or value["architectures"] != ["amd64", "arm64"]
        or not re.fullmatch(r"[0-9A-F]{40}", value["signing_fingerprint"])
    ):
        raise ValueError("Invalid official App package consumer contract")
    return value


def platform():
    value = json.loads((ROOT / "packaging/app-platform.json").read_text())
    if value["schema"] != "claw.app-platform/v1" or value["runtime_abi"] != 1 or (
        not re.fullmatch(r"0|[1-9][0-9]*", str(value["runtime_abi"]))
        or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", value["version"])
    ):
        raise ValueError("Invalid App runtime implementation version/ABI")
    return value


def fields(text):
    result = {}
    current = None
    for line in text.splitlines():
        if line.startswith((" ", "\t")) and current:
            result[current] += "\n" + line
        else:
            name, separator, content = line.partition(":")
            if not separator or name in result or not re.fullmatch(r"[A-Za-z][A-Za-z0-9-]*", name):
                raise ValueError("Invalid Debian metadata")
            current = name
            result[name] = content.strip()
    return result


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def work_root():
    root = Path(os.environ.get(PACKAGE_ROOT_ENV, ROOT / "build/app-packages")).resolve()
    if not root.is_relative_to((ROOT / "build").resolve()):
        raise ValueError("App package inputs must be under the OS build directory")
    return root


def keyring(directory, configuration):
    home = directory / "gnupg"
    home.mkdir(mode=0o700, exist_ok=True)
    key = ROOT / configuration["signing_key"]
    output = run(["gpg", "--no-options", "--batch", "--homedir", home,
                  "--with-colons", "--show-keys", key]).stdout.decode()
    fingerprints = []
    primary = False
    for line in output.splitlines():
        parts = line.split(":")
        if parts[0] == "pub":
            primary = True
        elif parts[0] == "fpr" and primary:
            fingerprints.append(parts[9])
            primary = False
    if fingerprints != [configuration["signing_fingerprint"]]:
        raise ValueError("App archive key fingerprint mismatch")
    destination = directory / "archive-keyring.gpg"
    destination.write_bytes(run(["gpg", "--no-options", "--batch", "--homedir", home,
                                 "--output", "-", "--dearmor", key]).stdout)
    return destination


def apt_options(directory, architecture):
    return [
        "-o", f"Dir::Etc::sourcelist={directory / 'sources.list'}",
        "-o", "Dir::Etc::sourceparts=-", "-o", "Dir::Etc::parts=-",
        "-o", "Dir::Etc::main=-", "-o", f"Dir::State={directory / 'state'}",
        "-o", f"Dir::State::status={directory / 'status'}",
        "-o", f"Dir::Cache={directory / 'cache'}",
        "-o", f"Dir::Log={directory / 'log'}",
        "-o", f"APT::Architecture={architecture}",
        "-o", f"APT::Architectures::={architecture}",
        "-o", f"APT::Sandbox::User={pwd.getpwuid(os.geteuid()).pw_name}",
        "-o", "Acquire::Languages=none", "-o", "Acquire::Check-Valid-Until=true",
        "-o", "Acquire::AllowInsecureRepositories=false",
        "-o", "Acquire::AllowDowngradeToInsecureRepositories=false",
        "-o", "APT::Get::AllowUnauthenticated=false",
    ]


def authenticated_records(directory, architecture, configuration):
    ring = keyring(directory, configuration)
    releases = list((directory / "state/lists").glob("*_InRelease"))
    if len(releases) != 1:
        raise ValueError("Missing unique authenticated App APT release")
    result = run(["gpgv", "--status-fd", "2", "--keyring", ring, "--output", "-", releases[0]])
    signatures = [line.split() for line in result.stderr.decode().splitlines()
                  if line.startswith("[GNUPG:] VALIDSIG ")]
    if len(signatures) != 1 or signatures[0][-1] != configuration["signing_fingerprint"]:
        raise ValueError("Wrong App APT release signer")
    release = fields(result.stdout.decode())
    now = datetime.now(timezone.utc)
    issued = parsedate_to_datetime(release["Date"])
    expires = parsedate_to_datetime(release["Valid-Until"])
    if issued.tzinfo is None or expires.tzinfo is None or (
        issued > now + timedelta(minutes=5) or not issued < expires
        or expires <= now or expires - issued > timedelta(days=14)
    ):
        raise ValueError("App APT release is stale or has unbounded validity")
    if release.get("Suite") != configuration["suite"] or (
        architecture not in release.get("Architectures", "").split()
        or configuration["component"] not in release.get("Components", "").split()
    ):
        raise ValueError("App APT release does not match the selected suite/architecture")
    indexes = {}
    for line in release["SHA256"].strip().splitlines():
        digest, size, name = line.split()
        if name in indexes or not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise ValueError("Invalid signed App APT index inventory")
        indexes[name] = (digest, int(size))
    name = f"{configuration['component']}/binary-{architecture}/Packages"
    candidates = list((directory / "state/lists").glob(f"*_binary-{architecture}_Packages*"))
    if len(candidates) != 1:
        raise ValueError("Missing authenticated App APT package index")
    data = run(["/usr/lib/apt/apt-helper", "cat-file", candidates[0]]).stdout
    if (hashlib.sha256(data).hexdigest(), len(data)) != indexes.get(name):
        raise ValueError("App package index differs from its signed release")
    records = [fields(stanza) for stanza in data.decode().strip().split("\n\n") if stanza]
    return [record for record in records if record["Architecture"] in ("all", architecture)]


def compare(left, operation, right):
    result = subprocess.run(
        ["dpkg", "--compare-versions", left, operation, right],
        stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
    )
    if result.returncode not in (0, 1):
        raise ValueError("Invalid App Debian version")
    return result.returncode == 0


def require_os_version(version):
    policy = json.loads((ROOT / "packaging/release-security/policy.json").read_text())
    minimum = f"{policy['security_epoch']}:{settings()['runtime_minimum']}"
    if not compare(version, "ge", minimum):
        raise ValueError(f"OS App ownership cutover requires Agent/Desktop >= {minimum}")


def dependencies(record):
    for value in record.get("Depends", "").replace("\n", " ").split(","):
        value = value.strip()
        if not value:
            continue
        match = re.fullmatch(r"([a-z0-9][a-z0-9+.-]*)(?: \((<<|<=|=|>=|>>) ([^\s()]+)\))?", value)
        if match:
            if APP_NAME.fullmatch(match[1]):
                yield match[1], match[2], match[3]
        elif re.search(r"\bclaw-(?:app-|apps-|cap-)", value):
            raise ValueError("App package dependencies require an unambiguous exact owner")


def interfaces(record):
    for value in record.get("Provides", "").split(","):
        name = value.strip()
        if not name:
            continue
        if not APP_INTERFACE.fullmatch(name):
            raise ValueError("App package cannot provide an OS/package/authority ABI")
        yield name


def selection(records, variant, configuration):
    available = {}
    providers = {}
    identities = {}
    for record in records:
        name = record["Package"]
        if not APP_NAME.fullmatch(name):
            raise ValueError("Non-App package in the App APT channel")
        identity = (name, record["Version"], record.get("Architecture", "all"))
        if identity in identities:
            if identities[identity] != record:
                raise ValueError("Conflicting immutable App APT package tuple")
            continue
        identities[identity] = record
        available.setdefault(name, []).append(record)
        for interface in interfaces(record):
            providers.setdefault(interface, set()).add(name)
    pending = [(configuration["sets"][variant], None, None)]
    selected = {}
    constraints = {}
    steps = 0
    while pending:
        steps += 1
        if steps > 4096:
            raise ValueError("App package dependency closure exceeds its bound")
        name, operation, version = pending.pop()
        candidates = [name] if name in available else providers.get(name, [])
        if len(candidates) != 1:
            raise ValueError(f"Missing or ambiguous official App package: {name}")
        owner = next(iter(candidates))
        if operation and name != owner:
            raise ValueError(f"Unavailable matching App dependency: {name}")
        required = constraints.setdefault(owner, set())
        if operation:
            required.add((operation, version))
        matching = [record for record in available[owner]
                    if all(compare(record["Version"], op, bound) for op, bound in required)]
        if not matching:
            raise ValueError(f"Unavailable matching App dependency: {name}")
        record = matching[0]
        for candidate in matching[1:]:
            if compare(candidate["Version"], "gt", record["Version"]):
                record = candidate
        if selected.get(owner) == record:
            continue
        selected[owner] = record
        pending.extend(dependencies(record))
    reachable = set()
    pending = [configuration["sets"][variant]]
    while pending:
        name = pending.pop()
        owner = name if name in available else next(iter(providers[name]))
        if owner in reachable:
            continue
        reachable.add(owner)
        pending.extend(name for name, _, _ in dependencies(selected[owner]))
    selected = {name: record for name, record in selected.items() if name in reachable}
    minimum = configuration.get("runtime_minimum", platform()["version"])
    for record in selected.values():
        if record["Package"] in configuration["sets"].values():
            continue
        runtime = re.findall(r"(?:^|,\s*)claw-os-app-runtime-v1 \(>= ([^\s()]+)\)(?:,|$)",
                             record.get("Depends", "").replace("\n", " "))
        if len(runtime) != 1 or not compare(runtime[0], "ge", minimum) or not compare(
            platform()["version"], "ge", runtime[0],
        ):
            raise ValueError(f"Official App package lacks the coordinated runtime floor: {record['Package']}")
    expected = set(json.loads((ROOT / "packaging/apps.lock.json").read_text())["apps"])
    if variant == "agent":
        expected -= set((ROOT / "packaging/deb/claw-os-desktop/apps.list").read_text().split())
    owners = {}
    for record in selected.values():
        for app in record.get("X-Claw-App-Ids", "").replace(",", " ").split():
            if app in owners:
                raise ValueError(f"Multiple App package owners: {app}")
            owners[app] = record["Package"]
    if set(owners) != expected:
        raise ValueError("Official App packages do not cover the retained identity selection")
    return sorted(selected.values(), key=lambda record: record["Package"])


def package_files(path):
    inventory = set()
    data = run(["dpkg-deb", "--fsys-tarfile", path]).stdout
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        for member in archive:
            name = member.name.removeprefix("./").rstrip("/")
            if not name:
                continue
            relative = PurePosixPath(name)
            if relative.is_absolute() or ".." in relative.parts:
                raise ValueError("App package inventory escapes its root")
            if not member.isdir():
                if name in inventory:
                    raise ValueError("Duplicate App package payload path")
                inventory.add(name)
    return inventory


def verify_packages(directory, selected):
    owners = {}
    for record in selected:
        path = directory / "archives" / (record["Package"] + ".deb")
        if not path.is_file() or path.stat().st_size != int(record["Size"]) or sha256(path) != record["SHA256"]:
            raise ValueError(f"Missing or changed official App artifact: {record['Package']}")
        control = fields(run(["dpkg-deb", "-f", path]).stdout.decode().strip())
        list(interfaces(control))
        for field in ("Package", "Version", "Architecture", "Depends", "Provides", "X-Claw-App-Ids"):
            if control.get(field, "").replace("\n", " ") != record.get(field, "").replace("\n", " "):
                raise ValueError(f"App artifact differs from its signed package metadata: {field}")
        for name in package_files(path):
            if name in owners:
                raise ValueError(f"Multiple dpkg owners for {name}")
            if any(name == root or name.startswith(root + "/") for root in (
                "usr/lib/cos/python/claw_os_sdk", "usr/lib/cos/python/cos_runtime",
            )):
                raise ValueError("App package contains OS-owned SDK/runtime")
            owners[name] = record["Package"]
    return owners


def prepare(variant, architecture, directory):
    configuration = settings()
    if architecture not in configuration["architectures"]:
        raise ValueError("Unsupported App package architecture")
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    ring = keyring(directory, configuration)
    (directory / "status").touch()
    (directory / "archives").mkdir()
    for relative in ("state/lists/partial", "cache/archives/partial", "log"):
        (directory / relative).mkdir(parents=True)
    (directory / "sources.list").write_text(
        f"deb [arch={architecture} signed-by={ring} check-valid-until=yes "
        "allow-insecure=no allow-weak=no allow-downgrade-to-insecure=no by-hash=force] "
        f"{configuration['url']} {configuration['suite']} {configuration['component']}\n"
    )
    run(["apt-get", *apt_options(directory, architecture), "update", "--error-on=any", "-qq"])
    selected = selection(authenticated_records(directory, architecture, configuration), variant, configuration)
    for record in selected:
        relative = PurePosixPath(record["Filename"])
        if not record["Filename"].startswith("pool/") or ".." in relative.parts or relative.is_absolute():
            raise ValueError("Invalid official App package URL")
        run(["curl", "--proto", "=https", "--proto-redir", "=https", "--fail", "--silent", "--show-error",
             "--location", "--max-filesize", record["Size"], configuration["url"] + relative.as_posix(),
             "--output", directory / "archives" / (record["Package"] + ".deb")])
    verify_packages(directory, selected)
    (directory / "selection.json").write_text(json.dumps(selected, sort_keys=True, indent=2) + "\n")
    return directory


def check(variant, architecture, directory, stage=None):
    configuration = settings()
    if not (directory / "selection.json").is_file():
        raise ValueError("Official App artifacts are required; run scripts/app_packages.py prepare first")
    selected = selection(authenticated_records(directory, architecture, configuration), variant, configuration)
    if selected != json.loads((directory / "selection.json").read_text()):
        raise ValueError("App package inputs differ from the authenticated release selection")
    owners = verify_packages(directory, selected)
    if stage is not None:
        check_stage(stage, owners)
    return selected


def check_stage(stage, owners):
    for path in stage.rglob("*"):
        if path.is_dir() and not path.is_symlink():
            continue
        name = path.relative_to(stage).as_posix()
        if name.startswith("usr/lib/cos/apps/") or name in owners or (
            name.startswith("usr/lib/cos/python/")
            and name.split("/")[4] not in ("claw_os_sdk", "cos_runtime")
        ):
            raise ValueError(f"OS package still owns an App/support/library payload: {name}")


def native_boundary():
    linked = [
        path.relative_to(ROOT).as_posix()
        for source in ("desktop/applets", "desktop/panel")
        for path in (ROOT / source).glob("*/Cargo.toml")
        if "build/native-apps/" in path.read_text()
    ]
    if linked:
        raise ValueError(
            "Production desktop still links App-owned UI/private libraries: "
            + ", ".join(sorted(linked))
            + ". Use explicit source fixtures only; generic shell/interface cutover remains a release blocker."
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("prepare", "check", "native-boundary", "runtime-version", "check-version"))
    parser.add_argument("--variant", choices=("agent", "desktop"))
    parser.add_argument("--architecture", choices=("amd64", "arm64"))
    parser.add_argument("--stage", type=Path)
    parser.add_argument("--version")
    options = parser.parse_args()
    if options.command == "runtime-version":
        print(platform()["version"])
        return
    if options.command == "native-boundary":
        native_boundary()
        return
    if options.command == "check-version":
        if not options.version:
            parser.error("--version is required")
        require_os_version(options.version)
        return
    if not options.variant or not options.architecture:
        parser.error("--variant and --architecture are required")
    directory = work_root() / f"{options.variant}-{options.architecture}"
    if options.command == "prepare":
        prepare(options.variant, options.architecture, directory)
    else:
        check(options.variant, options.architecture, directory, options.stage)
    print(directory)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"error: official App package consumption failed: {error}")
