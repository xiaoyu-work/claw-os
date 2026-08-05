# Security Diagnosis

Use for suspicious logins, exposed ports, permission denials, unexpected
services, or possible compromise.

## Initial evidence

```json
{"command":"run","args":["--domain","security","there may be a suspicious login"]}
```

Inspect:

- `sessions`: currently logged-in users;
- `network`: listening sockets and TCP states;
- `journal-errors`: warnings and errors;
- `kernel-log`: AppArmor denials and other kernel security messages.

## Decision tree

1. **Unexpected login session**
   - Confirm username, terminal, source, and login time.
2. **Unexpected listening socket**
   - Map the port to a process before blocking or killing anything.
3. **AppArmor denial**
   - Identify the profile, operation, and target path.
   - A denial can indicate either an attack or a legitimate missing rule.
4. **Repeated authentication errors**
   - Correlate by source and time; generic journal errors are not enough to
     prove brute force.
5. **Unknown process**
   - Record executable, command line, owner, parent, open ports, and start time
     before containment.

## Containment order

1. Preserve evidence.
2. Restrict the specific process or host.
3. Revoke exposed credentials.
4. Stop the confirmed malicious process.
5. Escalate to broader isolation only with user approval.

Use Security Center for dedicated evidence:

```bash
cos app security-center summary
cos app security-center auth
cos app security-center ssh
cos app security-center sudo
cos app security-center ports
cos app security-center events
```

Firewall policy and package-integrity verification remain separate checks;
do not infer either solely from listening sockets or authentication logs.
