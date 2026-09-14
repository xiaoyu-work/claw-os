#!/usr/bin/env python3
"""Build the pinned, unmodified upstream TUI outside the Claw Rust workspace."""

import argparse
from contextlib import ExitStack
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

from artifacts import BuildError
from artifacts import SUPPORTED_TARGETS
from artifacts import artifact_receipt
from artifacts import modifications_bytes
from artifacts import read_patch_inputs
from artifacts import read_staged_pin
from artifacts import relative_path
from artifacts import require_same_compilation
from artifacts import sha256
from artifacts import validate_elf
from artifacts import validate_patch_entries
from artifacts import validate_staging_recipe
from artifacts import verify_artifact
from staging import select_strip_tool
from staging import strip_staged_binary


MODULE = Path(__file__).resolve().parent
REPOSITORY = MODULE.parent
BUILD = REPOSITORY / "build" / "agent-tui"


def load_pin(path=MODULE / "source.json"):
    pin = json.loads(path.read_text(encoding="utf-8"))
    if pin["schema_version"] != 1:
        raise BuildError("Unsupported source manifest version")
    if pin["upstream"]["repository"] != "https://github.com/openai/codex.git":
        raise BuildError("The source pin must identify the public OpenAI Codex repository")
    if not re.fullmatch(r"[0-9a-f]{40}", pin["upstream"]["revision"]):
        raise BuildError("Pin a full lowercase 40-character Git commit, not a branch or tag")
    validate_patch_entries(pin)
    validate_staging_recipe(pin.get("staging"))
    required_inputs = {"LICENSE", "NOTICE", "codex-rs/Cargo.lock", "codex-rs/rust-toolchain.toml"}
    if not required_inputs.issubset(pin["checksums"]):
        raise BuildError("Pin the lockfile, upstream toolchain file, LICENSE and NOTICE checksums")
    for name, checksum in pin["checksums"].items():
        relative_path(name)
        if not re.fullmatch(r"[0-9a-f]{64}", checksum):
            raise BuildError(f"Invalid SHA-256 for {name}")
    cargo = pin["cargo"]
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", cargo["toolchain"]):
        raise BuildError("Pin an exact Rust toolchain version, not a moving channel")
    if not cargo["default_features"] or cargo["features"]:
        raise BuildError("Build the upstream default feature set without removing TUI functionality")
    if (cargo["package"], cargo["binary"], cargo["profile"]) != ("codex-cli", "codex", "release"):
        raise BuildError("This builder stages the real release-profile codex-cli binary")
    if not {"LICENSE", "NOTICE"}.issubset(pin["runtime"]["license_files"]):
        raise BuildError("Runtime assets must preserve upstream LICENSE and NOTICE")
    for name in (
        cargo["workspace"],
        pin["runtime"]["binary"],
        pin["runtime"]["assets_directory"],
        *pin["runtime"]["license_files"],
    ):
        relative_path(name)
    checks = pin.get("checks", {})
    if not isinstance(checks, dict) or set(checks) - {"rust_stdlib_tests"}:
        raise BuildError("Unsupported pinned build checks")
    if not isinstance(checks.get("rust_stdlib_tests", []), list):
        raise BuildError("Rust patch checks must be an ordered list")
    for name in checks.get("rust_stdlib_tests", []):
        path = relative_path(name)
        if path.parts[0] != cargo["workspace"] or path.suffix != ".rs":
            raise BuildError("Rust patch checks must be source files in the upstream workspace")
    return pin


def run(args, *, cwd=None, capture=False, env=None):
    result = subprocess.run(
        [str(arg) for arg in args],
        cwd=cwd,
        env=env,
        check=True,
        stdout=subprocess.PIPE if capture else None,
        text=capture,
    )
    return result.stdout.strip() if capture else None


def git(source, *args, capture=True):
    return run(["git", "--no-optional-locks", "-C", source, *args], capture=capture)


def verify_checkout(source, revision):
    if git(source, "rev-parse", "HEAD") != revision:
        raise BuildError(f"The development source cache is not at pinned revision {revision}")
    if git(source, "status", "--porcelain=v1", "--untracked-files=all"):
        raise BuildError("The development source cache is dirty; use a clean exact-pin checkout")


def acquire_repository(pin, local_source, cache):
    revision = pin["upstream"]["revision"]
    if local_source is not None:
        source = local_source.resolve(strict=True)
        print("Verifying the exact revision and cleanliness of the development source cache", flush=True)
        verify_checkout(source, revision)
        return source
    source = cache / "upstream.git"
    if not source.exists():
        run(["git", "init", "--bare", "--quiet", source])
    has_revision = subprocess.run(
        ["git", "-C", str(source), "cat-file", "-e", f"{revision}^{{commit}}"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    if has_revision.returncode != 0:
        run(
            [
                "git", "-c", "credential.helper=", "-c", "core.askPass=",
                "-C", source, "fetch", "--quiet", "--no-tags", "--depth=1",
                pin["upstream"]["repository"], revision,
            ],
            env={**os.environ, "GIT_TERMINAL_PROMPT": "0"},
        )
    if git(source, "rev-parse", f"{revision}^{{commit}}") != revision:
        raise BuildError("Fetched source does not resolve to the exact pinned commit")
    return source


def verify_inputs(source, pin):
    for name, expected in pin["checksums"].items():
        if sha256(source / relative_path(name)) != expected:
            raise BuildError(f"Pinned input checksum mismatch: {name}")


def extract_source(source_repository, revision, destination):
    if destination.is_symlink():
        raise BuildError(f"Refusing to replace a symlinked source cache: {destination}")
    if destination.exists():
        shutil.rmtree(destination)
    destination.mkdir(parents=True)
    # Archive Git objects, not the checkout: autocrlf and local ignored files
    # must not change the sources compiled on Linux.
    archive = subprocess.Popen(
        ["git", "-C", str(source_repository), "archive", "--format=tar", revision],
        stdout=subprocess.PIPE,
    )
    try:
        with subprocess.Popen(
            [
                "tar", "--extract", "--file=-", "--directory", str(destination),
                "--no-same-owner", "--no-same-permissions",
            ],
            stdin=archive.stdout,
        ) as extractor:
            archive.stdout.close()
            extraction_returncode = extractor.wait()
    finally:
        archive.stdout.close()
        returncode = archive.wait()
    if returncode or extraction_returncode:
        raise BuildError(
            f"Source extraction failed (git archive: {returncode}, tar: {extraction_returncode})"
        )


def apply_source_patches(source, patch_inputs):
    environment = os.environ.copy()
    for name in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"):
        environment.pop(name, None)
    environment["GIT_CEILING_DIRECTORIES"] = str(source.parent.resolve())
    for entry, data in patch_inputs:
        print(f"Applying maintained patch: {entry['path']}", flush=True)
        for options in (["--check"], []):
            subprocess.run(
                ["git", "apply", "--whitespace=error-all", *options, "-"],
                cwd=source, env=environment, input=data, check=True,
            )


def sync_source_tree(prepared, source):
    if source.is_symlink():
        raise BuildError(f"Refusing a symlinked source cache: {source}")
    source.mkdir(parents=True, exist_ok=True)
    # Checksums repair modified cache files; omitting --times preserves unchanged
    # source timestamps so metadata-only changes do not invalidate Cargo builds.
    run(["rsync", "--recursive", "--links", "--perms", "--checksum", "--delete",
         "--", str(prepared) + os.sep, str(source) + os.sep])


def stage_artifacts(source, compiled_binary, pin, build, build_info, patch_inputs=None, strip_tool=None):
    validate_elf(compiled_binary, build_info["target"])
    if strip_tool is None:
        strip_tool = select_strip_tool(build_info["target"])
    build_info = dict(build_info)
    build_info["staging_input_sha256"] = sha256(compiled_binary)
    if patch_inputs is None:
        patch_inputs = read_patch_inputs(pin, MODULE)
    pending = build / "stage"
    if pending.exists():
        shutil.rmtree(pending)
    binary = pending / relative_path(pin["runtime"]["binary"])
    binary.parent.mkdir(parents=True)
    shutil.copy2(compiled_binary, binary)
    binary.chmod(0o755)
    build_info["strip_tool"] = strip_staged_binary(
        binary, build_info["target"], pin["staging"], strip_tool,
    )
    assets = pending / relative_path(pin["runtime"]["assets_directory"])
    assets.mkdir(parents=True)
    for name in pin["runtime"]["license_files"]:
        target = assets / relative_path(name)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source / relative_path(name), target)
    for entry, data in patch_inputs:
        target = assets / relative_path(entry["path"])
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    if patch_inputs:
        (assets / "MODIFICATIONS").write_bytes(modifications_bytes(pin))
    (assets / "source.json").write_text(
        json.dumps(pin, indent=2) + "\n", encoding="utf-8"
    )
    receipt = artifact_receipt(pin, build_info, binary, assets)
    (assets / "build-info.json").write_text(
        json.dumps(receipt, indent=2) + "\n", encoding="utf-8"
    )
    verify_artifact(pending, pin, build_info["target"])
    final_assets = build / relative_path(pin["runtime"]["assets_directory"])
    final_assets.parent.mkdir(parents=True, exist_ok=True)
    if final_assets.exists():
        shutil.rmtree(final_assets)
    assets.replace(final_assets)
    final_binary = build / relative_path(pin["runtime"]["binary"])
    final_binary.parent.mkdir(parents=True, exist_ok=True)
    binary.replace(final_binary)
    shutil.rmtree(pending)
    return final_binary


def run_patch_checks(source, pin, environment):
    checks = pin.get("checks", {}).get("rust_stdlib_tests", [])
    if not checks:
        return
    output = BUILD / "patch-checks"
    output.mkdir(exist_ok=True)
    for index, name in enumerate(checks):
        executable = output / f"stdlib-check-{index}"
        run(
            [
                "rustup", "run", pin["cargo"]["toolchain"], "rustc",
                "--edition", "2024", "--test", source / relative_path(name),
                "-o", executable,
            ],
            cwd=source, env=environment,
        )
        run([executable, "--test-threads=1"], cwd=source, env=environment)


def build(args):
    pin = load_pin()
    patch_inputs = read_patch_inputs(pin, MODULE)
    BUILD.mkdir(parents=True, exist_ok=True)
    with (BUILD / "build.lock").open("w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise BuildError("Another agent-tui build is using this cache") from error
        source_repository = acquire_repository(pin, args.source, BUILD)
        revision = pin["upstream"]["revision"]
        source = BUILD / "sources" / revision
        prepared = BUILD / "source-stage" / revision
        if args.source is not None and args.source.resolve().is_relative_to(source):
            raise BuildError("The development source checkout cannot be inside the extraction directory")
        print(f"Extracting OpenAI Codex {revision}", flush=True)
        extract_source(source_repository, revision, prepared)
        verify_inputs(prepared, pin)
        apply_source_patches(prepared, patch_inputs)
        verify_inputs(prepared, pin)
        sync_source_tree(prepared, source)
        shutil.rmtree(prepared)
        if args.prepare_only:
            print(f"Verified pinned sources: {source}")
            return

        cargo = pin["cargo"]
        toolchain = ["rustup", "run", cargo["toolchain"]]
        compiler = run([*toolchain, "rustc", "--version", "--verbose"], capture=True)
        cargo_version = run([*toolchain, "cargo", "--version"], capture=True)
        host = next(line.removeprefix("host: ") for line in compiler.splitlines() if line.startswith("host: "))
        if host not in SUPPORTED_TARGETS:
            raise BuildError(f"Build on a supported GNU/Linux host, not {host}")
        target = args.target or host
        strip_tool = select_strip_tool(target)
        source_date_epoch = git(source_repository, "show", "-s", "--format=%ct", revision)
        environment = os.environ.copy()
        environment["SOURCE_DATE_EPOCH"] = source_date_epoch
        environment["STABLE_GIT_COMMIT"] = revision
        environment["GIT_CEILING_DIRECTORIES"] = str(BUILD)
        for name, directory in (
            ("CARGO_HOME", "cargo-home"),
            ("CARGO_TARGET_DIR", "cargo-target"),
            ("XDG_CACHE_HOME", "cache"),
            ("TMPDIR", "scratch"),
            ("TMP", "scratch"),
            ("TEMP", "scratch"),
        ):
            path = BUILD / directory
            path.mkdir(exist_ok=True)
            environment[name] = str(path)
        run_patch_checks(source, pin, environment)
        command = [
            *toolchain, "cargo", "build", "--quiet", "--locked", "--release",
            "--package", cargo["package"], "--bin", cargo["binary"], "--target", target,
        ]
        if args.jobs is not None:
            command.extend(["--jobs", str(args.jobs)])
        print(f"Compiling upstream {cargo['package']} ({target}, Rust {cargo['toolchain']})", flush=True)
        run(command, cwd=source / relative_path(cargo["workspace"]), env=environment)
        verify_inputs(source, pin)
        compiled = BUILD / "cargo-target" / target / cargo["profile"] / cargo["binary"]
        artifact = stage_artifacts(
            source, compiled, pin, BUILD,
            {
                "source_date_epoch": int(source_date_epoch),
                "source_acquisition": "verified-local-git" if args.source is not None else "public-git",
                "rustc": compiler,
                "cargo": cargo_version,
                "host": host,
                "target": target,
                "profile": cargo["profile"],
                "default_features": cargo["default_features"],
                "features": cargo["features"],
                "patches": pin["patches"],
            },
            patch_inputs,
            strip_tool,
        )
        print(f"Staged private TUI: {artifact}", flush=True)


def verify_existing(args):
    root = (args.artifact_root or BUILD).resolve(strict=True)
    with ExitStack() as stack:
        lock_path = root / "build.lock"
        if lock_path.is_symlink() or (lock_path.exists() and not lock_path.is_file()):
            raise BuildError("Artifact cache lock must be a regular file")
        if lock_path.exists():
            lock = stack.enter_context(lock_path.open("rb"))
            try:
                fcntl.flock(lock, fcntl.LOCK_SH | fcntl.LOCK_NB)
            except BlockingIOError as error:
                raise BuildError("A TUI build is in progress; do not package a changing artifact") from error
        pin = load_pin()
        read_patch_inputs(pin, MODULE)
        result = verify_artifact(root, pin, args.target)
    print(json.dumps(result, sort_keys=True))


def restage_existing(args):
    pin = load_pin()
    root = (args.artifact_root or BUILD).resolve(strict=True)
    patch_inputs = read_patch_inputs(pin, MODULE)
    strip_tool = select_strip_tool(args.target)
    BUILD.mkdir(parents=True, exist_ok=True)
    with ExitStack() as stack:
        output_lock = stack.enter_context((BUILD / "build.lock").open("a+b"))
        try:
            fcntl.flock(output_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise BuildError("Another TUI producer is using the output cache") from error
        input_lock_path = root / "build.lock"
        if root != BUILD and input_lock_path.exists():
            if input_lock_path.is_symlink() or not input_lock_path.is_file():
                raise BuildError("Artifact cache lock must be a regular file")
            input_lock = stack.enter_context(input_lock_path.open("rb"))
            try:
                fcntl.flock(input_lock, fcntl.LOCK_SH | fcntl.LOCK_NB)
            except BlockingIOError as error:
                raise BuildError("The input artifact is being rebuilt") from error
        previous_pin = read_staged_pin(root, pin["runtime"]["assets_directory"])
        require_same_compilation(previous_pin, pin)
        previous = verify_artifact(root, previous_pin, args.target)
        build_info = {
            key: value for key, value in previous.items()
            if key not in {"binary", "assets_directory", "provenance"}
        }
        artifact = stage_artifacts(
            Path(previous["assets_directory"]), Path(previous["binary"]),
            pin, BUILD, build_info, patch_inputs, strip_tool,
        )
        print(f"Restaged verified TUI without invoking Cargo: {artifact}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, help="optional clean Git checkout at the exact pin (no network source fetch)")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--prepare-only", action="store_true", help="acquire and verify sources without compiling")
    mode.add_argument("--verify-artifact", action="store_true", help="validate the existing bundle; print JSON on success, never build or fetch")
    mode.add_argument("--restage-verified", action="store_true", help="apply only the current staging recipe to a verified same-source bundle; never invoke Cargo")
    parser.add_argument("--target", choices=sorted(SUPPORTED_TARGETS), help="explicit Rust target triple (build default: compiler host)")
    parser.add_argument("--artifact-root", type=Path, help="input bundle root for verification/restaging (default: build/agent-tui)")
    parser.add_argument("--jobs", type=int, help="Cargo build concurrency")
    args = parser.parse_args()
    if sys.platform != "linux" or sys.version_info < (3, 12):
        parser.error("Use Linux with Python 3.12 or later; run terminal/build.sh in WSL on Windows")
    if args.jobs is not None and args.jobs < 1:
        parser.error("--jobs must be positive")
    artifact_mode = args.verify_artifact or args.restage_verified
    if artifact_mode and args.target is None:
        parser.error("--verify-artifact/--restage-verified requires an explicit --target")
    if args.artifact_root is not None and not artifact_mode:
        parser.error("--artifact-root is only supported with artifact verification/restaging")
    if artifact_mode and (args.source is not None or args.jobs is not None):
        parser.error("--source and --jobs are only supported when preparing/building sources")
    try:
        if args.verify_artifact:
            verify_existing(args)
        elif args.restage_verified:
            restage_existing(args)
        else:
            build(args)
    except (BuildError, OSError, subprocess.CalledProcessError) as error:
        action = "artifact verification" if args.verify_artifact else "build"
        print(f"Codex TUI {action} failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
