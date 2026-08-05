# Performance Diagnosis

Use for slow, frozen, high-load, CPU, memory, or responsiveness complaints.

## Initial evidence

```json
{"command":"run","args":["--domain","performance","the system is slow"]}
```

Inspect these evidence IDs:

- `resources`: host memory and workspace capacity;
- `load`: one-, five-, and fifteen-minute load per core;
- `top-cpu`: sampled CPU consumers;
- `top-memory`: resident-memory consumers;
- `cgroup`: memory, CPU, and PID limits;
- `disk-rate`: sampled block-device throughput;
- `sensors`: heat, fans, battery, and power.

## Decision tree

1. **Low available memory**
   - Confirm the largest resident process with `top-memory`.
   - Check whether the current cgroup is near `memory.max`.
   - Look for OOM evidence before assuming a leak.
2. **High load with a hot CPU process**
   - Inspect the process with `cos_proc stats`, `status`, and `output` when it
     belongs to a registered session.
   - High CPU can be expected work; compare with the user's task.
3. **High load without high CPU**
   - Treat blocked disk I/O or uninterruptible tasks as more likely.
   - Compare `disk-rate`, process states, and recent storage errors.
4. **Normal load but poor responsiveness**
   - Check memory pressure, thermal readings, cgroup limits, and desktop or
     compositor logs.
5. **High temperature**
   - Continue with the thermal playbook before changing process priority.

## Safe remediation order

1. Pause or reduce the workload if supported.
2. Lower priority with `cos_proc renice` for a registered process.
3. Gracefully stop the responsible task.
4. Kill only after identity verification and explicit approval.

Re-run the same performance diagnosis after any action.
