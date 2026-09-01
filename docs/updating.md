# Updating Claw OS

Claw OS has one signed APT update path across its installed system targets.
Most updates do not require reinstalling the operating system, re-importing a
WSL distribution, or replacing a VM. The package upgrade preserves users, home
directories, agent configuration, credentials, disk-backed Agent memory, and
application data. Services may restart during the upgrade, so in-process state
is not preserved.

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
| `claw-os-agent` | Reusable Agent, `clawd`, `claw-agentd`, `claw-extension-host`, browser/semantic runtimes, headless apps, skills, SDKs, and Agent services |
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

`clawd`, `claw-agentd`, and `claw-extension-host` ship in the same package and
are replaced together. Package configuration creates the dedicated
`cos-extension` system group before restarting `clawd`; existing user
memberships are not changed.
The package now provisions locked accounts `cos-ext-00..63` at fixed UIDs
`61000..61063`. Fresh installs use `cos-extension` GID `60999`. Upgrades from
the prior dynamic sysusers definition stop `clawd` and retain the existing GID
instead of rewriting it, but only when it has no unrelated group members,
primary users, processes, subordinate-ID overlap, or group-owned files. The
retained GID is recorded in `/var/lib/cos/extension-group.gid` and revalidated
on every later upgrade. `preinst` checks every name, UID, and GID,
the existing `cos-extension` group, shadow locking, systemd-homed, and all
`/etc/subuid`/`/etc/subgid` ranges before making changes. A collision aborts
without modifying the unrelated record; a partial attempt is rolled back.
`postinst` revalidates everything and writes
`/var/lib/cos/extension-identities.reserved`, which `clawd` requires.
Each active slot also has a root-owned durable cleanup record under
`/var/lib/cos/extension-quarantine/`. It is removed only after the host cgroup
is empty and gone, private tmpfs mounts are unmounted, task-local state is
recursively deleted, and routed ACLs are revoked. After a crash or failed
cleanup, the slot remains unavailable across restart until recovery proves all
residue is gone; administrators should investigate repeated
`cleanup-failed` audit events rather than deleting these records manually.
The service runs with primary group `root`, so systemd's runtime/state/log
directories are `root:root`; `clawd` also pins and repairs those roots through
directory descriptors before use. The primary broker socket is separately
assigned `root:sudo` mode `0660`. The service delegates its cgroup-v2 subtree and uses
`KillMode=control-group`. Agent tasks now fail closed unless the CPU, memory,
and pids controllers plus a working `cgroup.kill` are available; ordinary
non-agent `clawd` primitives remain available. On supported systemd hosts no
manual migration is required.

On the first task after upgrading, `clawd` securely migrates any legacy
task-owned `/run/cos/extension-hosts/<uid>` directory to a root-owned,
non-writable parent. It pins the directory without following links and removes
stale contents with descriptor-relative unlink operations; it never applies
root ownership or mode changes to child pathnames. `/run` is ephemeral, so no
operator migration is normally needed. A symlinked or otherwise unverifiable
legacy parent fails agent execution closed and should be removed only after
the administrator inspects it.
Existing dynamic App/MCP tasks now receive task-local home/data/cache/log
directories rather than task-owner home access, plus private tmpfs instances
for `/tmp`, `/var/tmp`, `/dev/shm`, and `/run/lock`. The remaining filesystem
is read-only except for the task-local runtime tree. Bundled and
system-installed extensions continue to work; custom MCP commands and working
directories must be system-readable, and direct writes outside the task tree
must use brokered App/SDK operations instead.

On package purge, only users listed in the package ownership marker and still
matching the exact account policy are removed. Preexisting correct accounts,
an older unmarked `cos-extension` group, any identity changed after install, or
an identity that still owns a process, runtime directory, or quarantine record
is retained with a warning. No home is created, and the package does not
delete files merely because their numeric ownership matches a reserved UID.
`cos` ships beside them and speaks the same broker protocol version, so an
upgrade replaces the whole set. Agent tasks run in `claw-agentd` processes that
`clawd` supervises, so an upgrade behaves as follows:

- In-flight workers are killed when the daemon restarts (`PR_SET_PDEATHSIG`),
  and their tasks are reconciled — retried, or failed once they have used up
  their recovery budget — the next time `clawd` starts.
- A `cos` binary from before broker protocol v1 fails closed against a new
  `clawd`: the daemon answers one line naming the protocol and closes without
  parsing, authorizing or dispatching anything. There is no compatibility
  listener, so the fix is to finish the upgrade — reinstall `claw-os-agent` and
  re-run the command.
- A mismatched pair (an old `clawd` with a new `claw-agentd`, or the reverse)
  fails closed: both sides check the worker protocol version and report
  `agentd protocol mismatch`, naming reinstallation as the fix. No task runs
  against a half-upgraded pair.
- If the worker binary is missing or the daemon is started with
  `CLAWD_AGENTD=off`, agent tasks stop being executed and say so, while every
  other `clawd` primitive keeps serving.
- Agent tasks must be submitted by a non-root account. A root-owned task is
  refused with a message naming that requirement, because the runtime has no
  lesser account to drop to. On a single-account image, create an ordinary user
  and submit as that user.

Confirm both binaries and the running daemon after an upgrade:

```bash
dpkg-query -L claw-os-agent | grep -E '/(clawd|claw-agentd|claw-extension-host)$'
getent group cos-extension
sudo systemctl status clawd
cos agent service list --status pending
```

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

## Removing the Claw OS integration package

Removing `claw-os-base` also removes the service that presents the managed
OverlayFS home. Its `prerm` therefore preserves the currently visible merged
home before any package file is deleted. It makes the merged mount read-only,
copies that view with ownership, modes, timestamps, links, ACLs, and extended
attributes, unmounts it, and materializes the copy as the ordinary home.
Copying the merged view rather than the upper directory resolves whiteouts and
opaque directories, so files deleted while the overlay was active stay
deleted.

Use the normal package command:

```bash
sudo apt remove claw-os-base
```

Removal stops instead of exposing an older underlying home when the snapshot
cannot be made consistent, a nested filesystem or active process prevents a
clean unmount, stale recovery state exists, or the flattened copy cannot be
installed. The package and OverlayFS data remain installed, and the error
prints the exact managed home and retained snapshot path when one exists.

After resolving the reported busy mount or filesystem error, restore the
managed view and retry:

```bash
sudo systemctl start cos-home-setup.service
sudo apt remove claw-os-base
```

Do not delete `/var/lib/cos/overlay`,
`/var/lib/cos/overlay/removal-snapshot`, or
`/var/lib/cos/overlay/removal-underlay` after a failed removal. They are
recovery sources. If the service cannot restore the managed view, preserve
those paths and repair or copy the retained snapshot from a recovery
environment before retrying package removal.

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
installations. Publication workflows are manually dispatched. A maintainer can
publish each package independently:

- **Publish Agent package** builds, installs, and publishes only
  `claw-os-agent`.
- **Publish Base package** builds and publishes only `claw-os-base`.
- **Publish Desktop package** builds and publishes only `claw-os-desktop`; it
  requires dedicated Linux runners labeled `claw-os-desktop-amd64` and
  `claw-os-desktop-arm64` with at least 50 GB free.
- **Release everything (test + Docker + WSL + APT)** invokes all distribution
  channels when a coordinated full release is wanted.

Each package workflow requires the repository Actions secret
`CLAW_OS_APT_SIGNING_PRIVATE_KEY`. Set
`CLAW_OS_APT_SIGNING_PASSPHRASE` only when that private key is encrypted. The
shared internal publisher restores the other packages from the current signed
repository, merges only the package built by the caller, signs the new
multi-architecture indexes, and deploys GitHub Pages. Publications are
serialized so independent package workflows cannot overwrite each other.
