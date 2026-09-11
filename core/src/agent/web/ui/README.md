# cos agent serve — web UI

This is a Vite + React 18 + Tailwind v4 + Radix UI single-page app
that gets compiled into the cos binary by include_dir!. The visual
language is ported verbatim from open-agents/apps/web (oklch dark
tokens, sidebar + inset shell, shadcn primitives).

## Re-building after a UI source change

The compiled bundle lives in dist/ and is **committed to git**
so cargo builds don't require bun/node.

To rebuild:

```bash
# one-time
~/workspace/claw-os/scripts/install-bun.sh

# every change
cd core/src/agent/web/ui
~/.local/bin/bun run typecheck
~/.local/bin/bun test
~/.local/bin/bun run build
```

Then cargo build -p cos --bin cos will pick up the new dist files
via include_dir!. Commit the regenerated dist/.
Install dependencies with `bun install` only when setting up a checkout or when
the chosen validation reports missing dependencies. If `dist/` already has
unrelated changes, do not overwrite it during validation; use the isolated
build below and integrate regenerated distribution files deliberately.

## Layout

- src/main.tsx          — React entry
- src/App.tsx           — sidebar + inset shell + hash router
- src/lib/api.ts        — fetch + SSE wrappers (token-aware)
- src/lib/notifications.tsx — live notification subscription and browser opt-in
- src/lib/router.ts     — hash-based router (no HTML5 history)
- src/components/       — sidebar, token gate
- src/components/ui/    — shadcn primitives (copied verbatim from OA)
- src/pages/            — Activities, durable chat/tasks, approvals, notification Inbox, raw system events, settings
- src/app/globals.css   — OA's oklch tokens (light + dark + sidebar)

Chat, Tasks, approvals, and Inbox use owner-scoped `clawd` routes. Session
history is read by the user-owned Web process from the same owner partition
the worker writes. Approval decisions invoke the installed polkit helper; the
Web process never gains direct permission-decision authority.

## Activities

`#/activities`, `#/activities/new`, and `#/activities/:id` present the
[shared Activity service](../../../../../docs/activities.md). The UI holds only
fetched views and unsaved forms; the broker owns persistence, state changes,
ownership, and work submission. The authenticated adapters are:

| HTTP route | Broker command |
| --- | --- |
| `GET /api/activities?state=active&limit=100` | `activity.list` |
| `POST /api/activities` | `activity.create` |
| `GET /api/activities/{id}?limit=100` | `activity.get` |
| `POST /api/activities/{id}/update` | `activity.update` |
| `POST /api/activities/{id}/transition` | `activity.transition` |
| `POST /api/activities/{id}/run` | `activity.run` |
| `GET /api/activities/{id}/objects` | `activity.objects` |
| `POST /api/activities/{id}/objects` | `activity.object.attach` |
| `POST /api/activities/{id}/operation-preview` | `activity.operation.preview` |
| `GET /api/activities/{id}/receipts?limit=100` | `activity.receipts` |
| `GET /api/activities/{id}/object-state?limit=100` | `activity.object_state.list` |
| `POST /api/activities/{id}/object-state` | `activity.object_state.record` |

Creation saves a goal without starting work. The detail view shows goal,
completion criteria, planning boundaries, inert resource references, job
progress/result previews, and associated sessions. Submit work in a new session
or explicitly continue an associated session. Tasks, approval decisions, and
conversation history open the existing `#/tasks`, `#/approvals`, and
`#/chat/:id` views; Activities introduce no new approval authority.

Completion requires a user-written confirmation note. Successful jobs do not
complete goals automatically. Pausing or cancelling gates subsequent work,
without undoing or stopping current effects; use Tasks to cancel a job.
Completed/cancelled goals must be explicitly reopened before editing or running.
Boundaries are planning text, not enforced permissions; ordinary capabilities
and approval checks remain in force.

### App object references

The detail view's **App object references** card attaches references using a
label, App ID, object type, opaque object ID, and optional revision. For example,
the attachment API takes typed fields, never a client-formatted URI:

```json
{
  "label": "Release status",
  "object": {
    "app_id": "kv",
    "object_type": "entry",
    "object_id": "release.status"
  }
}
```

The broker verifies the App/type and atomically appends or relabels the canonical
reference in the current Activity resource list. The UI refetches that metadata;
it does not parse/format App URIs, replace resources to attach an object, or
maintain an object-data store. Plain resources and existing create/edit/state
controls remain available. Finish a planning edit before attaching an object,
so an older edit form cannot overwrite the new resource.

`declared` authenticates only the current signed App manifest and object type.
It does **not** establish object existence, freshness, or permission to access
data. `unavailable` and `invalid` display the broker's diagnostic while retaining
the saved reference. Neither describing nor attaching an object runs App code.
References, labels, summaries, and optional invocation previews are inert text;
invocation arguments remain structured JSON, not a shell command or an
execution action. Ordinary capability and approval gates still govern explicit
App execution. See the [shared object contract](../../../../../docs/app-objects.md).

Object descriptions use the same abortable refresh guards as Activity views.
They refetch after attachment and other metadata changes; read failures or
malformed/untrusted descriptions are visible and hide stale descriptions until
a successful refresh, without disabling unrelated Activity work.

### App-declared effect previews

Each declared object offers **Preview effects** for its existing invocation.
The request uses `{app_id, operation, args}` with argv preserved as an array;
the Activity ID belongs only in the URL. No preview is requested automatically.
There is no execution, approval, undo action, URI navigation, or local effect
planner behind this button.
Previewing remains available for paused and completed Activities; it does not
require editability, resume work, or change the Activity's state.

The result displays the backend's package digest and operation identity,
App-declared effects and recovery labels, requested resource targets,
unresolved runtime arguments, and notes. It is metadata, not a receipt, file
diff, proof of safe execution, or authorization. Missing declarations mean
**unknown effects**, never an implicit read-only operation. Requested targets
are not final canonical paths. Recovery categories are App claims, not an
undo guarantee, and compensation need not erase an external effect.

The preview never displays raw argv as effect content or guesses targets from
arguments. All labels, targets, and notes remain inert text. Responses must
explicitly say `authorization_checked: false`, `executed: false`, and
`effects_confirmed: false`; malformed or mismatched responses show an error,
not a cached success. Retrying is another explicit preview request.

Preview state uses the existing abortable read guards in manual mode and is
scoped to the Activity, object reference, App version, and exact invocation.
Changing that scope aborts old reads; simultaneous object previews stay
independent. Previews do not modify Activity resources or task state. See the
[shared preview contract](../../../../../docs/operation-previews.md).

### Caller-reported receipts

Activity detail displays the shared ledger through a GET-only receipt adapter.
There is no Web receipt authoring, execution, retry-recording, update, or deletion
action. Receipts load with the initial detail view, participate in its existing
refresh action, and also have a dedicated **Refresh receipts** button. They
remain readable while an Activity is paused, completed, or cancelled.

The source is always **caller-reported**, not OS-confirmed execution or mutation
evidence. Outcomes are displayed as `returned`, `reported_error`, or
`indeterminate`, never as proof that changes were applied. An error does not
establish that no side effect occurred, and an indeterminate result does not
establish whether an App process started. Neither report content nor a returned
outcome completes the Activity or grants permissions.

The received time is the broker's recording time, not execution time. Result
kind, reported original byte count and SHA-256, bounded preview, truncation, and
reported errors are displayed as data. The hash is not an execution attestation
or a hash of the redacted preview. JSON/text previews are never parsed as HTML,
interpreted as instructions, or used to infer observed effects.

Each receipt has either a historical App declaration snapshot authenticated
at recording time or an explicit declaration error from that time. A snapshot
does not establish current App validity after package changes or revocation,
and never upgrades the execution report beyond `caller_reported`. Matching a
signed manifest authenticates that metadata only, not the execution, output,
effects, or goal achievement. Missing
declarations retain their diagnostic rather than hiding the report. Empty
effect metadata remains unknown, not an implicit read-only claim.
Database migrations remain core-owned; Activity and receipt presentation
schemas remain version 1.

Malformed provenance, mismatched Activity/owner identity, duplicate receipt
identifiers, or read failures hide the receipt list with a visible diagnostic.
The existing abortable read guards keep late responses from replacing another
Activity's reports. Receipt refreshes do not delay unrelated Activity actions.
See the [shared receipt contract](../../../../../docs/execution-receipts.md).

### Object state and history

The fixed **Object state and history** card reads and submits the same bounded
drafts as terminal/native clients. It distinguishes reported user statements,
Agent inferences, existing App receipt links, planning relationships and
retractions. Author classifications and reported time windows are not
authentication, semantic truth or verified freshness.

Subjects and relation targets are selected from existing App references;
the UI does not construct App URIs or fetch App data. Corrections append a
fresh entry with `supersedes`, while retractions keep the previous text and
source in history. Submission retries keep the same UUID; stale corrections
remain explicit conflicts. Forms and late responses are scoped to the selected
Activity. Read errors or an owner mismatch hide the history, not substitute a
local store. Terminal Activity states do not hide historical annotations.

See [object-state semantics](../../../../../docs/object-state.md). This card
neither authors execution receipts nor changes goal completion or permissions.

### Refresh behavior

Views refetch after mutations and on focus/notification changes. Activity
detail polls every three seconds while associated jobs are queued, running, or
waiting for approval, and every ten seconds otherwise. Lists, object declarations,
and receipts poll every ten seconds; effect previews remain explicitly requested.
Stale reads are aborted and cannot replace a newer selection. Read errors remain
visible; there is no local fallback. Lists, job projections, and receipts show
at most 100 records.

### Activity validation

From this directory, with existing dependencies, Node.js 22+ and an installed
Chromium-based browser:

```bash
bun run typecheck
bun test test/activities.test.ts test/activity-views.test.tsx test/operation-preview.test.ts test/activity-receipts.test.ts test/object-state.test.ts
bun run build --outDir .activity-validation/dist
bun run test:browser
```

When Bun is not on the native PATH, `npm exec --yes --package=bun -- bun test`
runs the same unit suite without adding a project dependency.

The browser regression serves that isolated production build, exercises the
token exchange and authenticated mocked API, and requires completed UI actions:
create/list/detail, persistence across reloads, live job progress, existing
approval/session/task flows, continuation, edits, explicit state changes, and
stale read/mutation/creation races. Object coverage includes typed attachment
with and without a revision, canonical backend resources, concurrent plain
resources, declaration status/errors, malformed or untrusted descriptions,
inert metadata/invocations, and stale object-read/attachment races.
Effect-preview coverage verifies explicit requests, opaque argv, every effect
kind and recovery category, requested-only targets, unresolved/unknown states,
rejected execution/authorization claims, inert output, and late previews
across objects and Activities.
Receipt cases cover source/identity validation, recording time, bounded inert
JSON/text/empty results, errors and uncertain outcomes, declaration failures,
existing/dedicated refresh, late reads, no goal completion, and read-only access
on paused and terminal Activities.
Console errors and unexpected outbound
requests fail the test. It neither contacts a model nor uses real credentials.
Browser profiles stay under `.activity-validation/` and are removed after the
test; the build never touches `dist/`. Set `ACTIVITY_BROWSER` to an installed
browser executable or `ACTIVITY_UI_DIST` to another prebuilt output if needed.

Object-state cases exercise actual completed form submissions, correction and
retraction history, expired/unknown window labels, inert statement/receipt
content, relationship links, unchanged Activity/job state, persistence across
reload and selection-safe late reads/writes.
