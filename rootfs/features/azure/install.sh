#!/usr/bin/env bash
# Azure-specific guest configuration layered over the generic cloud profile.

set -euo pipefail

if [ ! -x "$ROOTFS/usr/bin/cloud-init" ]; then
    echo "error: feature 'azure' requires feature 'cloud-init' before it" >&2
    exit 1
fi
if [ ! -x "$ROOTFS/usr/sbin/waagent" ]; then
    echo "error: waagent package is missing from the rootfs" >&2
    exit 1
fi

mkdir -p "$ROOTFS/etc/cloud/cloud.cfg.d"
cat > "$ROOTFS/etc/cloud/cloud.cfg.d/90-azure.cfg" <<'EOF'
# Restrict this artifact to Azure and use IMDS network metadata.
datasource_list: [ Azure ]
datasource:
  Azure:
    apply_network_config: true
    apply_network_config_set_name: false
    data_dir: /var/lib/waagent
EOF

set_waagent_option() {
    local key="$1"
    local value="$2"
    local config="$ROOTFS/etc/waagent.conf"
    local tmp

    tmp="$(mktemp)"
    awk -v key="$key" -v value="$value" '
        BEGIN { found = 0 }
        index($0, key "=") == 1 {
            print key "=" value
            found = 1
            next
        }
        { print }
        END {
            if (!found) {
                print key "=" value
            }
        }
    ' "$config" > "$tmp"
    install -m 0644 "$tmp" "$config"
    rm -f "$tmp"
}

# cloud-init owns provisioning. waagent remains enabled for fabric status,
# extensions, password reset, backup, and other Azure integrations.
set_waagent_option Provisioning.Agent cloud-init
set_waagent_option Extensions.Enabled y
set_waagent_option Extensions.WaitForCloudInit y
set_waagent_option ResourceDisk.Format n
set_waagent_option ResourceDisk.EnableSwap n

systemctl --root="$ROOTFS" enable walinuxagent.service >/dev/null

# Azure runs on Hyper-V. Force the storage and network drivers into initramfs
# even though this image is built under KVM/WSL and cannot auto-detect them.
INITRAMFS_MODULES="$ROOTFS/etc/initramfs-tools/modules"
for module in hv_vmbus hv_storvsc hv_netvsc udf vfat; do
    grep -qxF "$module" "$INITRAMFS_MODULES" 2>/dev/null \
        || echo "$module" >> "$INITRAMFS_MODULES"
done
chroot "$ROOTFS" update-initramfs -u -k all

# Override the local-VM presentation flags with Azure support diagnostics.
mkdir -p "$ROOTFS/etc/default/grub.d"
cat > "$ROOTFS/etc/default/grub.d/70-claw-os-azure.cfg" <<'EOF'
# Azure serial console and slow-storage tolerance.
GRUB_CMDLINE_LINUX_DEFAULT="rootdelay=300 console=tty0 console=ttyS0,115200n8 earlyprintk=ttyS0 net.ifnames=0"
EOF

echo "  :: Azure datasource, Linux Agent, Hyper-V initramfs, and serial console configured"
