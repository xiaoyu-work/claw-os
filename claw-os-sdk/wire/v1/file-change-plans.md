# App-Reported File Change Plans

[`file_change_plan.schema.json`](file_change_plan.schema.json) defines the
public `FileChangePlan` value for an App-owned staged file proposal. It may
contain an App-computed, bounded diff, but it is **App-reported data**, not an
OS-confirmed mutation, authority, or a universal rollback facility.

The SDK owns this value contract and its generated validators, not the FS
App's private plan store, lifecycle, permission checks, or replacement
implementation. Terminal and desktop presentations consume the same backend
value. A receipt display must not promote this value into a confirmed effect.

## Public Value

The object is closed to unknown fields. All fields in this table are required:

| Field | Contract |
| --- | --- |
| `schema`, `kind` | Exactly `1` and `"file_change_plan"` |
| `plan_id` | UUID identifying the proposal |
| `path` | Absolute target path, at most 4096 UTF-8 bytes |
| `state` | `draft`, `applying`, `applied`, `conflicted`, `indeterminate`, or `expired` |
| `before_exists` | Boolean describing the App-reported preimage |
| `before_sha256` | Canonical SHA256 string or explicit `null` |
| `after_sha256` | Canonical SHA256 string describing the proposal |
| `before_bytes`, `after_bytes` | Mathematical integers in `0..=65536` |
| `would_change` | App-reported prediction, not an observed mutation |
| `review` | SHA256 fingerprint binding the immutable proposal; **data, not authorization** |
| `reference` | Canonical App object URI identifying the path and plan |
| `diff`, `diff_truncated` | App-reported diff of at most 65536 UTF-8 bytes and a boolean truncation flag |
| `created_at`, `expires_at` | App-reported RFC3339 timestamps |
| `warnings` | At most 16 strings, each at most 1024 UTF-8 bytes |

The optional fields `snapshot`, `applied_at`, and `diagnostic` accept a string
or `null`; optional `changed` accepts a boolean or `null`. Non-null `applied_at`
is RFC3339. A snapshot value is an identifier or reference, not snapshot
contents or proof that restoration is possible. `changed` and `state` remain
App reports even when they say a plan was applied.

There are no owner, grant, authority, private preimage, or private proposed
contents fields. Private plan storage must not be serialized into this public
value. The bounded diff is the intentional review surface, not permission to
expose additional private contents.

All SHA256 values use `sha256:` followed by exactly 64 lowercase hexadecimal
digits. The App/core owns semantic validation: UUIDs, absolute paths, hashes,
timestamps, reference correspondence, UTF-8 byte limits, warning limits, and
consistency with the private proposal and its lifecycle.

## Proposal Identity and Lifecycle

Proposal content is immutable, while the App-reported state may evolve.
The `review` fingerprint binds the immutable proposal data so a reviewer can
refer to the proposal they saw. Possessing a plan ID, reference, or matching
fingerprint does not grant permission to apply it.

Normal permissions, approvals, and current-file precondition checks still
apply. Uncooperative external writers can race after the final precondition
check. **Do not describe this protocol as atomic compare-and-swap**, even if
an implementation uses an atomic replacement for the final filesystem step.
Do not promise that a snapshot supplies a universal inverse or overrides the
permissions needed for a later restoration.

This App-specific staged plan does not change the
[metadata-only operation preview](operation-effects.md) contract. A generic
metadata preview still must not execute App code, read object data or
credentials, or promise real file diffs.

## Existing App Object Reference

Use the existing [ObjectRef helpers](object-references.md), with:

```json
{
  "app_id": "fs",
  "object_type": "change-plan",
  "object_id": "/home/user/example.txt",
  "revision": "00000000-0000-4000-8000-000000000001"
}
```

The canonical URI is:

```text
app://fs/change-plan?id=%2Fhome%2Fuser%2Fexample.txt&revision=00000000-0000-4000-8000-000000000001
```

The resolver declaration uses `plan_show`, with `id_arg: "path"` and
`revision_arg: "plan"`. The path is the object ID, and the optional plan flag
selects a specific plan. Omitting that flag selects the newest plan for the
path; a public value's revision-bearing reference identifies its proposal.

Both the file-plan limits and the existing ObjectRef limits apply. In
particular, ObjectRef currently limits the path used as its object ID to 1024
UTF-8 bytes. Do not truncate a path or introduce another URI parser to force a
reference to fit; an unrepresentable reference must fail semantic validation.

## Generated SDK Surface

All four bindings expose the generated `FileChangePlan` type:

| Language | Structural validator |
| --- | --- |
| Rust | `generated::validate_file_change_plan(&serde_json::Value) -> Result<(), WireDecodeError>` |
| Python | `generated.validate_file_change_plan(value)`, raising `WireDecodeError` |
| Node | `validateFileChangePlan(value)`, asserting the `FileChangePlan` type or throwing `WireDecodeError` |
| Go | `ValidateFileChangePlan(value any) error` |

These validators enforce the supported structural contract: required fields,
constants, state enum, primitive and nullable types, closed field set, and
numeric byte-count bounds. Nullable fields use supported `oneOf` branches,
not an unsupported union-type shortcut.

The small generated validator subset does **not** enforce UTF-8 string bounds,
UUID/hash/path/time semantics, or `maxItems`. Those checks remain App/core
responsibilities; JSON Schema character counts must not be advertised as
UTF-8 byte limits. Validate before projecting the value, and perform the
semantic checks appropriate to the consumer. Validation alone is never an
authorization decision.

Required nullable `before_sha256` remains present as `null` when unknown.
Rust/Go serializers may omit optional null fields; omission and null both mean
that no value is reported for those optional fields. Schema and SDK package
versions remain unchanged by this additive value contract.

## Conformance

[`file_change_plan.vectors.json`](file_change_plan.vectors.json) is shared by
the Rust, Python, Node, and Go tests. It tests structural acceptance/rejection,
all states, nullable fields, numeric bounds and types, and forbidden owner,
grant, and private-content fields. These are structural vectors, not evidence
that a corresponding file, snapshot, or proposal exists.

Run Go conformance tests with `-count=1`: the shared vectors live outside the
Go module and must not be replaced by a cached test result.
