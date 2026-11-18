# Image architecture

Claw OS uses one source tree to build several distribution artifacts. The
pipeline is split into three layers so platform policy does not leak into the
operating-system capabilities:

```text
source packages and overlays
          |
          v
rootfs/features/*          capabilities installed into Debian
          |
          v
scripts/lib/image-profiles.sh
                           supported capability combinations
          |
          v
targets/*                  artifact packaging and platform finalization
```

This is the same separation used by mainstream distribution image pipelines:
the distribution owns the packages and root filesystem, a cloud or hardware
profile adds its integration, and an image builder emits the platform's
required artifact.

## Layer responsibilities

### Rootfs features

A feature owns one capability and must not assume an output format.

Examples:

- `kernel` installs the Debian kernel and initramfs tooling.
- `grub-disk` installs disk-boot configuration.
- `vm` configures generic virtual-machine console and power behavior.
- `local-user` creates a locked `cos` account and requires one-time password
  setup on the serial console for headless VMs or VT7 for desktop VMs.
- `cloud-init` adds provider-neutral first-boot provisioning.
- `azure` requires `cloud-init`, restricts it to the Azure datasource, configures
  Azure Linux Agent integration, forces Hyper-V storage/network modules into
  initramfs, and enables Azure serial-console boot diagnostics.
- `desktop` adds authenticated Hyprland login and requires the signed Desktop
  Shell App independently of the platform.
- `qwen3-embedding` supplies the fixed local Qwen3 model and its validated
  ONNX Runtime/GenAI pair in the system registry for real personal-file vectors.

In particular, `vm` does not create a user and `azure` does not contain the
Claw OS application stack. Profiles compose those independent capabilities.

The local file index is independent of Workspace tags: configured personal
directories are indexed once, while the shared broker filters each Workspace's
readable sources. Recoll remains full-text search. Unsupported formats or
missing local model/runtime are disclosed, never replaced with cloud or hash
embeddings. The model requires several GiB of memory; the semantic user service
limits its existing process/model-child group to 8 GiB.

### Image profiles

`scripts/lib/image-profiles.sh` is the single source of truth for supported
feature combinations. Targets use these defaults but continue to accept a
`FEATURES` override for development.

| Profile | Identity model | Platform integration |
| --- | --- | --- |
| Local VM | One-time console password setup for locked `cos` | Generic VM; optional VMware Tools |
| Azure | User, hostname, and SSH key supplied per instance | cloud-init, WALinuxAgent, Hyper-V |
| WSL | User created interactively by the WSL first-launch OOBE | Modern `.wsl` package conventions |
| Docker | User name and numeric identity supplied at container creation | Container/systemd conventions |
| Live/installer ISO | Temporary live user or installer-created user | Live boot and Calamares |

### Targets

Targets package a profile and may finalize a staged copy of its rootfs:

- `targets/common/disk-image.sh` is the internal GPT/GRUB disk packager.
- `vm` selects the local-VM profile and requests raw, QCOW2, VMDK, or VHDX.
- `azure` selects a generalized cloud profile, finalizes identity, and asks
  the disk builder for an Azure-compatible fixed VHD.
- `wsl` emits a modern `.wsl` package with a first-launch OOBE.
- `docker` builds an OCI container image.
- `iso-live` and `iso-installer` emit bootable ISO media.

Cloud generalization happens only in the staged disk filesystem. It does not
mutate the composed rootfs and does not affect local VM, WSL, or Docker
artifacts.

## Hardware enablement

Bootable images use the Debian kernel, firmware, and graphics stack selected by the hardware backports pin. The `kernel` feature includes Cirrus audio firmware and, on arm64, `firmware-qcom-soc`. The existing `firmware-libertas` package already supplies the Marvell Wi-Fi firmware used by applicable Intel Surface models. These packages supply firmware, not a board-specific boot path.

The following workarounds are adapted from [Omarchy's hardware setup and udev rules](https://github.com/omacom/omarchy/tree/3b36b9e19). They are packaged in this repository rather than installing Arch packages. They match devices on the running machine, not on the image builder.

| Hardware | Workaround |
| --- | --- |
| Framework Laptop 16 keyboard (`32ac:0012`) | Let the active local seat access the keyboard's QMK HID device. This does not install a QMK utility. |
| ASUS ROG Flow Z13 GZ302 | Classify the detachable keyboard's touchpad as internal so libinput can disable it while typing. |
| ASUS ExpertBook B9406 with the PixArt `093a:4f05` touchpad | Ignore the pad's binary pressure axes so libinput can retain motion events. This does not change the display or backlight. |
| Intel `MacBook8,1`, `MacBook9,1`, `MacBook10,1`, and `MacBookPro13,1`–`14,3` | Prevent the NVMe controller at `0000:01:00.0` from entering D3cold, avoiding a known resume failure. |

These are individual workarounds, not full-model certification. Apple Silicon needs its Asahi boot and kernel stack. Snapdragon X/X2 still need the right per-model device tree, early boot drivers, and any board-signed firmware absent from Debian. NVIDIA N1x and GB10 need their matching platform kernel, GPU stack, and boot configuration. Intel T2 and other fixes that depend on a custom kernel, DKMS package, or OEM payload are not supplied by these rules. Do not copy firmware from a vendor's recovery or Windows image into a distribution package without redistribution rights.

## Instance identity

Artifacts fall into two identity models.

**Locally managed artifacts** need a usable account because the runtime has no
metadata service. The default headless VM creates `cos` in a locked state and
blocks its normal serial getty with `cos-local-first-login.service` until the
console user sets a password. The desktop profile moves that password prompt to its local VT. WSL creates
an account interactively; Docker creates the requested account from `CLAW_USER`,
`CLAW_UID`, and `CLAW_GID` when the container starts.

The first-login service writes its completion marker only after `passwd`
succeeds. An interrupted setup repeats, while an account deliberately
provisioned with a password before boot is accepted. Azure profiles exclude
`local-user`, so Azure identity and credentials remain cloud-init managed.

**Cloud generalized artifacts** must contain no reusable human account or
machine identity. Before conversion, the Azure target:

- rejects any UID 1000-59999 login user;
- clears `machine-id`, SSH host keys, cloud-init state, DHCP leases, and agent
  state;
- locks direct root login;
- lets Azure metadata choose the administrator name, password/SSH key, and
  hostname;
- regenerates machine and SSH identities on first boot.

This is why `local-user` and `cloud-init` must never appear in the same
generalized profile.

## Adding another cloud

An AWS, GCP, OpenStack, or other cloud target should reuse:

1. the common Claw OS features;
2. `kernel`, `grub-disk`, and `vm`;
3. the provider-neutral `cloud-init` feature;
4. a small provider feature containing only its datasource/agent policy;
5. `generalize_cloud_image` during staged artifact finalization.

It should not fork the Debian bootstrap or duplicate the Claw OS packages.
