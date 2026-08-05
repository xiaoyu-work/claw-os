# Service Diagnosis

Use for failed, unhealthy, crash-looping, or unavailable services.

## Initial evidence

```json
{"command":"run","args":["--domain","service","the service failed"]}
```

Inspect:

- `failed-units`: native systemd failures;
- `journal-errors`: recent high-priority messages;
- `coredumps`: process crashes associated with the service.

## Decision tree

1. Confirm the exact unit or Claw-managed service name.
2. Determine whether it is:
   - a native systemd unit; or
   - a service registered with `cos_service`.
3. Check dependency, credential, path, port, and configuration errors.
4. Check whether another process owns the required port.
5. Correlate repeated exits with coredumps and package/configuration changes.

## Important distinction

`cos_service` manages Claw-defined services. It is not a general `systemctl`
replacement. Until the native systemd manager lands, do not claim the agent
can enable, disable, or restart arbitrary system units.

For a Claw-managed service, prefer `status`, `health`, and `logs` before
`restart`. Preserve the pre-action status and verify health afterward.
