#!/usr/bin/env python3
"""Generate a signed `claw.release-security/v1` manifest for one package.

The manifest is the artifact that lets an offline machine tell a *newer*
Claw OS release from an *older* one that is just as validly signed. It
binds, for exactly one package build:

* the release-security epoch and ABI generation;
* the exact Debian version and architecture;
* the SHA-256 of every security component the package installs;
* the protocol epochs those binaries speak;
* the lowest mutually compatible version of every sibling package;
* the repository suite/component it is published into;
* an explicit expiry.

Output is canonical JSON — sorted keys, no insignificant whitespace,
integers only, one trailing newline — because a detached signature
covers bytes, not meaning. `cos::update::canonical` re-encodes the file
before verifying it, so a second encoding of the same document is
refused rather than accepted.

Reproducibility: `--issued-at` (or `SOURCE_DATE_EPOCH`) fixes both
timestamps, so two builds of the same commit produce byte-identical
manifests.

Usage:
  make-manifest.py --package claw-os-agent --version 0.2.0+git1.gabc \\
      --arch amd64 --stage-dir build/deb-staging/claw-os-agent \\
      --output build/deb-staging/claw-os-agent/usr/lib/cos/release-security/claw-os-agent/manifest.json
"""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import os
import pathlib
import subprocess
import sys

DEFAULT_POLICY = pathlib.Path(__file__).resolve().parent / "policy.json"
FORMAT = "claw.release-security/v1"


def canonical_bytes(document: object) -> bytes:
    """Exactly the encoding `cos::update::canonical` accepts."""
    text = json.dumps(
        document,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
        allow_nan=False,
    )
    return text.encode("utf-8") + b"\n"


def sha256_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def resolve_issued_at(explicit: str | None) -> datetime.datetime:
    if explicit:
        parsed = datetime.datetime.fromisoformat(explicit.replace("Z", "+00:00"))
        return parsed.astimezone(datetime.timezone.utc)
    source_date_epoch = os.environ.get("SOURCE_DATE_EPOCH")
    if source_date_epoch:
        return datetime.datetime.fromtimestamp(
            int(source_date_epoch), tz=datetime.timezone.utc
        )
    return datetime.datetime.now(tz=datetime.timezone.utc)


def rfc3339(moment: datetime.datetime) -> str:
    return moment.astimezone(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def build_manifest(args: argparse.Namespace, policy: dict) -> dict:
    stage = pathlib.Path(args.stage_dir).resolve()
    components = []
    for entry in policy["components"]:
        if entry["package"] != args.package:
            continue
        installed = entry["path"].lstrip("/")
        staged = stage / installed
        if not staged.is_file():
            raise SystemExit(
                f"error: {args.package} declares component {entry['path']} "
                f"but it is not staged at {staged}"
            )
        components.append(
            {
                "name": entry["name"],
                "path": entry["path"],
                "sha256": sha256_file(staged),
            }
        )
    if not components:
        raise SystemExit(f"error: no components are declared for {args.package}")

    issued_at = resolve_issued_at(args.issued_at)
    valid_until = issued_at + datetime.timedelta(
        days=int(policy["manifest_validity_days"])
    )
    return {
        "abi": int(policy["abi"]),
        "components": components,
        "format": FORMAT,
        "issued_at": rfc3339(issued_at),
        "minimum_compatible": policy["minimum_compatible"],
        "protocols": policy["protocols"],
        "release": {
            "architecture": args.arch,
            "component": args.component,
            "package": args.package,
            "suite": args.suite,
            "version": args.version,
        },
        "revoked_digests": policy.get("revoked_digests", []),
        "revoked_keys": policy.get("revoked_keys", []),
        "security_epoch": int(policy["security_epoch"]),
        "valid_until": rfc3339(valid_until),
    }


def sign(output: pathlib.Path, key_id: str) -> None:
    """Detach-sign with the release/APT publisher key.

    No key material is generated or embedded here: the signature is only
    produced when a secret key is already available to `gpg`, which in
    practice means the publication workflow. An unsigned local build
    still produces a manifest, and the installed system decides what to
    do about a missing signature.

    The passphrase, when there is one, is written to `gpg`'s stdin under
    `--passphrase-fd 0`. It never appears in `argv`, where any local
    process could read it out of `/proc`.
    """
    signature = output.with_suffix(output.suffix + ".asc")
    passphrase = os.environ.get("GPG_PASSPHRASE", "")
    command = [
        "gpg",
        "--batch",
        "--yes",
        "--pinentry-mode",
        "loopback",
        "--default-key",
        key_id,
    ]
    if passphrase:
        command += ["--passphrase-fd", "0"]
    command += ["--detach-sign", "--armor", "-o", str(signature), str(output)]
    subprocess.run(
        command,
        check=True,
        input=(passphrase + "\n").encode("utf-8") if passphrase else b"",
    )


def require_security_epoch_in_version(version: str, security_epoch: int) -> None:
    """A security epoch APT cannot see is not enforceable.

    APT selects candidates by Debian version order, so an emergency
    release with a lower upstream version would never be chosen unless
    the security epoch is *also* the Debian epoch.
    """
    declared = version.split(":", 1)[0] if ":" in version else "0"
    if declared != str(security_epoch):
        raise SystemExit(
            f"error: version {version} has Debian epoch {declared} but the "
            f"release-security epoch is {security_epoch}. Build with "
            f"{security_epoch}:<upstream> so APT orders this release above "
            f"everything published before it."
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--arch", required=True)
    parser.add_argument("--stage-dir", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--suite", default=os.environ.get("SUITE", "trixie"))
    parser.add_argument("--component", default="main")
    parser.add_argument("--policy", default=str(DEFAULT_POLICY))
    parser.add_argument("--issued-at")
    parser.add_argument("--sign-key", default="")
    args = parser.parse_args()

    policy = json.loads(pathlib.Path(args.policy).read_text(encoding="utf-8"))
    if policy.get("format") != "claw.release-security-policy/v1":
        raise SystemExit("error: unexpected release-security policy format")
    if args.package not in policy["packages"]:
        raise SystemExit(f"error: {args.package} is not a gated Claw OS package")

    manifest = build_manifest(args, policy)
    require_security_epoch_in_version(args.version, int(policy["security_epoch"]))
    output = pathlib.Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_bytes(canonical_bytes(manifest))

    if args.sign_key:
        sign(output, args.sign_key)

    digest = hashlib.sha256(output.read_bytes()).hexdigest()
    print(digest)
    return 0


if __name__ == "__main__":
    sys.exit(main())
