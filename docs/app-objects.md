# App-owned objects

Apps can opt into an object contract without changing their runtime, identity,
permission model, or existing operations. An object reference identifies
App-owned data; it is neither a grant nor a copy of that data.

Terminal, Web, and native desktop Activities use the same core object catalogue
and owner-scoped broker operations. There is no desktop object database or
separate "native App" integration category.

## Declare an object type

Add an optional `objects` map to the existing `app.json`:

```json
{
  "objects": {
    "entry": {
      "label": { "en": "Key-value entry" },
      "summary": { "en": "An entry addressed by its exact key." },
      "resolve": { "operation": "get", "id_arg": "key" }
    }
  }
}
```

The resolver names an existing one-shot operation. Its ID argument must be a
required, non-repeatable string-kind argument (`name`, `text`, `path`, or
`host`). An optional `revision_arg` names an optional `name`/`text` flag.
Additional arguments must be optional flags. Resolver operations cannot
consume stdin or depend on trusted resolvers, derived defaults, or conditional
required arguments.

These constraints let the OS construct an invocation without guessing missing
context. Literal defaults, argument validation, capability derivation, App
permission review, worker isolation, and audit still belong to the ordinary
App execution path. Resolution has that operation's normal semantics and
possible effects; the declaration does not establish that arbitrary App code
is pure or safe.

Labels require an English translation and follow the existing localization
convention. Type names use `[a-z][a-z0-9_-]*`, at most 64 bytes. An App may
declare at most 64 object types; participating App IDs are at most 128 bytes.
Apps without declarations continue to work unchanged.

Bundled examples reuse existing operations: `kv/entry` resolves through
`kv get`, and `fs/file` resolves metadata through `fs stat`. File IDs must be
absolute paths so their meaning does not depend on whether a terminal or
desktop initiated the request. A file reference identifies the object at that
path, not a persistent inode; renames require updating the reference.

## Reference format

The public SDK wire definition is
[`object_ref.schema.json`](../claw-os-sdk/wire/v1/object_ref.schema.json).
Rust, Python, Node, and Go helpers share the same canonical representation:

```json
{
  "app_id": "kv",
  "object_type": "entry",
  "object_id": "release.status"
}
```

Its URI is:

```text
app://kv/entry?id=release.status
```

Opaque IDs occupy the query value, not path segments. Slashes, spaces, Unicode,
and shell metacharacters are percent-encoded without changing the ID:

```text
app://fs/file?id=%2Fhome%2Fuser%2Fproject%2Frelease.md
```

An optional revision is encoded after the ID as `&revision=...`. Resolving a
revision-pinned reference fails explicitly unless the declaration provides a
revision argument; it never drops the revision and reads something else.
The App is responsible for honoring its revision parameter. App package
version and object-data revision are different concepts.

IDs are nonempty, control-free UTF-8, at most 1024 bytes; revisions are at most
128 bytes. Canonical references fit the existing 4096-byte Activity reference
limit. Credentials, ports, fragments, unknown/repeated query keys, invalid
UTF-8, and noncanonical spellings are rejected. Use the SDK helpers or
`cos object reference` rather than concatenating unescaped strings.

## Inspect and resolve

These commands run inside Linux or WSL Claw OS:

```bash
cos object catalog
cos object catalog kv
cos object reference kv entry release.status
cos object describe 'app://kv/entry?id=release.status'
cos object resolve 'app://kv/entry?id=release.status'
```

`reference` only formats an identity. `catalog` and `describe` read authenticated
package metadata and never execute an App or fetch the referenced data.
`resolve` is explicit execution through the existing App gate, including
`agent.invoke` and the selected operation's normal checks. Resolution errors
are errors, not fabricated empty objects.

Opaque IDs are passed as argv values, never through a shell. CLI reference
creation accepts `--` before an ID that resembles an option:

```bash
cos object reference kv entry -- --schema
```

Invocation previews expose structured argv, not a command assembled by joining
unquoted strings. Quote a complete URI when supplying it to a shell.

## Associate an object with an Activity

```bash
activity_id="00000000-0000-4000-8000-000000000001" # use an existing Activity ID

cos activity attach-object "$activity_id" \
  --label "Release status" \
  --app kv --type entry --object-id release.status

cos activity objects "$activity_id"
```

The shared broker verifies the App package and declaration, then atomically
adds the canonical URI to the Activity's existing resource list. Reattaching
the same reference updates its display label rather than duplicating it.
This does not read the object, execute an App, or grant new permissions.

Web and native desktop offer the same attachment and description operations.
Descriptions distinguish:

| Status | Meaning |
| --- | --- |
| `declared` | The current authenticated App manifest declares this type and resolver |
| `unavailable` | The App/declaration cannot currently be used, including quarantine or unsupported revision binding |
| `invalid` | A stored App-style reference does not follow the canonical contract |

**A verified declaration does not prove object existence, freshness, semantic
truth, or permission to read it.** Only an explicit, authorized App operation
can return current object data.

Legacy file/URL references remain inert and readable. Uninstalling an App,
changing its authenticated declaration, or revoking its package does not erase
the Activity: its reference remains, with an actionable description error.
Executable bytes are checked separately by the ordinary App launch path;
manifest inspection is not a claim that changed executable code is valid.
Object data remains owned by its App.

Activity persistence stays at schema version 1: references use the existing
`label`/`reference` fields, not a second store or a migrated data copy.
Automatic object-data fetching, a global relationship graph, effect previews,
and event-driven delegation are later increments.
