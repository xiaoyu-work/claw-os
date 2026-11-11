# SDK Module

## Purpose

`claw-os-sdk/` defines the public, language-neutral app/agent contract and its
Rust, Python, Node, and Go bindings.

## Responsibilities

- Maintain versioned wire types and operation/capability schemas.
- Define the MCP-first App service contract: lifecycle, caller restrictions,
  manifest-declared tools, and capability needs.
- Admit System Agent, permitted external Agent, and authenticated CLI callers,
  never Apps or App-owned agents. Cross-App orchestration is system-Agent-owned.
- Provide public SDK calls without exposing internal broker details.
- AI chat retains its public options and error envelope while `cos ai chat`
  transports bounded prompt/system/tool/budget data to the authenticated broker.
  Hosted Apps neither resolve local session registries nor own provider access.
- Keep language bindings behaviorally compatible.
- Publish bounded App-owned events through the same live session and named IPC
  capability. Root derives identity/source; reported events never delegate work.
- Define pure, canonical App object references without discovery, authority,
  or a presentation-specific store.
- Define optional App effect metadata without treating declarations or
  metadata-only previews as authority or confirmed execution outcomes.
- Define public App-reported file-plan values without exposing private proposal
  contents or treating review fingerprints as authority.
- Own decoder validation and JSON-RPC error codes in `wire/v1/contract.json`
  plus the versioned schemas.
- Release every language binding at the same SDK SemVer through GitHub.
- Generate, rather than hand-edit, generated bindings.

## Key Files

| Path | Role |
| --- | --- |
| `wire/` | Versioned contract and code generation |
| `wire/v1/contract.json` | Generated decoder set, stable validation errors, and JSON-RPC codes |
| `wire/v1/object_ref.schema.json`, `wire/v1/object-references.md` | Object reference shape and normative identity/URI semantics |
| `wire/v1/object_ref.vectors.json` | Shared four-language object-reference conformance vectors |
| `wire/v1/manifest_extensions.vectors.json` | Shared MCP-only/mixed manifest cases retaining object metadata without exposing extra tools |
| `wire/v1/manifest.schema.json`, `wire/v1/operation-effects.md` | Optional effect declarations and truthful metadata-only preview semantics |
| `wire/v1/file_change_plan.schema.json`, `wire/v1/file-change-plans.md` | Closed App-reported staged-plan value and semantic validation boundaries |
| `wire/v1/file_change_plan.vectors.json` | Shared file-plan decoder conformance cases |
| `wire/v1/ask-claw-launcher.md` | Versioned secure desktop overlay launcher handshake |
| `wire/v1/gui_file_read.schema.json`, `wire/v1/gui-selected-read.md` | Draft closed GUI native-node read proposal/status DTOs; thin facades only, protected owner decisions/provider stay Root-owned |
| `wire/v1/manifest.schema.json` | Versioned App/MCP service, tool, access, and capability contract |
| `wire/v1/mcp_call_context.schema.json` | Gateway-authenticated caller identity, task/session correlation, and deadline |
| `rust/` | Rust public SDK |
| `rust/src/lib.rs` | Shared CLI wire/error decoder, including explicit executable selection and bounded stdin for controlled primitive business data |
| `rust/src/applet/`, `wire/v1/applet-services.md`, `wire/v1/applet-services.cases.json` | Public fixed-helper Calendar/task/telemetry client and history preflight, closed records and shared conformance cases; no OS provider or App implementation |
| `rust/notification-presentation/`, `wire/v1/notification-presentation.md` | OS-defined rendering/preferences companion contract; no App config/UI dependency, authority store or renderer feature coupling |
| `python/` | Python public SDK |
| `python/src/claw_os_sdk/events.py`, `rust/src/events.rs`, `node/src/events.ts`, `go/events.go` | App-scoped reported event publication on bounded stdin, sharing existing wire decoders |
| Four-language `channels` clients, `wire/v1/channel-ingress.md` | Exact-App managed channel status/start/stop and confirmed receipt-bound replies; no App-controlled owner/sender configuration or task submission |
| `node/` | Node public SDK |
| `go/` | Go public SDK |
| `python/src/claw_os_sdk/generated.py` | Generated Python wire bindings |
| `python/src/claw_os_sdk/mcp.py` | Manifest-bound MCP server, progress, and cooperative cancellation |
| `python/src/claw_os_sdk/kernel.py` | Explicit installed-binary stdin transport, inherited broker session, strict shared wire errors, deadlines and cancellation/reaping |
| `../.github/workflows/publish-sdk-release.yml` | GitHub-only multi-language SDK release |
| `../docs/app-platform.md` | Versioned App development artifact containing SDK/runtime/native library exports, never App support or private OS providers |

`cos-runtime/` is a separate internal package for bundled apps; public apps
must not depend on its policy/runtime internals.

SDK 0.2.0 adds the public `mail.accounts`, `mail.read`, `mail.draft` and
`mail.send` manifest vocabulary. Mailbox read/draft and sender rights remain
independent, profile-qualified named resources; metadata does not grant them.
Clients use the existing explicit-binary stdin transport, while OS Mail
execution, native-peer authentication and startup remain broker-owned.

Object resolver DTO fields remain `operation`, `id_arg`, and optional
`revision_arg`. Core binds that selector through the ordinary App command
contract, including MCP-only commands; SDK loaders merely admit the inert
`objects` field and keep tool discovery rooted in `mcp.tools`. No legacy
`session`/`serve` runtime or App-local operation fallback is restored.
Effect declarations are optional on both `operations.<command>.effects` and
`mcp.tools[].effects`, sharing the same item type, bounds and omission semantics.
MCP-only Apps need no shadow operation. Missing declarations still mean unknown
effects, not an inferred read-only classification. See the normative
object/effect contracts above.

The optional `desktop.entry`, `desktop.needs` and `desktop.entries` contract
retains separate signed GUI executables under one App identity. Every named
entry requires `entry`, one literal `exec`, localized `name`, and its own
`needs`; optional `panel_applet` is presentation only. Explicit default GUI
needs, including `[]`, never recover the legacy operation union; omission does.
Named selection is `cos app <id> --gui:<name>`, not an executable/path argument.
All four generated manifest types retain these fields, including Go's
presence-sensitive default needs. See
[the GUI contract](../docs/app-development.md#multiple-gui-entries-under-one-app-identity).
Wire/ABI 1 is unchanged; source changes alone publish no new SDK or platform.

`wire/v1/owner_path_need.schema.json` and `owner_path.vectors.json` define
bounded `scope: {kind: "owner-path", path: "Documents/**"}` requests. The public
four-language `manifest` helpers validate syntax and provide lexical previews,
without environment/filesystem access, grants or mounts. MCP loaders check the
binding on every declared operation/MCP/default/named-GUI surface. Root supplies
the authenticated owner home, rechecks canonical containment and retains normal
protected-resource checks and exact-owner capability approval. A preview is
never authorization. Root resolution without authenticated context fails closed;
SDK preview arguments do not authenticate an owner.

The additive GUI-only `optional_needs` arrays use the existing Need/localized
purpose models on the default desktop and each named entry. They default empty,
never weaken mandatory needs, and are included in authenticated review/digest
and deny inventory. Missing or denied optional rights do not block startup or
prompt: Root admits only existing authenticated parent/reusable exact-owner
authority and retains policy, provenance, lifetime and checked retirement.
All four generated bindings preserve the arrays; pure owner-path validation
also checks them. Root startup indexes in
`COS_APP_OPTIONAL_NEEDS_UNGRANTED_JSON` are presentation only. Existing live
policy/provider errors remain authoritative; no dynamic widening is implied.

The draft GUI `request_file_read(node)` / `file_read_status(selection_id, node)`
facades (idiomatic names in Node/Go) use the ordinary controlled stdin transport
and shared generated validators. A proposed native FUSE inode, pending status
or approval ID grants no content. Root alone binds exact owner/window/file,
consumes protected review once and installs a separate finite read lease,
without widening the startup/parent grant. No path, owner, window, boolean
approval or raw source FD crosses the facade. See the draft contract above;
this is not SDK publication, an artifact/pin adoption, a trusted picker UI or
production write/watch support.

## Dependencies

Wire schemas and `wire/v1/contract.json` are the source of truth. Core, MCP,
and every language SDK consume them.
Serialization changes stay backwards compatible unless introduced under a new
wire version.
JSON Schema integers use mathematical semantics: finite values with no
fractional component, including `1.0` and exponent notation. Type validation
runs before schema minimum/maximum checks in every generated decoder. Wire
number lexemes are preserved and evaluated as exact decimal rationals before
conversion; u64-domain Node values materialize as `bigint` when necessary.
Unrestricted payloads use `serde_json::Value` (Rust), `Decimal`/`WireDecimal` plus
`encode_wire_json` (Python), `WireDecimal`/`bigint` plus `stringifyWireJson`
(Node), and `json.Number` with `encoding/json` (Go).

All GUI bindings preserve their existing `open_agent_overlay` signatures but
delegate to the fixed `/usr/local/bin/cos-ask-claw-launcher`. The helper uses a
versioned, peer-credential-authenticated AF_UNIX protocol and exclusively owns the secure Agent UI
handoff; bindings do not invoke `cos-agent-ui`, consult `PATH`, or put hints in
argv/environment/files.

The Rust controlled-primitive stdin transport captures diagnostics by default.
Its explicit human-terminal variant only inherits an already-connected stderr
terminal; it never creates a TTY, changes identity, or bypasses the OS bootstrap
guards. Capture uses it for direct CLI compatibility, never for MCP calls.
Installed Settings uses the explicit-binary variant for `/usr/local/bin/cos`;
it does not depend on launcher PATH or a process-wide environment mutation.
Generic SDK clients still honor `CLAW_COS_BIN` and otherwise resolve `cos` on PATH.
Media Player uses `cos_call_json_async_with_binary`: it shares the strict wire
decoder and fixed binary selection, but dropping an MCP call kills/reaps the
CLI child. The OS deadline and fresh grant gate still govern undispatched work;
an already accepted playback action cannot be rolled back by cancellation.
Notifications uses `cos_call_json_async_with_stdin_binary` to keep bounded
business text off argv while retaining explicit executable selection, shared
wire errors and cancellation/reaping. It does not convey authority in stdin.
The Python equivalent is
`kernel.call_json_with_stdin_binary(binary, args, data, deadline_unix_ms=...,
check_cancelled=...)`. Callers provide the complete primitive argv and pass
the MCP context's cancellation check, not identity metadata or a new session.
It uses the existing wire decoder and never mutates PATH/environment.
Cancelled or timed-out accepted mutations are not automatically retried.

## Tests

Rust SDK unit tests mirror `rust/src/` under `rust/test/unit/`; production files
only contain cfg(test) include declarations.

Regenerate from this directory after wire changes:

```bash
cd claw-os-sdk
python3 wire/codegen.py
python3 wire/codegen.py --check
```

The generator writes the four SDK bindings plus the core and Rust SDK MCP
JSON-RPC constant modules, leaving unchanged outputs untouched. Add `--sdk-only`
to regenerate or check only files inside this SDK when the core output is
owned by a separate integration task:

```bash
python3 wire/codegen.py --sdk-only
python3 wire/codegen.py --sdk-only --check
```

Object-reference helper tests share `wire/v1/object_ref.vectors.json`.
The MCP tests in each language also consume
`wire/v1/manifest_extensions.vectors.json`: object/effect metadata must not
replace MCP tools, expose CLI-only arguments in tool schemas, or execute a
resolver while loading/listing a service. `wire/test_codegen.py` verifies that
SDK-only codegen checks exclude a missing core output without writing it.
From the SDK root, focused checks are:

```bash
python3 wire/codegen.py --check
cargo test -p claw-os-sdk --lib objects:: -- --test-threads=1
PYTHONPATH=python/src python3 -m pytest -q \
  python/src/claw_os_sdk/test_objects.py python/src/claw_os_sdk/test_wire.py \
  python/src/claw_os_sdk/test_mcp.py wire/test_codegen.py
(cd node && node node_modules/typescript/bin/tsc -p tsconfig.test.json \
  && node --test dist-test/objects.test.js dist-test/wire.test.js dist-test/mcp.test.js)
(cd go && go test -count=1 ./... -run 'ObjectReference|SharedObjectAndEffectMetadata')
```

File-plan schema checks use the existing wire conformance tests. From the SDK
root:

```bash
python3 wire/codegen.py --check
cargo test -p claw-os-sdk --lib generated_tests::file_change_plan
PYTHONPATH=python/src python3 -m pytest -q python/src/claw_os_sdk/test_wire.py -k file_change_plan
(cd node && node node_modules/typescript/bin/tsc -p tsconfig.test.json \
  && node --test --test-name-pattern="file change plan" dist-test/wire.test.js)
(cd go && go test -count=1 ./... -run FileChangePlan)
```

For broader affected-language checks, from the repository root:

```bash
PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \
  python3 -m pytest -q claw-os-sdk/python/src
cargo test -p claw-os-sdk
```
