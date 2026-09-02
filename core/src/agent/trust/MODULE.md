# Trust Module

## Purpose

`core/src/agent/trust/` attaches immutable provenance to every byte a model can
see, and keeps that provenance separate from chat role.

Claw's agent is kernel-resident: a turn can reach processes, credentials, the
policy engine and the desktop through gated `cos_*` tools. Its context is
assembled from sources with very different authority — the compiled operator
scaffold, the owner's message, `MEMORY.md`, a Skill catalogue entry, a remote
MCP server's tool description, a fetched web page, the model's own prior text.
Providers expose only `system`/`user`/`assistant`/`tool` channels and no
per-segment metadata field, so role alone cannot tell those apart.

## Responsibilities

- Define a closed, ordered trust lattice independent of chat role.
- Enumerate every model-input source and its declared behaviour in one place.
- Guarantee trust never rises under concatenation, summarisation, truncation,
  storage, replay or re-serialisation.
- Fence non-policy content in a bounded, unforgeable data envelope.
- State in the type system that a label confers no authority.

## Key Files

| Path | Role |
| --- | --- |
| `class.rs` | `TrustClass` lattice, `least`, the parse ceiling, clamped serde |
| `source.rs` | `SourceKind` registry, `SourceProfile`, `SourceRef` |
| `projection.rs` | `PromptProjection` — the policy / prelude / instruction channel split |
| `segment.rs` | `LabeledSegment`, `ModelInput`, `SegmentManifestEntry` |
| `envelope.rs` | `encode`/`decode`, `Seal`, `render`, `parse`, the process seal |
| `authority.rs` | `Evidence`, `NoAuthority`, the provenance/authority wall |
| `mod.rs` | Composition, threat statement, journal projections |
| `../../../test/unit/agent/trust/` | Registry coverage, envelope property/fuzz, channel split, adversarial, ingestion inventory, provider compatibility, policy-source ownership, builder inventory, migration |

## Trust Classes

Least to most authoritative:

| Class | Meaning |
| --- | --- |
| `LegacyUnknown` | Unlabelled stored row or unrecognised source |
| `UntrustedExternalContent` | Web, MCP, App and tool output |
| `ModelGenerated` | Assistant turns, compression summaries, reasoning |
| `ExtensionMetadata` | Skill catalogue, MCP/App tool name/description/schema |
| `UserControlledContext` | `USER.md`, `MEMORY.md`, recall, nudges, extras, an owner-writable prompt file |
| `UserInstruction` | The owner's message this turn |
| `SystemPolicy` | Compiled scaffold and a verified root-owned policy file |

## Channels

`PromptProjection::push` routes by class, not by caller intent:

| Channel | Contents | Shape |
| --- | --- | --- |
| `system` / `developer` | `SystemPolicy` only | verbatim, never fenced |
| `user` prelude | everything else, in assembly order | one fenced message per segment |
| `user` instruction | `UserInstruction` | verbatim, last |

Fencing non-policy content *inside* `system` is not sufficient and is not done:
a provider that treats `system` as the rules would still have
attacker-influenced bytes in the rules.

## Rules

- A class is minted only by `SourceKind::class`, i.e. by a trusted adapter
  naming the source it read from.
- Any label recovered from bytes — envelope header, stored column, serde — is
  clamped by `TrustClass::parse_ceiling`, so parsing can never yield
  `SystemPolicy` or `UserInstruction`.
- `SourceKind::profile` is one exhaustive `match`. A new variant does not
  compile until it declares class, persistence, projection and audit strategy;
  `registry_is_exhaustive_and_densely_indexed` then fails until it is listed in
  `SourceKind::ALL`.
- `envelope::encode` is a **fixpoint**: for any Unicode input the output
  contains no `[[`. The naive `replace("[[", …)` is unsafe — it rewrites `"[[["`
  to `"[<ZWSP>[["` and re-emits a live digraph — and the property tests cover
  arbitrary runs, crafted markers with a known nonce, and streaming chunks.
  Containment does not depend on the nonce, so nonce reuse is not a breakout.
- `bytes=` is the emitted encoded length; `parse` refuses an envelope whose
  declared length disagrees with what it read.
- A prompt file only becomes `SystemPolicy` when the file and every ancestor
  directory are root-owned and not group/other-writable, after canonicalisation
  so a symlink is judged by its resolved target. Otherwise it is
  `UserControlledContext`, because anything running as the owner can rewrite it.
- Tool *results* are labelled from the tool's registered identity via
  `SourceKind::for_tool_result` — identity is fixed by the registry before the
  model call, unlike the body. The fallback `BuiltinToolResult` is still
  untrusted: a kernel primitive faithfully reports process names, file contents
  and network responses a third party may control.
- Tool *definitions* are never fenced — that would break a provider's function
  schema. They are bounded and marker-stripped at ingestion instead.
- Fence framing costs exactly `envelope::OVERHEAD_BYTES` plus the encoded
  payload, and one segment can never exceed `MAX_SEGMENT_BYTES`, so context
  accounting is bounded rather than a surprise.

## Threat Statement

This module does not detect prompt injection and a label does not make model
output trustworthy. A malicious page, server, App or Skill can still persuade
the model to propose any text or tool call. What labelling buys is checkable:

- untrusted bytes never enter the immutable policy channel;
- untrusted bytes cannot gain trust through any transformation;
- untrusted bytes cannot forge or escape the fence around themselves;
- every model-visible byte is reconstructable from audit provenance.

The security boundary is still capabilities, guardrails, approvals and the
sandbox. None of them reads a label, and a model that ignores every marker here
gains nothing by doing so.

## Dependencies

Depends on `crate::audit_policy` for bounded, secret-safe locators,
`crate::crypto` for content digests, and `crate::session::journal` types for the
downward projections in `mod.rs`. Nothing in `caps`, `policy`, `clawd`,
`approvals` or `tools::guardrails` depends on this module — that absence is
enforced by `authority_modules_do_not_read_trust_labels`.

## Tests

```bash
cargo test -p cos agent::trust -- --test-threads=1
cargo test -p cos agent::trust::adversarial_tests -- --test-threads=1
cargo test -p cos agent::trust::projection_tests -- --test-threads=1
```

## Change Together

- A new model-visible source: `source.rs` (`SourceKind`, `ALL`, `ordinal`,
  `profile`), the ingestion adapter that reads it, and an adversarial test.
- A new trust class: `class.rs` (`ALL`, `wire_tag`, `rank`), the journal
  projections in `mod.rs`, and the lattice tests.
- A change to the envelope format: `envelope.rs`, the replay path in
  `../runtime/loop_.rs`, and `test/unit/agent/trust/envelope.rs` — including
  the fixpoint property tests.
- A change to channel placement: `projection.rs`, `../runtime/loop_.rs`
  (`resolve_projection`), and `test/unit/agent/trust/projection_shape.rs`.
- A new persisted provenance field: `../memory/sqlite_fts.rs`
  (`migrate_provenance_columns`, every message `SELECT`) and
  `test/unit/agent/trust/migration.rs`.
