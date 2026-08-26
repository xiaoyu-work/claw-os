#!/usr/bin/env bash
# Central image profile definitions.
#
# Features describe capabilities inside a rootfs. Profiles compose those
# capabilities into a product variant. Targets only package the selected
# profile into an artifact such as a tarball, container, ISO, or disk image.

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    echo "error: scripts/lib/image-profiles.sh must be sourced, not executed" >&2
    exit 1
fi

# Shared non-desktop runtime used by WSL and Docker.
IMAGE_FEATURES_HEADLESS_RUNTIME="base,cos-core,browser,systemd,gpu-drivers,apt-source,qwen3-embedding"

# Local virtual-machine profiles. local-user is deliberately separate from
# vm: local hypervisors cannot inject a login account, while cloud platforms
# create one per instance through cloud-init.
IMAGE_FEATURES_VM="base,cos-core,systemd,kernel,grub-disk,vm,gpu-drivers,apt-source,local-user"
IMAGE_FEATURES_DESKTOP_VM="base,cos-core,systemd,kernel,desktop,vmware,copilot-cli,grub-disk,vm,apt-source,local-user"

# Generalized Azure profiles. They must never include local-user.
IMAGE_FEATURES_AZURE="base,cos-core,systemd,kernel,grub-disk,vm,gpu-drivers,apt-source,cloud-init,azure"
IMAGE_FEATURES_AZURE_DESKTOP="base,cos-core,systemd,kernel,desktop,copilot-cli,grub-disk,vm,apt-source,cloud-init,azure"

# Bootable media profiles.
IMAGE_FEATURES_LIVE="base,cos-core,systemd,kernel,live,gpu-drivers,apt-source"
IMAGE_FEATURES_INSTALLER="base,cos-core,systemd,kernel,grub-disk,live,installer,gpu-drivers,apt-source"

case ",$IMAGE_FEATURES_VM," in
    *,local-user,*) ;;
    *)
        echo "error: the default headless VM profile must include local-user" >&2
        return 1
        ;;
esac

case ",$IMAGE_FEATURES_VM," in
    *,cloud-init,*)
        echo "error: the local VM profile must not include cloud-init" >&2
        return 1
        ;;
esac

case ",$IMAGE_FEATURES_AZURE,$IMAGE_FEATURES_AZURE_DESKTOP," in
    *,local-user,*)
        echo "error: generalized cloud profiles must not include local-user" >&2
        return 1
        ;;
esac
