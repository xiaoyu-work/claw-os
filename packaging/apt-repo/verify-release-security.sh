#!/usr/bin/env bash
# packaging/apt-repo/verify-release-security.sh — validate the
# coordinated release-security metadata of a repository pool before it
# is signed and published.
#
# APT's own signature says the publisher produced the artifacts. This
# says the artifacts are a *coherent, non-regressing* release:
#
#   * every Claw OS package in the pool carries a canonical release
#     manifest that describes exactly that package, version and
#     architecture;
#   * every manifest is signed by the publishing key and verifies with
#     `gpgv` against the keyring the repository publishes;
#   * no package regresses the security epoch or the Debian version of
#     the candidate already published for it;
#   * the packages in the pool are mutually compatible, so a merge of
#     independently published packages cannot produce a set the
#     installed systems would refuse;
#   * no manifest is already expired at publication time.
#
# The extracted manifests are copied into
# `dists/<suite>/release-security/`, so an installed system can fetch
# the current coordinated metadata without downloading a package.
#
# Usage:
#   verify-release-security.sh REPO_DIR SUITE COMPONENT KEYRING [PREVIOUS_DIR]
#
# PREVIOUS_DIR, when given, holds the manifests of the currently
# published repository; regressions are measured against it.

set -euo pipefail

if [ "$#" -lt 4 ]; then
    echo "usage: $0 REPO_DIR SUITE COMPONENT KEYRING [PREVIOUS_DIR]" >&2
    exit 2
fi

REPO_DIR="$1"
SUITE="$2"
COMPONENT="$3"
KEYRING="$4"
PREVIOUS_DIR="${5:-}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
POLICY="$PROJECT_DIR/packaging/release-security/policy.json"
BASELINE_FORMAT="claw.release-security-baseline/v1"

command -v dpkg-deb >/dev/null 2>&1 || {
    echo "error: dpkg-deb is required" >&2; exit 1; }
command -v python3 >/dev/null 2>&1 || {
    echo "error: python3 is required" >&2; exit 1; }
command -v gpgv >/dev/null 2>&1 || {
    echo "error: gpgv is required" >&2; exit 1; }

# ---------------------------------------------------------------------------
# Baseline: is this repository already known to publish release-security
# metadata?
#
# `sync-existing-packages.sh` answers that from the *signed* InRelease
# of the published repository and leaves a marker. Publication is only
# allowed to proceed without one when an operator has explicitly asked
# for the one-time migration, and even then only if the published
# repository genuinely has no baseline yet.
# ---------------------------------------------------------------------------
baseline_established=0
pre_protection=0
if [ -n "$PREVIOUS_DIR" ]; then
    [ -e "$PREVIOUS_DIR/.baseline-established" ] && baseline_established=1
    { [ -e "$PREVIOUS_DIR/.pre-protection-repository" ] \
        || [ -e "$PREVIOUS_DIR/.no-existing-repository" ]; } && pre_protection=1
fi
bootstrap="${COS_RELEASE_SECURITY_BOOTSTRAP:-0}"

if [ "$baseline_established" != "1" ] && [ "$pre_protection" != "1" ]; then
    echo "error: the published repository state was never established." >&2
    echo "       Run packaging/apt-repo/sync-existing-packages.sh first; it is" >&2
    echo "       what authenticates the current repository and decides whether a" >&2
    echo "       release-security baseline already exists." >&2
    exit 1
fi

if [ "$baseline_established" = "1" ]; then
    if [ "$bootstrap" = "1" ]; then
        echo "error: this repository has already established a release-security" >&2
        echo "       baseline; the one-time migration cannot be run again." >&2
        exit 1
    fi
    if [ ! -s "$PREVIOUS_DIR/baseline.json" ]; then
        echo "error: the published repository advertises a release-security baseline" >&2
        echo "       but its signed marker was not preserved. Refusing to publish." >&2
        exit 1
    fi
elif [ "$bootstrap" != "1" ]; then
    echo "error: this repository has no release-security baseline yet." >&2
    echo "       Establishing one is a deliberate, one-time migration: re-run the" >&2
    echo "       publication workflow with its release-security bootstrap input" >&2
    echo "       enabled. An ordinary build cannot set it." >&2
    exit 1
else
    echo ":: establishing the release-security baseline for this repository"
fi

OUT_DIR="$REPO_DIR/dists/$SUITE/release-security"
WORK_DIR="$REPO_DIR/.release-security-work"
# The structured result `build-repo.sh` consumes. It is written exactly
# once, at the end, and only describes what was actually produced — so
# `Release` can never advertise a baseline this run did not publish.
STATUS_FILE="$REPO_DIR/.release-security-status"
rm -rf "$WORK_DIR" "$STATUS_FILE"
mkdir -p "$OUT_DIR" "$WORK_DIR"

write_status() {
    local baseline="$1" reason="$2"
    {
        printf 'baseline=%s\n' "$baseline"
        printf 'suite=%s\n' "$SUITE"
        printf 'component=%s\n' "$COMPONENT"
        printf 'reason=%s\n' "$reason"
    } > "$STATUS_FILE"
}

shopt -s nullglob
debs=("$REPO_DIR/pool/$COMPONENT"/*/claw-os-*/*.deb)
if [ "${#debs[@]}" -eq 0 ]; then
    echo "error: no Claw OS packages found in $REPO_DIR/pool/$COMPONENT" >&2
    exit 1
fi

extracted=0
for deb in "${debs[@]}"; do
    package="$(dpkg-deb --field "$deb" Package)"
    version="$(dpkg-deb --field "$deb" Version)"
    arch="$(dpkg-deb --field "$deb" Architecture)"
    case "$package" in
        claw-os-agent|claw-os-base|claw-os-desktop) ;;
        *) continue ;;
    esac

    stage="$WORK_DIR/$package-$arch"
    mkdir -p "$stage"
    dpkg-deb --fsys-tarfile "$deb" \
        | tar -x -C "$stage" "./usr/lib/cos/release-security/$package" 2>/dev/null || true
    manifest="$stage/usr/lib/cos/release-security/$package/manifest.json"
    signature="$manifest.asc"
    if [ ! -s "$manifest" ]; then
        # Migration ratchet: an artifact published before downgrade
        # protection existed carries no manifest. That is tolerated only
        # while this repository has no baseline at all — as soon as one
        # exists, a manifest-less artifact is a regression and is
        # refused.
        if [ "$baseline_established" = "1" ]; then
            echo "error: $package $version ($arch) carries no release-security" >&2
            echo "       manifest, but this repository has an established baseline" >&2
            exit 1
        fi
        if [ -n "$PREVIOUS_DIR" ] && [ -s "$PREVIOUS_DIR/${package}_${arch}.json" ]; then
            echo "error: $package $version ($arch) carries no release-security" >&2
            echo "       manifest, but one is already published for it" >&2
            exit 1
        fi
        echo "  :: note: $package $version ($arch) predates release-security metadata"
        continue
    fi
    if [ ! -s "$signature" ]; then
        echo "error: $package $version carries an unsigned release manifest;" >&2
        echo "       publication requires signed release-security metadata" >&2
        exit 1
    fi
    if ! gpgv --keyring "$KEYRING" "$signature" "$manifest" >/dev/null 2>&1; then
        echo "error: the release manifest of $package $version does not verify" >&2
        echo "       against the publishing keyring" >&2
        exit 1
    fi

    python3 - "$manifest" "$package" "$version" "$arch" "$SUITE" "$COMPONENT" <<'PY'
import datetime, json, sys

path, package, version, arch, suite, component = sys.argv[1:7]
raw = open(path, "rb").read()
text = raw.decode("utf-8")
document = json.loads(text)
canonical = json.dumps(
    document, sort_keys=True, separators=(",", ":"), ensure_ascii=False
) + "\n"
if canonical != text:
    raise SystemExit(f"error: {package} manifest is not in canonical encoding")
release = document["release"]
if release["package"] != package:
    raise SystemExit(f"error: manifest names {release['package']}, not {package}")
if release["version"] != version:
    raise SystemExit(
        f"error: {package} manifest names version {release['version']}, not {version}"
    )
if release["architecture"] != arch:
    raise SystemExit(
        f"error: {package} manifest names architecture {release['architecture']}"
    )
if release["suite"] != suite or release["component"] != component:
    raise SystemExit(f"error: {package} manifest is published for another suite")
valid_until = datetime.datetime.fromisoformat(
    document["valid_until"].replace("Z", "+00:00")
)
if valid_until <= datetime.datetime.now(tz=datetime.timezone.utc):
    raise SystemExit(f"error: {package} manifest has already expired")
PY

    target="$OUT_DIR/${package}_${arch}.json"
    cp "$manifest" "$target"
    cp "$signature" "$target.asc"
    chmod 0644 "$target" "$target.asc"
    extracted=$((extracted + 1))
    echo "  :: release-security $package $version ($arch) verified"
done

if [ "$extracted" -eq 0 ]; then
    if [ "$baseline_established" = "1" ]; then
        echo "error: this repository has an established release-security baseline" >&2
        echo "       but the pool carries no release-security metadata at all" >&2
        exit 1
    fi
    # Nothing to anchor a baseline to. Publish honestly *without* the
    # marker rather than advertising protection that does not exist.
    echo "warning: no Claw OS release-security metadata was published;" >&2
    echo "         every package in this pool predates downgrade protection" >&2
    rmdir "$OUT_DIR" 2>/dev/null || true
    write_status 0 "pool carries no release-security metadata"
    rm -rf "$WORK_DIR"
    exit 0
fi

# Regression and mutual-compatibility checks across the whole set.
python3 - "$OUT_DIR" "$POLICY" "$PREVIOUS_DIR" <<'PY'
import json, os, pathlib, sys

out_dir, policy_path, previous_dir = sys.argv[1:4]
policy = json.load(open(policy_path, encoding="utf-8"))


def load(directory):
    found = {}
    if not directory or not os.path.isdir(directory):
        return found
    for path in sorted(pathlib.Path(directory).glob("*.json")):
        document = json.loads(path.read_text(encoding="utf-8"))
        found[path.stem] = document
    return found


def compare_versions(left, right):
    """dpkg version ordering, restricted to what publication needs."""
    import subprocess

    for relation, result in (("lt", -1), ("eq", 0), ("gt", 1)):
        if subprocess.run(
            ["dpkg", "--compare-versions", left, relation, right]
        ).returncode == 0:
            return result
    raise SystemExit(f"error: cannot compare {left} and {right}")


published = load(out_dir)
previous = load(previous_dir)

for name, document in published.items():
    former = previous.get(name)
    if not former:
        continue
    if document["security_epoch"] < former["security_epoch"]:
        raise SystemExit(
            f"error: {name} would republish security epoch "
            f"{document['security_epoch']} below the published "
            f"{former['security_epoch']}"
        )
    if document["security_epoch"] == former["security_epoch"]:
        ordering = compare_versions(
            document["release"]["version"], former["release"]["version"]
        )
        if ordering < 0:
            raise SystemExit(
                f"error: {name} would republish {document['release']['version']} "
                f"below the published {former['release']['version']}"
            )
        if ordering == 0 and document != former:
            raise SystemExit(
                f"error: {name} would replace the published "
                f"{former['release']['version']} with different content"
            )

versions = {
    document["release"]["package"]: document["release"]["version"]
    for document in published.values()
}
for document in published.values():
    package = document["release"]["package"]
    for other, minimum in document["minimum_compatible"].items():
        if other == package or other not in versions:
            continue
        if compare_versions(versions[other], minimum) < 0:
            raise SystemExit(
                f"error: {package} {document['release']['version']} requires "
                f"{other} {minimum} or newer, but the repository would publish "
                f"{versions[other]}"
            )

epochs = {document["security_epoch"] for document in published.values()}
abis = {document["abi"] for document in published.values()}
if len(abis) > 1:
    raise SystemExit(f"error: the repository would publish mixed ABI generations {abis}")
if policy["security_epoch"] not in epochs and epochs:
    print(
        "  :: note: published epochs "
        f"{sorted(epochs)} differ from the working-tree policy "
        f"{policy['security_epoch']}"
    )
print(f"  :: release-security set verified for {sorted(versions)}")
PY

# ---------------------------------------------------------------------------
# Publish the baseline marker. Once this is signed into the repository,
# every later publication must present it, and the one-time migration
# can never be run again.
# ---------------------------------------------------------------------------
GPG_KEY_ID="${GPG_KEY_ID:-}"
if [ -z "$GPG_KEY_ID" ]; then
    echo "error: GPG_KEY_ID is required to sign the release-security baseline" >&2
    exit 1
fi

if [ "$baseline_established" = "1" ]; then
    # Carry the established marker forward verbatim, so its
    # `established_at` keeps naming the migration that created it.
    cp "$PREVIOUS_DIR/baseline.json" "$OUT_DIR/baseline.json"
    cp "$PREVIOUS_DIR/baseline.json.asc" "$OUT_DIR/baseline.json.asc"
else
    python3 - "$OUT_DIR/baseline.json" "$BASELINE_FORMAT" "$SUITE" "$COMPONENT" <<'PY'
import datetime, json, sys

path, fmt, suite, component = sys.argv[1:5]
document = {
    "component": component,
    "established_at": datetime.datetime.now(tz=datetime.timezone.utc).strftime(
        "%Y-%m-%dT%H:%M:%SZ"
    ),
    "format": fmt,
    "suite": suite,
}
body = json.dumps(document, sort_keys=True, separators=(",", ":")) + "\n"
open(path, "w", encoding="utf-8").write(body)
PY
    gpg_sign="$PROJECT_DIR/packaging/release-security/gpg-sign.sh"
    # shellcheck source=/dev/null
    source "$gpg_sign"
    claw_gpg_sign_detached "$GPG_KEY_ID" "$OUT_DIR/baseline.json" \
        "$OUT_DIR/baseline.json.asc"
    echo "  :: release-security baseline established"
fi
chmod 0644 "$OUT_DIR/baseline.json" "$OUT_DIR/baseline.json.asc"
gpgv --keyring "$KEYRING" "$OUT_DIR/baseline.json.asc" "$OUT_DIR/baseline.json" \
    >/dev/null 2>&1 || {
    echo "error: the release-security baseline marker does not verify" >&2
    exit 1
}

# Only now, with a signed and verified marker on disk, may `Release`
# advertise the baseline.
write_status 1 "signed baseline marker verified in $OUT_DIR"

rm -rf "$WORK_DIR"
