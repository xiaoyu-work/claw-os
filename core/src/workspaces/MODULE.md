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
| `../../../crates/clawd-client/src/workspaces.rs` | Closed shared wire definitions and input bounds |
| `../activities/sqlite/workspaces.rs` | Schema-10 transactional provider in `activities.db` |
| `../clawd/workspaces.rs` | Owner management, source validation and deletion coordination |
| `../clawd/owner_files.rs` | Shared no-symlink descriptor traversal |

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

## Validation

```bash
cargo test -p cos activities::sqlite::workspaces:: -- --test-threads=1
cargo test -p clawd-client --lib --locked
```
