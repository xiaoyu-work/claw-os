# Activity capability policies

An Activity capability policy adds enforceable constraints to work associated
with that Activity. Terminal, Agent Web and native desktop clients present the
same owner-scoped policy; they do not keep separate permission stores.

**A policy never grants a capability, bypasses App provenance, or replaces the
ordinary permission checks.** Activity goals and free-text planning boundaries
are still untrusted context, not policy enforcement. These controls are separate
from [finite execution limits](activity-execution-limits.md).

## Rule meanings

Each rule names one known verb from the
[core capability catalogue](../core/src/caps/catalog.rs):

| Mode | Meaning |
| --- | --- |
| `normal` | Continue through existing capability, provenance and other permission checks, within the listed scopes |
| `require_approval` | Require exact, single-use approval even when existing capabilities already cover the request |
| `deny` | Deny the entire verb, for every scope; approval cannot override it |

A missing rule means Normal with no additional Activity scope restriction.
An enabled policy with `rules: []` therefore adds no constraints; it does
**not** grant any permission.

Normal and Ask rules require 1–32 compatible scopes. The entire requested
capability must be covered by at least one listed scope, or the request is
denied. Combining partial matches is not a way to cover a broader request.
An Ask rule does not offer approval for requests outside its scope boundary.
Deny rules require `scopes: []`: this version does not support scoped exceptions
to a Deny rule.

The wire uses typed scopes:

```json
{"kind":"path","value":"/home/user/project/**"}
{"kind":"host","value":"example.com:443"}
{"kind":"name","value":"release/token"}
{"kind":"self-ref","value":"self"}
{"kind":"wild"}
```

The verb's catalogue entry determines its compatible scope kind. Raw `wild`
is illegal for path, host and name verbs; it is not a shortcut for a properly
typed scope. The broker rejects unknown verbs, duplicates, incompatible scopes,
control characters and malformed drafts, and owns canonicalization and actual
coverage decisions. Clients display the saved canonical values rather than
independently deciding access.

Path scopes must already be canonical absolute Linux paths. Home shortcuts,
`$` placeholders, dot segments and redundant separators are rejected, not
expanded by a form. Saved host scopes are ASCII-lowercased; rule and scope
ordering may change and duplicate scopes may be removed.
The editor does not trim scope values or invent additional resource-name rules.

A draft is at most 16 KiB of JSON and contains at most 64 unique known verbs.
Its only top-level field is `rules`; each rule has `verb`, `mode` and `scopes`.
Owner, enabled state, revision and timestamps are not draft fields.

## Terminal controls

Run from an installed Linux or WSL Claw OS environment with `clawd` available.
No model or graphical session is needed to read or manage policies.

```bash
activity_id="00000000-0000-4000-8000-000000000001" # replace with your Activity ID

cos activity capability-policy "$activity_id"

# Initial creation: omit --expected-revision only when no policy exists.
cos activity set-capability-policy "$activity_id" --policy '{
  "rules": [
    {"verb":"fs.read","mode":"normal","scopes":[
      {"kind":"path","value":"/home/user/project/**"}
    ]},
    {"verb":"net.dial","mode":"require_approval","scopes":[
      {"kind":"host","value":"example.com:443"}
    ]},
    {"verb":"fs.delete","mode":"deny","scopes":[]}
  ]
}'
```

The initial policy is revision 1 and enabled. Read the current policy before
each subsequent mutation, and use its returned revision:

```bash
cos activity capability-policy "$activity_id"

# These numbers assume no intervening policy changes.
cos activity disable-capability-policy "$activity_id" --expected-revision 1
cos activity set-capability-policy "$activity_id" --expected-revision 2 \
  --policy '{"rules":[{"verb":"fs.delete","mode":"deny","scopes":[]}]}'
cos activity enable-capability-policy "$activity_id" --expected-revision 3
```

`set-capability-policy` replaces the rule list, not just one row. Updating an
existing policy requires its current revision and preserves enabled state;
it does not silently re-enable a disabled policy. Stale revisions fail instead
of overwriting a newer edit. Read and review the new policy before explicitly
resubmitting; never automatically retry with a newer revision.

These commands support normal JSON output and the existing
`cos activity --help` and `cos activity <command> --schema` discovery.
`--policy` takes bounded draft JSON, not a filename or executable expression.

## Disabled is a stop boundary, not unrestricted access

**Disabling a stored policy blocks all controlled capability checks for the
Activity.** It does not delete the policy, remove its rules, reset its revision,
or turn constraints off to allow unrestricted work.

Creation, editing and enabling are allowed for active or paused Activities.
Reading and disabling an existing policy also remain available for completed
or cancelled Activities. Reopening a goal and enabling its policy are separate,
explicit actions.

Creating the first policy also invalidates attempts that started without one.
Policy edits and enable/disable changes invalidate old attempts through the
existing worker and App cleanup path. They do not undo effects already admitted
by the broker, and they do not themselves complete the goal. Pausing or
cancelling an Activity remains a separate lifecycle control.

## Approval and protection limits

Deny is checked before approval side effects: a denied request cannot trigger
or consume an override approval. Ask requires an exact, single-use confirmation
even for a held capability. A broader-scope grant cannot substitute for that
exact Ask approval. The approval is retired after one use even if its original
consent duration was `session` or `forever`.

App invocations and stateful tool calls settle their full requirement set
all-or-none at root, rather than consuming approvals in a local preflight.
Subsequent effects recheck the immutable root-grant policy binding without
asking again or spending another confirmation for that invocation or call.
Ordinary provenance and capability checks still apply.

This constraint is **not process-wide kernel protection against a compromised
same-UID Agent**. The exact App sandbox and broker boundary remain authoritative;
policy checks do not turn arbitrary code running under the Agent's UID into an
isolated process.

**Denying `fs.delete` is not a content-protection guarantee.** Content can still
be changed or destroyed through other admitted operations, such as `fs.write`
or process execution (`proc.exec`). Review the capabilities of the actual tool
or App rather than treating a verb name as a complete statement of its effects.
Capability policy is not a file diff, rollback mechanism, or promise that an
already-admitted operation can be stopped.

## Agent Web

The Activity detail page has a fixed **Capability policy** card. It displays
enabled state, revision and saved rule rows. **Configure capability policy** or
**Edit capability policy** opens typed verb, mode and scope fields. Catalogue
metadata is compiled into the core; loading the catalogue or editing a form
does not read files, inspect credentials, resolve objects or execute Apps.

Read errors, invalid responses, owner mismatches and busy states disable
mutations instead of falling back to local policy. Unsaved drafts survive
failed writes. Background refreshes do not rebase an edit or retry it. After a
conflict, use **Refresh**, compare the saved rules, then explicitly choose
**Use refreshed revision for this draft** before saving again. An ambiguous
acknowledgement also requires refresh; the original request may already have
succeeded.

Write acknowledgements must match the requested rules and scope values, allowing
only the documented ASCII host folding, ordering and deduplication. Replacing
a scope with a broader or narrower one, trimming values or rewriting a path is
not an acceptable acknowledgement, even when the returned verb and mode match.
The client compares data; it never substitutes its own coverage or filesystem
resolution for the broker's decision.

The card can disable an existing policy on a terminal Activity but cannot edit
or re-enable it until the goal is reopened. Rule labels and scope values are
inert text, not links, commands or model-generated UI. No policy is persisted
in browser storage.

## Shared contract

| Broker request | Result |
| --- | --- |
| `activity.capability_policy.get {id}` | `{schema:1, activity_id, capability_policy: Policy \| null}` |
| `activity.capability_policy.set {id, expected_revision?, policy:{rules}}` | Saved `Policy` directly |
| `activity.capability_policy.enabled {id, expected_revision, enabled}` | Saved `Policy` directly |

`Policy` contains `activity_id`, `owner_uid`, `revision`, `enabled`, `rules`,
`created_at` and `updated_at`. The broker derives ownership from authenticated
peer credentials; clients cannot select another owner or submit an authority
grant. Revision checks and durable state live in the shared core service.

Root derives the Activity from its retained Job and lease and records a bounded
policy snapshot in the task stream. In private worker protocol v8, capability
mediation uses bounded verb/scope `Boundary`, `Consume` and `Request` messages,
not worker-supplied policy or identity selectors. This is an internal worker
protocol version, not a change to the public policy schema above. The recorded
snapshot supports reconstruction; it is not a model prompt enforcement scheme.

The authenticated Web adapters use `GET`/`POST
/api/activities/{id}/capability-policy` and `POST
/api/activities/{id}/capability-policy/enabled`. The Activity ID is in the URL,
not the body. The static metadata-only `GET
/api/activities/capability-policy-catalog` supplies catalogue labels and scope
kinds, not permission state.

For goal lifecycle, task associations and persistence, see
[Activities](activities.md). For the Web implementation and validation commands,
see the [Agent Web README](../core/src/agent/web/ui/README.md).
