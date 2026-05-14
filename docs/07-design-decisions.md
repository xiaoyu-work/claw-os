# Permission System — Design Decisions

This document records the five outstanding design calls for the
permission system that the user asked be resolved before the
approval-gate UI and fs-undo work landed. Decisions here are
opinionated defaults — easy to revisit, hard to refactor away from
silently, so they are written down once.

## 1. Default role when `cos proc spawn` runs without `--role` / `--caps`

**Decision:** **No default role.** A bare `cos proc spawn -- foo` does
not synthesize any caps for the child. The child gets `caps: None`
in its [`SessionInfo`](../core/src/proc.rs), which the strict-mode
gate treats as "nothing granted."

**Rationale:** Picking *any* role as a silent default — even
`worker` — would silently grant fs.read/fs.write/fs.meta to every
unconfigured child, defeating the whole "explicit consent"
posture. The legacy `--tier` flag is still honoured for the
back-compat window so existing scripts don't break.

**Implication:** `permissive` mode (`COS_PERMS_MODE=permissive`)
remains the escape hatch for shells / test harnesses / first-run
scenarios that need everything to "just work".

## 2. TUI vs GUI approval-gate priority

**Decision:** **Both in parallel.** CLI/TUI ships first because
SSH / cron / `cos agent run` streams in a terminal need it; the
GUI applet ships alongside because the desktop is the primary
surface for ad-hoc interactive consent.

**Rationale:** The user picked this directly when asked. Both
front-ends read the same on-disk queue
(`$COS_DATA_DIR/approvals/{pending,approved,denied}/<id>.json`)
so there is exactly one source of truth and either can satisfy
any request.

## 3. fs snapshot implementation

**Decision:** **Pure copy** to `$COS_DATA_DIR/trash/<sid>/<seq>/blob`
with a sibling `meta.json`. No filesystem-specific tricks (no
btrfs reflink, no ZFS snapshot, no overlayfs).

**Rationale:** Pure copy works on every filesystem (ext4, xfs,
tmpfs, macOS HFS+ for dev), is trivial to undo (copy back),
trivial to GC (just `rm -rf` the directory), and trivial to
audit (you can `cat meta.json` to see what changed). Reflink and
copy-on-write are a future opt-in optimisation gated by detection
of filesystem capabilities — they do not change the on-disk
contract.

**Cost:** A `fs.write` on a 100 MB file pays for one 100 MB copy.
Acceptable for v1. Future work: skip snapshot when the path
matches a user-declared "ephemeral" glob, or when `--no-snapshot`
is passed.

## 4. Extra confirmation for `critical`-risk caps

**Decision:** **Typed-phrase confirmation.** Approving a cap whose
catalog entry carries `Risk::Critical` requires the operator to
type back the literal session id (or a generated 4-word
confirmation phrase) before the grant is recorded. No 2FA, no
hardware key, no biometrics.

**Rationale:** A typed phrase defeats reflex "Y" hammering
without depending on hardware the OS may not have. It is the same
posture `rm -rf /` prompts and `git push --force` confirmations
take. 2FA / hardware-key integration is a bigger story (key
storage, fallback paths, recovery) and deferred until a clear
threat model demands it.

**Applies to:** any cap whose [`Risk`](../core/src/caps/risk.rs)
field in the catalog is `Risk::Critical`, regardless of role.

## 5. New "vertical" roles (developer, researcher, media-editor…)

**Decision:** **No new built-in roles for now.** The seven
existing roles (observer, worker, curator, connector, automator,
agent-host, admin) cover the trust-level axis and are kept
deliberately small so the user-facing description stays
memorisable.

**Rationale:** Vertical / domain-shaped grants are better
expressed as `--caps verb1,verb2,...` lists or — once we add
them — *saved cap-set templates* the user can name. Hard-coding
"developer" or "researcher" into the kernel role registry would
freeze a categorisation that is inherently a user-workflow
concern.

**Future:** If a specific vertical recurs in field use (e.g.
data-analyst with `fs.read,db.query,ai.embed`), promote it from
template to first-class role in a follow-up — same shape as the
existing seven, just with a more specialised cap list.

---

## Open follow-ups (not in scope for this doc)

- `cos perms grant` CLI — saved cap-set templates with friendly names.
- Approval-gate audit log — every approve/deny written to a
  structured log alongside the existing trace stream.
- `--snapshot=off` per-call opt-out for batch operations where the
  snapshot cost is unacceptable.
