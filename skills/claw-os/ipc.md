# Inter-Process Communication

Message passing, locks, and barriers for agent coordination:

```bash
cos ipc send target-session "build complete" --from build-1
cos ipc recv my-session
cos ipc recv my-session --timeout 30
cos ipc recv my-session --peek
cos ipc list my-session
cos ipc clear my-session
```

## Locks

Mutual exclusion for shared resources. Stale locks from dead processes are auto-reclaimed:

```bash
cos ipc lock database-write --holder agent-1
cos ipc unlock database-write --holder agent-1
cos ipc locks
```

## Barriers

Wait until N agents reach a synchronization point:

```bash
cos ipc barrier merge-ready --expect 3 --session search-1 --timeout 60
```

## Streaming Pipes

Named channels for structured message streaming between processes. Unlike message queues (send/recv), pipes support replay, backpressure, and follow mode:

```bash
cos ipc pipe create my-events --buffer-size 500
cos ipc pipe publish my-events '{"type":"progress","value":42}' --from worker-1
cos ipc pipe subscribe my-events --since 000003 --limit 10
cos ipc pipe subscribe my-events --follow --timeout 30
cos ipc pipe list
cos ipc pipe destroy my-events
```
