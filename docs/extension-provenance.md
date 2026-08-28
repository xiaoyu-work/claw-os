# Extension Provenance

Apps, Skills and MCP/adapter packages are third-party code and
third-party text. Before any of it is trusted — before a manifest can
influence a capability grant, before an executable is launched in the
sandbox, before skill instructions reach the model, before a tool
schema is registered — the package is authenticated against a
publisher key and its **complete** file tree is verified.

This document describes the format, the trust model, the install
pipeline and the operator/developer workflows.

## The envelope: `claw.provenance/v1`

A package is a directory. Its envelope lives at `.provenance.json`
inside that directory:

```json
{
  "schema": "claw.provenance/v1",
  "package": {
    "kind": "app",
    "id": "notes",
    "version": "1.2.0",
    "manifest_schema": "cos.app-manifest/v1",
    "manifest_path": "app.json",
    "entrypoints": ["main.py"],
    "resources": [],
    "files": [
      {"path": "app.json", "type": "file", "mode": 420, "size": 812,
       "digest": "sha256:…"},
      {"path": "lib",      "type": "dir",  "mode": 493, "size": 0,
       "digest": ""},
      {"path": "lib/util.py", "type": "file", "mode": 420, "size": 91,
       "digest": "sha256:…"},
      {"path": "main.py",  "type": "file", "mode": 493, "size": 1204,
       "digest": "sha256:…"}
    ],
    "content_digest": "sha256:…"
  },
  "signature": {
    "algorithm": "ed25519",
    "key_id": "sha256:…",
    "public_key": "<64 hex>",
    "value": "<128 hex>"
  }
}
```

What the signature binds:

| Field | Why it is signed |
| --- | --- |
| `kind` | An App package can never be presented as a Skill to reach a different ceiling. |
| `id`, `version` | Package identity is the publisher's claim, not the directory name's. |
| `manifest_schema`, `manifest_path` | The manifest cannot be reinterpreted under another schema or read from another file. |
| `entrypoints`, `resources` | Only signed files may be executed or disclosed. |
| `files` | Path, node type, permission bits, size and SHA-256 of every node. |
| `content_digest` | One stable handle for revocation, retention and rollback. |
| `algorithm`, `key_id`, `public_key` | Algorithm/key substitution changes the signed message. |

### Canonical signing bytes

The message is a length-prefixed, domain-separated encoding: every
field is written as `u32le(key.len()) || key || u64le(value.len()) ||
value`, preceded by the domain separator
`claw-provenance/v1\0package-envelope\0`. The encoding is injective —
`id="ab", version="c"` and `id="a", version="bc"` produce different
bytes — so no value can be shifted across a field boundary.

The file tree is sorted by path and the sort order is enforced at parse
time, so a tree has exactly one valid encoding.

### What parsing rejects

Unknown top-level or nested fields, unknown schema strings, unknown
signature algorithms (including different casing), unknown node types,
key ids that do not equal `sha256(public_key)`, unsorted or duplicate
paths, case-colliding paths, absolute paths, `..`, backslashes,
control characters, trailing dot/space names, group/world-writable
modes, directories with non-zero size, and any envelope over 8 MiB.

## Trust roots

| Tier | Root | Required owner |
| --- | --- | --- |
| `vendor` | `/usr/lib/cos/trust/publishers.d` | `root` |
| `system` | `/etc/cos/trust/publishers.d` | `root` |
| `user` | `~/.config/cos/trust/publishers.d` | the owner |
| `developer` | `~/.config/cos/trust/developer.d` | the owner |

A root contributes nothing unless it *and every ancestor up to `/`* is
a non-symlink directory owned by the required uid and free of group and
world write bits. A shared ancestor is accepted only when it carries
the sticky bit. A rejected root is reported as a diagnostic
(`cos provenance trust list`), never skipped silently.

The per-user roots resolve from the **passwd home of the effective
uid**, not from `HOME` or `COS_USER_CONFIG_DIR`. There is deliberately
no environment variable that appends a trust root, relaxes the
ownership checks or disables verification, and no model-reachable
surface that can add one.

### Key ids, usage constraints, rotation and revocation

A key id is `sha256:<hex>` over the raw 32-byte Ed25519 verifying key,
so ids are collision-resistant bindings to key material rather than
operator-chosen aliases. An entry whose declared id does not match its
key is refused, and a package must present both the trusted id *and*
the matching key.

Every key declares:

- `usages` — must contain `package-signing`. A key minted for release
  artifacts or TLS cannot authorise extension code by accident.
- `kinds` — which of `app` / `skill` / `mcp` it may sign.
- `status` — `active` or `revoked`.
- `not_before` / `not_after` — optional validity window.

Both bounds are parsed as **strict RFC 3339** and normalised to UTC
before comparison, so `2026-01-01T00:00:00+01:00` and
`2025-12-31T23:00:00Z` are the same instant. There is no lexicographic
comparison anywhere. A value that is malformed, uses a space instead of
`T`, omits an offset, names an impossible date, falls outside
2000–2200, or describes a window that can never be satisfied
(`not_before >= not_after`) **rejects the whole trust entry**. A key
whose expiry cannot be understood must not authorise anything.

Revocation covers both keys (`status: "revoked"` or `revoked_keys`) and
individual artifacts (`revoked_packages`, by content digest). Any
change moves the store's *generation* digest, which invalidates every
cached verification, so a revocation takes effect for the next launch,
disclosure or attach without restarting.

`cos provenance trust revoke` writes to the caller's own trust root —
the uid comes from `geteuid`, the directory from that uid's verified
passwd home — so one user can never revoke, or restore, another user's
trust. System-wide roots under `/etc/cos` and `/usr/lib/cos` are
root-owned and managed by the package manager.

### What happens to something already running

An App session or MCP server that was verified at launch may run for
hours. Two mechanisms cover it, and they give different guarantees:

| | Guarantee | Where |
| --- | --- | --- |
| **On use** | The *next* authority call fails. Not "soon" — the call in front of it. | `provenance::runtime::assert_live` on the capability path, the worker broker endpoint, `app_session.relay`, and every MCP/App-session tool call |
| **When idle** | Bounded by the tick of whichever supervision loop owns that view. | `provenance::runtime::lifecycle_tick`, called from the `clawd` authority sweep and the `agentd` reconcile pass |

Neither waits for a grant to expire and neither needs a daemon restart.
The mechanism is the durable generation, not a notification: a daemon
re-stats it before each decision and rebuilds the store when it moved,
so a revocation written by a different process is visible immediately
and no message can be lost. `cos provenance trust revoke` additionally
runs a lifecycle pass in its own process, so instances it can see are
stopped before the command returns.

Every launch records an *instance* — the package digest and publisher
key id it came from, plus the owner uid, pid, `/proc` start-time ticks
and cgroup of the child once it is bound.

The record is addressed by an **explicit owner uid**, never by whatever
path view the current process happens to be in, so a direct `cos` run,
the `agentd` supervisor and `clawd` acting for that owner all name the
same file: `/run/cos/caps/<uid>/provenance-running.json` when the
daemon's routed partition exists (root-owned, so a session cannot
rewrite its own provenance), and an owner-qualified file in the owner's
own data directory when it does not. Reads and writes both take a
`flock` on a side file, and every mutation is a read-modify-write
*inside* that lock, so two processes registering different instances
cannot lose one another's update. Pure reads never rewrite the file.

Reading it validates it: a symlink, a hard link, a wrong owner, a
group- or world-writable mode, or unparseable content is an error, not
an empty record. For a session the caller has already established is
package-backed — a relay grant names one, a session row carries an
`app_id` — a missing or unreadable record is a **denial**. The record
is the only thing that could say the package behind that session is
still trusted, and failing to read it is not evidence that it is.

Stopping one means signalling the whole **process group**: an App
worker and an MCP server each `setsid` at spawn, so a pid-only kill
would leave descendants running. Before any signal the recorded
identity is re-read from `/proc` and must still match on uid, start
time and cgroup — a pid that was recycled after the instance exited
names a different process and is never signalled. `SIGTERM`, a bounded
grace, then `SIGKILL`; then the runtime record and the session's
capability grants are cleared together, so no live grant outlives the
process it was issued to.

A revoked MCP server is not merely refused: the transport is closed and
its process group is stopped, because revoked code holding an open
stdio channel to the agent is the thing being removed, not just its
next answer.

Instances are classified, and one class is deliberately outside all of
this. An MCP server the machine owner wrote into `config.json` is
recorded as `mcp-operator-config`: it has no envelope, no publisher and
nothing a revocation could name. It is still sandboxed by the same
worker policy and still recorded, so it is a visible category rather
than a gap — but it is governed by the owner having written it, not by
package provenance. Because the kind is explicit, a missing record for
a *package* instance is never mistaken for one of these.

## App session servers

An App with a `session` block runs as a stdio MCP server, and that
launch is bound to one snapshot like any other:

1. `AppLaunch` is built from the verified package, so the manifest, the
   runtime selection, the session block and the capability ceiling all
   come from one parse of one set of signed bytes.
2. The session entry — the explicit `session.entry` or the runtime
   default — must be a **declared signed entrypoint**. Being present in
   the package and covered by the signed file tree is not enough; a
   signed package must not become a launcher for anything shipped
   alongside it.
3. `bind_for_launch` opens the manifest and the entry by descriptor and
   holds them. The binding is kept for the whole life of the session,
   not dropped after `spawn`.
4. The pinned inodes are re-asserted immediately before `spawn`, on
   every reuse of a cached session, and on every tool call. A warm
   cache is exactly where a replaced script would otherwise go
   unnoticed, so "it is already in the table" is never sufficient.

The same `(dev, ino)` identities a sandbox policy would bind through
`package_identity` and `pinned_entries` are recorded in the launch
audit, but on this path they are enforced by the held descriptors and
the re-assertions rather than by a mount.

**This path is not sandboxed.** The App-session stdio child is spawned
directly rather than through `worker::prepare`, so it has no mount
namespace, no egress policy and no seccomp filter. That is a
pre-existing property of the App-session code path — the binding
described above adds provenance pinning to it and changes nothing about
isolation. It is not a deliberate trade-off either: `worker::derive`
already supports `StdioPlan::Streamed`, which the MCP/adapter attach
path uses for the same shape of child. Migrating the App-session launch
onto the worker sandbox is tracked separately.

**Honest limit.** None of this defends against root on the same
machine, which can rewrite the trust files, the generation state and
the instance records alike. What it buys is that a revocation cannot be
undone by restoring a single file, and that a long-lived daemon cannot
keep honouring trust that was withdrawn while it was running.

## Trust sources for a package

1. **Publisher signature** — a valid envelope signed by a trusted,
   non-revoked key. This is the only route for user-installed content.
2. **Vendor (package-manager) trust** — the package sits under an
   approved root (`/usr/lib/cos`, `/usr/share/cos`, `/usr/lib/claw`,
   `/usr/share/claw`) where every path component is root-owned,
   non-symlink and not group/world-writable. The tree digest is still
   computed and pinned in `<data_dir>/provenance/vendor-pins.json`; a
   change rotates the pin and writes a `provenance.vendor_pin_rotated`
   audit record, so post-install tampering is surfaced before use. A
   development checkout is **never** promoted to this tier — the roots
   are fixed absolute paths, and an environment override that points a
   root elsewhere gets nothing.
3. **Developer trust** — an explicit, persisted grant for one unsigned
   tree at one content digest (see below).

## Install pipeline

`cos app install`, `cos agent skills install` and adapter installation
all follow the same shape:

1. Read the bundle as untrusted bytes. Archive extraction is bounded on
   total uncompressed size, entry count, per-entry size, path depth and
   compression ratio.
2. Re-scan the extracted/copied tree and reject absolute paths, `..`,
   alternate separators, duplicate and case-colliding names, symlinks,
   hard links, device/FIFO/socket nodes and group/world-writable modes.
3. Copy into a private staging directory (mode `0700`) on the
   destination filesystem, then `fsync` files and directories.
4. Verify the envelope signature and **every** file digest on the
   staged copy. A staging directory is private scratch space, so
   neither vendor nor developer trust applies there — only a publisher
   signature counts.
5. Retain the verified artifact in the content-addressed store at
   `<data_dir>/provenance/artifacts/<kind>/<id>/<digest>/`.
6. Atomically rename the staged tree into the live location and
   `fsync` the parent. A live install is never merged into: activation
   is always a whole-directory rename, so discovery can never observe a
   half-written package.

An aborted install removes its staging directory; nothing appears at
the live path.

## Verify-then-use

Verification returns a snapshot that owns the package's directory
descriptor. Every later read goes through `openat(…, O_NOFOLLOW)` on
that descriptor and re-checks the digest.

### One snapshot for the whole launch

`bridge::AppLaunch` parses the manifest **once**, out of the verified
snapshot. The App id, the capability `needs`, the runtime selection, the
entry file and the stdin contract all come from that one parse. Nothing
in the launch path re-reads `app.json` or re-resolves the package by
path, so the bytes that decided the capability grant and the bytes that
execute cannot diverge.

### Execution is bound to inodes, not paths

Before the sandbox is prepared, `AppLaunch::bind` re-hashes the manifest
and every entrypoint from the pinned directory descriptor and **keeps
those descriptors open** until the child has been spawned. The resulting
`(st_dev, st_ino)` identities travel into the worker policy, and the
Linux provider refuses to bind a mount source whose inode differs from
the one that was verified. Concretely:

* the package directory is bound by inode;
* each signed entrypoint is bound over it, also by inode;
* replacing `main.py` after verification is therefore either **detected**
  (identity mismatch → launch refused) or **irrelevant** (the sandbox
  still sees the verified inode).

The same applies on a cache hit: `assert_current` re-checks the
directory identity and the trust generation, and `bind` re-hashes, so a
cached snapshot cannot outlive the bytes it describes.

### The rest of the surface

- The Skill body and every disclosed child resource are read from the
  snapshot at disclosure time. A resource swapped after the catalog was
  built fails the disclosure instead of injecting new model text.
- The MCP manifest, its command and any package-relative script or env
  path must be **declared entrypoints**, are bound by inode before the
  spawn, and are never re-resolved by path afterwards.

An MCP package may run either its own signed, declared entrypoint or a
distribution-installed interpreter under `/usr`, `/bin`, `/sbin`,
`/lib`, `/lib64` or `/opt` that is root-owned and not group/world
writable. A writable interpreter earlier on `PATH`, or a script the
package never declared, is refused.

## The capability ceiling

A trust tier is an upper bound on authority, applied wherever
capabilities are derived — not a label.

| Tier | Ceiling |
| --- | --- |
| `vendor`, `system`, `user` | The manifest-declared surface, as before |
| `developer` | The closed allow-list below, and nothing else |

Unsigned developer content may hold only `fs.read`, `fs.write`,
`fs.meta`, `data.kv.read`, `data.kv.write`, `data.log.read`,
`data.log.write`, `memory.read`, `memory.write` and `ui.notify`. Every
other verb in the catalog — all of `sys.*`, `secret.*`, `net.*`,
`proc.*`, `device.*`, `clipboard.*`, `browser.*`, `desktop.*`,
`agent.*`, plus `fs.exec`, `fs.delete`, `ui.input`, `ui.window`,
`ui.prompt`, `ui.accessibility` and `data.backup` — is denied. The
allow-list is closed, so a verb added to the catalog tomorrow is denied
to developer content until somebody deliberately adds it; a test asserts
exactly that against the whole catalog.

Developer content is additionally refused:

* any wildcard scope over a resource namespace, even on an allowed verb,
  and any manifest `wild` scope binding for such a verb — an unsigned
  package never borrows the launching session's reach;
* every broker audience except `AppLaunch` — no `AppRelay`, no
  `SystemService`, no `Credential`, no scheduler, journal, permission or
  identity route;
* a relay grant of any kind, so the launcher's relay slot stays empty
  and every relayed privileged route is refused rather than left
  addressable through a dead handle;
* any host path mount derived from a capability — the sandbox sees the
  package read-only and the App's own data partition, nothing else;
* `agent.invoke` on any identity but its own;
* an MCP attach altogether — a running server holding a live broker
  endpoint is a standing attack surface even with no capabilities.

### Where the ceiling is enforced

`clawd` is the enforcement point, not the launcher.
`clawd::app_sessions` resolves the ceiling from **its own** verified
package on every `app_session.register`, `bind` and `set_transient`, and
clamps the fully resolved capability plan immediately before
`authorize_plan`, before the session row is written and before any grant
is minted. The clamp runs *ahead of* the approvals store, so a package
outside its tier can never consume a user approval for a capability it
could not hold anyway.

`app_session.register` returns the set `clawd` actually granted. The
launcher adopts that answer for its sandbox policy and refuses to launch
if it is wider than the ceiling it computed independently, so the
isolation shape and the live grant always describe the same world. The
launcher applies the same clamp locally as defence in depth, but it does
so silently: the `provenance.ceiling_applied` audit record is written by
`clawd`, where authority is actually withheld, rather than by
unprivileged code claiming a restriction it did not enforce.

Whatever the ceiling removes is written to the provenance audit log, so
a restricted package is visibly restricted rather than mysteriously
broken.

Provenance authenticates *publisher and content*, not semantics: MCP
tool names, descriptions and results remain untrusted model input even
when the package is signed.

## Quarantine

A structurally valid extension whose provenance fails is **quarantined**
rather than dropped:

- `cos app` lists it with `"trust": "quarantined"`, `"runnable": false`
  and the reason.
- Capability derivation, AI policy resolution, session binding and
  command dispatch all refuse it.
- Skills and MCP packages appear in the loader/discovery diagnostics
  with the same reason.

## Workflows

### Publisher

```bash
# One-time: create a signing key. Never commit this file.
cos provenance keygen --out ~/.secrets/claw-release.json --comment "release 2026"

# Publish the printed `trust_entry` object into a trust root:
#   packaging/deb/claw-os-agent/trust/publishers.d/<name>.json  (vendor)
#   /etc/cos/trust/publishers.d/<name>.json                     (operator)
#   cos provenance trust add --file <entry.json>                (per-user)

# Sign a package tree.
cos provenance sign \
  --kind app --id notes --version 1.2.0 \
  --path ./notes --key ~/.secrets/claw-release.json \
  --entrypoint main.py

# Confirm it authenticates against the active trust store.
cos provenance verify --kind app --path ./notes --id notes
```

### Operator

```bash
cos provenance trust list            # keys, revocations, grants, diagnostics
cos provenance trust roots           # the compiled-in root list
cos provenance trust add --file entry.json
cos provenance trust revoke --key-id sha256:…
cos provenance trust revoke --digest sha256:…
cos provenance artifacts --kind app --id notes
cos provenance rollback --kind app --id notes \
    --digest sha256:… --dest /usr/lib/cos/apps/notes
```

Rollback re-verifies the retained artifact before activating it, so it
can only ever land on content that passed verification and has not been
revoked. The `--digest` argument accepts the full `sha256:<64 hex>`
value or an unambiguous prefix of it (artifact directories are named
with a shortened digest, so the value copied out of
`cos provenance artifacts` resolves). An ambiguous prefix lists the
candidates instead of guessing.

### Developer (unsigned local trees)

Unsigned development content is permitted only through an explicit
**human** decision recorded in the segregated developer root:

```bash
cos provenance dev-trust --kind app --id scratch --path ./apps/scratch
cos app install ./apps/scratch --dev-trust
cos provenance dev-untrust --kind app --id scratch
```

Both commands demand, in order:

1. a real controlling terminal on stdin **and** stderr — no pipes, no
   `nohup`, no CI runner, no agent subprocess;
2. no Agent, App or MCP session active for this owner, so a running
   model cannot drive the prompt through a hijacked terminal;
3. the operator typing an exact phrase naming the package —
   `trust unsigned app scratch` — not `y`, not `yes`, so it cannot be
   produced by a stray keystroke or a `yes |` pipeline.

`--yes` does **not** satisfy this and never will; passing it only logs
that it was ignored. Automation that genuinely needs unsigned content
uses an **offline signed developer grant**: a `claw.trust-dev/v1` file
produced on a workstation and copied into the developer root. That is a
deliberate, auditable artifact rather than a runtime flag.

A developer grant:

- is bound to one absolute path **and** one content digest — editing
  the tree invalidates it and it must be re-approved;
- covers only the package's **declared** manifest entry and session
  entry, never every regular file in the tree;
- lands in `TrustTier::Developer` and inherits the capability ceiling
  above;
- is recorded in the provenance audit log and listed by
  `cos provenance trust list`.

**What consent does not defend against.** Malware already running as the
same user can write the developer root directly; consent is not a
sandbox against yourself. It exists so the *model*, an App, an MCP
server or a script cannot escalate unsigned code into trusted code, and
so the human decision is recorded and visible afterwards.

There is no ambient environment flag. The former
`COS_SKILLS_REQUIRE_SIGNATURE` / `COS_SKILLS_TRUSTED_KEYS` opt-in has
been removed: signature verification is mandatory, and a `signature:`
block in SKILL.md frontmatter is now a hard parse error pointing at
`cos provenance sign` (that scheme authenticated the frontmatter but
neither the instruction body nor the skill's scripts).

### Platform support

`cos provenance` refuses to run on a non-Unix host rather than returning
a hollow success. Package verification rests on POSIX ownership and mode
checks, `openat`-based reads and durable renames; a platform that cannot
provide them gets an explicit refusal, not an advisory result.

## Migration

Existing **unsigned user installs fail closed**. They are not
grandfathered:

| Content | Behaviour after upgrade |
| --- | --- |
| Apps under `/usr/lib/cos/apps` | Continue through vendor trust; digest pinned on first use. |
| Skills under `/usr/lib/cos/skills` | Same. |
| User-installed Apps/Skills/MCP packages | Quarantined with an actionable message: re-install a signed package, or record a developer grant. |
| Loose `*.json` agent-API manifests outside an approved package root | Refused; install a signed package directory instead. |
| App data under `<data_dir>/apps/<id>` | Untouched — data is kept separate from the code artifact, so re-installing or rolling back never destroys it. |

Adapters (`adapters/`) have **no packaging today**. They are source-tree
content, are therefore not vendor-trusted, and are quarantined until
they are either signed and installed as packages or given an explicit
developer grant.

## Audit

Install, activate, reject, revoke and use are recorded in
`<log_dir>/provenance.jsonl`:

```json
{"kind":"provenance.app_launch","package_kind":"app","id":"notes",
 "version":"1.2.0","content_digest":"sha256:…","trust":"publisher",
 "tier":"user","publisher_key_id":"sha256:…","files":12}
```

Records reference the publisher key id and the package content digest.
They never contain key material, bundle bytes or model-visible text.

## Tests

```bash
# Format, trust, verification, install and CLI unit tests
cargo test -p cos --lib provenance:: -- --test-threads=1

# Real signing, real archives, real renames, TOCTOU and concurrency
cargo test -p cos --test extension_provenance_process -- --test-threads=1

# Consumers
cargo test -p cos --lib agent::skills:: agent::tools::mcp:: apps:: router:: \
    -- --test-threads=1
```
