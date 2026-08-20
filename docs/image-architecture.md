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
- `local-user` creates the `cos` account needed by an unmanaged local VM.
- `cloud-init` adds provider-neutral first-boot provisioning.
- `azure` adds only the Azure datasource policy, Azure Linux Agent, and
  Hyper-V integration.
- `desktop` adds the COSMIC desktop independently of the platform.

In particular, `vm` does not create a user and `azure` does not contain the
Claw OS application stack. Profiles compose those independent capabilities.

### Image profiles

`scripts/lib/image-profiles.sh` is the single source of truth for supported
feature combinations. Targets use these defaults but continue to accept a
`FEATURES` override for development.

| Profile | Identity model | Platform integration |
| --- | --- | --- |
| Local VM | Baked `cos` user or desktop first-boot wizard | Generic VM; optional VMware Tools |
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

## Instance identity

Artifacts fall into two identity models.

**Locally managed artifacts** need a usable account because the runtime has no
metadata service. The desktop and WSL profiles create one interactively;
Docker creates the requested account from `CLAW_USER`, `CLAW_UID`, and
`CLAW_GID` when the container starts.

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
