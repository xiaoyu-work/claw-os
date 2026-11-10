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
            usr/lib/cos/bin/claw-nearby-worker
            usr/lib/cos/bin/claw-nearby-radio
            usr/share/doc/claw-os-agent/nearby-owly.LICENSE
            usr/share/doc/claw-os-agent/nearby-PROVENANCE.md
            usr/share/doc/claw-os-agent/vendor/owlyd/PROVENANCE.md
            usr/share/doc/claw-os-agent/vendor/owlyd/LICENSE
            usr/lib/systemd/system/claw-nearby-radio.service
            usr/share/dbus-1/system-services/io.github.t4t5.Owly.service
            usr/share/dbus-1/system.d/io.github.t4t5.Owly.conf
            usr/lib/NetworkManager/conf.d/90-claw-nearby-unmanaged.conf
            usr/lib/systemd/network/10-claw-nearby-awdl0.link
            usr/local/bin/claw-gui-runner
            usr/local/bin/claw-display-session
            usr/lib/cos/bin/claw-display-host
            "usr/lib/$(dpkg-architecture -a"$ARCH" -qDEB_HOST_MULTIARCH)/security/pam_claw_display.so"
            usr/lib/sysusers.d/claw-os-agent.conf
            usr/local/bin/cos-browser
            usr/local/bin/claw-semantic
            usr/local/bin/claw-semantic-daemon
            usr/lib/cos/python/claw_os_sdk/__init__.py
            usr/lib/cos/python/cos_runtime/__init__.py
            usr/share/keyrings/claw-apps-archive-key.asc
            etc/apt/sources.list.d/claw-apps.sources
            usr/lib/cos/bin/claw-app-apt-hook
            usr/lib/cos/app-apt-metadata.py
            usr/lib/cos/skills/claw-os/SKILL.md
            usr/lib/cos/skills/claw-os/diagnostics.md
            usr/lib/systemd/system/clawd.service
            usr/lib/cos/bin/claw-security-floor
            usr/lib/cos/apt/security-floor-hook
            etc/apt/apt.conf.d/50claw-os-security-floor
        )
        dpkg-deb -f "$deb" Provides | grep -qw claw-os-calendar-read-v1
        dpkg-deb -f "$deb" Provides | grep -qw claw-os-nearby-v1
        dpkg-deb -f "$deb" Provides | grep -qw claw-os-display-session-v1
        if grep -Fxq './etc/cos/oauth.conf' "$files" \
            || grep -q '/usr/lib/cos/apps/' "$files"; then
            echo "error: Agent contains excluded OAuth configuration or App payloads" >&2
            exit 1
        fi
        dpkg-deb -x "$deb" "$extract/payload"
        test "$(stat -c '%a' "$extract/payload/usr/lib/cos/skills/claw-os")" = "755"
        test "$(stat -c '%a' "$extract/payload/usr/lib/cos/skills/claw-os/SKILL.md")" = "644"
        runtime_version="$(python3 scripts/app_packages.py runtime-version)"
        dpkg-deb -f "$deb" Provides | grep -Fq "claw-os-app-runtime-v1 (= $runtime_version)"
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
    if [ "$component" = desktop ]; then
        dpkg-deb -x "$deb" "$extract/payload"
        python3 scripts/app_packages.py native-boundary
    fi
    python3 scripts/app_packages.py check --variant "$component" \
        --architecture "$arch" --stage "$extract/payload" >/dev/null
fi

bash packaging/release-security/verify-package-manifest.sh "$deb" \
    --arch "$arch" --require-signature \
    --keyring "$CLAW_OS_RELEASE_SECURITY_KEYRING"
