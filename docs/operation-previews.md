# App operation effect previews

An App can declare expected effects alongside an existing operation:

```json
{
  "effects": [
    {
      "kind": "update",
      "label": { "en": "Write the requested file contents" },
      "target_arg": "path",
      "recovery": "unknown"
    }
  ]
}
```

The operation's runtime, arguments, capability needs, and execution behavior
remain unchanged. Effect declarations describe intent; they never grant
authority or prove what App code will actually do.

## Shared preview service

Inside an installed Linux or WSL Claw OS system:

```bash
cos operation preview fs write -- /home/user/draft.md --content Draft
cos operation preview kv get -- release.status
cos operation preview fs stat --activity ACTIVITY_ID -- /home/user/draft.md
```

Replace `ACTIVITY_ID` with an existing Activity UUID. The delimiter separates
preview options from the App's argv; App arguments are data and are not run.

The terminal uses `operation.preview` or `activity.operation.preview`. Activity
Web and native desktop object views call the same owner-scoped service using
the declared object's existing invocation. They do not implement a local
planner, execute the operation, or approve anything.

## What a preview means

The response includes the authenticated App package digest, operation identity,
App-declared effects, requested resource targets, unresolved runtime arguments,
and explicit caveats. It always reports:

```json
{
  "authorization_checked": false,
  "executed": false,
  "effects_confirmed": false
}
```

This service validates the invocation's declared argument grammar and literal
defaults without executing App code. It does not read object data or
credentials. Package files may be read to authenticate the declaration.

Path targets are **requested values**: the preview does not canonicalize
symlinks or claim which final filesystem object will be authorized. Normal App
execution still resolves paths, selects runtime providers, performs consent
and capability checks, and records its actual result.

An operation with no effect declarations has **unknown effects**, not an
implicit read-only guarantee. A missing target is `unresolved`; a declaration
without `target_arg` is `unspecified`. Runtime-selected arguments remain
explicitly unresolved rather than triggering credential lookup or a guessed
provider choice.

Previews contain resource targets, not arbitrary text/content arguments. They
are not receipts or file diffs, and displaying one never starts a job.

## Declaration contract

Kinds are `read`, `create`, `update`, `delete`, `external`, and `execute`.
Recovery is `not_applicable`, `reversible`, `compensatable`, `irreversible`, or
`unknown` (the default).

Recovery categories are App-declared guidance only. `reversible` does not
prove an inverse was recorded; `compensatable` does not mean an external
effect can be erased. The first bundled write/delete declarations therefore
use `unknown` until actual execution evidence can establish more.

Each operation may declare at most 16 effects. Labels require English, are
limited to 512 UTF-8 bytes per locale, and cannot contain controls.
`target_arg`, when present, must name a declared `path`, `host`, or `name`
argument; repeated resource arguments are supported. Arbitrary text and
message content cannot be exposed as resource targets.

Existing manifests remain valid. Omitted or empty `effects` means no effect
metadata was supplied. The same declaration shape is generated into all
public SDK language bindings.

Execution receipts, OS-confirmed mutations, and genuine staged file-change
previews are separate contracts built on the existing journal and snapshot
boundaries; this metadata preview does not claim to implement them.

The explicit [execution receipt path](execution-receipts.md) now captures App
return values as caller-reported evidence while preserving the normal App
permission boundary. It does not promote preview declarations or reports into
OS-confirmed changes.

[Staged file plans](file-change-plans.md) provide a separate, permission-gated
baseline read and real diff. They do not turn this metadata-only preview
service into a file reader or execution authority.
