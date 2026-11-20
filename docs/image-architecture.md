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
- `asahi-rootfs` emits a desktop root filesystem archive for a separately provisioned Apple Silicon boot chain. `asahi-rootfs --installer-payload` emits the Linux partition image and EFI files instead.

Cloud generalization happens only in the staged disk filesystem. It does not
mutate the composed rootfs and does not affect local VM, WSL, or Docker
artifacts.

## Hardware enablement

Bootable images use the Debian kernel, firmware, and graphics stack selected by the hardware backports pin. The `kernel` feature includes Cirrus audio firmware and, on arm64, `firmware-qcom-soc`. The existing `firmware-libertas` package supplies the Marvell Wi-Fi firmware used by applicable Intel Surface models. Desktop installs include the PipeWire libcamera backend for cameras already supported by the installed kernel and libcamera.

The following workarounds are adapted from [Omarchy's hardware setup and udev rules](https://github.com/omacom/omarchy/tree/3b36b9e19). They are packaged in this repository rather than installing Arch packages. They match devices on the running machine, not on the image builder.

| Hardware | Workaround |
| --- | --- |
| Framework Laptop 16 keyboard (`32ac:0012`) | Let the active local seat access the keyboard's QMK HID device. The matched installer pack adds `qmk_hid`. |
| Dell XPS with the Synaptics haptic touchpad (`VEN_06CB:00`) | Install the haptic daemon and presets offline. Use `sudo dell-xps-touchpad-haptics set low`, `mid`, or `high` to change the system setting. |
| Elgato Cam Link 4K on the stock amd64 kernel | With the device connected during install, cache and install the matching loopback module and relay. The camera appears as a fixed 1280×720 virtual device instead of a stretched browser input. |
| Supported Intel hybrid laptops with a system battery | Install Intel LPMD for the matched CPU family and model. This does not select Intel desktops or computers with only a battery-powered peripheral. |
| TUXEDO laptops on the stock amd64 kernel | Install the vendor's compatibility-checked DKMS keyboard/backlight modules and matching kernel headers. Claw OS does not disable the vendor's model check or delete existing modules. |
| Supported Qualcomm WCN6855, WCN7850 and QCC2072 PCI radios | For a mapped adapter missing from Debian's firmware, select its documented family record from the installed `board-2.bin`. The live and installed systems reuse that data without overriding stock or administrator-supplied files. Surface Pro 11 and Laptop 8 can use their mapped reference record. |
| ASUS ROG Flow Z13 GZ302 | Classify the detachable keyboard's touchpad as internal so libinput can disable it while typing. |
| ASUS ROG laptops | Install `asusctl` and its daemon by DMI. The service starts from the matched ASUS WMI device and its control interface is available to administrators. The package does not include ROG Control Center or bundled animation artwork. |
| ASUS ExpertBook B9406 with the PixArt `093a:4f05` touchpad | Ignore the pad's binary pressure axes so libinput can retain motion events. |
| Intel `MacBook8,1`, `MacBook9,1`, `MacBook10,1`, and `MacBookPro13,1`–`14,3` | Prevent the NVMe controller at `0000:01:00.0` from entering D3cold, avoiding a known resume failure. |
| ASUS ExpertBook B9406 and Zenbook UX5406AA with Panther Lake graphics | Use DPCD backlight control; on the B9406, also disable the panel-replay path that can leave the display stuck. |
| Panther Lake Dell XPS 14/16 with the affected Intel Wi-Fi 7 PCI IDs | Disable EHT for those cards to use the working Wi-Fi 6 path. |
| Lenovo Yoga Pro 7 14IAH10 | Select the SOF HDA codec model that routes audio to both amplifiers. |
| Intel Macs with the listed Broadcom `brcmfmac` PCI devices | Let the host supplicant handle WPA instead of the affected firmware offload. |
| Broadcom BCM4331 and BCM4360 (`14e4:4331` and `14e4:43a0`) | Cache and install Debian's STA DKMS driver with the exact stock amd64 kernel headers. Fix the current backport's open-source Linux 7.2 compatibility layer without changing the Broadcom binary. Nonmatching adapters keep their existing drivers. |

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

The installer caches model-specific packages when building the ISO. Calamares recognizes the actual machine, installs its matching packages offline before reboot, and removes the remaining cache. It recognizes the Intel Surface Laptop 7 by ACPI `MSHW0551`, the Snapdragon 13.8-inch and 15-inch Surface Laptop 7 by their device-tree compatible IDs, and the Lenovo Yoga Pro 7 14IAH10 by DMI. It can select a PCI driver pack alongside a model pack. The Broadcom STA driver is built against the target's Debian kernel; Secure Boot enforcement needs a locally enrolled signing key for the resulting module. Boot-critical modules and firmware are already on the media so input and networking do not depend on the wizard.

If matching Debian packages or indexed OEM firmware is missing, the target leaves a pending job. NetworkManager starts it when it has internet access; the wizard displays the job only after the Network page. An explicit offline or skip choice lets the user finish, and a later connection can retry without restarting setup. Debian packages come from the signed APT repositories, locally built drivers come from the installer cache, and OEM firmware comes from the verified archives described below.

On the stock Debian 7.2 amd64 kernel, the installer also matches `OVTI08F4` cameras with Intel IPU7 or IPU7.5. It installs the hardware ISP modules, unmodified Intel firmware and camera libraries, and the GStreamer relay offline. Applications see the processed **Hardware ISP Camera**; direct access to the ISP processing node and its raw capture nodes is restricted to root. The pack uses Debian's native sensor bridge, does not disable libcamera for other devices, and does not apply to IPU8. Secure Boot enforcement requires a locally enrolled DKMS signing key.

### Experimental Snapdragon X boot

The arm64 live and installer ISOs list recognized Snapdragon X models in a separate GRUB submenu when the selected kernel ships the exact device tree. Choose the exact model and panel/SoC variant; the generic entry is unchanged. The installer records the selected tree only if it matches the running device tree. On disk, GRUB offers only the selected board's tree for each kernel that ships it and has its matching initramfs; older compatible kernels stay available.

The Qualcomm initramfs includes the available X/X2 bus, display, input and USB drivers for disk unlock. The Samsung Galaxy Book4 Edge 14 boot option keeps AOSS awake for resume; the Lenovo Yoga Slim 7x option bypasses simpledrm before the native display driver takes over. The ASUS Zenbook A16 UX3607OA uses a text passphrase prompt and disables system sleep. The Zenbook 14 UX3480QA uses suspend-to-idle instead of deep sleep and leaves hibernation disabled. These settings apply only to the matching board options.

Build the Qualcomm desktop installer with `sudo env CLAW_PLATFORM=aarch64-qualcomm ./build.sh iso-installer` on arm64 Debian 13. One image carries three Surface Laptop 7 entries: 13.8-inch I2C (`MSHW0468`), 13.8-inch SPI (`MSHW0461`), and 15-inch SPI. Select the matching digitizer; neither 13.8-inch tree enables the other digitizer. Both sizes support multi-touch input, not [active Surface Pen input](https://learn.microsoft.com/en-us/answers/questions/2308291/does-the-surface-laptop-7-work-with-any-microsoft). They use their own touchpad wiring and an OV02C10 front RGB camera with the privacy LED. Calamares installs the matched Debian `iptsd`, audio configuration, and libcamera/PipeWire stack offline. Wi-Fi and Bluetooth address recovery is available on the live medium and remains installed only on matching Surface models.

When a matching installed Surface gets internet access, the hardware job verifies and extracts only the needed GPU, Iris, ADSP and cDSP files from the pinned [Surface Laptop 7](https://download.microsoft.com/download/b7ca2c3f-d320-4795-be0f-529a0117abb4/SurfaceLaptop7_ARM_Win11_26100_26.101.6391.0.msi) and [Surface Pro 11](https://download.microsoft.com/download/92821b24-203a-4ce5-9a3a-5df3160a2fe3/SurfacePro11_ARM_Win11_26100_26.094.41119.0.msi) MSIs (about 1.18 GB total). It does not install unrelated Pro 11 drivers, run either MSI, or flash the laptop. For an offline import, use `sudo claw-import-board-firmware --surface-laptop-7 /path/to/laptop7.msi --surface-pro-11 /path/to/pro11.msi`. Neither archive is bundled or retained.

Both sizes enable Qualcomm Iris. The signed Adreno shader is included in the rebuilt initramfs for the preinstalled Mesa Freedreno and Turnip drivers to use on the next boot. The live and default installed entries keep DSP disabled because it can disconnect USB storage. After firmware setup, NVMe installs offer **OEM audio and compute (experimental)** in GRUB. Reboot into that entry with USB storage disconnected. The vendor kernel's upstream speaker limits remain in place. On installations made from an older image, updating Base alone does not replace the board kernel or the Surface driver pack.

The matched Debian pack includes Qualcomm FastRPC 1.0.9 and `fastrpc-healthcheck`. It grants the active desktop seat access to the non-secure `fastrpc-cdsp` device and its system DMA heap, not the secure RPC node. For optional ONNX Runtime HTP inference, obtain the [official Hexagon SDK](https://www.qualcomm.com/developer/software/hexagon-dsp-sdk) and review the Qualcomm license in the [QNN 2.8.0 release](https://github.com/onnxruntime/onnxruntime-qnn/releases/tag/v2.8.0). Run `sudo claw-import-board-firmware --install-qnn /path/to/v73/G0/pic --accept-qnn-license`, passing the SDK directory containing the real v73 `libc.so` and `libgcc.so` files. An optional `--qnn-archive /path/to/release.tgz` uses a local copy. Run a compatible ONNX Runtime 1.29 application through `claw-qnn-run`; it sets the library paths for that process and exposes `CLAW_QNN_PROVIDER` and `CLAW_QNN_HTP` for registering the plugin and choosing its HTP backend. This does not change the default Claw inference backend or silently accept the Qualcomm license.

For units with the VD55G0 infrared sensor (`MSHW0472`), the installer trees add the separate CCI0/CSIPHY0 camera path and its OEM voltages. The kernel links each camera as it probes so an absent IR sensor does not prevent RGB capture. The IR sensor is available through the V4L2 media graph, not in the default video-call picker. The IR emitter and face login are not enabled.

The Qualcomm installer builds its common kernel from the pinned upstream tree and Omarchy patch series with hibernation and the Surface lid posture fix enabled. The Surface pack restarts `iptsd` after suspend. The board trees force the multiport USB controller to reinitialize on wake rather than retain stale USB2 PHY state. Devices on that controller no longer wake the system; use the lid or power button.

For manual disk hibernation on a Snapdragon Surface Laptop 7, select **Swap (with Hibernate)** in Calamares. The target needs one persistent NVMe swap partition at least as large as RAM; encrypted installations need persistent LUKS swap as well. The installer configures the initramfs and makes GRUB restore the selected board with the matched kernel. Swap files, zram, USB-backed swap, and automatic suspend-then-hibernate are not enabled for this path. Use `systemctl hibernate` to save a session to disk and power off, then boot Claw OS to restore it. Kernel lockdown may disallow hibernation; Claw OS does not change Secure Boot policy. Do not boot another OS against the same disks while the Claw OS session is hibernated. A cold boot after a failed restore requires deliberately discarding the saved session (`noresume`).

On other Snapdragon models, the installer carries a pinned OEM index, not the firmware itself. The pending job uses the active device tree's firmware folders to select exact members of OEM driver downloads and Windows Update cabinets. Windows Update metadata is fetched over TLS using Microsoft's pinned Update CA; public cabinet URLs can use HTTP, but only on the returned Microsoft download host without redirects, and the archive must still match its pinned SHA-256. The job verifies each selected file, extracts without running any installer, preserves firmware already present, and refreshes the boot artifacts. A missing file outside the index is not automatically downloaded. `sudo claw-import-board-firmware --qualcomm-oem` runs the same import manually, and `claw-import-board-firmware --list-missing` shows files requested by the running device tree. `sudo claw-import-board-firmware /path/to/extracted-oem-driver` imports locally supplied matching files without downloading or overwriting firmware.

The default boot still leaves the DSP off. For other Snapdragon models on NVMe, GRUB offers **OEM DSP (experimental)** when the running device tree matches and all enabled PAS remote processors have their named firmware. Disconnect USB storage before choosing it. A DSP entry does not imply a matching ALSA speaker profile or a working NPU runtime.

On Snapdragon Surface Laptop 8 (13.8- and 15-inch), UEFI already starts SOCCP; Linux attaches to it rather than requiring OEM files for that processor. The firmware job provisions ADSP, cDSP and both USB4 router images from the same verified Microsoft MSI, including on full-disk installs. It checks each router image after extracting it from the Windows driver without running it or reading the Windows partition. The installer uses suspend-to-idle, and the initramfs carries the board's early display and USB-C dependencies for the OEM DSP entry. The default and USB-storage boot restrictions are unchanged.

Calamares installs Laptop 8's matched speaker and microphone profile offline on both sizes; sound uses the OEM DSP boot entry and retains the kernel's amplifier limits. The driver pack also enables USB runtime suspend on these models and applies the ELAN pressure-pad quirk on the 13.8-inch model only. Wi-Fi and Bluetooth address recovery runs only on the matching Snapdragon Surface boards.

On Surface Pro 11 LCD and OLED, Calamares installs the shared Surface touch and pen daemon, camera tuning for the front IMX681, and the Flex Keyboard pairing and resume helpers offline. The front and rear cameras use libcamera; the monochrome IR sensor is not presented as a general webcam. The same pack stages the matched microphone and speaker routes and a multiband speaker limiter, used with the **OEM DSP (experimental)** boot entry. WirePlumber hides direct speaker playback and only creates the filtered output when the speaker device appears. Both displays use the conservative -3 dB digital and 0 dB amplifier gain settings.

The Surface Pro 11 also gets an offline ambient-light sensor service. The OEM firmware job verifies the sensor configuration from the same pinned Microsoft MSI as the DSP firmware and copies only the configuration; Windows does not need to be installed. Reboot into the **OEM DSP (experimental)** entry to use the sensor. The service waits for the audio DSP and exposes the light reading through `net.hadess.SensorProxy`; its suspend hook stops the stream so it cannot wake the device.

Calamares also installs matched ALSA configuration on Acer Aspire 16 AI A16-11M, ASUS ProArt PZ13 HT5306QA, ASUS Zenbook 14 UX3480QA, ASUS Zenbook A16 UX3607OA, and Snapdragon X HP OmniBook 3 14-hz0 and 16-bz0. The OmniBook uses the ThinkPad T14s topology verified for the 14-inch model; the 16-bz0 has the same device-tree audio wiring. This does not cover the SC7280-based 16-ct0. The Zenbook A16 uses software volume and keeps its speaker SoundWire controllers awake to prevent audio dropouts. The package does not change ALSA routing on unrelated machines.

### Experimental vendor kernels

The generic Debian kernel remains the default. Build an opt-in image on a native Debian host with `sudo env CLAW_PLATFORM=<profile> ./build.sh iso-installer` or `iso-live`. Install the ISO tools plus `python3`, `gnupg`, `gpgv`, `zstd`, `kmod` and `device-tree-compiler` on the build host. The profile uses the exact kernel and matching initramfs for the ISO and the installed system rather than choosing an unrelated kernel by version.

| Profile | Native artifact and remaining limits |
| --- | --- |
| `x86-t2` | Debian 13 T2 kernel with early virtual USB for internal input, matched Apple ALSA profiles, and a T2-only fan service. The installer tries to import Wi-Fi and Bluetooth firmware from the owner's local macOS system volume; the firmware is not bundled. |
| `x86-surface-laptop-7-intel` | Desktop ISO for the Surface Laptop for Business 7th Edition with Intel Core Ultra 200V (ACPI `MSHW0551`), not the Snapdragon edition. See the [build and hardware checks](#intel-surface-laptop-7). |
| `aarch64-qualcomm` | Patched kernel and version-matched retail Snapdragon X/X2 device trees. Choose the exact model and variant in GRUB; the desktop installer selects Surface driver packages automatically. DSP remains off by default. |
| `aarch64-n1x` / `aarch64-gb10` | Matched NVIDIA open kernel modules, GPU firmware and driver libraries. GB10 disables unsupported system sleep and limits the CPU to shallow idle to avoid the firmware's excessive wake latency. This can increase idle power use. N1x restores CPPC frequency ranges and uses a larger USB4 resource window on ASUS ProArt P14 H7407BA. |
| `aarch64-apple` | Aurora kernel, matching device trees and m1n1/U-Boot stage-two assets. Use `iso-live` for pre-provisioned Asahi UEFI or `sudo ./build.sh asahi-rootfs` for a desktop rootfs archive. Generic Calamares is disabled for this profile: it does not provision macOS recovery, an Apple boot policy or a dedicated Asahi ESP. |

`python3 scripts/platform_kernels.py <profile>` prepares the corresponding Debian packages and boot assets without building the full OS. Kernel and driver inputs are pinned by SHA-256; Omarchy artifacts also require signatures from the pinned archive key. Neither the image nor the install adds an Arch package repository. Generated platform artifacts carry their upstream source references and are not published by the regular Claw OS APT workflow.

Build `x86-t2` on Debian 13 amd64 with Cargo, Rust and a C linker. The T2 installer mounts the recognized internal macOS APFS volume read-only and imports only Broadcom wireless firmware. If the partition was erased, is encrypted, or is otherwise unreadable, use `sudo claw-import-t2-firmware /path/to/macos/usr/share/firmware` with a local copy and reboot. The fan service keeps the upstream firmware limits and returns fans to automatic control when it stops.

For an N1x laptop with pre-release `0.x` firmware, choose the **N1x firmware framebuffer** entry on the live or installer media. After installation, the detected `0.x` BIOS keeps the NVIDIA modules disabled; the firmware framebuffer remains available. The initramfs raises a dim NVIDIA panel to a readable brightness for disk unlock when the driver exposes the backlight. Normal acceleration still requires working production firmware. After updating the BIOS, run `sudo claw-hardware-setup` and `sudo update-grub` to remove the obsolete boot guard.

On an N1x with a known factory GPU memory reservation, `sudo /usr/lib/cos/hw/claw-n1x-setup gpu-memory` sets the firmware reservation to 4 GiB and requires a reboot. The installer does not change this system-wide EFI setting automatically; the command refuses unfamiliar firmware data.

The N1x image includes the signed ASUS ProArt P14 H7407BA amplifier firmware and speaker tuning, preserving the original Cirrus payloads. The driver selects them only for the CS35L56 SoundWire subsystem `1043:33A1`; an uncompressed kernel firmware match takes precedence when available.

Apple provisioning must first create and enroll a dedicated Asahi boot chain without overwriting macOS or recovery. The rootfs archive is for that Linux root and ESP only. It carries the matched stage-two image at `/usr/lib/cos/asahi/boot.bin` (`boot-j700.bin` for MacBook Neo); it does not activate either image or replace an enrolled Apple Stage 1.

On the installed system, Claw OS imports the Mac's Asahi `vendorfw/firmware.tar` and `manifest.txt` from the EFI partition identified by the boot device tree before udev coldplug and the first-run wizard. The partition is mounted read-only and existing firmware is kept. If the bundle is missing, boot continues; on an enrolled Mac, `sudo claw-asahi-import-firmware /path/to/vendorfw` can import a local copy and rebuild the initramfs. Both paths check hashes and hard links against the manifest. The live ISO does not import automatically, and the Linux importer does not download firmware. Do not bundle material from a vendor recovery or Windows image without redistribution rights.

On a native ARM64 Debian build host with `rsync`, `e2fsprogs` and `zip`, `sudo ./build.sh asahi-rootfs --installer-payload` builds `build/claw-os-asahi-arm64-installer.zip`. It contains an unencrypted ext4 `root.img` that grows into a larger Linux partition on boot and an `esp/` tree with ARM64 GRUB and both stage-two images. A compatible installer must select the correct stage two and write it as `m1n1/boot.bin` on the dedicated ESP. The ZIP also carries `stage1-j700.bin`, the unconfigured raw MacBook Neo boot template from the signed Aurora package. It stays outside the ESP tree. The payload neither repartitions a disk nor authorizes Recovery and contains no per-Mac firmware.

For experimental installation from macOS on supported M1–M3 models or MacBook Neo J700, copy the ZIP from a trusted Claw OS build and record its SHA-256 on the build host. From the Claw OS checkout on the Mac, run `sudo /bin/sh targets/asahi-rootfs/install-macos.sh --plan /path/to/claw-os-asahi-arm64-installer.zip SHA256` with that digest. The preflight checks the actual Mac, firmware choice and Linux payload without changing partitions. Both modes need temporary space for a private copy of the ZIP and internet access to obtain the verified installer. Sign in as a recognized macOS volume owner and back up macOS before replacing `--plan` with `--install`. The upstream Asahi menu handles resizing and installing into free space. Claw OS does not repair macOS Preboot, offer a whole-disk erase, or maintain existing Asahi installs. M3 Ultra and M4 or later are not admitted.

On the Neo, the installer also verifies the pinned, Developer ID-signed restore helper from the Omarchy release and selects Apple's macOS 26.6.2 `25G83` restore. It collects the Neo's own radio calibration and matching Apple radio firmware before offering a resize. It binds the Neo boot objects to the new EFI partition and leaves fingerprint calibration untouched. In the new install's Recovery, the owner must explicitly authorize the Claw OS boot volume, including its custom boot arguments. The existing macOS boot policy is unchanged. Native installation and the physical peripherals have not been verified on a Mac.

After the Linux root boots, the fresh image opens Claw OS's graphical first-run wizard to set up networking and create the first local owner. It does not ship a default account or password. The live ISO and cloud profiles are not armed for this setup.

The Apple Silicon build also requires GNU Make to stage the official Asahi DSP presets. The native image uses the same `speakersafetyd` 2.0.1 release as Omarchy for models with feedback sensing. On MacBook Neo J700, it instead uses the model's `speakerguardd` protection with the matched Aurora speaker, headphone and microphone profiles. WirePlumber uses the normal microphone array and leaves the always-on low-power microphone out of the desktop. The driver requires the pinned Aurora kernel and matching protection assets; the kernel keeps its gain limit unless the model's daemon holds and refreshes the lock. Unmatched models remain disabled.

Touch ID is off by default. On an enrolled Asahi installation with this Mac's local fingerprint calibration, supported M2 MacBook Air/Pro and M3 MacBook Air models can use `sudo claw-asahi-touch-id enable`. Back up macOS before accepting the terminal prompt: Touch ID shares Secure Enclave state, and an incorrect firmware write can prevent macOS from booting. Reboot, check `sudo claw-asahi-touch-id status`, then enroll using `fprintd-enroll`. Without SEP, the standard daemon continues to serve other readers. If SEP appeared but did not pass activation, the fingerprint service stops rather than opening it; password login is unchanged. `sudo claw-asahi-touch-id disable` stops using Touch ID after reboot without deleting the Linux keybag or enrollments. This does not enable M1 or MacBook Neo, extract factory keys, or change macOS boot policy.

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
