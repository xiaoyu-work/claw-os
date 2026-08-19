#!/usr/bin/env bash
# Finalize a staged root filesystem as a generalized Azure image.

set -euo pipefail

ROOTFS="${1:?usage: finalize-rootfs.sh <staged-rootfs>}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

source "$PROJECT_DIR/scripts/lib/generalize-cloud-image.sh"

if [ ! -x "$ROOTFS/usr/sbin/waagent" ]; then
    echo "error: staged Azure rootfs is missing waagent" >&2
    exit 1
fi
if [ ! -f "$ROOTFS/etc/cloud/cloud.cfg.d/90-azure.cfg" ]; then
    echo "error: staged Azure rootfs is missing the Azure cloud-init datasource" >&2
    exit 1
fi
if ! grep -qxF 'Provisioning.Agent=cloud-init' "$ROOTFS/etc/waagent.conf"; then
    echo "error: staged Azure rootfs does not delegate provisioning to cloud-init" >&2
    exit 1
fi
for service_link in \
    etc/systemd/system/cloud-init.target.wants/cloud-init-main.service \
    etc/systemd/system/multi-user.target.wants/ssh.service \
    etc/systemd/system/multi-user.target.wants/walinuxagent.service; do
    if [ ! -L "$ROOTFS/$service_link" ]; then
        echo "error: staged Azure rootfs has a disabled service: $service_link" >&2
        exit 1
    fi
done

generalize_cloud_image "$ROOTFS"

# waagent state is per VM and must not be cloned into the gallery image.
rm -rf "$ROOTFS/var/lib/waagent"
install -d -m 0755 "$ROOTFS/var/lib/waagent"
rm -rf "$ROOTFS/var/log/azure"
install -d -m 0755 "$ROOTFS/var/log/azure"

if [ -s "$ROOTFS/etc/machine-id" ]; then
    echo "error: Azure finalizer left a non-empty machine-id" >&2
    exit 1
fi
if find "$ROOTFS/etc/ssh" -maxdepth 1 -type f -name 'ssh_host_*' -print -quit \
        2>/dev/null | grep -q .; then
    echo "error: Azure finalizer left SSH host keys in the image" >&2
    exit 1
fi

echo ":: generalized staged rootfs for Azure"
