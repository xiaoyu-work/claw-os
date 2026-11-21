# Building the Claw OS Desktop Image

The desktop profile builds Hyprland and the graphical Agent against Debian 13.
`claw-os-desktop` also contains the signed Desktop Shell (Quickshell, Caelestia
and private Qt). Every other Claw App is installed by default through its own
package. The build stops if the approved Shell input or signed App APT inputs
are unavailable.

On a Linux host or in WSL2, keep the checkout and build directory on a Linux
filesystem. From the repository root, install the normal disk-image tools
and prepare the authenticated inputs:

```bash
sudo apt install git debootstrap qemu-utils parted dosfstools mtools rsync \
  util-linux e2fsprogs grub-efi-amd64-bin grub-pc-bin dpkg-dev curl bubblewrap
python3 scripts/app_packages.py prepare --variant agent --architecture amd64
python3 scripts/app_packages.py prepare --variant desktop --architecture amd64
python3 scripts/desktop_shell.py prepare --architecture amd64 --version "${DESKTOP_SHELL_RELEASE:?set the approved version}"
sudo ./presets/desktop.sh
```

`DESKTOP_SHELL_RELEASE` is the version of an approved immutable
`desktop-shell-v<version>` release in `xiaoyu-work/clawos-app`. Supply the
same version as `desktop_shell_version` when dispatching the OS `desktop`,
`installer` or `all` target. It is a build input, not a package the user installs. Changing
it for a published Desktop requires a new OS package version.
The build's signature check uses Bubblewrap with only the packaged publisher;
it does not read or change the build host's trust store.

Use the native architecture and corresponding GRUB package on arm64.
The default output is `build/claw-os-vm-amd64.vmdk`; import it into VMware.
See [the Azure build](building-azure.md) for a generalized Gen2 VHD.

For a bootable amd64 installer, the manual **Release Claw OS → installer**
target builds a BIOS/UEFI Calamares ISO from the same run's verified Agent,
Base and Desktop packages. The ISO and its SHA-256 checksum are run artifacts,
not a public release or hardware certification. Verify a clean installation and
an upgrade on a disposable disk before distributing the image.

## Login

The local desktop VM opens the graphical setup wizard without asking you to
sign in. Create your account, optionally choose an avatar, and select
**Finish** to enter the desktop.
After logout or reboot, sign in with that account and password. Sign-in and
lock show the Claw wallpaper full-screen, with the time above and the
password field at the bottom. First-run setup keeps its frosted-glass panels.

The signed system shell starts with its built-in desktop permissions.
Other App permission requests still appear in the graphical OS review
window. The headless VM still sets up its `cos` account on the serial console.

On an installed machine, the greeter and desktop use the connected display's
preferred mode as reported by its graphics driver. The setup wizard fits that
screen and uses a compact layout on narrow displays. A VM uses the mode its
virtual display advertises; resizing the viewer window is hypervisor-specific.

With the matching signed Shell, selecting an installed App in the desktop
menu submits its launch without a separate review. Claw Apps keep their
signed identity and separate permission checks; ordinary Linux desktop apps
open as the logged-in user and receive no Claw App authority. Shutdown and
restart from the system menu go directly to the OS;
other Apps and the Agent receive no power permission from a menu click.

The session menu's Lock button uses the OS locker and the login account's
PAM authentication. The Shell can request a lock but neither sees input on
the lock screen nor offers an unlock action.
The session menu's Sleep button also requires the OS to confirm that native
lock before it sends sleep to logind. Return to the graphical session after
resume so it can present
the lock screen.

Use the [coordinated logout/update procedure](updating.md#root-managed-desktop-session-lifetime)
when replacing the compositor and display authority on an existing system.
