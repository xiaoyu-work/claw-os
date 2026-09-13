# App Effect Declarations and Metadata-Only Previews

An App may describe an ordinary operation's or MCP tool's expected effects
in its manifest. This is optional, App-declared guidance for a metadata-only preview. It is not
authority, evidence of execution, or a system-confirmed outcome.

Terminal and desktop presentations share the same backend metadata contract.
There is no separate presentation-specific effect store or execution path.

## Declaring Effects

Add an `effects` array to an existing operation or `mcp.tools` declaration in
[`manifest.schema.json`](manifest.schema.json). For a command that already
declares a `path` argument, this fragment describes a possible update:

```json
{
  "effects": [
    {
      "kind": "update",
      "label": {"en": "Update the requested document"},
      "target_arg": "path",
      "recovery": "unknown"
    }
  ]
}
```

Keep the command's normal `args`, `needs`, and execution behavior. Declaring
an effect neither adds permissions nor replaces any capability, approval,
validation, or audit requirement.

The same optional declaration attaches to `operations.<command>.effects` and
`mcp.tools[].effects`. Both use the identical bounded `operationEffect` item
contract. MCP-only commands can carry this metadata without an App-local
legacy operation or execution fallback.

SDK MCP runtimes accept the metadata while keeping tool arguments rooted in
the declared `args`. Effects do not become tool inputs, inferred read-only
annotations, permission grants or execution results. Generated manifest
bindings retain the declarations for metadata consumers. Core validates their
bindings against the selected ordinary operation or MCP tool before producing
a preview through the existing App command contract.

The array contains at most 16 entries. Each entry is closed to unknown fields:

| Field | Contract |
| --- | --- |
| `kind` | Required: `read`, `create`, `update`, `delete`, `external`, or `execute` |
| `label` | Required localized text, including `en`; every locale is at most 512 UTF-8 bytes |
| `target_arg` | Optional name of a declared `path`, `host`, or `name` argument; repeatable arguments are allowed |
| `recovery` | Optional: `not_applicable`, `reversible`, `compensatable`, `irreversible`, or `unknown`; omission means `unknown` |

Core manifest validation owns the target binding and condition checks and the
per-locale UTF-8 byte bound. The shared `localizedText` schema still requires
English and string-valued locales. JSON Schema character counts are not UTF-8
byte counts; generated bindings must not claim to enforce that byte bound.

**Missing `effects` means unknown effects, not purity or read-only behavior.**
An explicitly empty array records no declared entries, but it is still not
proof that execution has no effects. Even an explicit `read` declaration is
App metadata, not a permission check or an OS observation.

Recovery is also App-declared guidance. `reversible` does not prove that an
inverse exists, and `compensatable` does not prove that compensation can run
or restore the original state. No declaration promises automatic rollback.

## Truthful Metadata-Only Previews

A metadata-only preview describes declarations and requested arguments. It
must not:

- execute App entrypoint or operation code;
- read object data or credentials;
- grant authority or represent a capability check as already satisfied;
- promise real file diffs, final filesystem targets, or confirmed outcomes.

Target values are **requested values**, not canonicalized or authorized final
paths, hosts, or names. Repeatable targets remain requested value lists.
Runtime-selected arguments must be explicitly marked **unresolved**; preview
must not invoke trusted resolvers or discover credentials to fill them in.
An omitted target binding does not mean that nothing will be affected.

Present declarations as App claims, not as OS-confirmed effects. Unknown
effects or unresolved targets must remain visible rather than becoming empty
success-shaped previews. Actual execution still follows normal App dispatch.

Receipts, OS-confirmed effects, and real file-diff previews are separate
contracts, not implied capabilities of this metadata.

## SDK Bindings and Compatibility

The existing generator emits optional `Operation.effects` and `Mcptool.effects`
fields using the same `Operationeffect` item type in all four bindings. Optional fields remain
optional; schema `default: "unknown"` describes recovery semantics rather
than inserting a recovery claim during serialization.

Go represents the optional effect list and optional effect strings with
pointers so omission, an explicit empty list, and explicitly supplied values
are not collapsed during JSON round-trips. Legacy operations without an
effect declaration remain unchanged, including MCP tools without effects.

The generated SDK schema-validator subset covers structural rules, not all
manifest semantic checks. Core remains responsible for accepting a valid
manifest. This addition introduces no SDK execution/preview helper, transport,
authority, object-reference parser change, or package version change.
