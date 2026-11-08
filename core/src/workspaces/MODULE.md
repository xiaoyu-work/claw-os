# Workspaces Module

## Purpose

Workspaces are owner-scoped, durable context domains above Activities. They are
not task working directories, App private stores, or execution grants.

## Responsibilities

- Define shared context, sourced notes, recursive read-only resource tags,
  exact revisions, archive and deletion semantics.
- Reuse the Activity SQLite provider and its owner boundary.
- Keep user goals and execution evidence after Workspace deletion.
- Route human management through the authenticated broker.

## Key Files

| Path | Role |
| --- | --- |
| `mod.rs` | Workspace service definition and shared client DTO exports |
| `cli.rs` | Human `cos workspace` commands |
| `association.rs` | Optional Root conversation/Job domains, pre-effect admission and live claim checks |
| `context_access.rs` | Non-portable visibility epochs and closed temporary-context replay constraints; original evidence remains |
| `boundary.rs` | Read-source constraints carried through worker gates and attenuated Root App grants |
| `gateway.rs` | Bounded task-local context/recall/report operations with no owner or domain selector |
| `../../../crates/clawd-client/src/workspaces.rs` | Closed shared wire definitions and input bounds |
| `../activities/sqlite/workspaces.rs` | Schema-10 transactional provider in `activities.db` |
| `../clawd/workspaces.rs` | Owner management, source validation and deletion coordination |
| `../clawd/owner_files.rs` | Shared no-symlink descriptor traversal |
| `../clawd/workspaces/reads.rs` | Root live-tag/source reads, scoped memory gateway, local file search and index diagnostics |
| `../agent/terminal/workspaces.rs`, `../agent/terminal/plain.rs` | TUI center and plain chat over the same durable Root backend |
| `../agent/web/routes/workspaces.rs`, `../agent/web/ui/src/pages/workspaces.tsx` | Authenticated Web adapter and exact-revision human forms |

## Authority

Workspace metadata is partitioned by the kernel-authenticated owner. Management
refuses Root, Apps and isolated Agent workers. Tagging is an explicit owner
read-sharing action, not filesystem write, execution, network, AI consent or
permission to use another App.

Folder tags describe a recursive scope including future descendants. Source
reads must independently check the live tag, owner filesystem identity, pinned
no-symlink path and protected-store exclusion. A directory prefix is not proof
that a source is readable or fresh.

Every metadata change uses the exact displayed Workspace revision. Deletion
durably removes shared notes and tags, pauses member goals and coordinates
automatic-work shutdown and task cancellation. `cleanup_pending` must not be
presented as checked worker retirement. Queries do not cancel work.

Root conversations retain a visibility epoch and bounded temporary-read
constraints in their existing metadata/state. These constrain future model
projection, not human inspection or capability authority. New conversations
are required after relevant revocation; forks retain constraints rather than
turning scoped evidence into portable context. Job-only reads do not pass to
later Jobs. Temporary-context work cannot create automatic shared findings.

Private worker protocol v20 adds the correlated, count/byte/cancellation-bounded
context exchange and a Root check before each Workspace model turn. Its paired
release-security policy and compiled protocol constant must agree. Normal
App/filesystem read grants retain Workspace constraints through attenuation;
tags never supply write, execution or App permission.

File search consumes the independent owner-default personal-file corpus and
the pinned local embedder. Namespace/key eligibility precedes index text,
vector validation and top-K. Current source digests and live rights are
rechecked before disclosure. Explicit source registration uses the existing
owner indexer, not a second database or daemon.

Search resolves protected Workspace/session sharing metadata at Root before
entering the owner filesystem identity. The pre-ranking predicate uses that
bounded scope snapshot, and Root rechecks live rights before returning hits;
owner-I/O failures must not masquerade as missing Root metadata.

Human linked note/file previews carry a closed outer Job/conversation target.
Root resolves that exact live same-owner record and retains temporary-context
replay constraints; a conversation selector never widens Job-only access.
The private `WorkspaceRead` and worker exchange remain selector-free. Preview
pagination preserves the link, target and displayed content revision.

## Validation

```bash
cargo test -p cos activities::sqlite::workspaces:: -- --test-threads=1
cargo test -p clawd-client --lib --locked
```
