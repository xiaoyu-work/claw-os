# Activities

An Activity is a persistent user goal that can span multiple Agent jobs and
conversation sessions. It is not a renamed job or a second agent runtime.

| Record | Owns |
| --- | --- |
| Activity | Goal, completion criteria, planning boundaries, resource references, and explicit lifecycle state |
| Job | One execution attempt, its status, results, and approval waits |
| Session | Conversation history and the existing capability/audit context |

## One backend, different presentations

Terminal-only and desktop installations use the same service in
[`core/src/activities/`](../core/src/activities/MODULE.md). `clawd` derives the
owner from authenticated peer credentials and owns the persistent Activity
database. The terminal command, Agent Web, and native desktop Agent are
clients of the same `activity.*` broker routes.

The service does not depend on a desktop session, window system, browser, or
LLM provider. A graphical client must not maintain its own authoritative
Activity database, perform lifecycle transitions locally, or infer goal
achievement from a successful model response.

## Terminal use

Run these commands in an installed Linux or WSL Claw OS environment with
`clawd` available. Activity management does not require a configured model;
starting Agent work requires the normal Agent setup and a non-root task owner.

```bash
cos activity create "Release v2" \
  --goal "Publish the project next Friday" \
  --criteria "The release has been reviewed and is available" \
  --boundaries "Ask before publishing publicly" \
  --resource "Release draft=/home/user/project/release.md"

cos activity list --state active
```

Use the returned `id` in later commands:

```bash
activity_id="00000000-0000-4000-8000-000000000001" # replace with the returned ID

cos activity show "$activity_id"
cos activity run "$activity_id" "Prepare a release draft"
cos activity pause "$activity_id"
cos activity update "$activity_id" --criteria "Reviewed, published, and announced"
cos activity resume "$activity_id"
cos activity complete "$activity_id" --note "I reviewed and published the release"
```

`run` returns the ordinary durable task record. Use existing
`cos agent service status`, `result`, and `cancel` commands to inspect or stop
that execution. `cos activity run ID --session SESSION_ID` continues a related
conversation. The Activity association is also available through
`cos agent service submit --activity ID` and `list --activity ID`.

Outputs follow the normal `cos` JSON/pretty-printing behavior, including
`--compact` and `--pretty`. `cos activity --help` and
`cos activity <command> --schema` are available without connecting to a
desktop or starting a model.

## Lifecycle and outcomes

Activities start `active`. Users can pause, resume, explicitly complete, or
cancel them. A terminal Activity must be explicitly reopened with `resume`
before it accepts new work or planning edits. Late receipts and object-state
annotations remain recordable without reopening the goal.

- Pausing prevents subsequent work from being admitted or claimed. It does
  not undo effects or automatically stop an already-running job.
- Cancelling ends the goal without claiming it was achieved. Use the existing
  task cancellation control when an in-flight execution also needs to stop.
- Completing requires an explicit user confirmation note. A job returning
  `ok`, a model producing a final answer, or exhausting a turn budget never
  automatically completes an Activity.
- Failed, waiting, cancelled, and successful jobs remain distinct execution
  outcomes. The Activity detail view projects bounded recent results from the
  existing job store; it does not keep a competing copy of job state.

Ordinary conversations and jobs without an Activity remain supported.
Associated session continuations retain their Activity; a session already
assigned to one Activity cannot silently be moved to another.

## Resources and boundaries

Resources initially contain a label and an inert reference. Adding a reference
does not read, copy, execute, or grant access to the referenced object.
Repeated `--resource LABEL=REFERENCE` flags replace the reference list;
`update --clear-resources` removes references, not the referenced data.

The goal, criteria, boundaries, and references are recorded planning context.
They are supplied to associated tasks through the existing transient-context
recording path rather than rewriting a frozen system prompt or pretending to
be new user messages. **Free-text boundaries are not a new policy language or
an authorization grant.** Existing capabilities, approvals, budgets, and
worker isolation continue to govern every execution.

App-owned object references can now be attached and described through the same
backend; see [App-owned objects](app-objects.md). Declaration inspection does
not fetch object data. Executable delegation policies, automatic event-driven
progression and cross-device continuation remain later steps.

[Object-state annotations](object-state.md) distinguish reported user
statements, Agent inferences, linked App receipts, planning relationships and
corrections. They share the same backend and preserve source/validity caveats;
no annotation is authority or automatic goal completion. Associated jobs
receive a bounded, recorded, untrusted snapshot when claimed.

## Broker contract

| Route | Result |
| --- | --- |
| `activity.create` | New Activity metadata |
| `activity.list` | Owner-scoped Activity metadata list |
| `activity.get` | Metadata plus related jobs and session references |
| `activity.update` | Updated metadata; only supplied fields change |
| `activity.transition` | Updated explicit lifecycle state |
| `activity.run` | An ordinary submitted Agent job associated with the Activity |
| `activity.objects` | Authenticated declaration metadata or explicit diagnostics for App references |
| `activity.object.attach` | Atomically attached reference; no App execution or new authority |
| `activity.operation.preview` | App-declared expected effects; no execution, authorization, or confirmed changes |
| `activity.receipts` | Immutable caller-reported results for the authenticated owner's Activity |
| `activity.receipt.record` | Append or idempotently retry a report; never execute or create authority |
| `activity.object_state.list` | Bounded observations, relationships and correction history |
| `activity.object_state.record` | Append an owner-scoped annotation or retraction without changing goal state |

No request accepts an owner UID, capability set, or grant. Root does not receive
an implicit cross-owner Activity view. Mutation routes use the existing
authorization, audit projection, and durable journal brackets. Goal text,
resource references, and confirmation notes are not copied into broker audit
metadata.

The database lives beside the daemon's other state as `activities.db`; it is
not part of a desktop package. Its schema version and lifecycle are owned by
the shared core service. Existing installed-state preservation applies during
package upgrades.

Activity object views can inspect [operation previews](operation-previews.md)
through that same backend. These previews are metadata, not execution receipts
or actual before/after file diffs.

[Execution receipts](execution-receipts.md) retain reported outcomes through
the same backend. Recording does not change Activity state, and a receipt is
not evidence that an App's declared effects or the user's goal were achieved.
