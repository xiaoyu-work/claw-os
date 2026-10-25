#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."
scripts=(
    scripts/ci/*.sh
    scripts/lib/image-identity.sh
    rootfs/features/claw-mail-ai/install.sh
    tools/install-mail-ai.sh
    packaging/deb/build-debs.sh
    packaging/deb/build-desktop-deb.sh
    desktop/greeter/cosmic-greeter-start.sh
    desktop/session/data/start-cosmic
    rootfs/features/desktop/overlay/usr/lib/cos/firstboot-session
    crates/claw-display-login/test/fixtures/bootstrap.sh
    crates/claw-display-login/test/fixtures/authenticate.sh
    targets/docker/build.sh
    targets/docker/container-entrypoint.sh
    targets/wsl/build.sh
    targets/wsl/overlay/usr/lib/cos/init/wsl-oobe
)
for script in "${scripts[@]}"; do
    bash -n "$script"
done
if grep -Il $'\r' "${scripts[@]}"; then
    echo "Bash provisioning scripts must use LF line endings" >&2
    exit 1
fi
