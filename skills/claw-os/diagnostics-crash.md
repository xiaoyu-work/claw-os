# Crash and OOM Diagnosis

Use for crashes, freezes followed by exit, OOM kills, segmentation faults, or
repeated restarts.

## Initial evidence

```json
{"command":"run","args":["--domain","crash","the application crashed"]}
```

Inspect:

- `coredumps`: recent crash records;
- `journal-errors`: high-priority service and application messages;
- `kernel-log`: OOM, segfault, I/O, driver, and policy-denial messages;
- `resources`: current memory and disk pressure.

## Correlation rules

Correlate by timestamp, executable, PID, unit, and user. Do not assume the
newest coredump belongs to the reported symptom.

1. **Kernel OOM message**
   - Identify the killed process and cgroup.
   - Check whether host memory or only a cgroup limit was exhausted.
2. **Segmentation fault plus coredump**
   - Treat the coredump as primary evidence.
   - Check recent package or library changes.
3. **No coredump**
   - The process may have been cleanly terminated, killed, sandboxed, or unable
     to write a dump.
4. **Repeated service exit**
   - Continue with the service playbook.
5. **I/O error near the crash**
   - Continue with the storage playbook before restarting write-heavy work.

## Backtraces

Use Crash Doctor when metadata indicates a matching dump:

```bash
cos app crash-doctor diagnose 60 20
cos app crash-doctor backtrace <boot-id>:<pid>:<timestamp-us>
```

The backtrace command returns the recorded `coredumpctl info` stack and, when
GDB and the core file are available, a constrained live stack. Restart only
after preserving this evidence. Repeated blind restarts can erase the state
needed to find the cause.
