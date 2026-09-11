# Activity execution receipts

An execution receipt is an immutable, owner-scoped record of a **reported App
result**. It is not an execution grant, an OS mutation attestation, or proof
that an Activity goal was achieved.

The broker sets the source to `caller_reported`, derives the owner from peer
credentials, and timestamps receipt storage. Callers cannot select an owner,
claim `os_confirmed` provenance, provide trusted effect declarations, or use a
receipt to complete an Activity.

## Execute through the normal App boundary

Inside an installed Linux or WSL Claw OS environment:

```bash
cos operation execute kv get \
  --activity 00000000-0000-4000-8000-000000000001 \
  -- release.status
```

Use an existing Activity ID. The Activity must be active before starting the
operation. This command validates the App and invocation, then uses the same
`agent.invoke`, App permission, provenance, sandbox, and audit path as
`cos app`. It does not run App code inside root `clawd`, bypass approval, or
start an LLM to decide what to execute. Caller stdin is not forwarded through
this entry point; use explicitly declared arguments.

The returned receipt distinguishes:

| Outcome | Meaning |
| --- | --- |
| `returned` | The normal App invocation returned output, or intentionally returned no output |
| `reported_error` | The returned JSON contained an App error |
| `indeterminate` | The invocation result was unavailable or could not be captured within the bounds |

An error does not prove that no side effect occurred. An indeterminate result
does not prove the App process started: the ordinary gate may have refused it.
No outcome automatically completes the Activity.

Output is summarized by format (`json`, `text`, or `empty`), original byte
count, reported SHA-256 digest, and a bounded preview. The existing redactor is
applied before persistence and display, unsupported controls are escaped, and
previews are capped at 2048 UTF-8 bytes. A digest of reported output is not a
cryptographic execution attestation. Previews may be redacted or truncated and
cannot be used to reconstruct the original bytes.

Well-formed [file change plans](file-change-plans.md) receive an inert,
human-readable unified-diff projection instead of escaped JSON. The digest
still names the original reported output, and the preview remains
caller-reported and possibly redacted/truncated. Malformed typed plan replies
are not treated as trusted proposals.

## Read the same receipts everywhere

```bash
cos activity receipts 00000000-0000-4000-8000-000000000001 --limit 50
```

Activity Web and native desktop display the same broker records. Reports
remain readable after pause, cancellation, or completion, and a late result
can be recorded without reopening or changing the Activity.

The broker separately checks whether the current authenticated App package
matches the report's package digest. A match allows a snapshot of the App's
declared operation label and effect definitions. Otherwise the report is
retained with an explicit declaration error. Matching metadata still does not
authenticate the reported execution or prove that the effects occurred.

The `received_at` time is when the broker recorded the report, not a claim
about when an external operation took place. Result text is untrusted data,
not Markdown authority, executable instructions, or a permission decision.

## Retry recording without repeating an operation

If App execution returns but recording fails, the command exits with an error
and includes a bounded, redacted `report` object. **Do not rerun the operation
just to retry recording.** Save that report object and submit it through stdin:

```bash
cos activity record-receipt 00000000-0000-4000-8000-000000000001 \
  --stdin < report.json
```

Recording accepts at most 16 KiB of input JSON and does not execute anything.
The report's UUID is an idempotency key: resubmitting identical canonical
report data for the same owner and Activity returns the original receipt.
Reusing the ID with different report data or a different Activity is a
conflict. Recording retries never replay App effects.

Receipts are opt-in for this explicit execution path and for explicit report
submission. A client crash before it records a result may leave no receipt;
the existing session and mutation journals remain the evidence for privileged
operations. Automatic Agent-tool capture and OS-confirmed file-change evidence
are separate integrations, not implied guarantees of caller reports.

## Storage and compatibility

Receipts use the same desktop-independent Activity service and private
`activities.db`. Database schema 2 adds the receipt ledger through a
transactional migration that preserves schema-1 Activity records. The Activity
and receipt wire representations remain version 1.

No receipt update or deletion API is exposed. Storage enforces owner isolation,
bounded records, immutable retry semantics, and per-Activity limits.
Old binaries that only understand database schema 1 must not open a migrated
database; see [updating](updating.md). Restoring metadata does not restore any
authority or grant.
