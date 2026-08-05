# Storage Diagnosis

Use for disk-full, slow storage, missing filesystems, or heavy I/O.

## Initial evidence

```json
{"command":"run","args":["--domain","storage","--path","/home/cos","disk is full"]}
```

Inspect:

- `resources`: free space for the active workspace filesystem;
- `mounts`: source, target, filesystem type, and mount options;
- `disk-rate`: sampled read and write throughput;
- `largest-files`: largest files on the requested filesystem only.

## Decision tree

1. **Free space below 8%**
   - Treat as critical.
   - Identify large files before deleting anything.
2. **High write throughput**
   - Correlate with active jobs, package operations, logs, downloads, or
     database writes.
3. **Expected mount missing**
   - Confirm the source device or network share exists.
   - Do not substitute another mount point silently.
4. **Read-only or unusual mount options**
   - Check kernel and journal errors before attempting remount.
5. **Large files**
   - Separate caches and reproducible artifacts from user data.
   - Prefer archive, package-cache cleanup, or retention changes over deletion.

## Safety

Never recursively delete a broad directory based only on size. Name every
target, explain recoverability, and use checkpoints or backups where possible.

Use Storage Manager for device-level evidence:

```bash
cos app storage-manager status
cos app storage-manager health /dev/sdb
cos app storage-manager check /dev/sdb1
```

SMART health does not replace a backup, and a clean SMART result does not prove
the filesystem is consistent. Offline checks are read-only and require the
filesystem to remain unmounted for the result to be valid.
