#!/usr/bin/env bash
set -euo pipefail

cd "${GITHUB_WORKSPACE:?GITHUB_WORKSPACE is required}"
: "${RUNNER_TEMP:?RUNNER_TEMP is required}"
: "${COS_PACKAGE_VERSION:?COS_PACKAGE_VERSION is required}"
: "${CLAW_OS_RELEASE_SECURITY_KEYRING:?CLAW_OS_RELEASE_SECURITY_KEYRING is required}"
component="${1:-}"
case "$component" in
    agent|desktop) arch="${ARCH:?ARCH is required}" ;;
    base) arch=all ;;
    *) echo "usage: $0 {agent|base|desktop}" >&2; exit 2 ;;
esac
file_version="${COS_PACKAGE_VERSION//:/%3a}"
deb="build/debs/claw-os-${component}_${file_version}_${arch}.deb"
test -f "$deb"
test "$(dpkg-deb -f "$deb" Package)" = "claw-os-$component"
test "$(dpkg-deb -f "$deb" Version)" = "$COS_PACKAGE_VERSION"
test "$(dpkg-deb -f "$deb" Architecture)" = "$arch"

extract="$(mktemp -d "$RUNNER_TEMP/claw-os-package-check.XXXXXX")"
trap 'rm -rf -- "$extract"' EXIT
files="$extract/files"
dpkg-deb --fsys-tarfile "$deb" | tar -tf - > "$files"
required=()
case "$component" in
    agent)
        required=(
            usr/local/bin/cos
            usr/local/bin/clawd
            usr/local/bin/claw-agentd
            usr/local/bin/claw-extension-host
            usr/lib/cos/bin/claw-calendar-reader
            usr/share/doc/claw-os-agent/calendar-reader.LICENSE
            usr/local/bin/claw-gui-runner
            usr/local/bin/claw-display-session
            usr/lib/cos/bin/claw-display-host
            "usr/lib/$(dpkg-architecture -a"$ARCH" -qDEB_HOST_MULTIARCH)/security/pam_claw_display.so"
            usr/lib/sysusers.d/claw-os-agent.conf
            usr/local/bin/cos-browser
            usr/local/bin/claw-semantic
            usr/local/bin/claw-semantic-daemon
            usr/lib/cos/apps/web/app.json
            usr/lib/cos/skills/claw-os/SKILL.md
            usr/lib/cos/skills/claw-os/diagnostics.md
            usr/lib/systemd/system/clawd.service
            usr/lib/cos/bin/claw-security-floor
            usr/lib/cos/apt/security-floor-hook
            etc/apt/apt.conf.d/50claw-os-security-floor
        )
        dpkg-deb -f "$deb" Provides | grep -qw claw-os-calendar-read-v1
        dpkg-deb -f "$deb" Provides | grep -qw claw-os-display-session-v1
        if grep -Fxq './etc/cos/oauth.conf' "$files" \
            || grep -q '/apps/panel-calendar/' "$files"; then
            echo "error: Agent contains excluded OAuth configuration or Desktop Apps" >&2
            exit 1
        fi
        dpkg-deb -x "$deb" "$extract/payload"
        test "$(stat -c '%a' "$extract/payload/usr/lib/cos/skills/claw-os")" = "755"
        test "$(stat -c '%a' "$extract/payload/usr/lib/cos/skills/claw-os/SKILL.md")" = "644"
        ;;
    base)
        required=(usr/local/bin/cos-init usr/lib/systemd/system/cos-home-setup.service)
        if grep -Fxq './usr/local/bin/cos' "$files" \
            || grep -q '/usr/lib/cos/apps/' "$files"; then
            echo "error: Base contains Agent-owned binaries or Apps" >&2
            exit 1
        fi
        ;;
    desktop)
        required=(
            usr/local/bin/cos-agent-ui
            usr/local/bin/cos-agent-bridge
            usr/local/bin/cos-ask-claw-launcher
            usr/share/applications/com.clawos.Agent.desktop
        )
        ;;
esac
for path in "${required[@]}"; do
    if ! grep -Fxq "./$path" "$files"; then
        echo "error: $deb is missing $path" >&2
        exit 1
    fi
done

if [ "$component" != base ]; then
    snapshot="usr/share/doc/claw-os-$component/app-sources.json"
    dpkg-deb --fsys-tarfile "$deb" | tar -xOf - "./$snapshot" > "$extract/app-sources.json"
    python3 - "${CLAW_APP_SOURCE_LOCK:?CLAW_APP_SOURCE_LOCK is required}" \
        "$extract/app-sources.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    expected = json.load(handle)
with open(sys.argv[2], encoding="utf-8") as handle:
    actual = json.load(handle)
if actual != expected:
    raise SystemExit("error: package App sources do not match this release snapshot")
PY
fi

bash packaging/release-security/verify-package-manifest.sh "$deb" \
    --arch "$arch" --require-signature \
    --keyring "$CLAW_OS_RELEASE_SECURITY_KEYRING"
