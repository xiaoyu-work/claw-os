# Staged file changes

The Files App can prepare a real, bounded unified diff without changing the
target, then apply the reviewed proposal through the normal App permission
path. A plan is App-owned data, not write authority.

## Prepare and inspect

Inside Linux or WSL Claw OS:

```bash
cos app fs plan_write /home/user/project/config.txt \
  --content "proposed content" --ttl-seconds 3600
```

The operation requires `fs.read` on the target and `data.db.write` on
`fs-change-plans`. It reads the baseline, computes a real diff, and saves a
private proposal without requiring or exercising target write permission.
The baseline is the file visible to this isolated App launch. For an absent
target, an exact read grant does not expose its host parent or siblings;
the broker checks real host absence and parent existence when applying
the proposed creation.
For larger text, explicitly forward stdin:

```bash
cat proposed.txt | cos app fs plan_write /home/user/project/config.txt --stdin
```

An empty proposal must be explicit (`--content ""`); missing content and empty
unforwarded stdin are not silently interpreted as an empty replacement.

The result contains a plan UUID, canonical target path, baseline and proposed
hashes, a `review` fingerprint, expiry, and a canonical object reference. The
private baseline/proposed contents are not included as separate public fields.
Use the returned values:

```bash
cos app fs plan_show /home/user/project/config.txt --plan PLAN_UUID
cos object resolve 'app://fs/change-plan?id=%2Fhome%2Fuser%2Fproject%2Fconfig.txt&revision=PLAN_UUID'
```

The object ID is the absolute target path and its revision selects a specific
plan. Without `--plan`, `plan_show` returns the newest stored plan for that
target. Reading a cached plan still requires current `fs.read` permission for
the target, plus `data.db.read` for the plan store.

## Apply explicitly

```bash
cos app fs plan_apply /home/user/project/config.txt \
  --plan PLAN_UUID --review 'sha256:REVIEW_FINGERPRINT' --confirm=true
```

Replace the placeholders with the exact preparation result. Application
requires fresh `fs.read` and `fs.write` for the explicitly supplied target,
plus plan-store read/write permissions. The stored path must match; a stored
plan cannot select another target or grant access to it.

The review fingerprint binds the immutable proposal identity, path, baseline
fingerprint, proposed content hash, and lifetime. Changing a proposal after
review requires a new fingerprint; the old confirmation cannot silently apply
different content.

Before any target write, the App durably records an `applying` bracket.
Replacement crosses the session-authenticated file broker, which independently
checks exact read/write capabilities and the current file fingerprint. The
broker stages and syncs the new bytes, checks the precondition again before
commit, atomically replaces an existing file or exclusively creates a missing
one, and syncs the parent directory. The broker adds no directory mount to
enable rename. Existing worker mount rules remain unchanged, including the
parent mount already used for a write grant to an initially absent target.

This protects against observed changes and serializes cooperative replacement
calls. **It is not a filesystem compare-and-swap guarantee against every
uncooperative external writer:** a host writer can race after the final check.
Do not treat a plan or review hash as authorization, universal undo, or proof
that the target still has those bytes later.

## Outcomes and recovery

- `draft`: proposal saved, target unchanged.
- `conflicted`: the observed baseline or review no longer matches; no automatic overwrite.
- `applying` / `indeterminate`: an apply bracket is not conclusively closed; automatic replay is refused.
- `applied`: the App reports a completed broker response; `changed` distinguishes a write from an unchanged result.
- `expired`: the proposal is no longer eligible for application.

Existing snapshot hooks run before replacement. A snapshot reference is
reported when available, but recovery remains `unknown`: it does not prove an
OS rollback entry exists or that an inverse is safe. The private proposal
retains its bounded baseline for inspection and manual recovery.

Once consumed, conflicted, or indeterminate, a plan cannot be replayed. Inspect
the target and create a fresh plan instead. If replacement may have occurred
but the response, fsync, or final plan record fails, the operation reports an
indeterminate outcome rather than ordinary failure or false success.
The stored diagnostic preserves the bounded failure reason for inspection.

## Activity presentation

All OS forms use the same Files App and broker implementation. Attach the
plan through the existing shared Activity object contract:

```bash
cos activity attach-object ACTIVITY_UUID --label "Config proposal" \
  --app fs --type change-plan \
  --object-id /home/user/project/config.txt --revision PLAN_UUID

cos operation execute fs plan_show --activity ACTIVITY_UUID -- \
  /home/user/project/config.txt --plan PLAN_UUID
```

Well-formed `file_change_plan` replies receive a readable unified-diff
projection in the existing receipt preview. Terminal, Web, and native desktop
therefore display the same reported diff rather than implementing their own
planner. Receipts remain caller-reported, and long diffs are visibly truncated
by the receipt preview limit. Inspect the full plan before applying it.

## Bounds and storage

Plans support regular, single-link UTF-8 files without NUL, up to 64 KiB before
and after. Target parents must already exist when applying. Special modes,
unsupported extended attributes, symlink swaps, and special files are refused
rather than silently losing metadata. Target paths must be absolute and
literal, without traversal, wildcard or redirection syntax. Canonical paths
fit the object reference's 1024-byte identity bound.

Plan files live only in the Files App's private data partition, in per-target
hash buckets. Directories are `0700`, files `0600`; stored plans are bounded,
closed JSON with content/review validation. At most 64 plans are retained per
target. Lifetime is 60 seconds to 24 hours, default one hour.

Consumed or expired records can be pruned explicitly:

```bash
cos app fs plan_prune /home/user/project/config.txt --keep 10 --confirm=true
```

Pruning never deletes the target and preserves live or indeterminate plans.
It does not silently erase evidence while preparing a new proposal.
