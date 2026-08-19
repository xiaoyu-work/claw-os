#!/usr/bin/env bash
# Provider-neutral cloud image configuration.

set -euo pipefail

if [ ! -x "$ROOTFS/usr/bin/cloud-init" ]; then
    echo "error: cloud-init package is missing from the rootfs" >&2
    exit 1
fi

# The datasource controls password authentication from instance metadata.
# Keep root disabled and let cloud-init create the per-instance administrator.
chroot "$ROOTFS" passwd -l root >/dev/null
rm -f "$ROOTFS/etc/cloud/cloud-init.disabled"

mkdir -p "$ROOTFS/etc/cloud/cloud.cfg.d"
cat > "$ROOTFS/etc/cloud/cloud.cfg.d/10-claw-os-cloud.cfg" <<'EOF'
# Provider-neutral Claw OS cloud defaults.
preserve_hostname: false
disable_root: true
ssh_deletekeys: true
ssh_genkeytypes: [rsa, ecdsa, ed25519]
output: {all: '| tee -a /var/log/cloud-init-output.log'}
EOF

# cloud-init-generator activates cloud-init.target when it detects a
# datasource. Enable the all-stages service within that target explicitly;
# SSH also needs to be enabled for remote login.
systemctl --root="$ROOTFS" enable cloud-init-main.service ssh.service >/dev/null

echo "  :: cloud-init installed; root login disabled; ssh enabled"
