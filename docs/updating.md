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

`full-upgrade` is preferred over plain `upgrade` so APT can resolve package
dependency changes when Agent, Base, and Desktop advance on independent
release schedules:

| Package | Contents |
| --- | --- |
| `claw-os-agent` | Reusable Agent, `clawd`, browser/semantic runtimes, headless apps, skills, SDKs, and Agent services |
| `claw-os-base` | Claw OS recovery, managed agent home, and distribution boot/service policy |
| `claw-os-desktop` | Desktop shell, graphical Agent UI, and graphical applications, when installed |

Each published package version is generated from the Git commit count and SHA,
for example `0.1.0+git1226.g876f3ad810ca`. A later repository build therefore
sorts as a newer Debian package and is selected by APT.

On a running systemd target, `claw-os-agent` reloads systemd and runs
`try-restart` for `clawd.service` and the opt-in browser service during an
upgrade. Newly launched `cos` commands use the replaced binary immediately;
the running daemon is restarted automatically. `claw-os-base` separately
restarts the managed-home service on Claw OS systems. Rebooting or replacing
the system is not normally needed.

Check the installed and available versions with:

```bash
dpkg-query -W 'claw-os-*'
apt-cache policy claw-os-agent claw-os-base claw-os-desktop
sudo systemctl status clawd
```

After an LLM-provider update, its live connectivity can be retested without
rerunning the setup wizard:

```bash
cos agent setup text --verify-only
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

## Add the repository to Debian or Ubuntu

Add the signed repository without changing the host's distribution identity:

```bash
curl -fsSL https://xiaoyu-work.github.io/claw-os/claw-os-archive-keyring.gpg \
  | sudo tee /usr/share/keyrings/claw-os-archive-keyring.gpg >/dev/null

echo "deb [signed-by=/usr/share/keyrings/claw-os-archive-keyring.gpg] https://xiaoyu-work.github.io/claw-os trixie main" \
  | sudo tee /etc/apt/sources.list.d/claw-os.list

sudo apt update
sudo apt install claw-os-agent
```

Do not use `trusted=yes` or bypass signature verification. If the key,
`InRelease`, or package index cannot be downloaded or verified, stop and fix
the repository configuration instead of installing unsigned packages.

Claw OS image composition installs `claw-os-base` instead. Its runtime
dependency pulls in the same `claw-os-agent` package used by Ubuntu:

```bash
sudo apt install claw-os-base
```

## Image-specific guidance

APT is sufficient for changes delivered by the Claw OS Debian packages,
including fixes to `cos`, `clawd`, apps, skills, browser automation,
distribution integration, and systemd units.

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
can publish each package independently:

- **Publish Agent package** builds, installs, and publishes only
  `claw-os-agent`.
- **Publish Base package** builds and publishes only `claw-os-base`.
- **Publish Desktop package** builds and publishes only `claw-os-desktop`; it
  requires dedicated Linux runners labeled `claw-os-desktop-amd64` and
  `claw-os-desktop-arm64` with at least 50 GB free.
- **Release everything (test + Docker + WSL + APT)** invokes all distribution
  channels when a coordinated full release is wanted.

Each package workflow requires the repository Actions secrets
`CLAW_OS_APT_SIGNING_PRIVATE_KEY` and
`CLAW_OS_APT_SIGNING_PASSPHRASE`. The shared internal publisher restores the
other packages from the current signed repository, merges only the package
built by the caller, signs the new multi-architecture indexes, and deploys
GitHub Pages. Publications are serialized so independent package workflows
cannot overwrite each other.
