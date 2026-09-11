# Files App

## Purpose

Provide filesystem operations through the ordinary App manifest, capability,
worker and audit boundaries. Object declarations do not create another App
integration category.

## Key Files

| Path | Role |
| --- | --- |
| `app.json` | Operations, bindings, exact scopes, object types and effect declarations |
| `main.py` | Existing filesystem handlers and dispatch |
| `file_plans.py` | Bounded App-owned proposals, real diffs, review fingerprints, lifecycle and guarded broker apply |
| `test_main.py`, `test_file_plans.py` | Existing operation compatibility and staged-change regressions |
| `../_shared/atomic.py` | Shared atomic writes; strict mode for durable plan metadata |

## Staged changes

`plan_write` reads a bounded baseline and stores a private proposal.
`plan_show` requires current target read authority even though it displays
cached data. The `change-plan` object uses target path as identity and an
optional plan UUID revision. `plan_apply` takes the target explicitly, requires
normal read/write authority, checks the review and baseline, and durably opens
an apply bracket before invoking the session-authenticated file broker.

Replacement adds no parent-directory mount just to perform rename. Existing
worker rules for initially absent write targets remain unchanged.
Post-dispatch uncertainty is recorded as indeterminate; consumed or unknown
plans are not automatically replayed. `plan_prune` removes only retired
App-owned records after confirmation, never the target or unknown outcomes.
Strict metadata fsync failures are errors, not success-shaped fallbacks.

New plan handlers consume the full `parse_canonical_argv` grammar before
legacy list-token normalization. Existing handlers retain their behavior.

See [file change plans](../../docs/file-change-plans.md) for constraints,
commands, and the boundary between App reports and OS evidence.

## Tests

```bash
PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \
  python3 -m pytest -q apps/fs/test_main.py apps/fs/test_file_plans.py
```

The opt-in core process regression runs this App, the real hidden CLI, the
worker relay and the file provider against isolated files and journals:

```bash
cargo build -p cos --bin cos
COS_FILE_PLAN_COS_BIN="$PWD/target/debug/cos" \
  cargo test -p cos --lib file_plan_real_app_worker_cli_and_broker_round_trip \
  -- --ignored --test-threads=1
```

Run this regression as a non-root App owner. It requires Linux sandbox
facilities and fails if they are unavailable; it never falls back to a mocked
replacement. Root provider invariants are covered separately by the
`clawd::file_changes` unit tests.
