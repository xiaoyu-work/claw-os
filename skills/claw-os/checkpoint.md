# Checkpoints (Undo / Rollback)

The workspace is mounted with OverlayFS. Every file change is captured automatically — regardless of how it's made. You can snapshot, diff, and rollback at any time:

```bash
yu checkpoint create "before refactoring"
yu checkpoint diff
yu checkpoint rollback
yu checkpoint rollback 001
yu checkpoint list
yu checkpoint status
```

`yu checkpoint diff` shows all files created, modified, or deleted since the last checkpoint — without scanning or comparing files manually. `yu checkpoint rollback` reverts the entire workspace instantly.
