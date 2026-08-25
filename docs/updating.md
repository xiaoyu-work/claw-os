# Updating Claw OS

Claw OS has one signed APT update path across its installed system targets.
Most updates do not require reinstalling the operating system, re-importing a
WSL distribution, or replacing a VM. The package upgrade preserves users, home
directories, agent configuration, credentials, memory, and application data.

This update path applies to:

- WSL installations.
- Installed desktop, ISO-installer, and VM systems.
- Azure instances.
- Long-running Docker or other system containers that are maintained in place.

## Normal update

Run these commands inside the installed Claw OS system:

```bash
sudo apt update
sudo apt full-upgrade
```

`full-upgrade` is preferred over plain `upgrade` because the Claw OS packages
use matching version constraints and should move forward together:

| Package | Contents |
| --- | --- |
| `claw-os-base` | `cos`, `clawd`, helpers, bundled apps, skills, and SDK files |
| `claw-os-systemd` | `clawd` and other system/user service units |
| `claw-os-browser` | Browser automation binaries |
| `claw-os-desktop` | Desktop shell and graphical applications, when installed |

Each published package version is generated from the Git commit count and SHA,
for example `0.1.0+git1226.g876f3ad810ca`. A later repository build therefore
sorts as a newer Debian package and is selected by APT.

On a running systemd target, the `claw-os-systemd` package reloads systemd and
runs `try-restart clawd.service` during an upgrade. Newly launched `cos`
commands use the replaced binary immediately; the running `clawd` daemon is
restarted automatically. Rebooting or replacing the system is not normally
needed.

Check the installed and available versions with:

```bash
dpkg-query -W 'claw-os-*'
apt-cache policy claw-os-base claw-os-systemd claw-os-browser
sudo systemctl status clawd
```

After an LLM-provider update, its live connectivity can be retested without
rerunning the setup wizard:

```bash
cos agent setup llm --verify-only
```

## Check that the Claw OS repository is configured

Official installed images built with the `apt-source` feature contain:

```bash
cat /etc/apt/sources.list.d/claw-os.list
```

The file should reference the signed repository:

```text
deb [signed-by=/usr/share/keyrings/claw-os-archive-keyring.gpg] https://xiaoyu-work.github.io/claw-os trixie main
```

The keyring should also exist:

```bash
test -s /usr/share/keyrings/claw-os-archive-keyring.gpg
```

## Add the repository to an older installation

An image built before the `apt-source` feature was introduced, or a build made
without the repository signing key, may not contain the source file. Add it
without reinstalling the system:

```bash
curl -fsSL https://xiaoyu-work.github.io/claw-os/claw-os-archive-keyring.gpg \
  | sudo tee /usr/share/keyrings/claw-os-archive-keyring.gpg >/dev/null

echo "deb [signed-by=/usr/share/keyrings/claw-os-archive-keyring.gpg] https://xiaoyu-work.github.io/claw-os trixie main" \
  | sudo tee /etc/apt/sources.list.d/claw-os.list

sudo apt update
sudo apt install claw-os-base claw-os-systemd claw-os-browser
```

Do not use `trusted=yes` or bypass signature verification. If the key,
`InRelease`, or package index cannot be downloaded or verified, stop and fix
the repository configuration instead of installing unsigned packages.

## Image-specific guidance

APT is sufficient for changes delivered by the Claw OS Debian packages,
including fixes to `cos`, `clawd`, apps, skills, browser automation, and
systemd units.

Use a replacement image only when the release notes explicitly require it:

- **WSL:** install a new `.wsl` package for an incompatible base-rootfs or OOBE
  layout migration that cannot be handled by packages, or to recover a damaged
  distribution. Do not unregister a working distribution for a normal update.
- **Installed desktop, VM, or Azure:** use APT normally. Replace/reprovision the
  machine only for an explicitly documented image-level migration.
- **Docker:** APT can update a long-running persistent container, but rebuilding
  or recreating it from the latest tagged image is preferable when immutable,
  reproducible container deployment is the goal. Keep durable state in mounted
  volumes before replacing a container.
- **Live ISO:** a non-persistent live session cannot retain package upgrades;
  use a newly built ISO. A system installed from the ISO uses the normal APT
  path afterward.

## Maintainer publication step

Pushing a commit does not immediately make it available to existing
installations. The repository workflows are manually dispatched. A maintainer
must run one of:

- **Build APT repo (.deb packages)**, to build and publish only the APT channel.
- **Release everything (test + Docker + WSL + APT)**, to publish every channel.

APT publication requires the repository Actions secrets
`CLAW_OS_APT_SIGNING_PRIVATE_KEY` and
`CLAW_OS_APT_SIGNING_PASSPHRASE`. The workflow builds both amd64 and arm64
packages, signs the multi-architecture repository, and deploys it to GitHub
Pages. Existing installations can upgrade only after that deployment succeeds.
