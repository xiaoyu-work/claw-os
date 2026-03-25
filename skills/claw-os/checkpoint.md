# Checkpoints (Undo / Rollback)

The workspace is mounted with OverlayFS. Every file change is captured automatically — regardless of how it's made. You can snapshot, diff, and rollback at any time:

```bash
cos checkpoint create "before refactoring"
cos checkpoint diff
cos checkpoint rollback
cos checkpoint rollback 001
cos checkpoint list
cos checkpoint status
```

`cos checkpoint diff` shows all files created, modified, or deleted since the last checkpoint — without scanning or comparing files manually. `cos checkpoint rollback` reverts the entire workspace instantly.
