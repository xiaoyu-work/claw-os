# App Object References

This is the normative wire-v1 data contract for identifying App-owned objects.
It adds neither a store nor a transport. App data stays in Apps. A reference
does not prove existence, freshness, readability, or permission to invoke an
operation.

Headless and desktop presentations use the same SDK semantics and shared
backend. Neither presentation may reinterpret an object ID through a separate
URI parser or treat a reference as authority.

## Structural Contract

[`object_ref.schema.json`](object_ref.schema.json) defines `ObjectRef`:

```json
{
  "app_id": "notes",
  "object_type": "note",
  "object_id": "draft/1",
  "revision": "7"
}
```

`app_id`, `object_type`, and `object_id` are required strings. `revision` is an
optional string; when absent, omit the key, not a `null` or empty value.
Additional fields, including owner, grant, URI, or data payload fields, are
invalid. There is no nested `wire_version`: this contract is versioned by its
location under `wire/v1`.

When decoding JSON, use the generated structural validator before materializing
a language struct. Ordinary struct deserialization alone is not full JSON
Schema validation. Generated validators enforce required fields, string types,
and the closed field set. The pure object helpers enforce the additional
semantic rules below; unsupported JSON Schema keywords are not presented as
generated runtime checks.

## Semantic Bounds

| Component | Rule |
| --- | --- |
| `app_id` | Whole-string `[a-z][a-z0-9_-]*`, at most 128 UTF-8 bytes |
| `object_type` | The same alphabet, at most 64 UTF-8 bytes |
| `object_id` | Nonempty valid UTF-8, at most 1024 bytes |
| `revision` | When present, nonempty valid UTF-8, at most 128 bytes |
| Complete URI | At most 4096 UTF-8 bytes |

Object IDs and revisions reject Unicode General Category `Cc` control
characters, including NUL, C0 controls, DEL, and C1 controls. Unpaired surrogates
are not valid Unicode scalar values. Helpers must never replacement-decode
invalid UTF-8 or replacement-encode an invalid string into a different identity.

Opaque IDs and revisions are never trimmed, case-folded, Unicode-normalized,
or interpreted as paths. Leading/trailing spaces, slashes, dot segments, and
Unicode normalization differences remain significant.

## Canonical URI

The only accepted spelling is:

```text
app://<app_id>/<object_type>?id=<encoded-object_id>
app://<app_id>/<object_type>?id=<encoded-object_id>&revision=<encoded-revision>
```

Query values are encoded from UTF-8 bytes using RFC3986 percent encoding.
ASCII `A-Z`, `a-z`, `0-9`, `-`, `.`, `_`, and `~` remain unchanged. Every other
byte becomes `%XX` with uppercase hexadecimal digits. Spaces become `%20`,
never `+`. For example:

```text
app://notes/note?id=draft%2F1
app://notes/note?id=%20draft%201%20&revision=review%2F7
```

Keeping the opaque ID in a query value prevents URL path normalization from
changing slashes or dot segments. Decode percent escapes exactly once.

Parsing requires the exact lowercase scheme, one App component and one type
segment, and the literal `id` key followed only by an optional `revision` key.
Credentials, ports, fragments, unknown or repeated keys, missing IDs, malformed
escapes, and invalid UTF-8 are rejected. Reformat the parsed value and require
byte-for-byte equality with the input. This also rejects lowercase hex escapes,
escaped unreserved characters, reordered keys, and alternative URL spellings.

## Public Helpers

| Language | Pure API |
| --- | --- |
| Rust | `objects::validate(&ObjectRef)`, `objects::format_reference(&ObjectRef)`, `objects::parse_reference(&str)` |
| Python | `objects.format_reference(ref)`, `objects.parse_reference(uri)` |
| Node | `objects.format_reference(ref)`, `objects.parse_reference(uri)`; camelCase aliases `formatReference` and `parseReference` |
| Go | `FormatReference(ObjectRef)`, `ParseReference(string)` |

Rust exposes `ObjectRefError` through both `objects` and the crate root.
Python and Node expose `objects.ObjectRefError`; Go returns `*ObjectRefError`.
Go's generated `Revision` is `*string` so a present empty revision cannot be
silently collapsed into absence. None of these helpers performs App discovery,
filesystem access, IPC, resolution, or permission checks.

## App Manifest Declaration

An existing App may add this optional fragment to its manifest:

```json
{
  "objects": {
    "note": {
      "label": {"en": "Note"},
      "summary": {"en": "An App-owned note"},
      "resolve": {
        "operation": "show",
        "id_arg": "note_id",
        "revision_arg": "revision"
      }
    }
  }
}
```

[`manifest.schema.json`](manifest.schema.json) defines at most 64 object types.
Keys use the object-type component rules. Each closed declaration requires a
localized `label` and a `resolve` object, with an optional localized `summary`.
Localized text requires English (`en`). The closed resolver requires
`operation` and `id_arg`; `revision_arg` is optional. There is no new App
category.

Before trusting a declaration, the backend must authenticate its App package
and validate the declaration against the operation manifest. The operation and
bound ID/revision arguments must exist; bound arguments must be non-repeatable
string-kind arguments (`text`, `name`, `path`, or `host`). The operation must
not consume stdin or leave required/conditional arguments unbound. Other
arguments must not use `trusted_resolver` or `default_from`.

Manifest key/count limits and operation cross-reference checks belong to the
full manifest schema and the backend's verified-manifest validation, not to
the small generated SDK validator subset. Resolving a reference uses ordinary
App operation dispatch, capabilities, approvals, and audit. A resolver
declaration does not promise purity or permission, and a requested revision
must not be silently discarded.

## Conformance

All four language test suites consume
[`object_ref.vectors.json`](object_ref.vectors.json). It covers canonical
round-trips, preserved opaque identity, rejection of alternative URI spellings,
malformed UTF-8, byte-boundary cases, and generated decoder errors. Existing
manifests without `objects` and references without `revision` remain valid.
Use `go test -count=1` when checking wire changes: the shared vector file lives
outside the Go module, so a cached test result must not stand in for a fresh run.
