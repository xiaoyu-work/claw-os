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

Associated work uses Main's durable PREPARE/COMMIT task admission. Model
orchestration stays in `claw-agentd`; dynamic App/MCP execution belongs to the
isolated extension Host, not a worker-local AppHost fallback. Existing
nonce-, digest- and context-bound approval checks remain authoritative.

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
cos activity attention "$activity_id"
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
annotations remain recordable without reopening the goal. Existing capability
policies remain readable and can be disabled on terminal Activities; editing
or re-enabling them requires reopening.

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

## Attention and decisions

`cos activity attention ID [--limit N]` reads the same owner-scoped summary in
terminal-only and desktop installations. It combines associated Job counts,
pending or recorded permission decisions, prioritized execution issues, and
retained task-linked notifications. Counts and totals cover the full retained
scope; the 1-100 limit applies independently to each returned detail list.

The view is informational and does not mutate the Activity, Jobs, approvals,
or notifications. Pending decisions link to the existing protected OS review
presenter. An approved entry is historical consent evidence, not proof that
authority is still valid, the Job resumed, an effect succeeded, or the goal is
complete. Missing, corrupt, foreign, or mismatched approval details appear as
bounded `unavailable` entries without exposing capability metadata.

Notification sender labels, text, task tags, read state, and acknowledgement
are never authority or decisions. Reading or acknowledging a notification
cannot approve work. Indeterminate execution is counted in addition to the
Job's ordinary status and is prioritized above waiting and failed issues so an
uncertain effect is not hidden by lower-risk attention.

## Resources and boundaries

Resources initially contain a label and an inert reference. Adding a reference
does not read, copy, execute, or grant access to the referenced object.
Repeated `--resource LABEL=REFERENCE` flags replace the reference list;
`update --clear-resources` removes references, not the referenced data.

The goal, criteria, boundaries, and references are recorded planning context.
Associated tasks receive bounded, provenance-labelled owner data through the
trust/data-prelude path, not a replacement for operator or system policy.
Rendering that data does not promote its trust or authority.
**Free-text boundaries are not a new policy language or
an authorization grant.** Existing capabilities, approvals, budgets, and
worker isolation continue to govern every execution.

Explicit [execution limits](activity-execution-limits.md) add finite
attempt/turn/expiry controls through that same backend. They constrain work
without granting capabilities; free-text boundaries remain planning guidance.

Shared [capability policies](activity-capability-policies.md) add typed Normal,
Ask and Deny rules. They constrain both existing capabilities and approval
escalation; they are not permission grants or model instructions. Disabling
a policy blocks controlled capability checks rather than removing constraints.
Terminal and graphical clients use the same revision-checked backend.
This is not process-wide kernel protection against a compromised same-UID
Agent; the exact App sandbox and broker remain authoritative.

App installation review, background-service review and runtime capability
approval remain separate. Use `cos review` or the native OS review presenter
for an App review. An Activity policy cannot confirm that review, enable an
unreviewed service, or grant the permissions the App requested.

App-owned object references can now be attached and described through the same
backend; see [App-owned objects](app-objects.md). Declaration inspection does
not fetch object data. Activity-linked triggers provide the event-driven path
below; cross-device continuation remains separate work.

## Event-driven work

An owner can create an event trigger associated with an Activity. Configure
enabled, unexpired [execution limits](activity-execution-limits.md) first, then
select an actual context-event producer and event type:

```bash
cos triggers add --id release-changed \
  --activity "$activity_id" \
  --source project-watcher --event-type artifact.changed \
  --prompt "Refresh the release draft; ask before publishing anything"

cos triggers list --activity "$activity_id"
cos triggers disable release-changed
cos triggers enable release-changed
cos triggers run release-changed
cos triggers remove release-changed
```

`project-watcher` and `artifact.changed` are illustrative producer names.
Attaching a resource does not install a filesystem watcher or make an App
publish events. Use events your installed producer actually emits.

Creation and re-enabling still cross the existing scheduler capability and
approval boundary. An Activity identifier grants no scheduler or App authority.
The broker checks the Activity association before spending scheduler consent;
dispatch rechecks its current owner, lifecycle and finite limits before
creating work. Event-triggered jobs are unattended and remain subject to the
same capability policy, exact consent rules, queue admission and worker limits
as other Activity attempts. A trigger cannot make a paused or ended Activity
runnable or turn its result into goal completion.

Matching events observed while an Activity is paused, ended, or out of its
configured limits are consumed with a `blocked` diagnostic. The rule stays
enabled; resuming the Activity does not replay those old events. A definite
failure before publication is reported as `failed` and also consumes that
delivery. Explicit `run` requests return an error when admission is refused.

Activity delivery records a Root-created correlation UUID durably before
publishing its Job. After interruption, a matching existing owner/Activity
Job is reported as `recovered`, never republished or reset, even if the
Activity has since paused. Missing or conflicting publication evidence is
`indeterminate`: the affected rule is disabled and the work is not
automatically retried. Inspect any reported Job and its outcome before
re-enabling a rule or requesting new work; enabling is not permission to
replay an uncertain operation.

`cos triggers list --activity "$activity_id"` includes each rule's
`last_delivery`: `status`, `at_ms`, `message`, and available `job_id` and
`session_id`. The heartbeat's `tick` result reports blocked and failed
deliveries in `skipped` with an `error`; user broker requests cannot invoke
`tick`. Re-enabling or replacing a rule advances its generation and retires
old undelivered work as `skipped`, without cancelling already-published Jobs.
These are dispatch outcomes, not proof of App effects or goal achievement.

Missing or corrupt cursor state is an error, not permission to start scanning
the retained event log from the beginning. Activity rule creation durably
initializes progress and its persistent marker before publishing the rule.
The marker survives rule removal, so removal and recreation cannot bypass
missing-progress protection. Listing, disabling and removal remain available
for inspection; restore the original matching cursor from backup rather than
deleting or resetting progress to make a rule run.

The deterministic heartbeat scans events without consulting an LLM. Idle or
nonmatching scans create no model work. Trigger configuration currently uses
`cos triggers`; associated jobs, results and receipts appear in the existing
terminal, Web and native Activity views through the same backend. There is no
frontend-owned scheduler or second Activity store.

## Object-state annotations

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
| `activity.attention` | Read-only Job, protected approval, issue, and task-linked notification projection |
| `activity.update` | Updated metadata; only supplied fields change |
| `activity.transition` | Updated explicit lifecycle state |
| `activity.run` | An ordinary submitted Agent job associated with the Activity |
| `activity.execution_limits.get` | Owner-scoped finite execution policy and usage |
| `activity.execution_limits.set` | Version-checked limits update without resetting usage or granting authority |
| `activity.execution_limits.enabled` | Explicit enable/disable, not policy deletion or unlimited execution |
| `activity.capability_policy.get` | Owner-scoped capability policy or explicit absence |
| `activity.capability_policy.set` | Revision-checked Normal/Ask/Deny rules without granting authority or changing enabled state |
| `activity.capability_policy.enabled` | Explicit enable/disable; disabled blocks controlled checks and preserves the rules |
| `activity.objects` | Authenticated declaration metadata or explicit diagnostics for App references |
| `activity.object.attach` | Atomically attached reference; no App execution or new authority |
| `activity.operation.preview` | App-declared expected effects; no execution, authorization, or confirmed changes |
| `activity.receipts` | Immutable caller-reported results for the authenticated owner's Activity |
| `activity.receipt.record` | Append or idempotently retry a report; never execute or create authority |
| `activity.object_state.list` | Bounded observations, relationships and correction history |
| `activity.object_state.record` | Append an owner-scoped annotation or retraction without changing goal state |

No request accepts an owner UID or authorization grant. Policy drafts name
constraints, never permission sets. Root does not receive an implicit
cross-owner Activity view. Mutation routes use the existing
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
