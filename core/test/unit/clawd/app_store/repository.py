"""Private Linux APT fixture plumbing; never a catalog or runtime trust override."""

import ctypes
import datetime
import email.utils
import fcntl
import functools
import http.server
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys


BASE = Path("/run/cos-app-apt-fixture")
BUNDLE = "claw-app-acceptance-bundle"
SUPPORT = "claw-app-acceptance-support"
SIDE_EFFECT = "claw-package-acceptance-side-effect"
VERSION = "1.0.0"
PORT = 38081
PRIVATE_MOUNTS = (
    "/run", "/var/lib", "/var/cache", "/var/log", "/etc", "/etc/apt", "/etc/dpkg",
    "/root", "/usr",
)


def require_namespace():
    if os.getuid() != 0 or os.geteuid() != 0:
        raise RuntimeError("the private fixture requires actual Root")
    for namespace in ("mnt", "pid", "net"):
        parent = os.environ.get(f"COS_APP_APT_PARENT_{namespace.upper()}")
        if not parent or os.readlink(f"/proc/self/ns/{namespace}") == parent:
            raise RuntimeError(f"refusing a host {namespace} namespace")


def mount(source, target, filesystem=None, flags=0, options=None):
    libc = ctypes.CDLL(None, use_errno=True)
    libc.mount.argtypes = (
        ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p,
        ctypes.c_ulong, ctypes.c_char_p,
    )
    arguments = (
        os.fsencode(source) if source is not None else None,
        os.fsencode(target),
        os.fsencode(filesystem) if filesystem is not None else None,
        flags,
        os.fsencode(options) if options is not None else None,
    )
    if libc.mount(*arguments):
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error), str(target))


def mirror(source, destination, excluded=()):
    destination.mkdir(parents=True, exist_ok=True)
    for entry in source.iterdir():
        if entry.name in excluded:
            continue
        target = destination / entry.name
        if entry.is_symlink():
            target.symlink_to(os.readlink(entry))
            continue
        if entry.is_dir():
            target.mkdir()
        elif entry.is_file():
            target.touch()
        else:
            raise RuntimeError(f"unsupported system runtime entry: {entry}")
        mount(entry, target, flags=4096)
        mount(None, target, flags=4096 | 32 | 1)


def isolate():
    require_namespace()
    mount(None, "/", flags=(1 << 18) | 16384)
    for target in PRIVATE_MOUNTS[:-1]:
        if target in ("/etc", "/etc/apt", "/etc/dpkg"):
            continue
        if not Path(target).is_dir():
            raise RuntimeError(f"required system mountpoint is absent: {target}")
        mount("tmpfs", target, "tmpfs", 2 | 4, "mode=0755,size=1073741824")
    BASE.mkdir(mode=0o755)
    identities = {
        "passwd": Path("/etc/passwd").read_bytes(),
        "group": Path("/etc/group").read_bytes(),
        "shadow": b"root:!:20000:0:99999:7:::\n",
        "subuid": b"",
        "subgid": b"",
    }
    etc = BASE / "etc"
    mirror(
        Path("/etc"), etc,
        ("cos", "apt", "dpkg", *identities),
    )
    for name, body in identities.items():
        target = etc / name
        target.write_bytes(body)
        target.chmod(0o600 if name == "shadow" else 0o644)
    (etc / "cos/trust/publishers.d").mkdir(parents=True)
    for name in ("apt", "dpkg"):
        (etc / name).mkdir()
    mount(etc, "/etc", flags=4096 | 16384)
    for target in ("/etc/apt", "/etc/dpkg"):
        mount("tmpfs", target, "tmpfs", 2 | 4, "mode=0755,size=1073741824")
    shadow = BASE / "usr"
    mirror(Path("/usr"), shadow, ("lib", "share"))
    mirror(Path("/usr/lib"), shadow / "lib", ("cos",))
    mirror(Path("/usr/share"), shadow / "share", ("doc",))
    for directory in ("lib/cos/apps", "lib/cos/python", "lib/cos/bin", "share/doc"):
        (shadow / directory).mkdir(parents=True, exist_ok=True)
    mount(shadow, "/usr", flags=4096 | 16384)
    mount("tmpfs", "/usr/local/bin", "tmpfs", 2 | 4, "mode=0755,size=1073741824")
    for directory in (
        "scratch", "payloads/bundle", "payloads/support", "state", "runtime",
    ):
        (BASE / directory).mkdir(parents=True, exist_ok=True)
    (BASE / "scratch").chmod(0o700)
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as control:
        interface = struct.pack("256s", b"lo")
        flags = fcntl.ioctl(control, 0x8913, interface)
        flags = struct.unpack_from("H", flags, 16)[0]
        fcntl.ioctl(control, 0x8914, struct.pack("16sH238x", b"lo", flags | 1))


def require_private_mounts():
    require_namespace()
    mounts = {}
    for line in Path("/proc/self/mountinfo").read_text().splitlines():
        fields = line.split()
        mounts[fields[4]] = fields[fields.index("-") + 1]
    if any(mounts.get(target) != "tmpfs" for target in (*PRIVATE_MOUNTS, "/usr/local/bin")):
        raise RuntimeError("all package, runtime and broker paths must be privately mounted")


def run(*arguments, cwd=BASE, input=None):
    result = subprocess.run(
        arguments, cwd=cwd, input=input, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, timeout=45,
        env={
            "PATH": "/usr/sbin:/usr/bin:/sbin:/bin",
            "HOME": str(BASE),
            "TMPDIR": str(BASE / "scratch"),
            "LC_ALL": "C.UTF-8",
            "DEBIAN_FRONTEND": "noninteractive",
        },
    )
    if result.returncode:
        raise RuntimeError(
            f"{arguments[0]} exited {result.returncode}:\n"
            + (result.stdout + result.stderr).decode(errors="replace")[-16384:]
        )
    return result.stdout


def repository(*, native_package=False):
    require_private_mounts()
    for directory in (
        "/var/lib/dpkg/info", "/var/lib/dpkg/updates", "/var/lib/dpkg/triggers",
        "/var/lib/apt/lists/partial", "/var/cache/apt/archives/partial",
        "/etc/apt/apt.conf.d", "/etc/apt/auth.conf.d", "/etc/apt/sources.list.d",
        "/etc/apt/keyrings", "/etc/apt/trusted.gpg.d", "/etc/dpkg/dpkg.cfg.d",
    ):
        Path(directory).mkdir(parents=True, exist_ok=True)
    for name in ("status", "available", "lock", "lock-frontend"):
        Path("/var/lib/dpkg", name).write_bytes(b"")
    Path("/etc/dpkg/dpkg.cfg").write_bytes(b"")
    Path("/etc/apt/apt.conf.d/00private-fixture").write_text(
        'APT::Sandbox::User "root";\n'
        'APT::Get::AllowUnauthenticated "false";\n'
        'Acquire::AllowInsecureRepositories "false";\n'
        'Acquire::AllowDowngradeToInsecureRepositories "false";\n'
        'Acquire::Check-Valid-Until "true";\n'
        'Acquire::Languages "none";\n'
        'DPkg::Use-Pty "false";\n'
        'Dir::State::status "/var/lib/dpkg/status";\n'
        'Dir::Cache::pkgcache "";\n'
        'Dir::Cache::srcpkgcache "";\n'
    )
    repo = BASE / "repo"
    pool = repo / "pool"
    pool.mkdir(parents=True)
    effect = BASE / "payloads/effects"
    marker = effect / "usr/lib/cos/fixture-side-effect"
    marker.parent.mkdir(parents=True)
    marker.write_text("A normal, successful private package install\n")
    for payload, package, dependency in (
        ("bundle", BUNDLE, f"Depends: {SUPPORT} (= {VERSION})\n"),
        ("support", SUPPORT, ""),
        ("effects", SIDE_EFFECT, ""),
    ):
        root = BASE / "payloads" / payload
        control = root / "DEBIAN"
        control.mkdir(mode=0o755)
        (control / "control").write_text(
            f"Package: {package}\nVersion: {VERSION}\nArchitecture: all\n"
            "Section: misc\nPriority: optional\n"
            "Maintainer: Private Acceptance <noreply@fixture.invalid>\n"
            f"{dependency}Description: Isolated signed App delivery acceptance fixture\n"
        )
        if package == SIDE_EFFECT and not native_package:
            postinst = control / "postinst"
            postinst.write_text(
                "#!/bin/sh\nset -eu\n"
                "mkdir -p /usr/lib/cos/apps/apt-unreviewed\n"
                "cp /usr/lib/cos/apps/apt-fixture-alpha/.provenance.json "
                "/usr/lib/cos/apps/apt-unreviewed/.provenance.json\n"
                "printf '%s\\n' "
                "'{\"schema_version\":2,\"id\":\"apt-unreviewed\",\"version\":\"1.0.0\","
                "\"runtime\":\"python\",\"name\":{\"en\":\"Unreviewed private App\"},"
                "\"summary\":{\"en\":\"No App delivery confirmation\"},"
                "\"operations\":{\"probe\":{\"label\":{\"en\":\"Probe\"},"
                "\"needs\":[],\"args\":[]}}}' "
                "> /usr/lib/cos/apps/apt-unreviewed/app.json\n"
                "printf '%s\\n' 'def run(command, args):' "
                "'    return {\"fixture\": True}' "
                "> /usr/lib/cos/apps/apt-unreviewed/main.py\n"
            )
            postinst.chmod(0o755)
        run(
            "/usr/bin/dpkg-deb", "--root-owner-group", "-Znone", "--build",
            str(root), str(pool / f"{package}_{VERSION}_all.deb"),
        )
    architecture = run("/usr/bin/dpkg", "--print-architecture").decode().strip()
    suite = repo / "dists/stable"
    index = suite / f"main/binary-{architecture}"
    index.mkdir(parents=True)
    (index / "Packages").write_bytes(
        run("/usr/bin/apt-ftparchive", "packages", "pool", cwd=repo)
    )
    expiry = email.utils.format_datetime(
        datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(hours=1),
        usegmt=True,
    )
    release = run(
        "/usr/bin/apt-ftparchive",
        "-o", "APT::FTPArchive::Release::Origin=Private App Acceptance",
        "-o", "APT::FTPArchive::Release::Label=Private App Acceptance",
        "-o", "APT::FTPArchive::Release::Suite=stable",
        "-o", "APT::FTPArchive::Release::Codename=stable",
        "-o", f"APT::FTPArchive::Release::Architectures={architecture}",
        "-o", "APT::FTPArchive::Release::Components=main",
        "-o", f"APT::FTPArchive::Release::Valid-Until={expiry}",
        "release", "dists/stable", cwd=repo,
    )
    (suite / "Release").write_bytes(release)
    home = BASE / "gnupg"
    home.mkdir(mode=0o700)
    gpg = ("/usr/bin/gpg", "--no-options", "--homedir", str(home), "--batch")
    try:
        run(
            *gpg, "--pinentry-mode", "loopback", "--passphrase", "",
            "--quick-generate-key", "Private App APT <apt@fixture.invalid>",
            "rsa2048", "sign", "0",
        )
        Path("/etc/apt/keyrings/fixture.gpg").write_bytes(run(*gpg, "--export"))
        run(*gpg, "--yes", "--clearsign", "--output", str(suite / "InRelease"),
            str(suite / "Release"))
        run(*gpg, "--yes", "--detach-sign", "--output", str(suite / "Release.gpg"),
            str(suite / "Release"))
    finally:
        run("/usr/bin/gpgconf", "--homedir", str(home), "--kill", "gpg-agent")
    Path("/etc/apt/sources.list.d/fixture.sources").write_text(
        "Types: deb\n"
        f"URIs: http://127.0.0.1:{PORT}\nSuites: stable\nComponents: main\n"
        f"Architectures: {architecture}\nSigned-By: /etc/apt/keyrings/fixture.gpg\n"
    )


def serve():
    require_private_mounts()
    handler = functools.partial(
        http.server.SimpleHTTPRequestHandler, directory=str(BASE / "repo"),
    )
    with http.server.ThreadingHTTPServer(("127.0.0.1", PORT), handler) as server:
        server.serve_forever()


def update():
    require_private_mounts()
    run("/usr/bin/apt-get", "update", "--error-on=any")


if __name__ == "__main__":
    if sys.argv[1:] == ["isolate"]:
        isolate()
    elif sys.argv[1:] == ["repository"]:
        repository()
    elif sys.argv[1:] == ["repository-native-package"]:
        # Preserve the original SIDE_EFFECT postinst/quarantine acceptance.
        repository(native_package=True)
    elif sys.argv[1:] == ["serve"]:
        serve()
    elif sys.argv[1:] == ["update"]:
        update()
    else:
        raise SystemExit("usage: repository.py {isolate|repository|repository-native-package|serve|update}")
