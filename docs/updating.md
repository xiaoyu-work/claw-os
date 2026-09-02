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

Updates only ever move forward. An older Claw OS release stays validly signed
forever, so the signature alone cannot tell a current release from a superseded
one; see [Downgrade protection](#downgrade-protection) for what stops one being
installed, activated or run.

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
primary users, processes, subordinate-ID overlap, group-owned files, or named
POSIX ACL entries. The ownership/ACL proof snapshots mountinfo and scans every
mounted filesystem independently, including nested, bind, tmpfs, persistent,
and network mounts. Stacked/duplicate mountpoints are ambiguous and rejected.
Each visible mount is opened and checked against the captured mount ID,
device, inode, mode, and ownership before and after descriptor-relative
scanning. Scans run in dedicated process groups with bounded TERM then SIGKILL
escalation and no-descendant verification. Package configuration aborts if
mountinfo is malformed or changes, a mount cannot be pinned/traversed,
`find`/real numeric `getfacl` fails or times out, or the candidate GID appears
as an access/default ACL qualifier. Kernel-generated
virtual filesystems are skipped only by the maintained allowlist because they
cannot retain discretionary ownership or POSIX ACL state across recreation.
The retained GID is recorded in `/var/lib/cos/extension-group.gid` and revalidated
on every later upgrade. Upgrade `preinst` stops `clawd`; provisioning moves to
`postinst`, after the unpacked root-owned helper and ordinary `acl`,
`findutils`, `coreutils`, and Python dependencies are guaranteed available.
It checks every name, UID, and GID, the existing `cos-extension` group, shadow
locking, systemd-homed, and all `/etc/subuid`/`/etc/subgid` ranges before
making changes. A collision aborts without modifying the unrelated record; a
partial attempt is rolled back. `postinst` then writes
`/var/lib/cos/extension-identities.reserved`, which `clawd` requires.
Subordinate-GID checks cover both the fixed UID pool and the exact retained
GID as separate intervals, including legacy GIDs `61064..61183`.
The package depends on `acl` and `findutils`; these tools are mandatory rather
than optional fallbacks.
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

Dynamic App and stdio MCP children now see an empty allowlisted filesystem and
private procfs. Custom extension code/config outside `/usr` is copied into a
bounded read-only task snapshot; symlinks, mount crossings, special files,
group/world-writable trees, oversized snapshots, and undeclared host paths are
rejected. Extensions that previously read arbitrary owner-home, `/var`, or
mounted paths must use explicit SDK/broker operations instead.

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
- Broker envelope v1 and v2 clients also fail closed when paired with the
  other version. Protocol v2 adds the durable Web task-control fields, so
  `cos`, `clawd`, and `cos-agent-bridge` must come from the same package set.
- A mismatched pair (an old `clawd` with a new `claw-agentd`, or the reverse)
  fails closed: both sides check the worker protocol version and report
  `agentd protocol mismatch`, naming reinstallation as the fix. No task runs
  against a half-upgraded pair. The broker additionally measures the installed
  worker and `claw-extension-host` binaries against the security floor before
  spawning them, so either component being replaced on disk is refused before
  it becomes a process. Extension-host control protocol v7 binds
  owner-qualified package verification receipts into the private bootstrap and
  has its own monotonic floor alongside the worker and broker protocols.
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

### App data moves into per-App directories

From the release that isolates App workers, an App no longer receives the
owner's data root. `COS_DATA_DIR` is its own directory,
`<data-root>/apps/<app-id>`, created `0700`, and no other App's directory or
owner-private store is inside its sandbox.

The state bundled Apps wrote before that — `calendar/`, `db/`, `kv.json`,
`launcher/`, `logs/`, `notifications.json`, `trash/`, the `exec` App's captured
`proc/stdout.*` and `proc/stderr.*`, and each gateway's
`apps/gateway-*/state.json` — is moved into the new directory automatically,
once, the first time that App runs. The move is a rename on the same
filesystem, so nothing is copied and nothing is duplicated. No action is needed
and no output is printed when it succeeds.

It stops and reports instead of guessing when:

- the App already has a file of the same name in its new directory *and* the
  old one is still in the data root — keep whichever is current and delete the
  other, then run the App again;
- the old path is a symlink, a hardlinked file, or a socket, FIFO or device
  node — move it across by hand;
- the old path is owned by another user, or sits on a different filesystem from
  the data root — move it across by hand.

In every one of those cases nothing has been changed and the App's existing
state is still where it was. An interrupted move is finished by the next run.

To see where an App's data now lives:

```bash
ls ~/.local/share/cos/apps/
```

Kernel-owned state — sessions, the capability registry at `proc/registry.json`,
the agent's memory, approvals and the journal — does not move and is never
inside a sandbox. The `exec` App wrote a file of the same name in the same
directory; the per-App directory is what finally separates the two, and the
App's own process registry starts empty there. Apps still write to the shared
agent memory through `cos_runtime.memory`, which the launcher carries out on
their behalf.

## Extension packages across an update

Apps, Skills and MCP/adapter packages are authenticated before use, and
an update does not change that:

* Vendor content under `/usr/lib/cos` is re-verified against its pinned
  content digest on first use after the upgrade. Because only root can
  write there, a changed digest is a package upgrade: the pin rotates
  and a `provenance.vendor_pin_rotated` record is written to
  `/var/log/cos/provenance.jsonl`. A path that stops being root-owned or
  becomes group/world-writable fails closed instead.
* Publisher trust roots (`/usr/lib/cos/trust/publishers.d`,
  `/etc/cos/trust/publishers.d`) survive the upgrade. `/etc/cos` is
  operator-owned configuration and is not overwritten.
* User-installed extensions that carry no signature **fail closed** into
  a quarantined state after upgrading. They are listed by
  `cos app` (and in the skill/MCP discovery diagnostics) with an
  actionable reason. Re-install a signed package, or record an explicit
  developer decision with `cos provenance dev-trust`. Nothing is
  silently grandfathered.
* App **data** lives under `<data_dir>/apps/<id>`, separate from the
  code artifact, so quarantine, re-install and rollback never touch it.
* Verified artifacts are retained under
  `<data_dir>/provenance/artifacts/`, so
  `cos provenance rollback --kind app --id <id> --digest sha256:… --dest <dir>`
  can re-activate a previous version. The artifact is verified again
  before activation, so a rollback can only land on content that passed
  verification and has not been revoked.

Revoking a compromised publisher key or a single artifact digest takes
effect immediately for later launches, disclosures and attachments:

```bash
cos provenance trust revoke --key-id sha256:…
cos provenance trust revoke --digest sha256:…
```

## Downgrade protection

A repository signature answers *who published this*. It cannot answer *is this
the current release*: an artifact that was validly signed two years ago is
still validly signed today. A stale mirror, a preserved repository snapshot, or
a plain `apt install claw-os-agent=<old-version>` would otherwise reinstate a
`clawd` whose vulnerabilities have already been fixed.

Claw OS therefore keeps a **security floor**: a root-owned, monotonic record of
the highest security epoch, package version and component content this machine
has ever accepted.

```bash
sudo /usr/lib/cos/bin/claw-security-floor show
```

### What each release carries

Every Claw OS package ships a canonical, detached-signed release manifest at
`/usr/lib/cos/release-security/<package>/manifest.json` — each package owns its
own subdirectory, so no two packages ever write the same file and a maintainer
script always reads its own release. It binds one release to its
security epoch, its exact Debian version and architecture, the SHA-256 of every
security component it installs, the protocol epochs those binaries speak, the
lowest mutually compatible version of each sibling package, the repository suite
it is published into, and an expiry. It is signed by the same publisher identity
that signs the APT repository, so a machine that can verify its package index
can verify its release manifest offline, without fetching anything.

The **security epoch** is also the package's Debian epoch. That is deliberate:
an epoch that only Claw OS could see would not change which candidate APT
selects, so an emergency release whose upstream version sorts *lower* than the
installed one would simply never be offered. Publishing `1:0.2.0+git…` makes
APT's own ordering prefer any higher security epoch, and the floor then refuses
to go back down. The package build, the embedded manifest and the publication
job each reject a release whose Debian epoch and security epoch disagree.

An emergency release therefore looks like this: raise `security_epoch` in
`packaging/release-security/policy.json`, and every artifact built from that
policy is versioned `<epoch>:<upstream>`. Its upstream version may be lower
than what is installed; the Debian epoch is what makes APT choose it anyway.

### Two views of the floor

The authoritative state under `/var/lib/cos/security` is `0700 root:root`,
because it holds the generation history and the one-use recovery
authorizations. Unprivileged Claw OS processes never read it.

Each commit publishes a minimal read-only **runtime projection** instead:

```text
/var/lib/cos-security/            0755 root:root
└── runtime-floor.json            0644 root:root
```

It carries the security epoch, ABI, protocol epochs, per-package versions and
component digests, plus the generation and digest of the authoritative floor it
came from — and no recovery, history or trust material. `cos`, `claw-agentd`,
the approval helper and the App runner enforce against it unprivileged;
`clawd`, which is root, reads the authority and republishes the projection when
the two disagree.

The projection is written only after the authoritative commit succeeds. If it
cannot be written, the package configuration fails with an explicit
*indeterminate* error rather than reporting success: the floor has moved
forward, the machine's runtime view has not. Re-running the configuration, or
`claw-security-floor project`, repairs it.

Once `/var/lib/cos-security` exists, a missing, corrupt, wrongly owned,
wrongly moded, symlinked or hardlinked projection fails closed for every
unprivileged Claw OS binary.

### Where it is enforced

| Gate | Refuses |
| --- | --- |
| APT pre-install hook | A whole candidate set that contains a superseded Claw OS package, before dpkg unpacks anything. Registered as a single executable token so APT sends it the version 2 protocol; the test suite proves APT aborts the transaction when it refuses |
| `prerm upgrade <new-version>` of the **installed** package | An older incoming version — the one gate a downgrade candidate cannot supply itself |
| `preinst` of the candidate | A lower epoch, a lower version, a substituted artifact, an expired or untrusted manifest |
| `postinst` | Advancing the floor at all, unless the unpacked files match the signed manifest; and reporting success if the runtime projection could not be published |
| `clawd` | Starting when this build is behind the authoritative floor, when a security component on disk no longer matches it, or when the runtime projection cannot be made to agree with it |
| `cos`, `claw-agentd`, the approval helper, the App runner | Running when the runtime projection is missing, insecure, corrupt or ahead of this build |

The daemon measures the `claw-agentd` binary before spawning it, so a replaced
worker is refused before it becomes a process rather than being asked to vouch
for itself. Package dependencies additionally pin an ABI generation
(`claw-os-agent` provides `claw-os-abi-<N>`, Base and Desktop depend on it), so
APT's own solver will not schedule an incompatible combination, and a broker
restart is deferred until the whole transaction has been configured.

Removing or purging a Claw OS package deliberately **does not** delete
`/var/lib/cos/security` or `/var/lib/cos-security`. That is what makes
`apt remove claw-os-agent` followed by an install of an old version still fail.

### What it does not protect against

The floor is software state on the machine's own filesystem. Local root, or
anyone who can replace the complete filesystem and its state together, can
rewrite the floor along with the binaries. Detecting that needs a TPM
measurement or a remote attestation anchor, and Claw OS has neither: **this is
not hardware anti-rollback**. What it does defeat is an unprivileged local
attacker, a stale or hostile mirror, a preserved old repository snapshot, an
accidental or scripted `apt install <pkg>=<old>`, and a component binary swapped
behind the package manager's back.

Restoring `/var/lib/cos/security/floor.json` on its own is detected: it and
`history.jsonl` chain to each other, so a single-file rollback leaves the state
behind its own recorded generation and everything fails closed. Restoring both
files together — a whole-state restore performed by root — is not detectable
from inside that state.

### Recovering from a bad release

There is no environment variable, APT option, package parameter or editable
configuration file that turns the floor off. Removing
`/etc/apt/apt.conf.d/50claw-os-security-floor` only removes the earliest of the
gates; the maintainer scripts and the binaries still enforce the floor.

When a release genuinely has to be rolled back, record a single authorization,
as root, at a terminal:

```bash
sudo /usr/lib/cos/bin/claw-security-floor recover authorize \
  --package claw-os-agent \
  --version 0.2.0+git1200.gabcdef123456 \
  --epoch 1 \
  --manifest-sha256 <sha256 of that release's manifest.json> \
  --reason "regression in the newer release" \
  --expires-in 2
```

The command prompts for an exact confirmation phrase and refuses to run without
a controlling terminal, without root, or while an Agent, App or MCP session is
active — so a model, a tool call or a script cannot reach it. The resulting
authorization names exactly one package, epoch, version and artifact, is bound
to the current floor generation, expires, is consumed atomically on first use,
and is journaled to `/var/log/cos/security-floor.jsonl` and to the system
journal.

```bash
sudo /usr/lib/cos/bin/claw-security-floor recover list
sudo /usr/lib/cos/bin/claw-security-floor recover revoke --id <id>
sudo apt install claw-os-agent=0.2.0+git1200.gabcdef123456
```

Be clear-eyed about what this is: local root can ultimately bypass any
software-only control on its own machine. The purpose of this path is that a
downgrade is never *accidental*, is always attributable, and is always
recorded.

### Diagnosing a refusal

```bash
sudo /usr/lib/cos/bin/claw-security-floor show
sudo /usr/lib/cos/bin/claw-security-floor verify-installed
/usr/lib/cos/bin/claw-security-floor runtime-check
sudo tail /var/log/cos/security-floor.jsonl
journalctl -t claw-security-floor
```

`runtime-check` reads only the unprivileged runtime projection: it is exactly
what `cos` and `claw-agentd` decide at startup, and it never repairs anything.
`verify-installed` is the privileged pass — it re-measures the components and
republishes the projection when it has drifted.

Common outcomes:

- `version_regression` / `security_epoch_regression` — the candidate is older
  than what this machine has accepted. Refresh the package index from a current
  mirror, or use the recovery workflow above.
- `manifest_expired` — the release manifest's validity window has passed. The
  mirror is stale; `sudo apt update` against the official repository.
- `manifest_unsigned` / `manifest_untrusted` — the candidate carries no release
  manifest, or one signed by a key this system does not trust. Reinstall from
  the signed repository, or import the publisher keyring into
  `/etc/cos/trust/release.d/`.
- `artifact_mismatch` — the version matches what is recorded but the content
  does not. Either a substituted package, or a security component replaced on
  disk; reinstall `claw-os-agent`.
- `security floor rollback detected` — the floor state and its history
  disagree. Nothing is trusted until it is resolved; see the recovery section.
- `runtime security floor is unreadable` / `is insecure` — the unprivileged
  projection under `/var/lib/cos-security` is missing, tampered with or has the
  wrong ownership or mode. Run `sudo /usr/lib/cos/bin/claw-security-floor
  project` to republish it from the authority.
- `the update is INDETERMINATE` — the floor was committed but its runtime view
  could not be written. Nothing has been weakened; re-run the package
  configuration (`sudo dpkg --configure -a`) or `claw-security-floor project`.

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
deb [signed-by=/usr/share/keyrings/claw-os-archive-keyring.gpg check-valid-until=yes allow-insecure=no allow-weak=no allow-downgrade-to-insecure=no by-hash=yes] https://xiaoyu-work.github.io/claw-os trixie main
```

`check-valid-until=yes` makes APT refuse an index whose signed `Valid-Until` has
passed, so a frozen mirror cannot keep offering a superseded release; the
`allow-*` options refuse the insecure fallbacks that would otherwise accept it.
The same defaults are asserted in `/etc/apt/apt.conf.d/50claw-os-repository`.

That bound only helps if the metadata is renewed before it expires. A
scheduled workflow, `.github/workflows/refresh-apt-metadata.yml`, re-signs
`Release`/`InRelease` weekly — well inside the 30-day window — without
rebuilding or changing any package bytes. It preserves and re-verifies the
release-security baseline, and fails loudly rather than publishing unsigned
metadata if the signing key is unavailable. A repository that publishes
packages rarely therefore does not silently expire and cut installed systems
off from security updates.

The publisher applies the same scepticism in the other direction. Before it
believes what the current `InRelease` says about the published state, it
verifies the signature, refuses an index dated in the future, refuses one whose
`Valid-Until` has passed, refuses one older than its freshness policy, and
requests everything with cache-busting, no-cache headers. Each preserved
release-security file is then cross-checked against the SHA-256 recorded for it
in that signed index, so an origin cannot pair a current index with an older,
separately signed manifest.

The residual risk is worth stating plainly: an attacker who controls the entire
origin can serve a *consistent* older snapshot that is still inside its
`Valid-Until` window, and no client-side check can distinguish that from the
truth. The defences against it are the short `Valid-Until`, the scheduled
refresh that keeps it short, and the per-machine security floor — not the
publisher.

The keyring should also exist:

```bash
test -s /usr/share/keyrings/claw-os-archive-keyring.gpg
```

## Add the repository to Debian or Ubuntu

Add the signed repository without changing the host's distribution identity:

```bash
curl -fsSL https://xiaoyu-work.github.io/claw-os/claw-os-archive-keyring.gpg \
  | sudo tee /usr/share/keyrings/claw-os-archive-keyring.gpg >/dev/null

echo "deb [signed-by=/usr/share/keyrings/claw-os-archive-keyring.gpg check-valid-until=yes allow-insecure=no allow-weak=no allow-downgrade-to-insecure=no by-hash=yes] https://xiaoyu-work.github.io/claw-os trixie main" \
  | sudo tee /etc/apt/sources.list.d/claw-os.list

sudo apt update
sudo apt install claw-os-agent
```

Do not use `trusted=yes`, `--allow-downgrades`, `--allow-insecure-repositories`
or `-o Acquire::Check-Valid-Until=false`. If the key, `InRelease`, or package
index cannot be downloaded or verified, stop and fix the repository
configuration instead of installing unsigned or stale packages.

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

### Downgrade protection across image replacement

A newly built image already carries a security floor: the packages are
configured during composition, so the image ships with the floor its own
release seeded, and the first boot validates it. An in-place APT update
preserves that floor across package removal and reinstall, because the state
lives outside any package.

Replacing an image is a different operation. A WSL distribution or a Docker
container is replaced wholesale, including `/var/lib/cos/security`, so
importing an *older* `.wsl` package or running an older container image
installs that release's floor with it. Nothing inside the replaced filesystem
can prevent that, and Claw OS does not claim otherwise: verify the artifact you
are importing, and prefer the APT path for updating a running system. Extension
provenance is unaffected — user-installed Apps, Skills and MCP packages keep
their own signature and revocation checks, and a `cos provenance rollback` still
re-verifies the artifact it activates.

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
