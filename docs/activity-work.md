# Long-running Activity work

Automatic progression is an explicit execution mode of an existing Activity,
not another goal type or daemon. The shared Activity backend persists progress;
the existing `clawd` heartbeat, trigger scheduler, durable Job queue and
`claw-agentd` workers execute bounded segments. Terminal-only and desktop
installations use the same state and authority.

## Start and stop

From an installed Linux or WSL terminal, first create an Activity and configure
its [finite execution limits](activity-execution-limits.md). Then use its ID:

```bash
activity_id="00000000-0000-4000-8000-000000000001"
cos activity set-work "$activity_id" \
  --task "Research the topic, prepare a draft, and request my review" \
  --check-interval-secs 3600
cos activity work "$activity_id"
cos activity start-work "$activity_id" --expected-revision 1
```

Configuration starts stopped. Starting crosses the existing scheduler consent
and delegation boundary; Activity metadata supplies no permission. If consent
is required, the terminal waits in the same process for the existing approval
surface. Web/native clients surface the refusal and require an explicit retry
after the exact request is approved. No client silently retries an ambiguous
start or rebases a stale revision.

The fetched revision changes as the scheduler reserves work and records
handoffs. Read it before each control mutation:

```bash
cos activity work "$activity_id"
cos activity stop-work "$activity_id" --expected-revision 4
```

Stopping prevents future segments, including a reserved Job that has not been
admitted. It does not cancel an already running Job or undo effects. Use the
existing task controls to cancel a Job. Activity pause/end also gates future
admission; Job completion never completes the Activity.

## Progress and waiting

Each automatic Job has a Root-created Activity/generation/Job/session binding.
The model receives the unchanged owner instruction and a separately labelled,
untrusted handoff. It can use ordinary authorized tools and must finish with
`cos_activity_checkpoint` as its sole tool call in that response. A successful
handoff closes operational tool admission for that segment. The final allowed
model turn offers only that reporting tool, not further operational tools.

Handoffs contain a bounded summary, reported progress flag, completed-step
labels, attached canonical App object artifact references and one next action:
continue with a concrete next task, wait, request owner review, or report a
blocker. These are caller reports, not verified facts, effects, authorization
or goal completion. The supervisor validates and redacts the report under its
own Job identity before persisting it. Progress is applied only after the Job
has durably succeeded and containment cleanup has succeeded.

A continuation is admitted promptly after completion, without waiting an hour.
A wait releases the worker and uses the configured check interval (60–86400
seconds, default one hour). An optional exact existing event source/type can
wake waiting work sooner:

```bash
cos activity set-work "$activity_id" --expected-revision 2 \
  --task "Inspect incoming material and maintain the draft" \
  --wake-source project-watcher --wake-event artifact.changed
```

Use a producer that is actually installed; this does not install a watcher.
Only same-owner-visible events after the wait's recorded event watermark can
wake it. Old backlog is not replayed. Idle checks do not call a model.
Events observed while the Activity is paused or ended are consumed without
waking work; resuming does not replay them. Unavailable Activity storage blocks
automatic work visibly without blocking unrelated, unassociated triggers.

Only one automatic segment is reserved at a time; automatic dispatch also
waits for other nonterminal Jobs associated with the Activity. Existing queue
priority, execution limits, configured monetary accounting, consent,
capabilities and approval waits retain their ordinary enforcement.

## Review and recovery

Three consecutive continuing segments reporting no progress stop for review.
An intentional wait is not counted as a stalled continuation. A result
without a structured handoff, an execution failure, cancellation, missing
correlation or an indeterminate outcome also stops instead of automatically
repeating the original prompt. Execution limits and expiry remain independent
hard ceilings; reported progress cannot extend them.

After inspecting the previous Job and external state, resume explicitly:

```bash
cos activity work "$activity_id"
cos activity start-work "$activity_id" --expected-revision 9 \
  --next-task "Verify the existing draft; do not repeat the earlier submission" \
  --review-note "Inspected the previous result and checked the destination"
```

This note is an owner instruction, not evidence that effects were undone or
verified. Configuration edits cannot bypass a review hold.

The dispatch correlation is durable before queue publication. After a restart,
the scheduler reattaches the exact existing Job or reconciles its terminal
result; it never publishes a replacement merely because the reserved Job is
missing. Ordinary queue recovery still requeues only pre-execution phases
proven safe. Committed uncertain work needs inspection, not blind replay.
This is not a general exactly-once guarantee for external services. Automatic
retry of an uncertain effect still requires an operation-specific durable
idempotency or reconciliation contract.

Work state and its bounded immutable handoff history are local execution data
in Activity database schema 9. They are excluded from portable continuity.
Imported Activities remain paused with no automatic-work configuration or
scheduler delegation.
