#!/usr/bin/env bash
# rootfs/features/vm/install.sh — VM-specific tweaks for persistent disk images.
#
# What we DO:
#  - Create a default 'cos' user (uid 1000, /bin/bash, passwordless sudo).
#    Same convention as the WSL target; users can tighten via `passwd cos`.
#  - Configure GRUB for serial-console-friendly boot:
#      GRUB_TERMINAL="serial console"
#      GRUB_SERIAL_COMMAND="serial --speed=115200 ..."
#      GRUB_CMDLINE_LINUX_DEFAULT="quiet console=tty0 console=ttyS0,115200n8 video=1920x1080"
#    Without GRUB_TERMINAL the menu would render on tty0 only — qemu
#    -nographic would appear to hang at the GRUB countdown.
#    video=1920x1080 forces a sane default mode so the greeter fills the
#    window — VMware/QEMU otherwise default to 1024x768 and guest auto-resize
#    only kicks in after login, leaving the login screen letterboxed.
#  - Enable serial-getty@ttyS0 so headless deploys have a login on
#    the serial port.
#
# What we DON'T do:
#  - Install GRUB to disk (targets/vm/build.sh does that in the
#    losetup'd image).
#  - Generate /boot/grub/grub.cfg (build.sh runs update-grub in chroot).
#  - Install cloud-init (out of M6 scope; intended for local hypervisors).
#
# Inherited from environment: ROOTFS, PROJECT_DIR.

set -euo pipefail

source "$PROJECT_DIR/scripts/lib/add-cos-user.sh"

# 1. Create 'cos' user (shared helper — also used by WSL and Docker targets).
#    EXCEPT: skip when the desktop feature has wired up the first-boot
#    wizard (cosmic-initial-setup). On a graphical VM image the wizard
#    will create the *real* user; pre-creating a passwordless-locked
#    `cos` would just clutter cosmic-greeter's user list and confuse
#    everyone. Headless VM builds (no desktop feature) still get `cos`.
if [ -f "$ROOTFS/etc/greetd/cosmic-greeter.toml" ] \
        && grep -q '^\[initial_session\]' "$ROOTFS/etc/greetd/cosmic-greeter.toml" 2>/dev/null; then
    echo "  :: desktop first-boot wizard present — skipping default 'cos' user"
else
    add_cos_user "$ROOTFS"
fi

# 2. /etc/default/grub for serial-friendly boot.
#    sed is in-place; the package ships a default file from grub-common.
GRUB_DEFAULT="$ROOTFS/etc/default/grub"
if [ -f "$GRUB_DEFAULT" ]; then
    sed -i \
        -e 's|^#\?GRUB_TERMINAL=.*|GRUB_TERMINAL="serial console"|' \
        -e 's|^#\?GRUB_SERIAL_COMMAND=.*|GRUB_SERIAL_COMMAND="serial --speed=115200 --unit=0 --word=8 --parity=no --stop=1"|' \
        "$GRUB_DEFAULT"

    # If the file didn't have the lines at all, append them.
    grep -q '^GRUB_TERMINAL=' "$GRUB_DEFAULT" || \
        echo 'GRUB_TERMINAL="serial console"' >> "$GRUB_DEFAULT"
    grep -q '^GRUB_SERIAL_COMMAND=' "$GRUB_DEFAULT" || \
        echo 'GRUB_SERIAL_COMMAND="serial --speed=115200 --unit=0 --word=8 --parity=no --stop=1"' >> "$GRUB_DEFAULT"
fi

# 2b. Kernel command line, as a grub.d drop-in.
#
# It has to be a drop-in, and it has to sort after grub-disk's
# 50-claw-os.cfg: grub-mkconfig reads /etc/default/grub first and the
# drop-ins after, so anything this feature wrote to /etc/default/grub for
# GRUB_CMDLINE_LINUX_DEFAULT was silently overwritten by 50-claw-os.cfg —
# the serial console never actually reached the kernel.
#
# `vt.global_cursor_default=0` hides the text-mode cursor. Logging in tears
# down the greeter's compositor and starts the user's, and for the moment
# in between there is nothing on the VT but a blinking console cursor. The
# gap is inherent to handing DRM master from one compositor to the other;
# hiding the cursor is what keeps it from reading as a glitch.
#
# `splash` is kept from 50-claw-os.cfg: it is what starts plymouth, which
# covers the boot-to-greeter transition.
mkdir -p "$ROOTFS/etc/default/grub.d"
cat > "$ROOTFS/etc/default/grub.d/60-claw-os-vm.cfg" <<'EOF'
# Claw OS VM overrides. Sorts after 50-claw-os.cfg, so this wins.
GRUB_CMDLINE_LINUX_DEFAULT="quiet splash vt.global_cursor_default=0 console=tty0 console=ttyS0,115200n8 video=1920x1080"
EOF

# 3. Enable serial-getty on ttyS0 for headless login.
mkdir -p "$ROOTFS/etc/systemd/system/getty.target.wants"
ln -sf /usr/lib/systemd/system/serial-getty@.service \
    "$ROOTFS/etc/systemd/system/getty.target.wants/serial-getty@ttyS0.service"

# 4. Don't auto-suspend a virtual machine.
#
# cosmic-idle suspends the whole machine after 30 minutes on AC (15 on
# battery) — sensible on a laptop, wrong in a VM. There is no battery to
# save, the host already manages power, and the guest cannot be woken from
# inside: moving the mouse does nothing, because the machine it would wake
# is suspended. The user is left staring at a black window that looks like
# a hang, and has to power-cycle the VM from the hypervisor UI.
#
# Screen-off (and the lock that follows it) is left alone: that one *is*
# wakeable from inside, since `process_input_event` turns the outputs back
# on with the first event it sees.
#
# Written as a system default under /usr/share/cosmic, so it seeds the
# value without pinning it — the user can still set a suspend time in
# Settings and their choice, stored under ~/.config, takes precedence.
IDLE_CONF_DIR="$ROOTFS/usr/share/cosmic/com.clawos.Idle/v1"
mkdir -p "$IDLE_CONF_DIR"
printf 'None' > "$IDLE_CONF_DIR/suspend_on_ac_time"
printf 'None' > "$IDLE_CONF_DIR/suspend_on_battery_time"

echo "  :: created 'cos' user, configured GRUB serial+console terminal, enabled serial-getty@ttyS0"
echo "  :: disabled idle auto-suspend (a suspended guest cannot be woken from inside)"
