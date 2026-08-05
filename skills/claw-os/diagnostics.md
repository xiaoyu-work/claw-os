# System Diagnosis Protocol

Use this protocol for every question about the live operating system.

## 1. Establish the symptom

Capture:

- what the user observed;
- when it started and whether it is continuous;
- the affected app, service, device, path, host, or port;
- any recent install, update, configuration, reboot, or hardware change.

If the target is ambiguous, ask one focused question before making changes.

## 2. Collect bounded evidence

Call `cos_diagnose`:

```json
{"command":"run","args":["--domain","performance","the computer is slow"]}
```

Supported domains: `general`, `performance`, `network`, `storage`, `service`,
`crash`, `thermal`, `security`.

Use `--quick` when latency matters. For storage questions, pass `--path` only
for a directory the user actually asked about:

```json
{"command":"run","args":["--domain","storage","--path","/home/cos","disk is full"]}
```

## 3. Form hypotheses

Produce at most three ranked hypotheses. Each hypothesis must include:

- supporting evidence IDs;
- contradicting or missing evidence;
- confidence: `high`, `medium`, or `low`;
- the next read-only probe that could confirm or reject it.

Do not convert correlation into causation. High CPU, a recent crash, or an
error log can be a consequence rather than the root cause.

## 4. Verify before acting

Use the relevant playbook. Prefer structured tools over shell scraping.
Re-sample counters such as CPU, network rate, and disk I/O rather than relying
on one instantaneous reading.

## 5. Propose a safe action

Before mutation:

1. state exactly what will change;
2. name the capability and scope required;
3. explain user-visible impact;
4. identify rollback or explain why rollback is impossible;
5. request approval for high-risk operations.

Never kill a process, restart a service, install software, edit configuration,
or delete files merely because one threshold fired.

## 6. Verify the result

Repeat the evidence that originally demonstrated the problem. Report:

- what changed;
- whether the symptom is resolved;
- any remaining warnings;
- how to undo the action.

If evidence remains inconclusive, say so instead of inventing certainty.
