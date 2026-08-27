# Capability Authority Module

## Purpose

`clawd/authority/` is the one place that decides what a broker request may do.
Authority is **held**, not described: a grant is a daemon-owned record bound to
an authenticated process, and every privileged route is authorized against one
before its handler runs.

## Responsibilities

- Issue grants bound to a principal, subject, audience, capability set, issuer,
  expiry, use budget and lineage.
- Reference them by opaque, non-enumerable handles that are never serialized,
  logged or stored in the clear.
- Enforce monotonic attenuation: caps, audience, expiry and use budget only ever
  shrink; lineage depth and child count are bounded.
- Resolve, spend and revoke atomically, so a one-shot cannot be double-spent and
  a multi-capability spend is all-or-none.
- Provide the mandatory route middleware and the decision handlers re-check
  through.
- Emit typed audit facts for every issuance, attenuation, use, exhaustion,
  expiry and revocation.

## Key Files

| Path | Role |
| --- | --- |
| `mod.rs` | Route authorization descriptors and the `authorize` middleware |
| `store.rs` | The grant store: issue, attenuate, resolve, consume, revoke, sweep, quotas |
| `grant.rs` | Grant record, principal/subject/audience types, attenuation rules |
| `handle.rs` | Opaque handles, store keys, keyed audit references |
| `decision.rs` | The per-request decision handlers consume |
| `audit.rs` | Typed authority facts |
| `../../../test/unit/clawd/authority/` | Store and handle unit tests |
| `../../../test/unit/clawd/authority.rs` | Descriptor coverage and middleware tests |

## Route Subjects

| Subject | Meaning | Used by |
| --- | --- | --- |
| `Peer` | Acts for the connecting process; no grant is resolved | daemon, task, context, permission, transaction, App registration, scheduler |
| `Session` | Addressed by an App/MCP session; runs under the grant derived at bind | privileged system providers |
| `PeerSession` | Addressed by the caller's own registered session; authenticated from process ancestry and given a single-use request-scoped grant | `system.package.restore`, `system.service.restore`, `credential.oauth-refresh` |
| `Handle` | Addressed by the opaque handle itself | App session bind / set-transient / deregister |

Each `PeerSession` route also declares whether an App session's one-call
transient capabilities count for it. `credential.oauth-refresh` excludes them,
preserving what the credential broker checked before this module existed; the
rollback routes include them, preserving what `packages` and `systemd` checked.

## Proof of Authorization

`Decision::require_all` returns an [`Authorized`] on success. It is
`#[must_use]`, has no public constructor, and is neither `Clone` nor `Copy`, so
a helper that takes one can only be reached through a spend that succeeded. The
highest-risk privileged mutations take it by reference:

| Module | Helper |
| --- | --- |
| `packages` | `run_package_action`, `restore_package_state_async` |
| `systemd` | `run_systemctl_action`, `restore_state_async` |
| `users` | `mutate` |
| `storage` | `mutate` |
| `config_editor` | `apply_config`, `restore_config` |

Two related refusals close the same gap from the other side: an empty capability
set is not an authorization, and only a *successful* spend marks the route's
obligation met, so a provider that dropped an `Err` has its response withheld.

## What Is Not Authority

- **A handle.** Possession is necessary and insufficient. Every resolve
  re-checks uid, pid, that pid's start time and — where `/proc` reports one —
  the cgroup, against the credentials the kernel stamped on *this* message.
- **A session id.** It is an index. Naming somebody else's session finds their
  grant and fails the principal check, which is the answer an unknown session
  also gives.
- **A serialized `CapSet`.** A description found on disk or in a request is
  re-derived and clamped, never promoted.
- **Process context.** Root euid, TTY, executable path, `NoNewPrivs`, socket
  group membership, prompt and model text select nothing.

## Dependencies

Depends on `crate::caps` for the capability vocabulary, `crate::proc` for
process identity, and `crate::crypto` for hashing. Consumed by
`clawd::routes`/`clawd::server` (the middleware), `clawd::app_sessions` (launch
and session grants), and every privileged provider through `Decision`.

Nothing here reads a request field for identity or authority.

## Tests

```bash
cargo test -p cos clawd::authority -- --test-threads=1
```

A change here needs adversarial coverage, not only a happy path: handle guessing
and theft, a same-uid sibling, pid reuse, wrong audience/session/App, an
expired, revoked or exhausted grant, a parent revocation cascade, an attenuation
that widens any dimension or introduces `Scope::Wild`, lineage depth and count,
concurrent double-use, an all-or-none multi-capability spend, and a fresh daemon
holding nothing.
