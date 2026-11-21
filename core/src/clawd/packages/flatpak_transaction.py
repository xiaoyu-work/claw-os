"""Resolve and apply only the retained, signed Flatpak operation graph."""

import base64
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys
from urllib.parse import urlsplit

import gi

gi.require_version("Flatpak", "1.0")
from gi.repository import Flatpak, GLib

CATALOG_CHUNK = 3 * 1024 * 1024
MAX_CATALOG_FILE = 128 * 1024 * 1024
MAX_CATALOG = 256 * 1024 * 1024


class NotDispatched(ValueError):
    pass


def remote_url(remote):
    url = remote.get_url() or ""
    parsed = urlsplit(url)
    if (remote.get_disabled() or not remote.get_gpg_verify()
        or parsed.scheme not in ("https", "file")
        or (parsed.scheme == "https" and not parsed.hostname)
        or parsed.username is not None or parsed.password is not None
        or parsed.query or parsed.fragment or len(url) > 512):
        raise ValueError("Flatpak source is disabled or lacks signature verification")
    return url


def trusted(installation, name):
    return remote_url(installation.get_remote_by_name(name, None))


def trusted_remotes(installation):
    remotes = {}
    for remote in installation.list_remotes(None):
        try:
            remote_url(remote)
        except ValueError:
            continue
        remotes[remote.get_name()] = remote
        if len(remotes) > 16:
            raise ValueError("Too many Flatpak sources")
    return remotes


def catalog(installation, cursor):
    def stamp(info):
        return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns)

    root = Path(installation.get_path().get_path(), "appstream").resolve()
    records = []
    size = 0
    for name, remote in sorted(trusted_remotes(installation).items()):
        if remote.get_noenumerate():
            continue
        directory = Path(remote.get_appstream_dir().get_path()).resolve()
        if not directory.is_relative_to(root):
            raise ValueError("Flatpak AppStream directory is outside its installation")
        for filename in ("appstream.xml.gz", "appstream.xml"):
            path = directory / filename
            try:
                info = path.stat(follow_symlinks=False)
            except FileNotFoundError:
                continue
            size += info.st_size
            if not stat.S_ISREG(info.st_mode) or not 0 < info.st_size <= MAX_CATALOG_FILE or size > MAX_CATALOG:
                raise ValueError("Flatpak catalog metadata exceeds its bound")
            records.append((name, remote_url(remote), path, stamp(info)))
            break
    revision = hashlib.sha256(json.dumps(records, default=str).encode()).hexdigest()
    position = 0
    if cursor:
        expected, index = cursor.split("-", 1)
        if expected != revision:
            raise ValueError("Flatpak AppStream changed; refresh the catalog")
        position = int(index)
    start = 0
    pages = [(record[3][2] + CATALOG_CHUNK - 1) // CATALOG_CHUNK for record in records]
    for (name, _, path, identity), count in zip(records, pages):
        if position < start + count:
            offset = (position - start) * CATALOG_CHUNK
            fd = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK)
            with os.fdopen(fd, "rb") as handle:
                if stamp(os.fstat(fd)) != identity:
                    raise ValueError("Flatpak AppStream changed; refresh the catalog")
                handle.seek(offset)
                data = handle.read(min(CATALOG_CHUNK, identity[2] - offset))
                if stamp(os.fstat(fd)) != identity:
                    raise ValueError("Flatpak AppStream changed; refresh the catalog")
            return {
                "revision": revision, "source": name, "offset": offset, "size": identity[2],
                "data": base64.b64encode(data).decode("ascii"),
                "next": f"{revision}-{position + 1}" if position + 1 < sum(pages) else None,
            }
        start += count
    if cursor:
        raise ValueError("The Flatpak catalog page is outside its snapshot")
    return {"revision": revision, "source": None, "offset": 0, "size": 0, "data": "", "next": None}


def inventory(installation, updates):
    remotes = trusted_remotes(installation)
    arch = Flatpak.get_default_arch()
    installed = [ref for ref in installation.list_installed_refs_by_kind(Flatpak.RefKind.APP)
                 if ref.get_arch() == arch and ref.get_origin() in remotes]
    if len(installed) > 2048:
        raise ValueError("Installed Flatpak inventory exceeds its bound")
    candidates = {}
    if updates:
        for origin in sorted({ref.get_origin() for ref in installed}):
            candidates[origin] = {
                ref.format_ref(): ref.get_commit()
                for ref in installation.list_remote_refs_sync_full(origin, Flatpak.QueryFlags(0))
                if ref.get_kind() == Flatpak.RefKind.APP and ref.get_arch() == arch
                and not ref.get_eol() and not ref.get_eol_rebase()
            }
    items = []
    for ref in installed:
        reference, origin, commit = ref.format_ref(), ref.get_origin(), ref.get_commit()
        available = candidates.get(origin, {}).get(reference)
        if updates and (not available or available == commit):
            continue
        items.append({
            "id": ref.get_name(), "ref": reference, "remote": origin,
            "installed": ref.get_appdata_version() or commit[:12],
            "candidate": available[:12] if updates else None,
        })
    return {"apps": items}


def operation_graph(transaction, installation):
    operations = []
    for operation in transaction.get_operations():
        if operation.get_is_skipped():
            continue
        kind = operation.get_operation_type()
        if kind not in (Flatpak.TransactionOperationType.INSTALL, Flatpak.TransactionOperationType.UPDATE):
            raise ValueError("Flatpak resolution attempted an unrequested removal, rebase or bundle")
        remote = operation.get_remote()
        url = trusted(installation, remote)
        commit = operation.get_commit()
        reference = operation.get_ref()
        if not re.fullmatch(r"[0-9a-f]{64}", commit or "") or not re.fullmatch(
            r"(?:app|runtime)/[A-Za-z0-9._-]+/[A-Za-z0-9_-]+/[A-Za-z0-9._-]+", reference or ""
        ):
            raise ValueError("Flatpak resolution omitted a complete signed deployment identity")
        operations.append({
            "remote": remote, "remote_url": url, "reference": reference, "commit": commit,
            "operation": "install" if kind == Flatpak.TransactionOperationType.INSTALL else "update",
        })
        if len(operations) > 64:
            raise ValueError("Flatpak dependency graph exceeds its limit")
    return sorted(operations, key=lambda item: (item["reference"], item["remote"], item["commit"]))


def installed_ref(installation, reference):
    kind, name, arch, branch = reference.split("/")
    kind = Flatpak.RefKind.APP if kind == "app" else Flatpak.RefKind.RUNTIME
    return installation.get_installed_ref(kind, name, arch, branch, None)


def verify(installation, plan):
    for operation in plan["operations"]:
        installed = installed_ref(installation, operation["reference"])
        if installed.get_origin() != operation["remote"] or installed.get_commit() != operation["commit"]:
            raise ValueError("Flatpak deployment does not match the retained source and commit")
    return {"status": "applied", "confirmed": True, "deployments": plan["operations"]}


def resolve(installation, target, install, prior=None, dispatched=None):
    remote, reference = target["remote"], target["ref"]
    _, _, arch, _ = reference.split("/")
    if arch != Flatpak.get_default_arch():
        raise ValueError("Flatpak release does not match the current machine")
    remote_url = trusted(installation, remote)
    if prior is not None and prior["remote_url"] != remote_url:
        raise ValueError("The Flatpak source changed; no deployment authorized")
    before = None
    if not install:
        installed = installed_ref(installation, reference)
        if installed.get_origin() != remote:
            raise ValueError("The installed Flatpak belongs to another source")
        before = installed.get_commit()
    if prior is not None and prior["previous_commit"] != before:
        raise ValueError("The installed Flatpak changed since review")

    transaction = Flatpak.Transaction.new_for_installation(installation, None)
    transaction.set_no_interaction(True)
    transaction.set_disable_related(True)
    transaction.set_include_unused_uninstall_ops(False)
    transaction.set_disable_prune(True)
    transaction.connect("add-new-remote", lambda *_: False)
    if install:
        transaction.add_install(remote, reference)
    else:
        commit = None if prior is None else next(
            item["commit"] for item in prior["operations"] if item["reference"] == reference
        )
        transaction.add_update(reference, None, commit)

    result = []
    failure = []

    def ready(_):
        try:
            operations = operation_graph(transaction, installation)
            selected = [item for item in operations if item["reference"] == reference]
            if len(selected) != 1 or selected[0]["remote"] != remote or selected[0]["operation"] != (
                "install" if install else "update"
            ) or any(item["reference"].startswith("app/") and item["reference"] != reference for item in operations):
                raise ValueError("Flatpak did not resolve exactly the requested application and source")
            plan = {"install": install, "remote_url": remote_url,
                    "previous_commit": before, "operations": operations}
            if prior is not None and plan != prior:
                raise ValueError("The signed Flatpak ref, commit or dependency plan changed; inspect and retry")
            result.append(plan)
            if dispatched is None:
                return False
            dispatched.append(True)
            return True
        except (ValueError, GLib.Error) as error:
            failure.append(str(error))
            return False

    transaction.connect("ready", ready)
    try:
        complete = transaction.run(None)
    except GLib.Error:
        if failure:
            raise ValueError(failure[0]) from None
        if prior is None and result:
            return result[0]
        raise
    if not complete or not result:
        raise ValueError("Flatpak did not resolve and confirm an operation")
    return verify(installation, result[0])


def main():
    request = json.loads(sys.stdin.buffer.readline(128 * 1024))
    mode = request["mode"]
    if mode in ("refresh", "catalog", "installed", "updates"):
        installation = installation_for(request["scope"])
        if mode == "catalog":
            return catalog(installation, request.get("cursor"))
        if mode != "refresh":
            return inventory(installation, mode == "updates")
        remotes = [name for name, remote in trusted_remotes(installation).items()
                   if not remote.get_noenumerate()]
        failures = []
        for name in remotes:
            try:
                installation.update_appstream_sync(name)
            except GLib.Error:
                failures.append(name)
        if failures:
            raise ValueError("Could not refresh Flatpak AppStream for " + ", ".join(failures))
        return {"refreshed": len(remotes)}
    target = request["target"]
    installation = installation_for(target["scope"])
    if mode == "verify":
        return verify(installation, request["plan"])
    if mode == "plan":
        return resolve(installation, target, request["install"])
    if mode == "apply":
        dispatched = []
        try:
            return resolve(installation, target, request["plan"]["install"], request["plan"], dispatched)
        except (ValueError, KeyError, GLib.Error) as error:
            if not dispatched:
                raise NotDispatched(str(error)) from None
            raise
    raise ValueError("Unknown Flatpak operation")


def installation_for(scope):
    return Flatpak.Installation.new_user(None) if scope == "user" else Flatpak.Installation.new_system(None)


try:
    print(json.dumps(main()))
except (ValueError, KeyError, OSError, GLib.Error) as error:
    print(str(error).replace("\n", " ")[:512], file=sys.stderr)
    raise SystemExit(2 if isinstance(error, NotDispatched) else 1)
