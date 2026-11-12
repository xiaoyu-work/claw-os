"""CI-only namespace/NSS/repository setup for one native package GUI case."""

import grp
import importlib.util
import os
from pathlib import Path
import pwd
import sys

SOURCE = Path(__file__).resolve().parents[2] / "app_store/repository.py"
SPEC = importlib.util.spec_from_file_location("private_native_package_apt", SOURCE)
REPOSITORY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REPOSITORY)


def unoccupied_identities():
    for uid in (62050, 62051, *range(61000, 61064)):
        try:
            pwd.getpwuid(uid)
        except KeyError:
            pass
        else:
            raise RuntimeError(f"refusing an existing host/reserved UID {uid}")
    for name in ("claw-display-test", "claw-display-other", *(f"cos-ext-{i:02}" for i in range(64))):
        try:
            pwd.getpwnam(name)
        except KeyError:
            pass
        else:
            raise RuntimeError(f"refusing an existing host/reserved account {name}")
    for gid in (60999, 62050, 62051):
        try:
            grp.getgrgid(gid)
        except KeyError:
            pass
        else:
            raise RuntimeError(f"refusing an existing host/reserved GID {gid}")
    for name in ("cos-extension", "claw-display-test", "claw-display-other"):
        try:
            grp.getgrnam(name)
        except KeyError:
            pass
        else:
            raise RuntimeError(f"refusing an existing host/reserved group {name}")


def accounts(private):
    REPOSITORY.require_private_mounts()
    unoccupied_identities()
    private = Path(private).resolve(strict=True)
    if not private.is_dir() or private == Path("/"):
        raise RuntimeError("invalid private fixture input")
    passwd = Path("/etc/passwd").read_text()
    groups = Path("/etc/group").read_text()
    for name, prefix in (("passwd", passwd), ("group", groups)):
        generated = (private / name).read_text().splitlines()
        (private / name).write_text(prefix + "".join(line + "\n" for line in generated if not line.startswith("root:")))
    with (private / "group").open("a") as output:
        output.write("cos-extension:x:60999:\n")
    with (private / "passwd").open("a") as passwd, (private / "shadow").open("a") as shadow:
        for index in range(64):
            name, uid = f"cos-ext-{index:02}", 61000 + index
            passwd.write(f"{name}:x:{uid}:60999:Claw OS extension slot {index}:/nonexistent:/usr/sbin/nologin\n")
            shadow.write(f"{name}:!:20000:0:99999:7:::\n")
    reservation = Path("/var/lib/cos/extension-identities.reserved")
    reservation.parent.mkdir(parents=True, exist_ok=True)
    reservation.write_text("version=1\ngroup=cos-extension:60999\n" + "".join(
        f"identity=cos-ext-{index:02}:{61000 + index}:60999:/nonexistent:/usr/sbin/nologin\n"
        for index in range(64)
    ))
    reservation.chmod(0o600)


def isolate():
    unoccupied_identities()
    REPOSITORY.isolate()
    # Only the native fixture needs to stage compiled compositor/PAM binaries.
    # Existing APT cases keep their original read-only system-runtime mirrors.
    for index, target in enumerate(("/usr/bin", "/usr/lib/x86_64-linux-gnu/security")):
        target = Path(target)
        if not target.is_dir():
            raise RuntimeError(f"required native fixture directory is absent: {target}")
        upper = REPOSITORY.BASE / f"native-overlay-{index}"
        work = REPOSITORY.BASE / f"native-overlay-work-{index}"
        upper.mkdir()
        work.mkdir()
        REPOSITORY.mount("overlay", target, "overlay", 0,
                         f"lowerdir={target},upperdir={upper},workdir={work}")


def main():
    REPOSITORY.require_namespace()
    if sys.argv[1:] == ["isolate"]:
        isolate()
    elif len(sys.argv) == 3 and sys.argv[1] == "accounts":
        accounts(sys.argv[2])
    else:
        raise SystemExit("usage: package_setup.py isolate | accounts <private-fixture-directory>")


if __name__ == "__main__":
    main()
