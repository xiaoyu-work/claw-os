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

On the local VM, enter `cos` at the graphical sign-in screen. PAM asks for a
new password on the first sign-in, then authenticates normally. Complete the
graphical setup wizard before opening the desktop. There is no default
password or automatic App permission approval. Review Desktop Shell requests
in the graphical OS review window. Headless VM password setup still uses the
serial console and does not require a desktop.

On an installed machine, the greeter and desktop use the connected display's
preferred mode as reported by its graphics driver. The setup wizard fits that
screen and uses a compact layout on narrow displays. A VM uses the mode its
virtual display advertises; resizing the viewer window is hypervisor-specific.

Selecting an installed signed App from the desktop menu creates one
`desktop.launch` review. Confirm it in the graphical OS review window. That
confirmation is spent on the selected App only; it does not skip the App's
own activation or approval prompts.

Shutdown and restart from the session menu use the same protected review UI.
Approval is only for the selected action in this login, not a reusable power
permission. Save work before approving; the machine can stop before it
responds.

The session menu's Lock button uses the OS locker and the login account's
PAM authentication. The Shell can request a lock but neither sees input on
the lock screen nor offers an unlock action.
The session menu's Sleep button also requires the OS to confirm that native
lock before it sends the approved sleep action to logind. If confirming on
another console, return promptly to the graphical session so it can present
the lock screen.

Use the [coordinated logout/update procedure](updating.md#root-managed-desktop-session-lifetime)
when replacing the compositor and display authority on an existing system.
