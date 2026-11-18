# Building the Claw OS Desktop Image

The desktop profile builds Hyprland and the graphical Agent against Debian
13. Quickshell, Caelestia and private Qt arrive through the signed
`claw-app-desktop-shell` package. If that package is unavailable in the
authenticated App channel, composition stops without installing an alternative.

On a Linux host or in WSL2, keep the checkout and build directory on a Linux
filesystem. From the repository root, install the normal disk-image tools
and prepare both signed App package sets:

```bash
sudo apt install git debootstrap qemu-utils parted dosfstools mtools rsync \
  util-linux e2fsprogs grub-efi-amd64-bin grub-pc-bin dpkg-dev curl
python3 scripts/app_packages.py prepare --variant agent --architecture amd64
python3 scripts/app_packages.py prepare --variant desktop --architecture amd64
sudo ./presets/desktop.sh
```

Use the native architecture and corresponding GRUB package on arm64.
The default output is `build/claw-os-vm-amd64.vmdk`; import it into VMware.
See [the Azure build](building-azure.md) for a generalized Gen2 VHD.

## Login

On the local VM, set the locked `cos` account's password at the first console
prompt. Log in using greetd on VT7. There is no default password or automatic
App permission approval. Use the independent text console and
`cos review pending` to inspect any Desktop Shell review raised on first
launch. Headless `cos` does not require the desktop.

Selecting an installed signed App from the desktop menu creates one
`desktop.launch` review. Confirm it in Claw OS reviews or on the independent
console with `cos review approve <review-id>`. That confirmation is spent on
the selected App only; it does not skip the App's own activation or approval
prompts.

Shutdown and restart from the session menu use the same protected review UI.
Approval is only for the selected action in this login, not a reusable power
permission. Save work before approving; the machine can stop before it
responds.

Use the [coordinated logout/update procedure](updating.md#root-managed-desktop-session-lifetime)
when replacing the compositor and display authority on an existing system.
