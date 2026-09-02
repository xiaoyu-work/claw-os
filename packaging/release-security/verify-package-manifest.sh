#!/usr/bin/env bash
# packaging/release-security/verify-package-manifest.sh — prove that a
# built `.deb` and the release-security manifest inside it describe the
# same thing.
#
# The manifest is what an installed system trusts offline, so a package
# whose manifest disagrees with its own control fields or its own files
# is worse than one with no manifest at all. This is the single check
# every package build and every publication workflow runs; there is no
# per-workflow copy to drift.
#
# Verified:
#
#   * the payload carries `/usr/lib/cos/release-security/<package>/`
#     and nothing belonging to another package;
#   * the manifest is canonical JSON and names this package, version
#     and architecture;
#   * its security epoch and ABI match the `XB-Claw-Os-*` control
#     fields, and the Debian *epoch* of the version equals the security
#     epoch, so APT's own ordering prefers a higher security epoch;
#   * every component the manifest lists is present in the payload with
#     exactly the recorded SHA-256;
#   * the rendered `preinst` embeds this manifest byte for byte;
#   * the ABI relationship the package's role requires is declared.
#
# Usage:
#   verify-package-manifest.sh DEB [--arch ARCH] [--require-signature]
#                                  [--keyring FILE]

set -euo pipefail

DEB=""
EXPECT_ARCH=""
REQUIRE_SIGNATURE=0
KEYRING=""

while [ "$#" -gt 0 ]; do
    case "$1" in
        --arch) EXPECT_ARCH="${2:?--arch needs a value}"; shift 2 ;;
        --require-signature) REQUIRE_SIGNATURE=1; shift ;;
        --keyring) KEYRING="${2:?--keyring needs a value}"; shift 2 ;;
        -h|--help)
            sed -n '2,30p' "$0"
            exit 0
            ;;
        -*) echo "error: unknown option $1" >&2; exit 2 ;;
        *)
            [ -z "$DEB" ] || { echo "error: only one .deb may be given" >&2; exit 2; }
            DEB="$1"
            shift
            ;;
    esac
done

[ -n "$DEB" ] || { echo "usage: $0 DEB [--arch ARCH] [--require-signature]" >&2; exit 2; }
[ -f "$DEB" ] || { echo "error: no such package: $DEB" >&2; exit 1; }

for tool in dpkg-deb python3 sha256sum; do
    command -v "$tool" >/dev/null 2>&1 || { echo "error: $tool is required" >&2; exit 1; }
done

PACKAGE="$(dpkg-deb -f "$DEB" Package)"
VERSION="$(dpkg-deb -f "$DEB" Version)"
ARCH="$(dpkg-deb -f "$DEB" Architecture)"
EPOCH="$(dpkg-deb -f "$DEB" XB-Claw-Os-Security-Epoch || true)"
ABI="$(dpkg-deb -f "$DEB" XB-Claw-Os-Abi || true)"
PROVIDES="$(dpkg-deb -f "$DEB" Provides || true)"
DEPENDS="$(dpkg-deb -f "$DEB" Depends || true)"

case "$PACKAGE" in
    claw-os-agent|claw-os-base|claw-os-desktop) ;;
    *) echo "error: $PACKAGE is not a gated Claw OS package" >&2; exit 1 ;;
esac
if [ -n "$EXPECT_ARCH" ] && [ "$ARCH" != "$EXPECT_ARCH" ]; then
    echo "error: $PACKAGE is built for $ARCH, expected $EXPECT_ARCH" >&2
    exit 1
fi
[ -n "$EPOCH" ] || { echo "error: $PACKAGE declares no XB-Claw-Os-Security-Epoch" >&2; exit 1; }
[ -n "$ABI" ] || { echo "error: $PACKAGE declares no XB-Claw-Os-Abi" >&2; exit 1; }

# The ABI generation has to be expressed as a relationship APT's solver
# understands, not only as an informational field.
#
# The match is token-exact. A substring test would let `claw-os-abi-1`
# be satisfied by a package declaring `claw-os-abi-12`, which is a
# different, incompatible ABI generation. Debian relationship fields are
# comma-separated, may carry alternatives (`|`), a version constraint
# (`(>= 1)`) and an architecture qualifier (`:any`), so each token is
# reduced to its bare package name before comparison.
declares_abi() {
    local field="$1" wanted="$2"
    python3 - "$field" "$wanted" <<'PY'
import re
import sys

field, wanted = sys.argv[1], sys.argv[2]
for clause in field.split(","):
    for alternative in clause.split("|"):
        token = alternative.strip()
        # Drop a version constraint, then an architecture qualifier.
        token = re.sub(r"\(.*", "", token).strip()
        token = re.sub(r"\[.*", "", token).strip()
        token = token.split(":", 1)[0].strip()
        if token == wanted:
            raise SystemExit(0)
raise SystemExit(1)
PY
}

case "$PACKAGE" in
    claw-os-agent)
        declares_abi "$PROVIDES" "claw-os-abi-$ABI" \
            || { echo "error: claw-os-agent must provide claw-os-abi-$ABI" >&2; exit 1; }
        ;;
    *)
        declares_abi "$DEPENDS" "claw-os-abi-$ABI" \
            || { echo "error: $PACKAGE must depend on claw-os-abi-$ABI" >&2; exit 1; }
        ;;
esac

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
dpkg-deb -x "$DEB" "$WORK/payload"
dpkg-deb -e "$DEB" "$WORK/control"

MANIFEST_DIR="$WORK/payload/usr/lib/cos/release-security"
MANIFEST="$MANIFEST_DIR/$PACKAGE/manifest.json"
[ -s "$MANIFEST" ] || {
    echo "error: $PACKAGE carries no release manifest at" >&2
    echo "       /usr/lib/cos/release-security/$PACKAGE/manifest.json" >&2
    exit 1
}
# One package, one manifest directory: a package that also shipped a
# sibling's directory would let dpkg decide whose manifest wins.
strays="$(find "$MANIFEST_DIR" -mindepth 1 -maxdepth 1 -type d ! -name "$PACKAGE" || true)"
[ -z "$strays" ] || {
    echo "error: $PACKAGE also ships another package's manifest directory:" >&2
    printf '       %s\n' $strays >&2
    exit 1
}

if [ "$REQUIRE_SIGNATURE" = "1" ]; then
    [ -s "$MANIFEST.asc" ] || {
        echo "error: $PACKAGE carries an unsigned release manifest" >&2
        exit 1
    }
    if [ -n "$KEYRING" ]; then
        command -v gpgv >/dev/null 2>&1 || { echo "error: gpgv is required" >&2; exit 1; }
        gpgv --keyring "$KEYRING" "$MANIFEST.asc" "$MANIFEST" >/dev/null 2>&1 || {
            echo "error: the release manifest of $PACKAGE does not verify" >&2
            exit 1
        }
    fi
fi

# The preinst carries the manifest verbatim, because it runs before any
# of the package's own files exist.
[ -x "$WORK/control/preinst" ] || { echo "error: $PACKAGE has no preinst" >&2; exit 1; }
python3 - "$WORK/control/preinst" "$MANIFEST" <<'PY'
import sys

preinst = open(sys.argv[1], encoding="utf-8").read()
manifest = open(sys.argv[2], encoding="utf-8").read()
if manifest.strip("\n") not in preinst:
    raise SystemExit("error: the preinst does not embed this package's manifest")
PY
[ -x "$WORK/control/prerm" ] || { echo "error: $PACKAGE has no prerm" >&2; exit 1; }
grep -q 'check-incoming' "$WORK/control/prerm" || {
    echo "error: the $PACKAGE prerm does not gate the incoming version" >&2
    exit 1
}

python3 - "$MANIFEST" "$PACKAGE" "$VERSION" "$ARCH" "$EPOCH" "$ABI" "$WORK/payload" <<'PY'
import hashlib
import json
import pathlib
import sys

path, package, version, arch, epoch, abi, payload = sys.argv[1:8]
raw = pathlib.Path(path).read_bytes().decode("utf-8")
document = json.loads(raw)
canonical = (
    json.dumps(document, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n"
)
if canonical != raw:
    raise SystemExit(f"error: the {package} manifest is not in canonical encoding")

release = document["release"]
if release["package"] != package:
    raise SystemExit(
        f"error: the manifest names {release['package']}, but the package is {package}"
    )
if release["version"] != version:
    raise SystemExit(
        f"error: the manifest names version {release['version']}, but the package is {version}"
    )
if release["architecture"] != arch:
    raise SystemExit(
        f"error: the manifest names architecture {release['architecture']}, not {arch}"
    )
if str(document["security_epoch"]) != str(epoch):
    raise SystemExit(
        f"error: the manifest names security epoch {document['security_epoch']}, "
        f"but the control field says {epoch}"
    )
if str(document["abi"]) != str(abi):
    raise SystemExit(
        f"error: the manifest names ABI {document['abi']}, but the control field says {abi}"
    )

# A higher security epoch has to win in APT's own ordering, and the only
# field that outranks every upstream version is the Debian epoch.
debian_epoch = version.split(":", 1)[0] if ":" in version else "0"
if debian_epoch != str(document["security_epoch"]):
    raise SystemExit(
        f"error: {package} {version} has Debian epoch {debian_epoch} but security epoch "
        f"{document['security_epoch']}. A release-security epoch is only enforceable in APT "
        f"when it is also the Debian epoch; rebuild with the matching version."
    )

components = document["components"]
if not components:
    raise SystemExit(f"error: the {package} manifest lists no components")
root = pathlib.Path(payload)
for component in components:
    installed = root / component["path"].lstrip("/")
    if not installed.is_file():
        raise SystemExit(
            f"error: the manifest lists {component['path']} but the package does not ship it"
        )
    digest = hashlib.sha256(installed.read_bytes()).hexdigest()
    if digest != component["sha256"]:
        raise SystemExit(
            f"error: {component['path']} does not match the digest its manifest records"
        )
print(
    f"  :: {package} {version} ({arch}) manifest binds "
    f"{len(components)} component(s), security epoch {document['security_epoch']}"
)
PY
