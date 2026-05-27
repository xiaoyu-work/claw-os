# scripts/lib/arch.sh — Shared architecture mapping for build scripts.
# shellcheck shell=bash
#
# Source this from any target/packaging build script that needs to know
# which CPU architecture it is building for. Sets exported variables
# derived from a single $ARCH value (Debian-style: amd64 or arm64).
#
# Usage:
#   source "$PROJECT_DIR/scripts/lib/arch.sh"
#   # → $ARCH, $DEB_ARCH, $RUST_TARGET, $KERNEL_PKG,
#   #   $GRUB_EFI_TARGET, $GRUB_EFI_PKG, $GRUB_BIOS_TARGET, $GRUB_BIOS_PKG
#
# Defaults: $ARCH = host arch (via dpkg --print-architecture), so scripts
# that don't care just work on whatever machine they run on.
#
# Native-only policy: claw-os does not cross-compile. If $ARCH does not
# match the host, this lib aborts. To build a different arch, run the
# build on a host of that arch (e.g. an arm64 VM/box for arm64 images).

# Guard against being executed instead of sourced.
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    echo "error: scripts/lib/arch.sh must be sourced, not executed" >&2
    exit 1
fi

# Resolve host arch first so we can both default and verify against it.
_arch_host="$(dpkg --print-architecture 2>/dev/null || true)"
if [ -z "$_arch_host" ]; then
    # Fallback when dpkg isn't on PATH (some minimal hosts). Map uname -m
    # to Debian names.
    case "$(uname -m)" in
        x86_64|amd64) _arch_host=amd64 ;;
        aarch64|arm64) _arch_host=arm64 ;;
        *) _arch_host="unknown" ;;
    esac
fi

ARCH="${ARCH:-$_arch_host}"

case "$ARCH" in
    amd64)
        DEB_ARCH=amd64
        RUST_TARGET=x86_64-unknown-linux-musl
        KERNEL_PKG=linux-image-amd64
        GRUB_EFI_TARGET=x86_64-efi
        GRUB_EFI_PKG=grub-efi-amd64-bin
        GRUB_BIOS_TARGET=i386-pc
        GRUB_BIOS_PKG=grub-pc-bin
        # CPU microcode is x86-only. Both intel-microcode and
        # amd64-microcode are safe to install on any amd64 box: the
        # kernel loads only the matching vendor blob at early boot.
        MICROCODE_INTEL_PKG=intel-microcode
        MICROCODE_AMD_PKG=amd64-microcode
        # thermald is Intel-centric but harmless on AMD (the unit
        # self-disables when no Intel temperature interface is found).
        # It's an x86-only package on Debian, so empty on arm64.
        THERMALD_PKG=thermald
        ;;
    arm64)
        DEB_ARCH=arm64
        RUST_TARGET=aarch64-unknown-linux-musl
        KERNEL_PKG=linux-image-arm64
        GRUB_EFI_TARGET=arm64-efi
        GRUB_EFI_PKG=grub-efi-arm64-bin
        # ARM64 has no legacy BIOS path; firmware is always UEFI (or
        # u-boot, which we don't currently support).
        GRUB_BIOS_TARGET=""
        GRUB_BIOS_PKG=""
        # No x86 microcode story on arm64; SoC firmware ships from the
        # bootloader instead.
        MICROCODE_INTEL_PKG=""
        MICROCODE_AMD_PKG=""
        # thermald is x86-only on Debian.
        THERMALD_PKG=""
        ;;
    *)
        echo "error: unsupported ARCH='$ARCH' (expected: amd64 or arm64)" >&2
        return 1 2>/dev/null || exit 1
        ;;
esac

# Native-only: refuse to build for a different arch than the host. This
# avoids the trap of producing half-built images via qemu-user-static
# without us realising. Override only when you know what you're doing.
if [ "$ARCH" != "$_arch_host" ] && [ "${COS_ALLOW_CROSS:-0}" != "1" ]; then
    echo "error: ARCH=$ARCH but host is $_arch_host." >&2
    echo "       claw-os builds natively only — run this on an $ARCH host," >&2
    echo "       or set COS_ALLOW_CROSS=1 if you have qemu-user-static set up." >&2
    return 1 2>/dev/null || exit 1
fi

export ARCH DEB_ARCH RUST_TARGET KERNEL_PKG
export GRUB_EFI_TARGET GRUB_EFI_PKG GRUB_BIOS_TARGET GRUB_BIOS_PKG
export MICROCODE_INTEL_PKG MICROCODE_AMD_PKG
export THERMALD_PKG

# Convenience: a short suffix for output filenames. Always "$DEB_ARCH"
# (so a binary built for amd64 is `*-amd64.qcow2`, arm64 is `*-arm64.qcow2`).
ARCH_SUFFIX="$DEB_ARCH"
export ARCH_SUFFIX

unset _arch_host
