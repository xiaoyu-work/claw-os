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
- `local-user` enables graphical account setup on fresh local desktop VMs.
  Headless VMs create a locked `cos` account and set its password on the
  serial console.
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
| Local VM | Graphical account creation on desktop; locked `cos` setup on headless | Generic VM; optional VMware Tools |
| Azure | User, hostname, and SSH key supplied per instance | cloud-init, WALinuxAgent, Hyper-V |
| WSL | User created interactively by the WSL first-launch OOBE | Modern `.wsl` package conventions |
| Docker | User name and numeric identity supplied at container creation | Container/systemd conventions |
| Live ISO / headless installer | Temporary live user or installer-created user | Live boot and Calamares |
| amd64 and Qualcomm desktop installers | Calamares-created account, then first-run setup inside the authenticated login | Debian or Qualcomm kernel, offline driver selection, and a desktop wizard before the Shell |

### Targets

Targets package a profile and may finalize a staged copy of its rootfs:

- `targets/common/disk-image.sh` is the internal GPT/GRUB disk packager.
- `vm` selects the local-VM profile and requests raw, QCOW2, VMDK, or VHDX.
- `azure` selects a generalized cloud profile, finalizes identity, and asks
  the disk builder for an Azure-compatible fixed VHD.
- `wsl` emits a modern `.wsl` package with a first-launch OOBE.
- `docker` builds an OCI container image.
- `iso-live` and `iso-installer` emit bootable ISO media.
- `asahi-rootfs` emits a desktop root filesystem archive for a separately provisioned Apple Silicon boot chain.

Cloud generalization happens only in the staged disk filesystem. It does not
mutate the composed rootfs and does not affect local VM, WSL, or Docker
artifacts.

## Hardware enablement

Bootable images use the Debian kernel, firmware, and graphics stack selected by the hardware backports pin. The `kernel` feature includes Cirrus audio firmware and, on arm64, `firmware-qcom-soc`. The existing `firmware-libertas` package supplies the Marvell Wi-Fi firmware used by applicable Intel Surface models. Desktop installs include the PipeWire libcamera backend for cameras already supported by the installed kernel and libcamera.

The following workarounds are adapted from [Omarchy's hardware setup and udev rules](https://github.com/omacom/omarchy/tree/3b36b9e19). They are packaged in this repository rather than installing Arch packages. They match devices on the running machine, not on the image builder.

| Hardware | Workaround |
| --- | --- |
| Framework Laptop 16 keyboard (`32ac:0012`) | Let the active local seat access the keyboard's QMK HID device. This does not install a QMK utility. |
| ASUS ROG Flow Z13 GZ302 | Classify the detachable keyboard's touchpad as internal so libinput can disable it while typing. |
| ASUS ExpertBook B9406 with the PixArt `093a:4f05` touchpad | Ignore the pad's binary pressure axes so libinput can retain motion events. |
| Intel `MacBook8,1`, `MacBook9,1`, `MacBook10,1`, and `MacBookPro13,1`–`14,3` | Prevent the NVMe controller at `0000:01:00.0` from entering D3cold, avoiding a known resume failure. |
| ASUS ExpertBook B9406 and Zenbook UX5406AA with Panther Lake graphics | Use DPCD backlight control; on the B9406, also disable the panel-replay path that can leave the display stuck. |
| Panther Lake Dell XPS 14/16 with the affected Intel Wi-Fi 7 PCI IDs | Disable EHT for those cards to use the working Wi-Fi 6 path. |
| Lenovo Yoga Pro 7 14IAH10 | Select the SOF HDA codec model that routes audio to both amplifiers. |
| Intel Macs with the listed Broadcom `brcmfmac` PCI devices | Let the host supplicant handle WPA instead of the affected firmware offload. |

`claw-hardware-setup` runs inside the Calamares target, not on the image builder. It applies the last four workarounds only when the target's installed kernel exposes the needed parameters, then rebuilds the initramfs. The x86 initramfs also includes Debian's available Apple SPI and Surface keyboard modules for early input; udev still decides what hardware is present.

### First-run desktop setup

A fresh local desktop VM has no human account. Before sign-in, the Quickshell
wizard runs in Cage. It skips network setup when NetworkManager confirms a
connection, then presents language, keyboard, Agent, account creation, time zone
and appearance. The language picker shows names for the installed Debian
UTF-8 locales. The display and taskbar use the system defaults.
**Finish** creates the local owner from the username and password and signs in
once through greetd/PAM with that password. Logout and later boots use the
normal graphical sign-in. Account creation is enabled only on the
unprovisioned local image. Account and model secrets are passed through stdin
to the provisioning helper.

The amd64 installer keeps Calamares and its account provisioning. Only an
authenticated account runs its follow-up network, Agent and optional hardware
configuration. Neither wizard needs to launch the signed Desktop Shell App
or use its private runtime.

The Qualcomm desktop installer uses the same Calamares and first-run flow.

The installer caches model-specific packages when building the ISO. Calamares recognizes the actual machine, installs its matching packages offline before reboot, and removes the remaining cache. It recognizes the Intel Surface Laptop 7 by ACPI `MSHW0551`, the Snapdragon 13.8-inch and 15-inch Surface Laptop 7 by their device-tree compatible IDs, and the Lenovo Yoga Pro 7 14IAH10 by DMI. Boot-critical modules and firmware are already on the media so input and networking do not depend on the wizard.

If matching Debian packages or the Surface Laptop 7 audio firmware are missing, the target leaves a pending job. NetworkManager starts it when it has internet access; the wizard displays the job only after the Network page. An explicit offline or skip choice lets the user finish, and a later connection can retry without restarting setup. Debian packages come from the signed APT repositories, locally built drivers come from the installer cache, and Surface audio firmware comes directly from the verified Microsoft archive described below.

### Experimental Snapdragon X boot

The arm64 live and installer ISOs list recognized Snapdragon X models in a separate GRUB submenu when the selected kernel ships the exact device tree. Choose the exact model and panel/SoC variant; the generic entry is unchanged. The installer records the selected tree only if it matches the running device tree. On disk, GRUB offers only the selected board's tree for each kernel that ships it and has its matching initramfs; older compatible kernels stay available.

Build the Qualcomm desktop installer with `sudo env CLAW_PLATFORM=aarch64-qualcomm ./build.sh iso-installer` on arm64 Debian 13. One image carries three Surface Laptop 7 entries: 13.8-inch I2C (`MSHW0468`), 13.8-inch SPI (`MSHW0461`), and 15-inch SPI. Select the matching digitizer; neither 13.8-inch tree enables the other digitizer. Both sizes support multi-touch input, not [active Surface Pen input](https://learn.microsoft.com/en-us/answers/questions/2308291/does-the-surface-laptop-7-work-with-any-microsoft). They use their own touchpad wiring and an OV02C10 front RGB camera with the privacy LED. Calamares installs the matched Debian `iptsd`, audio configuration, and libcamera/PipeWire stack offline. Wi-Fi and Bluetooth address recovery is available on the live medium and remains installed only on matching Surface models.

When a matching installed Surface first gets internet access, the hardware job downloads the [pinned Microsoft MSI](https://download.microsoft.com/download/b7ca2c3f-d320-4795-be0f-529a0117abb4/SurfaceLaptop7_ARM_Win11_26100_26.101.6391.0.msi) (531 MB), checks its SHA-256, and takes only the six verified audio firmware files. It never runs the MSI or flashes the laptop, and the archive is not bundled or retained. Use `sudo claw-import-board-firmware --surface-laptop-7 /path/to/official.msi` on the laptop to use a local copy. The live and default installed entries keep DSP disabled because it can disconnect USB storage. Once the files are installed, GRUB offers **OEM audio (experimental)** on NVMe installs. Reboot into that entry with USB storage disconnected. The vendor kernel's upstream speaker limits remain in place.

The installed Surface pack enables lid posture notifications and restarts `iptsd` after suspend. The posture modules are built against the exact Qualcomm kernel and are not signed for Secure Boot enforcement. The board trees force the multiport USB controller to reinitialize on wake rather than retain stale USB2 PHY state. Devices on that controller no longer wake the system; use the lid or power button. This is suspend/resume support, not hibernate-to-disk. The infrared camera and Windows Hello are not enabled. The actual multi-touch panel, camera, audio and repeated suspend still need verification on the laptop.

On other Snapdragon models, `claw-import-board-firmware --list-missing` shows files requested by the running device tree. `sudo claw-import-board-firmware /path/to/extracted-oem-driver` imports locally supplied matching files without downloading or overwriting firmware.

### Experimental vendor kernels

The generic Debian kernel remains the default. Build an opt-in image on a native Debian host with `sudo env CLAW_PLATFORM=<profile> ./build.sh iso-installer` or `iso-live`. Install the ISO tools plus `python3`, `gnupg`, `gpgv`, `zstd`, `kmod` and `device-tree-compiler` on the build host. The profile uses the exact kernel and matching initramfs for the ISO and the installed system rather than choosing an unrelated kernel by version.

| Profile | Native artifact and remaining limits |
| --- | --- |
| `x86-t2` | Debian 13 T2 kernel with early virtual USB for internal input. Mac-specific firmware for Wi-Fi and Bluetooth is not bundled. |
| `x86-surface-laptop-7-intel` | Desktop ISO for the Surface Laptop for Business 7th Edition with Intel Core Ultra 200V (ACPI `MSHW0551`), not the Snapdragon edition. See the [build and hardware checks](#intel-surface-laptop-7). |
| `aarch64-qualcomm` | Patched kernel and version-matched retail Snapdragon X/X2 device trees. Choose the exact model and variant in GRUB; the desktop installer selects Surface driver packages automatically. DSP remains off by default. |
| `aarch64-n1x` / `aarch64-gb10` | Matched NVIDIA open kernel modules, GPU firmware and driver libraries. GB10 media disables unsupported system sleep; N1x acceleration requires working production GPU firmware. |
| `aarch64-apple` | Aurora kernel, matching device trees and m1n1/U-Boot stage-two assets. Use `iso-live` for pre-provisioned Asahi UEFI or `sudo ./build.sh asahi-rootfs` for a desktop rootfs archive. Generic Calamares is disabled for this profile: it does not provision macOS recovery, an Apple boot policy or a dedicated Asahi ESP. |

`python3 scripts/platform_kernels.py <profile>` prepares the corresponding Debian packages and boot assets without building the full OS. Kernel and driver inputs are pinned by SHA-256; Omarchy artifacts also require signatures from the pinned archive key. Neither the image nor the install adds an Arch package repository. Generated platform artifacts carry their upstream source references and are not published by the regular Claw OS APT workflow.

Apple provisioning must first create and enroll a dedicated Asahi boot chain without overwriting macOS or recovery. The rootfs archive is for that Linux root and ESP only. It carries the matched stage-two image at `/usr/lib/cos/asahi/boot.bin` (`boot-j700.bin` for MacBook Neo); it does not activate either image or replace an enrolled Apple Stage 1. Internal speaker audio is disabled until the matching speaker protection stack is qualified. Other device-specific OEM firmware and user-space workarounds are not assumed to be covered by the kernel. Do not bundle material from a vendor recovery or Windows image without redistribution rights.

### Intel Surface Laptop 7

The standard amd64 desktop installer builds a Surface driver pack against the exact Debian kernel on the media. The live initramfs includes the patched Intel SSAM registry so the built-in keyboard can work before disk unlock. Calamares installs the full pack on `MSHW0551` systems and removes the temporary registry and rebuilds the stock initramfs on other PCs.

The pack adds the SSAM mapping for keyboard, battery, charger, fan and ambient-light sensor, Sensel `iptsd` touchpad gestures, SoundWire audio, IPU7 RGB and IR sensor drivers and matched libcamera/PipeWire userspace. It includes backlight and sleep workarounds, but does not enable the experimental IR illuminator or change login authentication. The dedicated Linux Surface kernel profile remains available. Intel graphics, Wi-Fi and Bluetooth firmware still come from Debian.

Build on native amd64 Debian 13 with the ISO tools above and a Debian package build environment. Install `build-essential`, `libelf-dev`, `meson`, `ninja-build`, `cmake`, `systemd-dev` and `libudev-dev`, plus the [libcamera 0.7.2 build dependencies](https://sources.debian.org/src/libcamera/0.7.2-1/debian/control/). The pinned `iptsd` sources use Meson wraps for their static dependencies. First build the stack by itself, or build the complete desktop installer:

```bash
python3 scripts/platform_kernels.py x86-surface-laptop-7-intel
sudo env CLAW_PLATFORM=x86-surface-laptop-7-intel ./build.sh iso-installer
```

Both installer paths check for the patched registry in the initramfs. The local modules are unsigned; they will not load under Secure Boot enforcement without a locally enrolled signing key. The standard path keeps the matched Debian kernel as the GRUB default on this model. Installing a new Debian kernel does not rebuild the Surface modules; build a matching pack before switching to a new kernel ABI.

Boot the USB before changing partitions. Verify internal input in the installer and after a cold boot (including the passphrase prompt if encrypted), battery/charging, brightness, speaker/mic/headphones, camera capture, touchpad gestures, Wi-Fi, Bluetooth, external ports and repeated lid sleep/wake. Test microSD sleep behavior on the 15-inch model. The profile is experimental until verified on the target hardware; intermittent sleep failures have been reported upstream.

## Instance identity

Artifacts fall into two identity models.

**Locally managed artifacts** need a usable account because the runtime has no
metadata service. The default headless VM creates `cos` in a locked state and
blocks its normal serial getty with `cos-local-first-login.service` until the
console user sets a password. The desktop profile creates the owner in the
graphical setup wizard before normal sign-in. WSL creates an account
interactively; Docker creates the requested account from `CLAW_USER`,
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
