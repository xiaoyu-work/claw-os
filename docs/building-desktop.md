# Building the Claw OS Desktop Image

This guide covers building a bootable Claw OS **desktop VM disk image** and loading
it in VMware. Only the steps are covered here — see `targets/vm/build.sh` for the
full details.

## What you get

The build produces a virtual disk under `build/`, e.g.
`build/claw-os-vm-amd64.vmdk`, that you can boot in a VM.

> **A real Linux host is required.** The build uses `debootstrap`, `chroot`,
> `losetup`, `mount` and `mkfs`, which need a Linux kernel and root. It also
> **builds natively only** — the image architecture must match the host
> (`amd64` host → `amd64` image, `arm64` host → `arm64` image).

## 1. Build steps

### Prerequisites (Debian/Ubuntu host)

```bash
sudo apt update
sudo apt install -y debootstrap qemu-utils parted dosfstools rsync \
                    util-linux e2fsprogs grub-efi-amd64-bin grub-pc-bin
# On an arm64 host, use grub-efi-arm64-bin instead (and drop grub-pc-bin).
```

### Build command

From the repository root:

```bash
sudo FEATURES=base,cos-core,systemd,kernel,desktop,vmware,copilot-cli,grub-disk,vm,apt-source \
     FORMATS=vmdk SIZE=16G ./build.sh vm
```

- `FORMATS` — output format: `vmdk` (VMware), `vhdx` (Hyper-V), `qcow2` (QEMU), or `raw`.
- `SIZE` — virtual disk size (sparse, so the file is much smaller).

Output: `build/claw-os-vm-<arch>.vmdk`.

### Linux

Run the prerequisites and build command above directly on a Debian/Ubuntu machine.

### Windows (WSL2)

1. Install a Debian or Ubuntu WSL2 distro:
   ```powershell
   wsl --install -d Ubuntu
   ```
2. Open the distro, then clone the repo and run the **Prerequisites** and
   **Build command** steps above inside WSL.

> On a **Windows-on-ARM** PC, WSL is `arm64`, so you can only build the `arm64`
> image — and VMware has no Windows-on-ARM build. Use Hyper-V (`FORMATS=vhdx`)
> instead, or build the `amd64` image on an x86 machine.

### macOS

macOS cannot run `debootstrap`/`chroot`/loop devices natively. Run the build
inside a **Linux VM** (UTM, VMware Fusion, Lima, …) or a privileged Linux
container, then follow the **Linux** steps inside it. On Apple Silicon the Linux
VM is `arm64`, so you get an `arm64` image.

## 2. Load it in VMware

1. Open **VMware Workstation / Player** (Windows/Linux) or **VMware Fusion** (macOS, Intel).
2. **Create a New Virtual Machine** → *Custom* → *I will install the operating
   system later*.
3. Guest OS: **Linux** → *Debian 11.x 64-bit* (or *ARM 64-bit* for an `arm64` image).
4. When prompted for a disk, choose **Use an existing virtual disk** and select
   the built `build/claw-os-vm-<arch>.vmdk`. Keep the existing format if asked.
5. (arm64 only) In *VM Settings → Options → Advanced*, set firmware to **UEFI**.
   amd64 images boot with either BIOS or UEFI.
6. **Power on** the VM.
